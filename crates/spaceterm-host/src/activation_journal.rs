//! Bounded native request journal facts, transition validation, and head-directed recovery.

use crate::{
    native_deployment::{
        bindings::{self, digest, identity, optional_digest},
        contracts::*,
    },
    native_registration::{RegistrationHeadV2, StartupBarrier},
};
use serde::{Deserialize, Serialize};
use std::{fmt, num::NonZeroU64};

/// One-to-one operation classification. It contains no command or executable selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeOperationKind {
    /// First authorized installation.
    Install,
    /// Authenticate a candidate without selecting it.
    Stage,
    /// Select an assessed installation and data generation together.
    Activate,
    /// Restore Running for only the approved installation.
    Start,
    /// Publish Stopped before draining runtime work.
    Stop,
    /// Restart the Running approved installation, never staging.
    Restart,
    /// Remove only assessed native resources after verified retirement.
    Uninstall,
    /// Explicitly reconcile validated native state.
    Repair,
    /// Collect only unreferenced generations.
    Collect,
    /// Grant the pairing owner narrowly scoped remote-access authority.
    AuthorizeRemoteAccess,
    /// Grant the pairing owner narrowly scoped owning-account enrollment authority.
    AuthorizeOwnerEnrollment,
}

/// Durable transition phase; a phase alone does not prove external effects occurred.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "kebab-case")]
pub enum ActivationJournalState {
    /// Fresh protected candidate construction, with old approval unchanged.
    Staging = 1,
    /// Candidate authenticated; no activation or migration.
    Staged = 2,
    /// Startup inhibition durably admitted before effects.
    ActivationPrepared = 3,
    /// Existing runtime admissions closed and work draining.
    Quiescing = 4,
    /// Exact prior process, retriever, Sessions and endpoint absence proved.
    Quiesced = 5,
    /// Owning-account migration uses a separate data generation.
    Migrating = 6,
    /// Candidate data validated durably; original remains recoverable.
    MigrationReady = 7,
    /// Atomic head publication selected installation and data together.
    ApprovalCommitted = 8,
    /// Only current committed selection may start if Running.
    Starting = 9,
    /// Original request outcome retained durably.
    Completed = 10,
    /// No approval commit; old selection remains authoritative.
    Aborted = 11,
    /// Startup stays inhibited until explicit validated repair.
    RecoveryRequired = 12,
}

/// Retained bounded result. Running and approved-but-Stopped remain distinct.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetainedNativeOutcome {
    /// Approved runtime authenticated and ready.
    Running,
    /// New approval committed without starting a process.
    ApprovedStopped,
    /// Candidate staged without activation.
    Staged,
    /// Deliberate Stop and exact previous-instance cleanup completed.
    Stopped,
    /// Native resources removed after exact retirement.
    Removed,
    /// Explicit reconciliation or collection completed.
    Applied,
    /// Precommit operation aborted without changing selection.
    Aborted,
}

/// One strict schema-one journal, at most 64 KiB. IDs and digests are comparison facts.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationJournalRecord {
    /// Journal schema, currently one.
    pub schema_version: u16,
    /// Original native request, separate from pairing operations.
    pub request: NativeRequestId,
    /// Fixed operation classification.
    pub operation: NativeOperationKind,
    /// Exact account/build binding.
    pub key: AccountBuildKey,
    /// Canonical core ServerId bytes.
    #[serde(with = "identity")]
    pub server_id: [u8; 16],
    /// Native account incarnation binding.
    #[serde(with = "digest")]
    pub account_binding_sha256: [u8; 32],
    /// Registration assessed before operation admission.
    pub expected_registration_generation: NonZeroU64,
    /// Lifecycle intent assessed before operation admission.
    pub expected_lifecycle_generation: NonZeroU64,
    /// Approved installation before admission.
    pub original_installation: InstallationId,
    /// Approved data before admission.
    pub original_data_generation: DataGenerationId,
    /// Immutable Install/Stage input. Generated installation and data IDs are progress, not input.
    pub candidate_input: Option<VerifiedCandidateId>,
    /// Activate input or Install/Stage publication result, never implicitly selected.
    pub candidate_installation: Option<InstallationId>,
    /// Candidate's authenticated manifest digest.
    #[serde(with = "optional_digest")]
    pub candidate_manifest_sha256: Option<[u8; 32]>,
    /// Validated data selection; compatible stopped activation retains the original generation.
    pub candidate_data_generation: Option<DataGenerationId>,
    /// Admitted lifecycle intent, subject to newer Stop fencing.
    pub desired_state: DesiredServerState,
    /// Exact assessment digest, not user-supplied authorization.
    #[serde(with = "optional_digest")]
    pub assessment_digest: Option<[u8; 32]>,
    /// Last durably admitted phase.
    pub state: ActivationJournalState,
    /// Retained original outcome, or null before terminal resolution.
    pub outcome: Option<RetainedNativeOutcome>,
}
impl fmt::Debug for ActivationJournalRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ActivationJournalRecord(..)")
    }
}
impl ActivationJournalRecord {
    /// Checks local invariants; external cleanup, migration and authority remain independent.
    pub fn validate(&self) -> Result<(), NativeFailure> {
        use ActivationJournalState as S;
        use NativeOperationKind as O;
        validate_key(self.key)?;
        nonzero(&self.server_id)?;
        nonzero(&self.account_binding_sha256)?;
        if self.schema_version != 1 {
            return Err(NativeFailure::UnsupportedSchema);
        }
        if matches!(self.operation, O::Install | O::Stage) != self.candidate_input.is_some()
            || self.candidate_installation.is_some() != self.candidate_manifest_sha256.is_some()
            || self.candidate_installation == Some(self.original_installation)
            || self.candidate_data_generation == Some(self.original_data_generation)
                && !(self.operation == O::Activate
                    && self.desired_state == DesiredServerState::Stopped)
            || self.operation == O::Activate
                && self.desired_state == DesiredServerState::Stopped
                && self
                    .candidate_data_generation
                    .is_some_and(|data| data != self.original_data_generation)
        {
            return Err(NativeFailure::InvalidRecord);
        }
        if let Some(d) = &self.candidate_manifest_sha256 {
            nonzero(d)?;
        }
        if let Some(d) = &self.assessment_digest {
            nonzero(d)?;
        }
        if matches!(self.state, S::Completed | S::Aborted) != self.outcome.is_some()
            || self.state == S::Aborted && self.outcome != Some(RetainedNativeOutcome::Aborted)
            || self.state == S::Completed && self.outcome != Some(self.completed_outcome())
            || !self.allows_state(self.state)
        {
            return Err(NativeFailure::InvalidRecord);
        }
        if matches!(self.operation, O::Start | O::Restart)
            && self.desired_state != DesiredServerState::Running
            || matches!(self.operation, O::Stop | O::Uninstall)
                && self.desired_state != DesiredServerState::Stopped
        {
            return Err(NativeFailure::InvalidRecord);
        }
        match self.operation {
            O::Stage => {
                if self.candidate_data_generation.is_some()
                    || matches!(self.state, S::Staged | S::Completed)
                        && self.candidate_installation.is_none()
                {
                    return Err(NativeFailure::InvalidRecord);
                }
            }
            O::Install | O::Activate => {
                if self.operation == O::Activate
                    && (self.candidate_installation.is_none() || self.assessment_digest.is_none())
                    || matches!(
                        self.state,
                        S::Staged | S::ApprovalCommitted | S::Starting | S::Completed
                    ) && self.candidate_installation.is_none()
                    || matches!(
                        self.state,
                        S::MigrationReady | S::ApprovalCommitted | S::Starting | S::Completed
                    ) && self.candidate_data_generation.is_none()
                    || self.desired_state == DesiredServerState::Stopped
                        && matches!(self.state, S::Migrating | S::Starting)
                {
                    return Err(NativeFailure::InvalidRecord);
                }
            }
            _ => {
                if self.candidate_installation.is_some() || self.candidate_data_generation.is_some()
                {
                    return Err(NativeFailure::InvalidRecord);
                }
            }
        }
        Ok(())
    }
    fn allows_state(&self, state: ActivationJournalState) -> bool {
        use ActivationJournalState as S;
        use NativeOperationKind as O;
        if matches!(state, S::Completed | S::Aborted | S::RecoveryRequired) {
            return true;
        }
        match self.operation {
            O::Install => matches!(
                state,
                S::Staging | S::Staged | S::ApprovalCommitted | S::Starting
            ),
            O::Stage => matches!(state, S::Staging | S::Staged),
            O::Activate => matches!(
                state,
                S::ActivationPrepared
                    | S::Quiescing
                    | S::Quiesced
                    | S::Migrating
                    | S::MigrationReady
                    | S::ApprovalCommitted
                    | S::Starting
            ),
            O::Start => matches!(state, S::ActivationPrepared | S::Starting),
            O::Stop | O::Uninstall => {
                matches!(state, S::ActivationPrepared | S::Quiescing | S::Quiesced)
            }
            O::Restart => matches!(
                state,
                S::ActivationPrepared | S::Quiescing | S::Quiesced | S::Starting
            ),
            O::Repair | O::Collect | O::AuthorizeRemoteAccess | O::AuthorizeOwnerEnrollment => {
                state == S::ActivationPrepared
            }
        }
    }
    fn completed_outcome(&self) -> RetainedNativeOutcome {
        use NativeOperationKind as O;
        use RetainedNativeOutcome as R;
        match self.operation {
            O::Install | O::Activate => match self.desired_state {
                DesiredServerState::Running => R::Running,
                DesiredServerState::Stopped => R::ApprovedStopped,
            },
            O::Stage => R::Staged,
            O::Start | O::Restart => R::Running,
            O::Stop => R::Stopped,
            O::Uninstall => R::Removed,
            O::Repair | O::Collect | O::AuthorizeRemoteAccess | O::AuthorizeOwnerEnrollment => {
                R::Applied
            }
        }
    }
    /// Encodes canonical JSON with every optional field present as null or a value.
    pub fn encode(&self) -> Result<Vec<u8>, NativeFailure> {
        self.validate()?;
        bindings::canonical(self, 64 * 1024)
    }
    /// Rejects unknown, duplicate, missing, trailing, and noncanonical input.
    pub fn decode(bytes: &[u8]) -> Result<Self, NativeFailure> {
        let record: Self = bindings::decode_canonical(bytes, 64 * 1024)?;
        record.validate()?;
        Ok(record)
    }
    /// Compares immutable admission parameters, not generated publication/migration progress.
    /// This is request recognition, not validation of a substituted progress result or OS authority.
    pub fn same_request(&self, proposed: &Self) -> Result<(), NativeFailure> {
        self.validate()?;
        proposed.validate()?;
        if self.request_parameters() == proposed.request_parameters() {
            Ok(())
        } else {
            Err(NativeFailure::IdentityMismatch)
        }
    }
    /// Resolves the original retained outcome without advancing generations or repeating effects.
    pub fn resolve_request(&self, proposed: &Self) -> Result<RetainedNativeOutcome, NativeFailure> {
        self.same_request(proposed)?;
        self.outcome.ok_or(NativeFailure::OutcomePending)
    }
    fn request_parameters(&self) -> JournalRequestParameters {
        let activating = self.operation == NativeOperationKind::Activate;
        JournalRequestParameters {
            request: self.request,
            operation: self.operation,
            key: self.key,
            server_id: self.server_id,
            account_binding_sha256: self.account_binding_sha256,
            expected_registration_generation: self.expected_registration_generation,
            expected_lifecycle_generation: self.expected_lifecycle_generation,
            original_installation: self.original_installation,
            original_data_generation: self.original_data_generation,
            candidate_input: self.candidate_input,
            activation_installation: activating.then_some(self.candidate_installation).flatten(),
            activation_manifest: activating
                .then_some(self.candidate_manifest_sha256)
                .flatten(),
            desired_state: self.desired_state,
            assessment_digest: self.assessment_digest,
        }
    }
    /// Validates phase ordering without claiming its external prerequisites are proven.
    pub fn transition(&self, next: ActivationJournalState) -> Result<Self, NativeFailure> {
        use ActivationJournalState as S;
        self.validate()?;
        use NativeOperationKind as O;
        let allowed = match self.operation {
            O::Install => {
                matches!(
                    (self.state, next),
                    (S::Staging, S::Staged)
                        | (S::Staged, S::ApprovalCommitted)
                        | (S::ApprovalCommitted, S::Starting)
                        | (S::Starting, S::Completed)
                ) || self.desired_state == DesiredServerState::Stopped
                    && (self.state, next) == (S::ApprovalCommitted, S::Completed)
            }
            O::Stage => matches!(
                (self.state, next),
                (S::Staging, S::Staged) | (S::Staged, S::Completed)
            ),
            O::Activate => {
                matches!(
                    (self.state, next),
                    (S::ActivationPrepared, S::Quiescing)
                        | (S::Quiescing, S::Quiesced)
                        | (S::Quiesced, S::Migrating)
                        | (S::Migrating, S::MigrationReady)
                        | (S::Quiesced, S::MigrationReady)
                        | (S::MigrationReady, S::ApprovalCommitted)
                        | (S::ApprovalCommitted, S::Starting)
                        | (S::Starting, S::Completed)
                ) || self.desired_state == DesiredServerState::Stopped
                    && (self.state, next) == (S::ApprovalCommitted, S::Completed)
            }
            O::Start => matches!(
                (self.state, next),
                (S::ActivationPrepared, S::Starting) | (S::Starting, S::Completed)
            ),
            O::Stop | O::Uninstall => matches!(
                (self.state, next),
                (S::ActivationPrepared, S::Quiescing)
                    | (S::Quiescing, S::Quiesced)
                    | (S::Quiesced, S::Completed)
            ),
            O::Restart => matches!(
                (self.state, next),
                (S::ActivationPrepared, S::Quiescing)
                    | (S::Quiescing, S::Quiesced)
                    | (S::Quiesced, S::Starting)
                    | (S::Starting, S::Completed)
            ),
            O::Repair | O::Collect | O::AuthorizeRemoteAccess | O::AuthorizeOwnerEnrollment => {
                (self.state, next) == (S::ActivationPrepared, S::Completed)
            }
        };
        if !allowed
            && next != S::RecoveryRequired
            && !(next == S::Aborted
                && !self.has_committed_phase()
                && !matches!(self.state, S::Completed | S::Aborted | S::RecoveryRequired))
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        if matches!(self.state, S::Completed | S::Aborted | S::RecoveryRequired) {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let mut record = self.clone();
        record.state = next;
        if next == S::Completed {
            record.outcome = Some(self.completed_outcome());
        } else if next == S::Aborted {
            record.outcome = Some(RetainedNativeOutcome::Aborted);
        }
        record.validate()?;
        Ok(record)
    }
    fn has_committed_phase(&self) -> bool {
        matches!(
            self.state,
            ActivationJournalState::ApprovalCommitted
                | ActivationJournalState::Starting
                | ActivationJournalState::Completed
        ) || self.advances_lifecycle()
            && matches!(
                self.state,
                ActivationJournalState::Quiescing | ActivationJournalState::Quiesced
            )
    }
    fn advances_lifecycle(&self) -> bool {
        matches!(
            self.operation,
            NativeOperationKind::Start | NativeOperationKind::Stop | NativeOperationKind::Restart
        )
    }
    fn validate_head_intent(&self, head: &RegistrationHeadV2) -> Result<bool, NativeFailure> {
        use ActivationJournalState as S;
        if !self.advances_lifecycle() {
            if head.lifecycle_generation != self.expected_lifecycle_generation
                || head.desired_state != self.desired_state
            {
                return Err(NativeFailure::GenerationMismatch);
            }
            return Ok(false);
        }
        if head.lifecycle_generation == self.expected_lifecycle_generation {
            if !matches!(
                self.state,
                S::ActivationPrepared | S::Aborted | S::RecoveryRequired
            ) {
                return Err(NativeFailure::UnsafeNativeState);
            }
            if self.operation == NativeOperationKind::Restart
                && head.desired_state != DesiredServerState::Running
            {
                return Err(NativeFailure::GenerationMismatch);
            }
            return Ok(false);
        }
        let committed_generation = self
            .expected_lifecycle_generation
            .get()
            .checked_add(1)
            .ok_or(NativeFailure::LimitExceeded)?;
        if head.lifecycle_generation.get() != committed_generation
            || head.generation <= self.expected_registration_generation
            || head.desired_state != self.desired_state
        {
            return Err(NativeFailure::GenerationMismatch);
        }
        // An unfinished request must still own the head's fence. A completed journal
        // retains the original result after its matching fence is released; a later
        // lifecycle advance is never treated as this request's own commit.
        if self.state == S::Aborted
            || head.active_request != Some(self.request)
                && !(self.state == S::Completed && head.startup_barrier == StartupBarrier::Open)
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        Ok(true)
    }
    /// Plans recovery strictly from the durable head. Journal intent never chooses a runtime.
    pub fn recovery_selection(
        &self,
        head: &RegistrationHeadV2,
        account_digest: &[u8; 32],
    ) -> Result<RecoverySelection, NativeFailure> {
        self.validate()?;
        crate::native_registration::validate_registration_head(head)?;
        if self.key != head.key
            || self.server_id != head.server_id
            || &self.account_binding_sha256 != account_digest
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        if head.generation < self.expected_registration_generation {
            return Err(NativeFailure::GenerationMismatch);
        }
        if let Some(active) = head.active_request
            && active != self.request
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        let lifecycle_committed = self.validate_head_intent(head)?;
        let original = head.approved_installation == self.original_installation
            && head.approved_data_generation == self.original_data_generation;
        let candidate = Some(head.approved_installation) == self.candidate_installation
            && Some(head.approved_data_generation) == self.candidate_data_generation
            && Some(head.approved_manifest_sha256) == self.candidate_manifest_sha256;
        if self.state == ActivationJournalState::RecoveryRequired
            || head.startup_barrier == StartupBarrier::RecoveryRequired
        {
            return Ok(RecoverySelection::RepairRequired);
        }
        if candidate {
            if head.generation <= self.expected_registration_generation
                || head.startup_barrier == StartupBarrier::Open
                    && self.state != ActivationJournalState::Completed
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
            // A crash after atomic approval but before journal advance still follows the head.
            if !matches!(
                self.state,
                ActivationJournalState::MigrationReady
                    | ActivationJournalState::ApprovalCommitted
                    | ActivationJournalState::Starting
                    | ActivationJournalState::Completed
            ) {
                return Err(NativeFailure::UnsafeNativeState);
            }
            return Ok(RecoverySelection::Committed {
                installation: head.approved_installation,
                data: head.approved_data_generation,
                desired_state: head.desired_state,
            });
        }
        if !original
            || self.has_committed_phase()
                && matches!(
                    self.operation,
                    NativeOperationKind::Install | NativeOperationKind::Activate
                )
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        if lifecycle_committed && head.startup_barrier == StartupBarrier::ApprovedStartOnly {
            if head.desired_state != DesiredServerState::Running
                || !matches!(
                    self.operation,
                    NativeOperationKind::Start | NativeOperationKind::Restart
                )
                || !matches!(
                    self.state,
                    ActivationJournalState::Starting | ActivationJournalState::Completed
                )
            {
                return Err(NativeFailure::UnsafeNativeState);
            }
            return Ok(RecoverySelection::Committed {
                installation: head.approved_installation,
                data: head.approved_data_generation,
                desired_state: head.desired_state,
            });
        }
        if (matches!(
            self.state,
            ActivationJournalState::Staging
                | ActivationJournalState::Staged
                | ActivationJournalState::Completed
                | ActivationJournalState::Aborted
        ) || self.advances_lifecycle()
            && !lifecycle_committed
            && self.state == ActivationJournalState::ActivationPrepared)
            && head.startup_barrier == StartupBarrier::Open
        {
            return Ok(RecoverySelection::Original {
                installation: head.approved_installation,
                data: head.approved_data_generation,
            });
        }
        if head.active_request != Some(self.request)
            || head.startup_barrier != StartupBarrier::Inhibited
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        Ok(RecoverySelection::InhibitedOriginal {
            installation: head.approved_installation,
            data: head.approved_data_generation,
        })
    }
}

#[derive(Eq, PartialEq)]
struct JournalRequestParameters {
    request: NativeRequestId,
    operation: NativeOperationKind,
    key: AccountBuildKey,
    server_id: [u8; 16],
    account_binding_sha256: [u8; 32],
    expected_registration_generation: NonZeroU64,
    expected_lifecycle_generation: NonZeroU64,
    original_installation: InstallationId,
    original_data_generation: DataGenerationId,
    candidate_input: Option<VerifiedCandidateId>,
    activation_installation: Option<InstallationId>,
    activation_manifest: Option<[u8; 32]>,
    desired_state: DesiredServerState,
    assessment_digest: Option<[u8; 32]>,
}

/// Comparison-only recovery plan. Starting or restoring supervision still requires authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoverySelection {
    /// Original selection is intact and startup is open.
    Original {
        /// Exact approved installation.
        installation: InstallationId,
        /// Exact approved data.
        data: DataGenerationId,
    },
    /// Original selection is intact but startup must remain inhibited.
    InhibitedOriginal {
        /// Exact approved installation.
        installation: InstallationId,
        /// Exact approved data.
        data: DataGenerationId,
    },
    /// Atomic approval or lifecycle intent committed; never silently roll back.
    Committed {
        /// Exact approved installation.
        installation: InstallationId,
        /// Exact approved data.
        data: DataGenerationId,
        /// Current durable intent.
        desired_state: DesiredServerState,
    },
    /// No validated safe automatic reconciliation exists.
    RepairRequired,
}
