//! Separately authorized reconciliation, embedded in the existing journal authority.
//! Repair preserves the actually selected installation/data and every burned allocation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    time::{Duration, Instant},
};

use super::{
    NativeHeadStorage, NativeRecordSync, NativeRegistrationStore, NativeTransactionLease,
    NativeTransactionPeer, NativeTransactions,
    head_storage::{HeadPublicationIntent, InstallationAllocation},
    journal_storage::JournalPublicationIntent,
    record_file::BoundRecord,
    records::{classify, prefix},
};
use crate::{
    activation_journal::{
        ActivationJournalRecord, ActivationJournalState as Phase, NativeOperationKind as Operation,
        RetainedNativeOutcome,
    },
    native_deployment::{
        bindings::{self, digest},
        contracts::*,
    },
    native_registration::{StartupBarrier, decode_registration_head, encode_registration_head},
    protected_directory::ProtectedDirectoryError,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressBinding {
    suffix: String,
    device: u64,
    inode: u64,
    bytes: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepairSelection {
    original: ActivationJournalRecord,
    reconciled: ActivationJournalRecord,
    before_head: Vec<u8>,
    after_head: Vec<u8>,
    head_progress: Option<ProgressBinding>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AdministrativeRepair {
    request: ActivationJournalRecord,
    #[serde(with = "digest")]
    assessment: [u8; 32],
    selection: RepairSelection,
    settled: bool,
}
impl AdministrativeRepair {
    pub(super) fn validate(&self, current: &ActivationJournalRecord) -> Result<(), NativeFailure> {
        self.request.validate()?;
        self.selection
            .original
            .same_request(&self.selection.reconciled)?;
        if self.request.operation != Operation::Repair
            || self.request.state != Phase::ActivationPrepared
            || self.request.outcome.is_some()
            || self.request.request == current.request
            || self.request.key != current.key
            || self.request.server_id != current.server_id
            || self.request.assessment_digest != Some(self.assessment)
            || self.selection.before_head.len() != 256
            || self.selection.after_head.len() != 256
            || *current
                != if self.settled {
                    self.selection.reconciled.clone()
                } else {
                    self.selection.original.clone()
                }
            || self.assessment != assessment_digest(&self.selection)?
        {
            return Err(NativeFailure::InvalidRecord);
        }
        let before = decode_registration_head(&self.selection.before_head, current.key)?;
        let after = decode_registration_head(&self.selection.after_head, current.key)?;
        crate::installation::validate_successor(&before, &after)?;
        if before.approved_installation != after.approved_installation
            || before.approved_data_generation != after.approved_data_generation
            || before.approved_descriptor_sha256 != after.approved_descriptor_sha256
            || before.approved_manifest_sha256 != after.approved_manifest_sha256
            || before.staged_installation != after.staged_installation
            || before.staged_descriptor_sha256 != after.staged_descriptor_sha256
            || before.profile_sha256 != after.profile_sha256
            || before.lifecycle_generation != after.lifecycle_generation
            || before.desired_state != after.desired_state
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        if let Some(progress) = &self.selection.head_progress {
            if !matches!(
                progress.suffix.as_str(),
                "head-publication" | "head-recovered"
            ) || progress.bytes.len() > 4096
            {
                return Err(NativeFailure::InvalidRecord);
            }
            let intent: HeadPublicationIntent = bindings::decode_canonical(&progress.bytes, 4096)?;
            if intent.key != current.key || intent.request != current.request {
                return Err(NativeFailure::IdentityMismatch);
            }
        }
        Ok(())
    }
}
fn assessment_digest(selection: &RepairSelection) -> Result<[u8; 32], NativeFailure> {
    let mut hash = Sha256::new();
    hash.update(b"SpaceTerm administrative native reconciliation v1\0");
    hash.update(bindings::canonical(selection, 32 * 1024)?);
    Ok(hash.finalize().into())
}

impl NativeRegistrationStore {
    /// Comparison-only assessment for a separately authorized Repair. It supplies no mutation lease.
    /// The native management peer binds this digest and the selected generations before approval.
    pub fn assess_repair(&self, key: AccountBuildKey) -> Result<[u8; 32], NativeFailure> {
        let locks = self.lock(key, Instant::now() + Duration::from_secs(2))?;
        let document = self.read_journal_document(key)?;
        let digest = if let Some(intent) = self.repair_admission(key)? {
            intent
                .administrative_assessment
                .ok_or(NativeFailure::Indeterminate)?
        } else if let Some(repair) = &document.repair {
            repair.assessment
        } else {
            assessment_digest(&self.repair_selection(key, false)?)?
        };
        self.verify_locks(&locks)?;
        Ok(digest)
    }
    fn repair_admission(
        &self,
        key: AccountBuildKey,
    ) -> Result<Option<JournalPublicationIntent>, NativeFailure> {
        match self.root.read(
            OsStr::new(&format!("{}.journal-publication", prefix(key))),
            4096,
        ) {
            Ok(bytes) => {
                let intent: JournalPublicationIntent = bindings::decode_canonical(&bytes, 4096)?;
                if intent.schema_version != 1 || intent.key != key {
                    return Err(NativeFailure::InvalidRecord);
                }
                Ok(Some(intent))
            }
            Err(ProtectedDirectoryError::Missing) => Ok(None),
            Err(error) => Err(classify(error)),
        }
    }
    fn repair_selection(
        &self,
        key: AccountBuildKey,
        own_admission: bool,
    ) -> Result<RepairSelection, NativeFailure> {
        let snapshot = self.read_snapshot(key)?;
        let original = snapshot.journal.ok_or(NativeFailure::RegistrationMissing)?;
        if original.server_id != snapshot.head.server_id
            || original.account_binding_sha256 != snapshot.profile.account().sha256()
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        let mut reconciled = original.clone();
        let mut after = snapshot.head.clone();
        let before_head = encode_registration_head(&after)?.to_vec();
        let mut head_progress = None;
        for suffix in ["head-publication", "head-recovered"] {
            let name = format!("{}.{}", prefix(key), suffix);
            match self.root.read(OsStr::new(&name), 4096) {
                Err(ProtectedDirectoryError::Missing) => (),
                Err(error) => return Err(classify(error)),
                Ok(bytes) => {
                    if head_progress.is_some() {
                        return Err(NativeFailure::UnsafeNativeState);
                    }
                    let (record, retained) =
                        BoundRecord::open(self, self.root.file(), &name, 4096)?;
                    if bytes != retained {
                        return Err(NativeFailure::GenerationMismatch);
                    }
                    let intent: HeadPublicationIntent = bindings::decode_canonical(&bytes, 4096)?;
                    super::journal_document::validate_staging_name(key, &intent.staging_name)?;
                    if intent.schema_version != 1
                        || intent.key != key
                        || intent.request != original.request
                    {
                        return Err(NativeFailure::IdentityMismatch);
                    }
                    let selected: [u8; 32] = Sha256::digest(&before_head).into();
                    if selected != intent.before_sha256 && selected != intent.after_sha256 {
                        return Err(NativeFailure::UnsafeNativeState);
                    }
                    if reconciled.state == Phase::RecoveryRequired {
                        reconciled.state = intent.journal_state;
                        reconciled.outcome = intent.journal_outcome;
                    }
                    if <[u8; 32]>::from(Sha256::digest(reconciled.encode()?))
                        != intent.journal_sha256
                    {
                        return Err(NativeFailure::UnsafeNativeState);
                    }
                    after.installation_high_water = after
                        .installation_high_water
                        .max(intent.installation_high_water);
                    after.data_high_water = after.data_high_water.max(intent.data_high_water);
                    let metadata = record
                        .file
                        .metadata()
                        .map_err(|_| NativeFailure::IoUnavailable)?;
                    head_progress = Some(ProgressBinding {
                        suffix: suffix.to_owned(),
                        device: metadata.dev(),
                        inode: metadata.ino(),
                        bytes,
                    });
                }
            }
        }
        // Other journal publications must be recovered under their original authority.
        if !own_admission && self.repair_admission(key)?.is_some() {
            return Err(NativeFailure::Indeterminate);
        }
        if let Some(candidate) = original.candidate_installation {
            after.installation_high_water = after.installation_high_water.max(candidate.0.get());
        }
        if let Some(data) = original.candidate_data_generation {
            after.data_high_water = after.data_high_water.max(data.get());
        }
        match self.root.read(
            OsStr::new(&format!("{}.installation-allocation", prefix(key))),
            4096,
        ) {
            Ok(bytes) => {
                let allocation: InstallationAllocation = bindings::decode_canonical(&bytes, 4096)?;
                if allocation.key != key
                    || allocation.server_id != after.server_id
                    || allocation.installation.0.get() > after.installation_high_water
                        && allocation.request != original.request
                {
                    return Err(NativeFailure::UnsafeNativeState);
                }
                if allocation.request == original.request {
                    if allocation.schema_version != 1
                        || Some(allocation.candidate) != original.candidate_input
                        || allocation.account_binding_sha256 != original.account_binding_sha256
                        || allocation.profile_sha256 != after.profile_sha256
                    {
                        return Err(NativeFailure::IdentityMismatch);
                    }
                    after.installation_high_water = after
                        .installation_high_water
                        .max(allocation.installation.0.get());
                }
            }
            Err(ProtectedDirectoryError::Missing) => (),
            Err(error) => return Err(classify(error)),
        }
        if head_progress.is_none()
            && original.state != Phase::RecoveryRequired
            && after.startup_barrier != StartupBarrier::RecoveryRequired
            && after.installation_high_water == snapshot.head.installation_high_water
            && after.data_high_water == snapshot.head.data_high_water
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let committed_candidate = Some(after.approved_installation)
            == original.candidate_installation
            && Some(after.approved_data_generation) == original.candidate_data_generation
            && Some(after.approved_manifest_sha256) == original.candidate_manifest_sha256;
        let original_selected = after.approved_installation == original.original_installation
            && after.approved_data_generation == original.original_data_generation;
        let lifecycle_committed = after.lifecycle_generation.get()
            == original
                .expected_lifecycle_generation
                .get()
                .checked_add(1)
                .ok_or(NativeFailure::LimitExceeded)?;
        if reconciled.outcome.is_some() && reconciled.state != Phase::RecoveryRequired {
            after.active_request = None;
            after.startup_barrier = StartupBarrier::Open;
        } else if committed_candidate
            && matches!(original.operation, Operation::Install | Operation::Activate)
        {
            reconciled.state = Phase::ApprovalCommitted;
            reconciled.outcome = None;
            after.active_request = Some(original.request);
            after.startup_barrier = StartupBarrier::ApprovedStartOnly;
        } else if original_selected
            && lifecycle_committed
            && matches!(
                original.operation,
                Operation::Start | Operation::Stop | Operation::Restart
            )
            && after.desired_state == original.desired_state
        {
            reconciled.state = if original.operation == Operation::Start {
                Phase::ActivationPrepared
            } else {
                Phase::Quiescing
            };
            reconciled.outcome = None;
            after.active_request = Some(original.request);
            after.startup_barrier = StartupBarrier::Inhibited;
        } else if original_selected
            && after.lifecycle_generation == original.expected_lifecycle_generation
        {
            if matches!(
                reconciled.state,
                Phase::ApprovalCommitted | Phase::Starting | Phase::Completed
            ) && matches!(original.operation, Operation::Install | Operation::Activate)
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
            reconciled.state = Phase::Aborted;
            reconciled.outcome = Some(RetainedNativeOutcome::Aborted);
            after.active_request = None;
            after.startup_barrier = StartupBarrier::Open;
        } else {
            return Err(NativeFailure::UnsafeNativeState);
        }
        after.generation = std::num::NonZeroU64::new(
            after
                .generation
                .get()
                .checked_add(1)
                .ok_or(NativeFailure::LimitExceeded)?,
        )
        .ok_or(NativeFailure::LimitExceeded)?;
        reconciled.recovery_selection(&after, &snapshot.profile.account().sha256())?;
        Ok(RepairSelection {
            original,
            reconciled,
            before_head,
            after_head: encode_registration_head(&after)?.to_vec(),
            head_progress,
        })
    }
}

impl<P: NativeTransactionPeer> NativeTransactions<P> {
    /// Applies only the assessed reconciliation under fresh, operation-specific Repair authorization.
    /// It cannot select rollback code/data or replace the interrupted request identity.
    pub fn repair<S: NativeRecordSync>(
        &self,
        request: &P::Request,
        sync: S,
    ) -> PublicationDisposition<RetainedNativeOutcome> {
        let prepare = || {
            let authorization = self.peer.authorize(request)?;
            authorization.revalidate()?;
            let proposed = authorization.journal();
            proposed.validate()?;
            if proposed.operation != Operation::Repair
                || proposed.state != Phase::ActivationPrepared
                || proposed.outcome.is_some()
                || proposed.assessment_digest.is_none()
            {
                return Err(NativeFailure::AuthorizationDenied);
            }
            let deadline = authorization.deadline();
            let locks = self.records.lock(proposed.key, deadline)?;
            let mut document = self.records.read_journal_document(proposed.key)?;
            let admission = self.records.repair_admission(proposed.key)?;
            if let Some(intent) = &admission
                && (intent.request != proposed.request
                    || intent.administrative_assessment != proposed.assessment_digest)
            {
                return Err(NativeFailure::IdentityMismatch);
            }
            if document.repair.is_none()
                && let Some(outcome) = document.find(proposed.request)
            {
                let outcome = outcome.resolve_request(proposed)?;
                authorization.revalidate()?;
                if Instant::now() >= deadline {
                    return Err(NativeFailure::TimedOut);
                }
                return Ok(Err(outcome));
            }
            if let Some(repair) = &document.repair {
                repair.request.same_request(proposed)?;
            } else {
                document.reserve_outcome(proposed.request)?;
                let selection = self
                    .records
                    .repair_selection(proposed.key, admission.is_some())?;
                let head = decode_registration_head(&selection.before_head, proposed.key)?;
                let assessment = assessment_digest(&selection)?;
                if proposed.assessment_digest != Some(assessment)
                    || proposed.expected_registration_generation != head.generation
                    || proposed.expected_lifecycle_generation != head.lifecycle_generation
                    || proposed.original_installation != head.approved_installation
                    || proposed.original_data_generation != head.approved_data_generation
                    || proposed.desired_state != head.desired_state
                    || proposed.server_id != head.server_id
                    || proposed.account_binding_sha256 != selection.original.account_binding_sha256
                {
                    return Err(NativeFailure::StaleAssessment);
                }
                document.repair = Some(AdministrativeRepair {
                    request: proposed.clone(),
                    assessment,
                    selection,
                    settled: false,
                });
            }
            document.encode()?;
            let lease = NativeTransactionLease {
                locks,
                records: std::sync::Arc::clone(&self.records),
                authorization: Some(authorization),
                admitting: true,
                deadline,
            };
            lease.revalidate_recovery()?;
            Ok(Ok((lease, document)))
        };
        let (lease, document) = match prepare() {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(outcome)) => return PublicationDisposition::Durable(outcome),
            Err(error) => return PublicationDisposition::NotApplied(error),
        };
        let storage = NativeHeadStorage::new(lease, sync);
        match storage.apply_repair(document) {
            Ok(()) => PublicationDisposition::Durable(RetainedNativeOutcome::Applied),
            Err(_) => PublicationDisposition::Indeterminate,
        }
    }
}
impl<S: NativeRecordSync> NativeHeadStorage<S> {
    fn publish_repair_admission(
        &self,
        document: &super::journal_document::JournalDocument,
    ) -> Result<(), NativeFailure> {
        let repair = document
            .repair
            .as_ref()
            .ok_or(NativeFailure::UnsafeNativeState)?;
        let bytes = document.encode()?;
        let selected = self
            .lease
            .records
            .read_journal_bytes(self.lease.locks.key)?;
        let selected_digest: [u8; 32] = Sha256::digest(&selected).into();
        let intent =
            if let Some(intent) = self.lease.records.repair_admission(self.lease.locks.key)? {
                if intent.request != repair.request.request
                    || intent.administrative_assessment != Some(repair.assessment)
                    || intent.after_sha256 != <[u8; 32]>::from(Sha256::digest(&bytes))
                    || Some(selected_digest) != intent.before_sha256
                        && selected_digest != intent.after_sha256
                {
                    return Err(NativeFailure::UnsafeNativeState);
                }
                intent
            } else {
                let intent = JournalPublicationIntent {
                    schema_version: 1,
                    key: self.lease.locks.key,
                    request: repair.request.request,
                    before_sha256: Some(selected_digest),
                    after_sha256: Sha256::digest(&bytes).into(),
                    administrative_assessment: Some(repair.assessment),
                };
                self.create_fresh(
                    &self.name("journal-publication"),
                    &bindings::canonical(&intent, 4096)?,
                )?;
                self.sync.sync_parent(self.lease.records.root.file())?;
                intent
            };
        self.replace_record("journal", &bytes)?;
        let (record, observed) = BoundRecord::open(
            &self.lease.records,
            self.lease.records.root.file(),
            &self.name("journal-publication"),
            4096,
        )?;
        if observed != bindings::canonical(&intent, 4096)? {
            return Err(NativeFailure::UnsafeNativeState);
        }
        record.verify(&self.lease.records, self.lease.records.root.file())?;
        self.lease.revalidate_recovery()?;
        // SAFETY: exact retained subordinate admission intent; the selected journal is synchronized.
        if unsafe {
            libc::unlinkat(
                self.lease.records.root.file().as_raw_fd(),
                record.name.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(NativeFailure::IoUnavailable);
        }
        // A crash can restore this cleanup marker and require original-request repair again.
        Ok(())
    }
    fn apply_repair(
        &self,
        document: super::journal_document::JournalDocument,
    ) -> Result<(), NativeFailure> {
        self.lease.revalidate_recovery()?;
        self.publish_repair_admission(&document)?;
        let repair = document.repair.ok_or(NativeFailure::UnsafeNativeState)?;
        let selected = self.read_head()?.to_vec();
        if selected != repair.selection.before_head && selected != repair.selection.after_head {
            return Err(NativeFailure::UnsafeNativeState);
        }
        if !repair.settled {
            self.replace_record("registration", &repair.selection.after_head)?;
            if let Some(progress) = &repair.selection.head_progress {
                let intent: HeadPublicationIntent =
                    bindings::decode_canonical(&progress.bytes, 4096)?;
                self.retire_staging(&intent)?;
                let name = self.name(&progress.suffix);
                match self.lease.records.root.read(OsStr::new(&name), 4096) {
                    Ok(bytes) => {
                        let (record, _) = BoundRecord::open(
                            &self.lease.records,
                            self.lease.records.root.file(),
                            &name,
                            4096,
                        )?;
                        let metadata = record
                            .file
                            .metadata()
                            .map_err(|_| NativeFailure::IoUnavailable)?;
                        if bytes != progress.bytes
                            || metadata.dev() != progress.device
                            || metadata.ino() != progress.inode
                        {
                            return Err(NativeFailure::UnsafeNativeState);
                        }
                        record.verify(&self.lease.records, self.lease.records.root.file())?;
                        self.lease.revalidate_recovery()?;
                        // SAFETY: exact retained progress inode, under the original shared transaction.
                        if unsafe {
                            libc::unlinkat(
                                self.lease.records.root.file().as_raw_fd(),
                                record.name.as_ptr(),
                                0,
                            )
                        } != 0
                        {
                            return Err(NativeFailure::IoUnavailable);
                        }
                    }
                    Err(ProtectedDirectoryError::Missing) => (),
                    Err(error) => return Err(classify(error)),
                }
            }
            self.sync.sync_parent(self.lease.records.root.file())?;
            let mut settled = self
                .lease
                .records
                .read_journal_document(self.lease.locks.key)?;
            settled.current = repair.selection.reconciled.clone();
            settled
                .repair
                .as_mut()
                .ok_or(NativeFailure::UnsafeNativeState)?
                .settled = true;
            settled.retain_outcome(repair.request.transition(Phase::Completed)?)?;
            self.replace_record("journal", &settled.encode()?)?;
        }
        let mut final_document = self
            .lease
            .records
            .read_journal_document(self.lease.locks.key)?;
        if !final_document
            .repair
            .as_ref()
            .is_some_and(|repair| repair.settled)
            || self.read_head()?.as_slice() != repair.selection.after_head
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        final_document.repair = None;
        // Selection, journal outcome and exact progress retirement are already synchronized.
        // Clearing the last fence is cleanup: a crash may restore the settled repair record.
        self.replace_cleanup_record("journal", &final_document.encode()?)
    }
}
