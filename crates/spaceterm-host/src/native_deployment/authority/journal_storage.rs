//! Journal admission and phase publication under the one native transaction authority.

use std::{
    ffi::{CString, OsStr},
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    NativeHeadStorage, NativeRecordSync, NativeRegistrationStore,
    records::{classify, prefix},
};
use crate::{
    activation_journal::{
        ActivationJournalRecord, ActivationJournalState as Phase, NativeOperationKind,
    },
    native_deployment::{
        bindings::{self, digest, optional_digest},
        contracts::*,
        publication::StagedInstallation,
    },
    native_registration::StartupBarrier,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalPublicationIntent {
    pub(super) schema_version: u16,
    pub(super) key: AccountBuildKey,
    pub(super) request: NativeRequestId,
    #[serde(with = "optional_digest")]
    pub(super) before_sha256: Option<[u8; 32]>,
    #[serde(with = "digest")]
    pub(super) after_sha256: [u8; 32],
    #[serde(default, with = "optional_digest")]
    pub(super) administrative_assessment: Option<[u8; 32]>,
}

pub(super) fn has_journal_intent(
    store: &NativeRegistrationStore,
    key: AccountBuildKey,
    request: NativeRequestId,
) -> Result<bool, NativeFailure> {
    let bytes = match store.root.read(
        OsStr::new(&format!("{}.journal-publication", prefix(key))),
        4096,
    ) {
        Ok(bytes) => bytes,
        Err(crate::protected_directory::ProtectedDirectoryError::Missing) => return Ok(false),
        Err(error) => return Err(classify(error)),
    };
    let intent: JournalPublicationIntent = bindings::decode_canonical(&bytes, 4096)?;
    if intent.schema_version != 1 || intent.key != key || intent.request != request {
        return Err(NativeFailure::IdentityMismatch);
    }
    Ok(true)
}

pub(super) fn require_allocation_coverage(
    journal: &ActivationJournalRecord,
    head: &crate::native_registration::RegistrationHeadV2,
) -> Result<(), NativeFailure> {
    if journal.original_installation.0.get() > head.installation_high_water
        || journal.original_data_generation.get() > head.data_high_water
        || journal
            .candidate_installation
            .is_some_and(|id| id.0.get() > head.installation_high_water)
        || journal
            .candidate_data_generation
            .is_some_and(|id| id.get() > head.data_high_water)
    {
        return Err(NativeFailure::Indeterminate);
    }
    Ok(())
}

impl<S: NativeRecordSync> NativeHeadStorage<S> {
    pub(super) fn admit_journal(
        &self,
        existing: Option<&ActivationJournalRecord>,
    ) -> Result<(), NativeFailure> {
        self.lease.revalidate_recovery()?;
        let authorized = self
            .lease
            .authorization
            .as_ref()
            .ok_or(NativeFailure::AuthorizationUnavailable)?;
        let request = authorized.journal();
        let initial = match request.operation {
            NativeOperationKind::Install | NativeOperationKind::Stage => Phase::Staging,
            _ => Phase::ActivationPrepared,
        };
        if !self.lease.admitting
            || request.state != initial
            || request.outcome.is_some()
            || request.candidate_data_generation.is_some()
            || matches!(
                request.operation,
                NativeOperationKind::Install | NativeOperationKind::Stage
            ) && request.candidate_installation.is_some()
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let snapshot = self.lease.records.read_snapshot(request.key)?;
        let head = &snapshot.head;
        if head.startup_barrier != StartupBarrier::Open || head.active_request.is_some() {
            return Err(NativeFailure::OutcomePending);
        }
        if request.expected_registration_generation != head.generation
            || request.expected_lifecycle_generation != head.lifecycle_generation
            || request.original_installation != head.approved_installation
            || request.original_data_generation != head.approved_data_generation
        {
            return Err(NativeFailure::StaleAssessment);
        }
        // Admission compares the assessed open head. Recovery instead checks an already
        // admitted request's phase and fence; those are not interchangeable validations.
        if request.server_id != head.server_id
            || request.account_binding_sha256 != snapshot.profile.account().sha256()
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        if request.operation == NativeOperationKind::Activate {
            crate::installation::plan_activation(head, request)?;
        } else if !matches!(
            request.operation,
            NativeOperationKind::Start | NativeOperationKind::Stop
        ) && request.desired_state != head.desired_state
        {
            return Err(NativeFailure::StaleAssessment);
        }
        // Reserve retained-outcome capacity before creating any publication intent.
        if let Some(previous) = existing {
            require_allocation_coverage(previous, head)?;
            previous.recovery_selection(head, &snapshot.profile.account().sha256())?;
        }
        self.lease.records.journal_bytes_with(request)?;
        self.publish_journal(existing, request)
            .and_then(|()| self.recover_journal().map(|_| ()))
            .map_err(|_| NativeFailure::Indeterminate)?;
        Ok(())
    }

    /// Persists a validated phase before its owning native integration performs the effects.
    /// This writer does not manufacture quiescence, migration, or runtime-readiness evidence.
    /// Install/Stage result fields can only change through record_staged.
    pub fn advance_journal(&self, next: &ActivationJournalRecord) -> PublicationDisposition<()> {
        let prepare = || {
            self.lease.revalidate()?;
            let current = self.lease.records.read_journal(self.lease.locks.key)?;
            current.same_request(next)?;
            if current.candidate_installation != next.candidate_installation
                || current.candidate_manifest_sha256 != next.candidate_manifest_sha256
                || current.candidate_data_generation != next.candidate_data_generation
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
            if current.transition(next.state)? != *next {
                return Err(NativeFailure::UnsafeNativeState);
            }
            let snapshot = self.lease.records.read_snapshot(current.key)?;
            if matches!(next.state, Phase::Completed | Phase::Aborted) {
                require_allocation_coverage(next, &snapshot.head)?;
            }
            next.recovery_selection(&snapshot.head, &snapshot.profile.account().sha256())?;
            if next.operation == NativeOperationKind::Stage
                && next.state == Phase::Completed
                && snapshot.head.staged_installation != next.candidate_installation
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
            Ok(current)
        };
        let current = match prepare() {
            Ok(current) => current,
            Err(error) => return PublicationDisposition::NotApplied(error),
        };
        match self
            .publish_journal(Some(&current), next)
            .and_then(|()| self.recover_journal().map(|_| ()))
        {
            Ok(()) => PublicationDisposition::Durable(()),
            Err(_) => PublicationDisposition::Indeterminate,
        }
    }

    /// Binds the actual immutable destination to the original reservation before publishing Staged.
    /// A caller-selected ID or descriptor digest cannot substitute for retained publication custody.
    pub fn record_staged(&self, staged: &StagedInstallation) -> PublicationDisposition<()> {
        let prepare = || {
            self.lease.revalidate()?;
            staged.revalidate()?;
            let allocation = self.retained_allocation()?;
            let descriptor = staged.descriptor();
            if descriptor.key != allocation.key
                || descriptor.installation != allocation.installation
                || descriptor.server_id != allocation.server_id
                || descriptor.account_binding_sha256 != allocation.account_binding_sha256
                || descriptor.registration_profile_sha256 != allocation.registration_profile_sha256
                || descriptor.publication_nonce != allocation.publication_nonce
            {
                return Err(NativeFailure::IdentityMismatch);
            }
            let current = self.lease.records.read_journal(self.lease.locks.key)?;
            if current.state != Phase::Staging
                || !matches!(
                    current.operation,
                    NativeOperationKind::Install | NativeOperationKind::Stage
                )
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
            let mut result = current.clone();
            result.candidate_installation = Some(descriptor.installation);
            result.candidate_manifest_sha256 = Some(descriptor.manifest_sha256);
            let next = result.transition(Phase::Staged)?;
            Ok((current, next))
        };
        let (current, next) = match prepare() {
            Ok(records) => records,
            Err(error) => return PublicationDisposition::NotApplied(error),
        };
        match self
            .publish_journal(Some(&current), &next)
            .and_then(|()| self.recover_journal().map(|_| ()))
        {
            Ok(()) => PublicationDisposition::Durable(()),
            Err(_) => PublicationDisposition::Indeterminate,
        }
    }

    /// Produces only the original request's reserved staged selection, without another allocation.
    pub fn propose_staging(
        &self,
        staged: &StagedInstallation,
    ) -> Result<crate::native_registration::RegistrationHeadV2, NativeFailure> {
        self.lease.revalidate()?;
        staged.revalidate()?;
        let allocation = self.retained_allocation()?;
        let descriptor = staged.descriptor();
        let journal = self.lease.records.read_journal(self.lease.locks.key)?;
        if journal.operation != NativeOperationKind::Stage
            || journal.state != Phase::Staged
            || journal.candidate_installation != Some(descriptor.installation)
            || journal.candidate_manifest_sha256 != Some(descriptor.manifest_sha256)
            || descriptor.installation != allocation.installation
            || descriptor.publication_nonce != allocation.publication_nonce
            || descriptor.account_binding_sha256 != allocation.account_binding_sha256
            || descriptor.registration_profile_sha256 != allocation.registration_profile_sha256
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        let snapshot = self.lease.records.read_snapshot(journal.key)?;
        crate::installation::propose_staging(
            &snapshot.head,
            allocation.installation,
            *staged.descriptor_sha256(),
        )
    }

    pub(super) fn publish_journal(
        &self,
        before: Option<&ActivationJournalRecord>,
        next: &ActivationJournalRecord,
    ) -> Result<(), NativeFailure> {
        let bytes = self.lease.records.journal_bytes_with(next)?;
        let intent = JournalPublicationIntent {
            schema_version: 1,
            key: next.key,
            request: next.request,
            before_sha256: before
                .map(|record| {
                    self.lease
                        .records
                        .read_journal_bytes(record.key)
                        .map(|bytes| Sha256::digest(bytes).into())
                })
                .transpose()?,
            after_sha256: Sha256::digest(&bytes).into(),
            administrative_assessment: None,
        };
        self.create_fresh(
            &self.name("journal-publication"),
            &bindings::canonical(&intent, 4096)?,
        )?;
        self.sync.sync_parent(self.lease.records.root.file())?;
        self.replace_record("journal", &bytes)
    }

    /// Validates and synchronizes the actually selected journal after an interrupted publication.
    /// It returns facts, never Durable for an earlier Indeterminate operation.
    pub fn recover_journal(&self) -> Result<Option<ActivationJournalRecord>, NativeFailure> {
        self.lease.revalidate_recovery()?;
        let root = &self.lease.records.root;
        let bytes = root
            .read(OsStr::new(&self.name("journal-publication")), 4096)
            .map_err(classify)?;
        let intent: JournalPublicationIntent = bindings::decode_canonical(&bytes, 4096)?;
        if intent.administrative_assessment.is_some() {
            return Err(NativeFailure::AuthorizationDenied);
        }
        let authorization = self
            .lease
            .authorization
            .as_ref()
            .ok_or(NativeFailure::AuthorizationUnavailable)?;
        if intent.schema_version != 1
            || intent.key != self.lease.locks.key
            || intent.request != authorization.journal().request
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        let selected = match self.lease.records.read_journal(intent.key) {
            Ok(record) => {
                let digest: [u8; 32] =
                    Sha256::digest(self.lease.records.read_journal_bytes(intent.key)?).into();
                if Some(digest) != intent.before_sha256 && digest != intent.after_sha256 {
                    return Err(NativeFailure::UnsafeNativeState);
                }
                if digest == intent.after_sha256 {
                    record.same_request(authorization.journal())?;
                }
                let name =
                    CString::new(self.name("journal")).map_err(|_| NativeFailure::InvalidRecord)?;
                // SAFETY: fixed bounded name relative to the retained protected parent.
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
                Some(record)
            }
            Err(NativeFailure::RegistrationMissing) if intent.before_sha256.is_none() => None,
            Err(error) => return Err(error),
        };
        self.sync.sync_parent(root.file())?;
        self.lease.revalidate_recovery()?;
        let name = CString::new(self.name("journal-publication"))
            .map_err(|_| NativeFailure::InvalidRecord)?;
        // SAFETY: exact fixed protected progress component under the shared native transaction.
        if unsafe { libc::unlinkat(root.file().as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        // The selected journal and parent are already durable. A crash may restore this
        // cleanup marker, which closes admission until content-validated recovery repeats.
        Ok(selected)
    }
}
