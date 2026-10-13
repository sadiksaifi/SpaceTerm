use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use spaceterm_core::{identity::ServerId, operations::RequestDigest};

use super::{ServiceFailure, ServiceSupervisor};
use crate::{
    native_deployment::contracts::NativeRequestId,
    startup::{AccountBuildKey, BuildIdentity},
};

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnerEnrollment {
    request_digest: RequestDigest,
    server: ServerId,
    claim_digest: RequestDigest,
}
impl OwnerEnrollment {
    pub(super) fn validate(
        &self,
        key: AccountBuildKey,
        request: NativeRequestId,
    ) -> Result<(), ServiceFailure> {
        if self.server.bytes() == [0; 16]
            || self.claim_digest.bytes() == [0; 32]
            || self.request_digest != digest(key, request, self.server, self.claim_digest)
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        Ok(())
    }
}

impl ServiceSupervisor {
    /// Computes the canonical account-host owner request binding before issuance.
    /// This comparison value is not authority. The request must already be reserved.
    pub fn owner_enrollment_digest(
        &self,
        request: NativeRequestId,
        server: ServerId,
        claim_digest: RequestDigest,
    ) -> Result<RequestDigest, ServiceFailure> {
        let (_, _, journal) = self.read_journal()?;
        journal.reservation(request)?;
        if server.bytes() == [0; 16] || claim_digest.bytes() == [0; 32] {
            return Err(ServiceFailure::InvalidRecord);
        }
        Ok(digest(self.key, request, server, claim_digest))
    }
    /// Reads an original owner binding, including its acknowledged archive.
    /// Unlike admission digest calculation, this cannot readmit a retired request.
    /// None means an allocated request has not retained owner authority yet.
    pub fn retained_owner_enrollment_digest(
        &self,
        request: NativeRequestId,
        server: ServerId,
        claim_digest: RequestDigest,
    ) -> Result<Option<RequestDigest>, ServiceFailure> {
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if registration.server_pin().identity != server.bytes() {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let (directory, _, journal) = self.read_journal()?;
        let original = match journal.reservation(request) {
            Ok(index) => journal.reservations[index].owner.clone(),
            Err(ServiceFailure::RequestRetired) => Some(
                read_archived_owner(&directory, self.key, request)?
                    .ok_or(ServiceFailure::RequestRetired)?,
            ),
            Err(failure) => return Err(failure),
        };
        original
            .map(|original| {
                original.validate(self.key, request)?;
                if original.server != server || original.claim_digest != claim_digest {
                    return Err(ServiceFailure::RequestConflict);
                }
                Ok(original.request_digest)
            })
            .transpose()
    }
    /// Deliberately records the exact reserved account/build/server/request/claim binding.
    /// Repeating identical issuance is idempotent. Changed contents are rejected.
    /// This method never starts a server or changes deliberate Stopped intent.
    pub fn record_owner_enrollment(
        &self,
        request: NativeRequestId,
        request_digest: RequestDigest,
        server: ServerId,
        claim_digest: RequestDigest,
    ) -> Result<(), ServiceFailure> {
        let owner = OwnerEnrollment {
            request_digest,
            server,
            claim_digest,
        };
        owner.validate(self.key, request)?;
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if registration.server_pin().identity != server.bytes() {
            return Err(ServiceFailure::IdentityMismatch);
        }
        self.mutate_request(request, |journal, index| {
            if journal.reservations[index].service.is_some()
                || journal.reservations[index].install.is_some()
            {
                return Err(ServiceFailure::RequestConflict);
            }
            let retained = &mut journal.reservations[index].owner;
            match retained {
                Some(original) if original == &owner => Ok(()),
                Some(_) => Err(ServiceFailure::RequestConflict),
                None => {
                    *retained = Some(owner.clone());
                    Ok(())
                }
            }
        })
    }
    /// Read-only validation of persisted owner authority after provider reconstruction.
    /// It never reserves, initializes, issues, starts, repairs or rewrites storage.
    /// Pairing separately enforces ticket lifetime, epoch and single-use policy.
    pub fn validate_owner_enrollment(
        &self,
        request: &NativeRequestId,
        request_digest: RequestDigest,
        server: ServerId,
        claim_digest: RequestDigest,
    ) -> Result<(), ServiceFailure> {
        let registration = self.discover()?.ok_or(ServiceFailure::Missing)?;
        if registration.server_pin().identity != server.bytes() {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let (directory, _, journal) = self.read_journal()?;
        let retained = match journal.reservation(*request) {
            Ok(index) => journal.reservations[index]
                .owner
                .clone()
                .ok_or(ServiceFailure::Missing)?,
            Err(ServiceFailure::RequestRetired) => {
                read_archived_owner(&directory, self.key, *request)?
                    .ok_or(ServiceFailure::RequestRetired)?
            }
            Err(error) => return Err(error),
        };
        let expected = OwnerEnrollment {
            request_digest,
            server,
            claim_digest,
        };
        expected.validate(self.key, *request)?;
        if retained != expected {
            return Err(ServiceFailure::RequestConflict);
        }
        Ok(())
    }
}

// Acknowledgement compacts lifecycle results, not host issuance authority that a
// retained pairing reservation may still need. Each archive is a bounded immutable
// private file. Its name derives only from a provider-issued ID checked against the
// retained namespace and high-water facts; it is never new admission authority.
const OWNER_RECORD_LIMIT: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchivedOwner {
    schema: u16,
    key: AccountBuildKey,
    request: NativeRequestId,
    owner: OwnerEnrollment,
}

fn owner_record_name(request: NativeRequestId) -> String {
    use std::fmt::Write as _;
    let mut name = String::from("owner-");
    for byte in request.as_bytes() {
        let _ = write!(name, "{byte:02x}");
    }
    name
}

fn read_archived_owner(
    directory: &crate::private_filesystem::Directory,
    key: AccountBuildKey,
    request: NativeRequestId,
) -> Result<Option<OwnerEnrollment>, ServiceFailure> {
    let Some(snapshot) = directory.read(&owner_record_name(request), OWNER_RECORD_LIMIT)? else {
        return Ok(None);
    };
    let record: ArchivedOwner = super::supervisor::decode(&snapshot.bytes, OWNER_RECORD_LIMIT)?;
    if record.schema != 1 {
        return Err(ServiceFailure::UnsupportedSchema);
    }
    if record.key != key || record.request != request {
        return Err(ServiceFailure::IdentityMismatch);
    }
    record.owner.validate(key, request)?;
    Ok(Some(record.owner))
}

impl ServiceSupervisor {
    pub(super) fn reconcile_owner_archive(
        &self,
        request: NativeRequestId,
    ) -> Result<(), ServiceFailure> {
        use super::registration::{REGISTRATION_LIMIT, Registration};
        use super::supervisor::{Journal, decode};
        use crate::private_filesystem::{Directory, Failure};
        let directory =
            Directory::open_for_recovery(&self.anchor)?.ok_or(ServiceFailure::Missing)?;
        let snapshot = directory
            .read_for_recovery("requests", super::supervisor::JOURNAL_LIMIT)?
            .ok_or(ServiceFailure::UnknownRequest)?;
        let journal: Journal = decode(&snapshot.bytes, super::supervisor::JOURNAL_LIMIT)?;
        journal.validate(self.key)?;
        let index = journal.reservation(request)?;
        let owner = journal.reservations[index]
            .owner
            .as_ref()
            .ok_or(ServiceFailure::Indeterminate)?;
        let snapshot = directory
            .read_for_recovery("registration", REGISTRATION_LIMIT)?
            .ok_or(ServiceFailure::Missing)?;
        let registration: Registration = decode(&snapshot.bytes, REGISTRATION_LIMIT)?;
        registration.validate(self.key)?;
        if owner.server.bytes() != registration.server {
            return Err(ServiceFailure::IdentityMismatch);
        }
        Directory::reconcile_publication(
            &self.anchor,
            &owner_record_name(request),
            OWNER_RECORD_LIMIT,
            |bytes| {
                let record: ArchivedOwner =
                    decode(bytes, OWNER_RECORD_LIMIT).map_err(|_| Failure::Unsafe)?;
                if record.schema != 1
                    || record.key != self.key
                    || record.request != request
                    || &record.owner != owner
                {
                    return Err(Failure::Unsafe);
                }
                record
                    .owner
                    .validate(self.key, request)
                    .map_err(|_| Failure::Unsafe)
            },
        )
        .map_err(Into::into)
    }

    pub(super) fn acknowledge_owner_request(
        &self,
        request: NativeRequestId,
    ) -> Result<(), ServiceFailure> {
        let (directory, _, journal) = self.read_journal()?;
        // This check also excludes foreign and never-allocated sequence values.
        journal.sequence(request)?;
        let index = match journal.reservation(request) {
            Ok(index) => index,
            Err(ServiceFailure::RequestRetired) => {
                return read_archived_owner(&directory, self.key, request)?
                    .map(|_| ())
                    .ok_or(ServiceFailure::RequestRetired);
            }
            Err(error) => return Err(error),
        };
        let owner = journal.reservations[index]
            .owner
            .clone()
            .ok_or(ServiceFailure::Pending)?;
        let record = ArchivedOwner {
            schema: 1,
            key: self.key,
            request,
            owner: owner.clone(),
        };
        let bytes = super::supervisor::encode(&record, OWNER_RECORD_LIMIT)?;
        match directory.publish(&owner_record_name(request), &bytes, None) {
            Ok(()) => {}
            Err(crate::private_filesystem::Failure::Conflict) => {
                if read_archived_owner(&directory, self.key, request)?.as_ref() != Some(&owner) {
                    return Err(ServiceFailure::RequestConflict);
                }
            }
            Err(error) => {
                if error == crate::private_filesystem::Failure::Indeterminate {
                    self.indeterminate
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                return Err(error.into());
            }
        }
        // Publication and its directory barrier precede removal from the journal.
        // A crash between them leaves two identical bindings, not missing authority.
        match self.mutate_request(request, |journal, index| {
            if journal.reservations[index].owner.as_ref() != Some(&owner) {
                return Err(ServiceFailure::RequestConflict);
            }
            journal.reservations.remove(index);
            Ok(())
        }) {
            Err(ServiceFailure::RequestRetired) => {
                if read_archived_owner(&directory, self.key, request)?.as_ref() == Some(&owner) {
                    Ok(())
                } else {
                    Err(ServiceFailure::RequestConflict)
                }
            }
            result => result,
        }
    }
}

fn digest(
    key: AccountBuildKey,
    request: NativeRequestId,
    server: ServerId,
    claim: RequestDigest,
) -> RequestDigest {
    let mut hasher = Sha256::new();
    hasher.update(b"spaceterm-account-owner-enrollment-v1\0");
    hasher.update(key.account_uid.to_be_bytes());
    hasher.update([match key.build_identity {
        BuildIdentity::Release => 1,
        BuildIdentity::Preflight => 2,
        BuildIdentity::Development => 3,
    }]);
    hasher.update(request.as_bytes());
    hasher.update(server.bytes());
    hasher.update(claim.bytes());
    RequestDigest::from_bytes(hasher.finalize().into())
}
