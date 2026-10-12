//! Write-ahead head publication subordinate to the existing admitted journal.
//! The intent remains after success or failure until content-validated recovery. A later fsync
//! or a recreated writer never clears it. Recovery never changes the original write disposition.

use super::{
    NativeTransactionLease,
    records::{classify, prefix},
};
use crate::{
    activation_journal::{
        ActivationJournalState, NativeOperationKind, RecoverySelection, RetainedNativeOutcome,
    },
    installation::validate_successor,
    native_deployment::{
        bindings::{self, digest},
        contracts::*,
        publication::FreshPublicationInode,
    },
    native_registration::{decode_registration_head, writer::RegistrationPublicationStorage},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsStr},
    fs::File,
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    sync::Mutex,
};

/// Narrow native synchronization edge, permitting injected I/O failures without a production switch.
pub trait NativeRecordSync: Send + Sync {
    /// Synchronizes the exact retained fresh file, including the native full-sync requirement.
    fn sync_file(&self, file: &File) -> Result<(), NativeFailure>;
    /// Synchronizes the retained registration parent after publication or recovery.
    fn sync_parent(&self, parent: &File) -> Result<(), NativeFailure>;
}
/// Native file/parent synchronization. No pathname is reopened.
#[derive(Default)]
pub struct SystemRecordSync;
impl NativeRecordSync for SystemRecordSync {
    fn sync_file(&self, file: &File) -> Result<(), NativeFailure> {
        file.sync_all().map_err(|_| NativeFailure::IoUnavailable)?;
        #[cfg(target_os = "macos")]
        // SAFETY: F_FULLFSYNC acts on the owned descriptor without pointer arguments.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        Ok(())
    }
    fn sync_parent(&self, parent: &File) -> Result<(), NativeFailure> {
        parent.sync_all().map_err(|_| NativeFailure::IoUnavailable)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HeadPublicationIntent {
    pub(super) schema_version: u16,
    pub(super) key: AccountBuildKey,
    pub(super) request: NativeRequestId,
    #[serde(with = "digest")]
    pub(super) before_sha256: [u8; 32],
    #[serde(with = "digest")]
    pub(super) after_sha256: [u8; 32],
    pub(super) installation_high_water: u64,
    pub(super) data_high_water: u64,
    pub(super) journal_state: ActivationJournalState,
    pub(super) journal_outcome: Option<RetainedNativeOutcome>,
    #[serde(with = "digest")]
    pub(super) journal_sha256: [u8; 32],
    pub(super) staging_name: String,
    pub(super) staging: Option<StagingIdentity>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StagingIdentity {
    device: u64,
    inode: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallationAllocation {
    pub(super) schema_version: u16,
    pub(super) key: AccountBuildKey,
    pub(super) request: NativeRequestId,
    pub(super) candidate: VerifiedCandidateId,
    pub(super) installation: InstallationId,
    #[serde(with = "crate::native_deployment::bindings::identity")]
    pub(super) server_id: [u8; 16],
    #[serde(with = "digest")]
    pub(super) account_binding_sha256: [u8; 32],
    #[serde(with = "digest")]
    pub(super) profile_sha256: [u8; 32],
    #[serde(with = "digest")]
    pub(super) publication_nonce: [u8; 32],
}
pub(super) type PreparedHead = super::record_file::BoundRecord;

/// Real protected head I/O under the same live transaction used by management and release.
/// The journal remains the request authority. The bounded .head-publication intent only records
/// one head's before/after binding and allocation marks; it cannot select an installation.
pub struct NativeHeadStorage<Sync = SystemRecordSync> {
    pub(super) lease: NativeTransactionLease,
    pub(super) sync: Sync,
    fresh: Mutex<Option<PreparedHead>>,
}
impl<S: NativeRecordSync> NativeHeadStorage<S> {
    /// Takes ownership of native serialization for the entire writer and subsequent recovery.
    pub fn new(lease: NativeTransactionLease, sync: S) -> Self {
        Self {
            lease,
            sync,
            fresh: Mutex::new(None),
        }
    }
    pub(super) fn into_lease(self) -> NativeTransactionLease {
        self.lease
    }
    /// Transfers the same journal-bound native serialization into the fresh publisher.
    /// The parent belongs to trusted native composition, not a client request.
    pub fn into_publication_scope(
        self,
        parent: &std::path::Path,
    ) -> Result<super::super::publication::PublicationScope, NativeFailure> {
        let identity = self.retained_allocation()?;
        let protection = std::sync::Arc::clone(&self.lease.records.protection);
        super::super::publication::PublicationScope::open(
            parent,
            identity,
            protection,
            Box::new(self.lease),
        )
    }
    /// Reserves a monotonic installation ID and one native random publication nonce under the journal.
    /// An existing original allocation is resolved separately, never returned as a new durable write.
    pub fn reserve_installation(
        &self,
    ) -> PublicationDisposition<super::super::publication::PublicationIdentity> {
        let prepare = || {
            self.lease.revalidate()?;
            let snapshot = self.lease.records.read_snapshot(self.lease.locks.key)?;
            let journal = self.lease.records.read_journal(self.lease.locks.key)?;
            if !matches!(
                journal.operation,
                NativeOperationKind::Install | NativeOperationKind::Stage
            ) || journal.state != crate::activation_journal::ActivationJournalState::Staging
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
            match self
                .lease
                .records
                .root
                .read(OsStr::new(&self.name("installation-allocation")), 4096)
            {
                Ok(bytes) => {
                    let previous: InstallationAllocation =
                        bindings::decode_canonical(&bytes, 4096)?;
                    if previous.request == journal.request {
                        return Err(NativeFailure::OutcomePending);
                    }
                    if previous.key != journal.key
                        || previous.server_id != snapshot.head.server_id
                        || previous.installation.0.get() > snapshot.head.installation_high_water
                    {
                        return Err(NativeFailure::UnsafeNativeState);
                    }
                }
                Err(crate::protected_directory::ProtectedDirectoryError::Missing) => (),
                Err(error) => return Err(classify(error)),
            }
            let mut next = snapshot.head.clone();
            next.generation = std::num::NonZeroU64::new(
                next.generation
                    .get()
                    .checked_add(1)
                    .ok_or(NativeFailure::LimitExceeded)?,
            )
            .ok_or(NativeFailure::LimitExceeded)?;
            next.installation_high_water = next
                .installation_high_water
                .checked_add(1)
                .ok_or(NativeFailure::LimitExceeded)?;
            let mut nonce = [0; 32];
            getrandom::fill(&mut nonce).map_err(|_| NativeFailure::NativeUnavailable)?;
            nonzero(&nonce)?;
            let allocation = InstallationAllocation {
                schema_version: 1,
                key: journal.key,
                request: journal.request,
                candidate: journal
                    .candidate_input
                    .ok_or(NativeFailure::InvalidRecord)?,
                installation: InstallationId(
                    std::num::NonZeroU64::new(next.installation_high_water)
                        .ok_or(NativeFailure::LimitExceeded)?,
                ),
                server_id: next.server_id,
                account_binding_sha256: snapshot.profile.account().sha256(),
                profile_sha256: next.profile_sha256,
                publication_nonce: nonce,
            };
            Ok((
                allocation,
                crate::native_registration::encode_registration_head(&next)?,
            ))
        };
        let (allocation, next) = match prepare() {
            Ok(prepared) => prepared,
            Err(failure) => return PublicationDisposition::NotApplied(failure),
        };
        let publish = || {
            // The durable head intent covers the allocation before its record or high-water changes.
            self.write_fresh(allocation.key, &next)?;
            let bytes = bindings::canonical(&allocation, 4096)?;
            let source = self.create_fresh(&self.name("allocation-next"), &bytes)?;
            let target = CString::new(self.name("installation-allocation"))
                .map_err(|_| NativeFailure::InvalidRecord)?;
            let parent = self.lease.records.root.file().as_raw_fd();
            self.lease.revalidate_recovery()?;
            // SAFETY: both canonical components are relative to the retained protected parent.
            if unsafe { libc::renameat(parent, source.name.as_ptr(), parent, target.as_ptr()) } != 0
            {
                return Err(NativeFailure::IoUnavailable);
            }
            self.sync.sync_parent(self.lease.records.root.file())?;
            self.sync_fresh()?;
            self.replace_head(allocation.key)?;
            self.sync_parent()?;
            self.recover()?;
            self.retained_allocation()
        };
        match publish() {
            Ok(identity) => PublicationDisposition::Durable(identity),
            Err(_) => PublicationDisposition::Indeterminate,
        }
    }
    /// Reads the original allocation only after validated recovery and native journal revalidation.
    /// Returned comparison facts do not change an earlier Indeterminate result into Durable.
    pub fn retained_allocation(
        &self,
    ) -> Result<super::super::publication::PublicationIdentity, NativeFailure> {
        self.lease.revalidate()?;
        let bytes = self
            .lease
            .records
            .root
            .read(OsStr::new(&self.name("installation-allocation")), 4096)
            .map_err(classify)?;
        let allocation: InstallationAllocation = bindings::decode_canonical(&bytes, 4096)?;
        let journal = self.lease.records.read_journal(self.lease.locks.key)?;
        let snapshot = self.lease.records.read_snapshot(self.lease.locks.key)?;
        if allocation.schema_version != 1
            || allocation.key != journal.key
            || allocation.request != journal.request
            || Some(allocation.candidate) != journal.candidate_input
            || allocation.server_id != snapshot.head.server_id
            || allocation.account_binding_sha256 != snapshot.profile.account().sha256()
            || allocation.profile_sha256 != snapshot.head.profile_sha256
            || allocation.installation.0.get() > snapshot.head.installation_high_water
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        nonzero(&allocation.publication_nonce)?;
        Ok(super::super::publication::PublicationIdentity {
            key: allocation.key,
            installation: allocation.installation,
            server_id: allocation.server_id,
            account_binding_sha256: allocation.account_binding_sha256,
            registration_profile_sha256: allocation.profile_sha256,
            publication_nonce: allocation.publication_nonce,
        })
    }
    pub(super) fn name(&self, suffix: &str) -> String {
        format!("{}.{}", prefix(self.lease.locks.key), suffix)
    }
    pub(super) fn read_head(&self) -> Result<[u8; 256], NativeFailure> {
        self.lease
            .records
            .root
            .read(OsStr::new(&self.name("registration")), 256)
            .map_err(classify)?
            .try_into()
            .map_err(|_| NativeFailure::InvalidRecord)
    }
    pub(super) fn create_fresh(
        &self,
        name: &str,
        bytes: &[u8],
    ) -> Result<PreparedHead, NativeFailure> {
        let name = CString::new(name).map_err(|_| NativeFailure::InvalidRecord)?;
        let parent = self.lease.records.root.file();
        // SAFETY: retained protected parent, bounded single component, exclusive fresh creation.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )
        };
        if fd < 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        // SAFETY: successful openat returned a new owned descriptor.
        let mut writer = unsafe { File::from_raw_fd(fd) };
        writer
            .write_all(bytes)
            .map_err(|_| NativeFailure::IoUnavailable)?;
        // SAFETY: this exclusively created inode is still owned by this writer.
        if unsafe { libc::fchmod(writer.as_raw_fd(), 0o400) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        let original = writer
            .metadata()
            .map_err(|_| NativeFailure::IoUnavailable)?;
        drop(writer);
        // All staging writers close before the peer records freshness or authenticates destination bytes.
        // SAFETY: retained parent and the exact exclusively created component, without following links.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        // SAFETY: successful openat returned a newly owned read-only descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        use std::os::unix::fs::MetadataExt;
        let current = file.metadata().map_err(|_| NativeFailure::IoUnavailable)?;
        if !current.is_file()
            || current.nlink() != 1
            || current.dev() != original.dev()
            || current.ino() != original.ino()
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        self.lease
            .records
            .protection
            .record_fresh(&FreshPublicationInode::new(parent, &file))?;
        self.lease.records.protection.verify_custody(&[&file])?;
        let (prepared, observed) =
            PreparedHead::retain_file(&self.lease.records, parent, file, name, bytes.len())?;
        if observed != bytes {
            return Err(NativeFailure::UnsafeNativeState);
        }
        self.sync.sync_file(&prepared.file)?;
        Ok(prepared)
    }
    pub(super) fn replace_record(&self, suffix: &str, bytes: &[u8]) -> Result<(), NativeFailure> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| NativeFailure::NativeUnavailable)?;
        let source = self.create_fresh(
            &self.name(&format!("record-{}", bindings::hex(&nonce))),
            bytes,
        )?;
        let target = CString::new(self.name(suffix)).map_err(|_| NativeFailure::InvalidRecord)?;
        let parent = self.lease.records.root.file().as_raw_fd();
        self.lease.revalidate_recovery()?;
        source.verify(&self.lease.records, self.lease.records.root.file())?;
        // SAFETY: fixed target and fresh bounded source, relative to the retained protected parent.
        if unsafe { libc::renameat(parent, source.name.as_ptr(), parent, target.as_ptr()) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        self.sync.sync_parent(self.lease.records.root.file())
    }
    pub(super) fn replace_cleanup_record(
        &self,
        suffix: &str,
        bytes: &[u8],
    ) -> Result<(), NativeFailure> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| NativeFailure::NativeUnavailable)?;
        let source = self.create_fresh(
            &self.name(&format!("record-{}", bindings::hex(&nonce))),
            bytes,
        )?;
        let target = CString::new(self.name(suffix)).map_err(|_| NativeFailure::InvalidRecord)?;
        let parent = self.lease.records.root.file();
        self.lease.revalidate_recovery()?;
        source.verify(&self.lease.records, parent)?;
        // SAFETY: retained protected parent and exact freshly authenticated source component.
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                source.name.as_ptr(),
                parent.as_raw_fd(),
                target.as_ptr(),
            )
        } != 0
        {
            return Err(NativeFailure::IoUnavailable);
        }
        Ok(())
    }
    pub(super) fn retire_staging(
        &self,
        intent: &HeadPublicationIntent,
    ) -> Result<(), NativeFailure> {
        if intent.staging.is_none() {
            // Creation can precede durable inode publication. Never adopt or delete that
            // unbound name. Preserve it in this journal and use a new name on the next attempt.
            let mut document = self.lease.records.read_journal_document(intent.key)?;
            document.preserve_unbound(&intent.staging_name)?;
            return self.replace_record("journal", &document.encode()?);
        }
        let name =
            CString::new(intent.staging_name.clone()).map_err(|_| NativeFailure::InvalidRecord)?;
        let parent = self.lease.records.root.file();
        // SAFETY: retained protected parent and fixed single component; no symlink following.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
                Ok(())
            } else {
                Err(NativeFailure::UnsafeNativeState)
            };
        }
        // SAFETY: successful open returned a newly owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        self.lease
            .records
            .protection
            .verify_regular_file(&file)
            .map_err(classify)?;
        self.lease.records.protection.verify_custody(&[&file])?;
        let metadata = file.metadata().map_err(|_| NativeFailure::IoUnavailable)?;
        let identity = intent
            .staging
            .as_ref()
            .ok_or(NativeFailure::UnsafeNativeState)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.dev() != identity.device
            || metadata.ino() != identity.inode
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let bytes = self
            .lease
            .records
            .root
            .read(OsStr::new(&intent.staging_name), 256)
            .map_err(classify)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if digest != intent.after_sha256 {
            return Err(NativeFailure::UnsafeNativeState);
        }
        self.lease.revalidate_recovery()?;
        // This lease excludes legitimate publishers; custody independently excludes writable aliases.
        // SAFETY: the exact protected inode was retained and checked under the shared transaction.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        Ok(())
    }
    /// Validates current durable bytes and operation-specific journal recovery before retiring the intent.
    /// Precommit allocations that the old head does not cover remain closed for explicit repair.
    /// This returns a recovery selection, never Durable for an earlier Indeterminate publication.
    pub fn recover(&self) -> Result<RecoverySelection, NativeFailure> {
        self.lease.revalidate_recovery()?;
        let root = &self.lease.records.root;
        let (bytes, retired) = match root.read(OsStr::new(&self.name("head-publication")), 4096) {
            Ok(bytes) => (bytes, false),
            Err(crate::protected_directory::ProtectedDirectoryError::Missing) => (
                root.read(OsStr::new(&self.name("head-recovered")), 4096)
                    .map_err(classify)?,
                true,
            ),
            Err(error) => return Err(classify(error)),
        };
        let intent: HeadPublicationIntent = bindings::decode_canonical(&bytes, 4096)?;
        super::journal_document::validate_staging_name(intent.key, &intent.staging_name)?;
        let mut journal = self.lease.records.read_journal(self.lease.locks.key)?;
        if intent.schema_version != 1
            || intent.key != journal.key
            || intent.request != journal.request
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        // Only this head intent can restore a phase latched by this recovery. It cannot
        // clear an unrelated administrative RecoveryRequired state or invent journal progress.
        if journal.state == ActivationJournalState::RecoveryRequired {
            journal.state = intent.journal_state;
            journal.outcome = intent.journal_outcome;
        }
        let journal_digest: [u8; 32] = Sha256::digest(journal.encode()?).into();
        if journal_digest != intent.journal_sha256 {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let current = self.read_head()?;
        let digest: [u8; 32] = Sha256::digest(current).into();
        if digest != intent.before_sha256 && digest != intent.after_sha256 {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let head = decode_registration_head(&current, intent.key)?;
        if head.installation_high_water < intent.installation_high_water
            || head.data_high_water < intent.data_high_water
        {
            return Err(NativeFailure::Indeterminate);
        }
        let snapshot = self.lease.records.read_snapshot(intent.key)?;
        let selection = journal.recovery_selection(&head, &snapshot.profile.account().sha256())?;
        if selection == RecoverySelection::RepairRequired {
            return Err(NativeFailure::Indeterminate);
        }
        let mut uncertain = journal.clone();
        uncertain.state = ActivationJournalState::RecoveryRequired;
        uncertain.outcome = None;
        self.replace_record(
            "journal",
            &self.lease.records.journal_bytes_with(&uncertain)?,
        )?;
        self.retire_staging(&intent)?;
        // Revalidate and synchronize the selected head itself, not just its directory.
        let name =
            CString::new(self.name("registration")).map_err(|_| NativeFailure::InvalidRecord)?;
        // SAFETY: fixed bounded component under the retained protected parent.
        let fd = unsafe {
            libc::openat(
                root.file().as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        // SAFETY: successful open returned a newly owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        self.lease.records.protection.verify_custody(&[&file])?;
        self.sync.sync_file(&file)?;
        self.sync.sync_parent(root.file())?;
        if !retired {
            let source = CString::new(self.name("head-publication"))
                .map_err(|_| NativeFailure::InvalidRecord)?;
            let target = CString::new(self.name("head-recovered"))
                .map_err(|_| NativeFailure::InvalidRecord)?;
            match root.read(OsStr::new(&self.name("head-recovered")), 4096) {
                Err(crate::protected_directory::ProtectedDirectoryError::Missing) => (),
                _ => return Err(NativeFailure::UnsafeNativeState),
            }
            // Keep subordinate progress as a fence while retiring the original intent.
            // SAFETY: canonical components under one retained parent and exclusive native locks.
            if unsafe {
                libc::renameat(
                    root.file().as_raw_fd(),
                    source.as_ptr(),
                    root.file().as_raw_fd(),
                    target.as_ptr(),
                )
            } != 0
            {
                return Err(NativeFailure::IoUnavailable);
            }
            self.sync.sync_parent(root.file())?;
        }
        // A failure here still leaves head-recovered authoritative, including in the live
        // filesystem after rename. Admission checks both subordinate progress names.
        self.replace_record("journal", &self.lease.records.journal_bytes_with(&journal)?)?;
        self.lease.revalidate_recovery()?;
        let name =
            CString::new(self.name("head-recovered")).map_err(|_| NativeFailure::InvalidRecord)?;
        let mut fresh = self
            .fresh
            .lock()
            .map_err(|_| NativeFailure::Indeterminate)?;
        // SAFETY: fixed protected progress component under native serialization and custody.
        if unsafe { libc::unlinkat(root.file().as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        // Head, journal, and exact staging retirement are already synchronized. The final
        // fence deletion is cleanup, not the durability commit. Do not add a fallible sync
        // after removing the last fence. A crash may restore it and require recovery again.
        fresh.take();
        Ok(selection)
    }
    fn prepare_head(
        &self,
        key: AccountBuildKey,
        bytes: &[u8; 256],
        finalizing: bool,
    ) -> Result<(), NativeFailure> {
        if key != self.lease.locks.key {
            return Err(NativeFailure::IdentityMismatch);
        }
        if finalizing {
            self.lease.revalidate_recovery()?;
            self.lease.records.require_clear_progress(key)?;
        } else {
            self.lease.revalidate()?;
        }
        let before = self.read_head()?;
        let current = decode_registration_head(&before, key)?;
        let next = decode_registration_head(bytes, key)?;
        validate_successor(&current, &next)?;
        let journal = self.lease.records.read_journal(key)?;
        if finalizing
            && (!matches!(
                journal.state,
                ActivationJournalState::Completed | ActivationJournalState::Aborted
            ) || current.active_request != Some(journal.request)
                || *bytes
                    != crate::native_registration::encode_registration_head(
                        &Self::open_matching_fence(&current, journal.request)?,
                    )?)
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let snapshot = self.lease.records.read_snapshot(key)?;
        journal.recovery_selection(&next, &snapshot.profile.account().sha256())?;
        if journal.operation == NativeOperationKind::Stage
            && next.staged_installation != current.staged_installation
        {
            let allocation = self.retained_allocation()?;
            if journal.state != ActivationJournalState::Staged
                || next.staged_installation != Some(allocation.installation)
                || next.staged_installation != journal.candidate_installation
                || allocation.installation.0.get() != current.installation_high_water
                || next.installation_high_water != current.installation_high_water
                || next.approved_installation != current.approved_installation
                || next.approved_data_generation != current.approved_data_generation
                || next.approved_descriptor_sha256 != current.approved_descriptor_sha256
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
        }
        self.lease
            .records
            .read_journal_document(key)?
            .require_staging_capacity()?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| NativeFailure::NativeUnavailable)?;
        let staging_name = self.name(&format!("head-next-{}", bindings::hex(&nonce)));
        let mut intent = HeadPublicationIntent {
            schema_version: 1,
            key,
            request: journal.request,
            before_sha256: Sha256::digest(before).into(),
            after_sha256: Sha256::digest(bytes).into(),
            installation_high_water: next.installation_high_water,
            data_high_water: next.data_high_water,
            journal_state: journal.state,
            journal_outcome: journal.outcome,
            journal_sha256: Sha256::digest(journal.encode()?).into(),
            staging_name,
            staging: None,
        };
        let intent_bytes = bindings::canonical(&intent, 4096)?;
        self.create_fresh(&self.name("head-publication"), &intent_bytes)?;
        // Durable uncertainty is armed before touching the head or its staging inode.
        self.sync.sync_parent(self.lease.records.root.file())?;
        let mut fresh = self
            .fresh
            .lock()
            .map_err(|_| NativeFailure::Indeterminate)?;
        if fresh.is_some() {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let prepared = self.create_fresh(&intent.staging_name, bytes)?;
        let metadata = prepared
            .file
            .metadata()
            .map_err(|_| NativeFailure::IoUnavailable)?;
        intent.staging = Some(StagingIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        });
        self.replace_record("head-publication", &bindings::canonical(&intent, 4096)?)?;
        *fresh = Some(prepared);
        Ok(())
    }

    fn open_matching_fence(
        head: &crate::native_registration::RegistrationHeadV2,
        request: NativeRequestId,
    ) -> Result<crate::native_registration::RegistrationHeadV2, NativeFailure> {
        if head.active_request != Some(request)
            || head.startup_barrier == crate::native_registration::StartupBarrier::RecoveryRequired
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let mut open = head.clone();
        open.generation = std::num::NonZeroU64::new(
            head.generation
                .get()
                .checked_add(1)
                .ok_or(NativeFailure::LimitExceeded)?,
        )
        .ok_or(NativeFailure::LimitExceeded)?;
        open.active_request = None;
        open.startup_barrier = crate::native_registration::StartupBarrier::Open;
        Ok(open)
    }
    /// Publishes only the original outcome and the release of its matching head fence.
    /// Terminal requests never gain ordinary mutation authority through this operation.
    pub fn finish_request(&self) -> PublicationDisposition<RetainedNativeOutcome> {
        let prepare = || {
            self.lease.revalidate_recovery()?;
            self.lease
                .records
                .require_clear_progress(self.lease.locks.key)?;
            let current = self.lease.records.read_journal(self.lease.locks.key)?;
            let terminal = if current.outcome.is_some() {
                current.clone()
            } else {
                current.transition(ActivationJournalState::Completed)?
            };
            let snapshot = self.lease.records.read_snapshot(terminal.key)?;
            super::journal_storage::require_allocation_coverage(&terminal, &snapshot.head)?;
            terminal.recovery_selection(&snapshot.head, &snapshot.profile.account().sha256())?;
            Ok((current.outcome.is_none(), terminal, snapshot.head))
        };
        let (publish_outcome, terminal, head) = match prepare() {
            Ok(prepared) => prepared,
            Err(error) => return PublicationDisposition::NotApplied(error),
        };
        if publish_outcome {
            match self.advance_journal(&terminal) {
                PublicationDisposition::Durable(()) => (),
                PublicationDisposition::NotApplied(error) => {
                    return PublicationDisposition::NotApplied(error);
                }
                PublicationDisposition::Indeterminate => {
                    return PublicationDisposition::Indeterminate;
                }
            }
        }
        let finish = || {
            if head.active_request.is_some() {
                let open = Self::open_matching_fence(&head, terminal.request)?;
                self.prepare_head(
                    head.key,
                    &crate::native_registration::encode_registration_head(&open)?,
                    true,
                )?;
                self.sync_fresh()?;
                self.replace_head(head.key)?;
                self.sync_parent()?;
                self.recover()?;
            }
            terminal.outcome.ok_or(NativeFailure::OutcomePending)
        };
        match finish() {
            Ok(outcome) => PublicationDisposition::Durable(outcome),
            Err(_) => PublicationDisposition::Indeterminate,
        }
    }
    pub(super) fn inhibit_admitted_request(&self) -> Result<(), NativeFailure> {
        self.lease.revalidate()?;
        let journal = self.lease.records.read_journal(self.lease.locks.key)?;
        if journal.state != ActivationJournalState::ActivationPrepared
            || matches!(
                journal.operation,
                NativeOperationKind::Start
                    | NativeOperationKind::Stop
                    | NativeOperationKind::Restart
            )
        {
            return Ok(());
        }
        let current = self.lease.records.read_snapshot(journal.key)?.head;
        if current.active_request == Some(journal.request)
            && current.startup_barrier == crate::native_registration::StartupBarrier::Inhibited
        {
            return Ok(());
        }
        if current.active_request.is_some()
            || current.startup_barrier != crate::native_registration::StartupBarrier::Open
        {
            return Err(NativeFailure::OutcomePending);
        }
        let mut next = current.clone();
        next.generation = std::num::NonZeroU64::new(
            current
                .generation
                .get()
                .checked_add(1)
                .ok_or(NativeFailure::LimitExceeded)?,
        )
        .ok_or(NativeFailure::LimitExceeded)?;
        next.active_request = Some(journal.request);
        next.startup_barrier = crate::native_registration::StartupBarrier::Inhibited;
        self.write_fresh(
            next.key,
            &crate::native_registration::encode_registration_head(&next)?,
        )?;
        self.sync_fresh()?;
        self.replace_head(next.key)?;
        self.sync_parent()?;
        self.recover()?;
        Ok(())
    }
}
impl<S: NativeRecordSync> RegistrationPublicationStorage for NativeHeadStorage<S> {
    fn read_current(&self, key: AccountBuildKey) -> Result<[u8; 256], NativeFailure> {
        if key != self.lease.locks.key {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.lease.revalidate()?;
        self.read_head()
    }
    fn write_fresh(&self, key: AccountBuildKey, bytes: &[u8; 256]) -> Result<(), NativeFailure> {
        self.prepare_head(key, bytes, false)
    }
    fn sync_fresh(&self) -> Result<(), NativeFailure> {
        self.lease.revalidate_recovery()?;
        let fresh = self
            .fresh
            .lock()
            .map_err(|_| NativeFailure::Indeterminate)?;
        self.sync
            .sync_file(&fresh.as_ref().ok_or(NativeFailure::UnsafeNativeState)?.file)
    }
    fn replace_head(&self, key: AccountBuildKey) -> Result<(), NativeFailure> {
        if key != self.lease.locks.key {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.lease.revalidate_recovery()?;
        let mut fresh = self
            .fresh
            .lock()
            .map_err(|_| NativeFailure::Indeterminate)?;
        let source = fresh.as_ref().ok_or(NativeFailure::UnsafeNativeState)?;
        source.verify(&self.lease.records, self.lease.records.root.file())?;
        let target =
            CString::new(self.name("registration")).map_err(|_| NativeFailure::InvalidRecord)?;
        let parent = self.lease.records.root.file().as_raw_fd();
        // SAFETY: both canonical components are relative to the same retained protected parent.
        if unsafe { libc::renameat(parent, source.name.as_ptr(), parent, target.as_ptr()) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        fresh.take();
        Ok(())
    }
    fn sync_parent(&self) -> Result<(), NativeFailure> {
        self.lease.revalidate_recovery()?;
        self.sync.sync_parent(self.lease.records.root.file())
    }
}
