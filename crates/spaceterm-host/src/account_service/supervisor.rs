use std::{
    ffi::{CStr, OsString},
    fmt,
    os::unix::ffi::OsStringExt,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};

use super::owner_enrollment::OwnerEnrollment;
use super::{
    RegisteredServer, ServiceFailure,
    registration::{REGISTRATION_LIMIT, Registration, validate_path},
};
use crate::{
    native_deployment::contracts::NativeRequestId,
    private_filesystem::{Directory, Failure, Snapshot},
    startup::{AccountBuildKey, BuildIdentity},
};

pub(super) const JOURNAL_LIMIT: usize = 256 * 1024;
pub(super) const MAX_REQUESTS: usize = 256;
const CAS_ATTEMPTS: usize = 64;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    pub(super) schema: u16,
    pub(super) key: AccountBuildKey,
    pub(super) namespace: [u8; 8],
    pub(super) high_water: u64,
    pub(super) reservations: Vec<Reservation>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reservation {
    pub(super) sequence: u64,
    pub(super) owner: Option<OwnerEnrollment>,
    pub(super) acknowledged: bool,
}

impl Journal {
    fn new(key: AccountBuildKey) -> Result<Self, ServiceFailure> {
        let mut namespace = [0; 8];
        getrandom::fill(&mut namespace).map_err(|_| ServiceFailure::StorageUnavailable)?;
        if namespace == [0; 8] {
            return Err(ServiceFailure::StorageUnavailable);
        }
        Ok(Self {
            schema: 1,
            key,
            namespace,
            high_water: 0,
            reservations: Vec::new(),
        })
    }
    fn validate(&self, key: AccountBuildKey) -> Result<(), ServiceFailure> {
        if self.schema != 1 {
            return Err(ServiceFailure::UnsupportedSchema);
        }
        if self.key != key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        if self.namespace == [0; 8]
            || self.reservations.len() > MAX_REQUESTS
            || !self
                .reservations
                .windows(2)
                .all(|p| p[0].sequence < p[1].sequence)
            || self.reservations.iter().any(|r| {
                r.sequence == 0
                    || r.sequence > self.high_water
                    || (r.acknowledged && r.owner.is_none())
            })
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        for reservation in &self.reservations {
            if let Some(owner) = &reservation.owner {
                owner.validate(key, self.request(reservation.sequence)?)?;
            }
        }
        Ok(())
    }
    pub(super) fn request(&self, sequence: u64) -> Result<NativeRequestId, ServiceFailure> {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&self.namespace);
        bytes[8..].copy_from_slice(&sequence.to_be_bytes());
        NativeRequestId::new(bytes).map_err(|_| ServiceFailure::InvalidRecord)
    }
    pub(super) fn sequence(&self, request: NativeRequestId) -> Result<u64, ServiceFailure> {
        if request.as_bytes()[..8] != self.namespace {
            return Err(ServiceFailure::UnknownRequest);
        }
        let sequence = u64::from_be_bytes(
            request.as_bytes()[8..]
                .try_into()
                .map_err(|_| ServiceFailure::UnknownRequest)?,
        );
        if sequence == 0 || sequence > self.high_water {
            return Err(ServiceFailure::UnknownRequest);
        }
        Ok(sequence)
    }
    pub(super) fn reservation(&self, request: NativeRequestId) -> Result<usize, ServiceFailure> {
        let sequence = self.sequence(request)?;
        self.reservations
            .binary_search_by_key(&sequence, |r| r.sequence)
            .map_err(|_| ServiceFailure::RequestRetired)
    }
}

/// Current-account discovery and durable request authority for the compiled identity.
///
/// This retained provider has no public account, directory or identity override.
/// Lifecycle mutation integration is separate from this reservation implementation.
pub struct ServiceSupervisor {
    pub(super) key: AccountBuildKey,
    pub(super) anchor: PathBuf,
    pub(super) indeterminate: AtomicBool,
}

impl fmt::Debug for ServiceSupervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServiceSupervisor(..)")
    }
}

impl ServiceSupervisor {
    /// Resolves the actual nonroot account and validated compiled build identity.
    /// It does not create files or contact a service manager.
    pub fn current() -> Result<Self, ServiceFailure> {
        let (uid, home) = current_account()?;
        let build = match env!("SPACETERM_HOST_BUILD") {
            "release" => BuildIdentity::Release,
            "preflight" => BuildIdentity::Preflight,
            "development" => BuildIdentity::Development,
            _ => return Err(ServiceFailure::UnsupportedHost),
        };
        let name = match build {
            BuildIdentity::Release => "release",
            BuildIdentity::Preflight => "preflight",
            BuildIdentity::Development => "development",
        };
        Ok(Self {
            key: AccountBuildKey {
                account_uid: uid,
                build_identity: build,
            },
            anchor: home.join(".spaceterm-services").join(name),
            indeterminate: AtomicBool::new(false),
        })
    }
    /// Returns the current account and compiled identity.
    pub fn key(&self) -> AccountBuildKey {
        self.key
    }
    /// Reads an existing fixed registration and opens its existing private storage.
    /// Missing registration never causes key, directory or database creation.
    pub fn discover(&self) -> Result<Option<RegisteredServer>, ServiceFailure> {
        self.available()?;
        let Some(anchor) = Directory::open(&self.anchor)? else {
            return Ok(None);
        };
        let Some(snapshot) = anchor.read("registration", REGISTRATION_LIMIT)? else {
            return Ok(None);
        };
        let record: Registration = decode(&snapshot.bytes, REGISTRATION_LIMIT)?;
        record.validate(self.key)?;
        let directory = Directory::open(&record.data)?.ok_or(ServiceFailure::Missing)?;
        directory.verify_established()?;
        Ok(Some(RegisteredServer { record, directory }))
    }
    /// Reserves a durable, provider-issued non-reused ID before submission.
    /// Reservation creates no identity, artifact, service or authorization effect.
    /// At capacity, the oldest never-submitted reservation can be retired. A caller
    /// retaining it must handle RequestRetired; no accepted outcome is evicted.
    pub fn allocate_request(&self) -> Result<NativeRequestId, ServiceFailure> {
        self.available()?;
        let directory = Directory::create(&self.anchor)?;
        for _ in 0..CAS_ATTEMPTS {
            let snapshot = directory.read("requests", JOURNAL_LIMIT)?;
            let mut journal = match &snapshot {
                Some(snapshot) => {
                    let journal: Journal = decode(&snapshot.bytes, JOURNAL_LIMIT)?;
                    journal.validate(self.key)?;
                    journal
                }
                None => Journal::new(self.key)?,
            };
            if journal.reservations.len() == MAX_REQUESTS {
                // Reservations have accepted no action. Under pressure retire the
                // oldest unsubmitted ID, never an unacknowledged original outcome.
                // Its sequence remains below the permanent high-water mark, so a
                // delayed submitter gets RequestRetired rather than fresh authority.
                let Some(unsubmitted) = journal.reservations.iter().position(|r| r.owner.is_none())
                else {
                    return Err(ServiceFailure::LimitExceeded);
                };
                journal.reservations.remove(unsubmitted);
            }
            journal.high_water = journal
                .high_water
                .checked_add(1)
                .ok_or(ServiceFailure::LimitExceeded)?;
            let request = journal.request(journal.high_water)?;
            journal.reservations.push(Reservation {
                sequence: journal.high_water,
                owner: None,
                acknowledged: false,
            });
            match self.publish_journal(&directory, &journal, snapshot.as_ref()) {
                Ok(()) => return Ok(request),
                Err(ServiceFailure::RequestConflict) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(ServiceFailure::Pending)
    }
    /// Retires a completed owner result after synchronizing its separate authority record.
    /// The namespace/high-water record permanently prevents readmission of the ID.
    /// Pairing can still revalidate the original issuance without consuming an active slot.
    /// Unsubmitted reservations are not completed outcomes and cannot be acknowledged.
    pub fn acknowledge_request(&self, request: NativeRequestId) -> Result<(), ServiceFailure> {
        self.acknowledge_owner_request(request)
    }
    pub(super) fn read_journal(&self) -> Result<(Directory, Snapshot, Journal), ServiceFailure> {
        self.available()?;
        let directory = Directory::open(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
        let snapshot = directory
            .read("requests", JOURNAL_LIMIT)?
            .ok_or(ServiceFailure::UnknownRequest)?;
        let journal: Journal = decode(&snapshot.bytes, JOURNAL_LIMIT)?;
        journal.validate(self.key)?;
        Ok((directory, snapshot, journal))
    }
    pub(super) fn mutate_request(
        &self,
        request: NativeRequestId,
        mutation: impl Fn(&mut Journal, usize) -> Result<(), ServiceFailure>,
    ) -> Result<(), ServiceFailure> {
        for _ in 0..CAS_ATTEMPTS {
            let (directory, snapshot, mut journal) = self.read_journal()?;
            let index = journal.reservation(request)?;
            mutation(&mut journal, index)?;
            match self.publish_journal(&directory, &journal, Some(&snapshot)) {
                Ok(()) => return Ok(()),
                Err(ServiceFailure::RequestConflict) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(ServiceFailure::Pending)
    }
    fn publish_journal(
        &self,
        directory: &Directory,
        journal: &Journal,
        expected: Option<&Snapshot>,
    ) -> Result<(), ServiceFailure> {
        let bytes = encode(journal, JOURNAL_LIMIT)?;
        if expected.is_some_and(|snapshot| snapshot.bytes.as_slice() == bytes.as_slice()) {
            return Ok(());
        }
        let result = directory.publish("requests", &bytes, expected);
        if result == Err(Failure::Indeterminate) {
            self.indeterminate.store(true, Ordering::Release);
        }
        result.map_err(Into::into)
    }
    pub(super) fn available(&self) -> Result<(), ServiceFailure> {
        if self.indeterminate.load(Ordering::Acquire) {
            Err(ServiceFailure::Indeterminate)
        } else {
            Ok(())
        }
    }
}

pub(super) fn encode<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, ServiceFailure> {
    let mut value = serde_json::to_value(value).map_err(|_| ServiceFailure::InvalidRecord)?;
    value.sort_all_objects();
    let bytes = serde_json::to_vec(&value).map_err(|_| ServiceFailure::InvalidRecord)?;
    if bytes.len() > limit {
        return Err(ServiceFailure::LimitExceeded);
    }
    Ok(bytes)
}
pub(super) fn decode<T: Serialize + serde::de::DeserializeOwned>(
    bytes: &[u8],
    limit: usize,
) -> Result<T, ServiceFailure> {
    if bytes.len() > limit {
        return Err(ServiceFailure::LimitExceeded);
    }
    let value: T = serde_json::from_slice(bytes).map_err(|_| ServiceFailure::InvalidRecord)?;
    if encode(&value, limit)? != bytes {
        return Err(ServiceFailure::InvalidRecord);
    }
    Ok(value)
}

fn current_account() -> Result<(u32, PathBuf), ServiceFailure> {
    // SAFETY: credential reads have no preconditions and do not change authority.
    let (uid, effective) = unsafe { (libc::getuid(), libc::geteuid()) };
    if uid == 0 || effective == 0 {
        return Err(ServiceFailure::RootAccount);
    }
    if uid != effective || uid == u32::MAX {
        return Err(ServiceFailure::IdentityMismatch);
    }
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut result = std::ptr::null_mut();
    let mut account = std::mem::MaybeUninit::<libc::passwd>::uninit();
    // SAFETY: writable passwd and buffer live through extraction. getpwuid_r
    // bounds all writes by the supplied length. result is checked before use.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            account.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return Err(ServiceFailure::StorageUnavailable);
    }
    let account = unsafe { account.assume_init() };
    if account.pw_dir.is_null() || account.pw_uid != uid {
        return Err(ServiceFailure::IdentityMismatch);
    }
    let home = PathBuf::from(OsString::from_vec(
        unsafe { CStr::from_ptr(account.pw_dir) }
            .to_bytes()
            .to_vec(),
    ));
    validate_path(&home)?;
    // Never manufacture a missing account home, including an unavailable volume.
    let facts = std::fs::symlink_metadata(&home).map_err(|_| ServiceFailure::StorageUnavailable)?;
    use std::os::unix::fs::MetadataExt as _;
    if !facts.is_dir() || facts.uid() != uid {
        return Err(ServiceFailure::UnsafeStorage);
    }
    Ok((uid, home))
}
