//! Head-selected installation policy and narrow runtime/store integration ports.

use crate::{
    activation_journal::{ActivationJournalRecord, ActivationJournalState, NativeOperationKind},
    approved_installation::RetainedApprovedInstallation,
    native_deployment::{contracts::*, profile::RegistrationProfile},
    native_registration::{RegistrationHeadV2, StartupBarrier, validate_registration_head},
};

/// Noncloneable registration authority. A decoded head/profile never constructs this handle.
pub struct RetainedRegistration {
    head: RegistrationHeadV2,
    profile: RegistrationProfile,
    _custody: crate::approved_installation::InstallationCustody,
}
impl RetainedRegistration {
    /// Returns the head authenticated through retained native custody.
    pub fn head(&self) -> &RegistrationHeadV2 {
        &self.head
    }
    /// Returns the immutable profile authenticated by this head.
    pub fn profile(&self) -> &RegistrationProfile {
        &self.profile
    }
}

/// Installation retention chosen by trusted native composition. There is no caller path input.
pub trait ApprovedInstallationRetention: Send + Sync {
    /// Authenticates and retains the approved file and entire execution closure through use.
    fn retain_approved(
        &self,
        registration: &RetainedRegistration,
    ) -> Result<RetainedApprovedInstallation, NativeFailure>;
}

/// Production retained authority is unavailable until a fresh-copy custody issuer is integrated.
#[derive(Default)]
pub struct SystemInstallationRetention;
impl ApprovedInstallationRetention for SystemInstallationRetention {
    fn retain_approved(
        &self,
        _registration: &RetainedRegistration,
    ) -> Result<RetainedApprovedInstallation, NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
}

/// Bounded lifecycle integration result, not an empty-registry inference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuiescenceProgress {
    /// Existing admitted work is still draining.
    Pending,
    /// Owning runtime proved exact prior-process, retriever, Session, and endpoint absence.
    Complete,
    /// Retirement could not be established; completion is forbidden.
    CleanupUnresolved,
}

/// Bounded migration result from the owning-account store, never a root database parser.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrationProgress {
    /// Owning-account work is still progressing.
    Pending,
    /// Separate data generation validated durably.
    Ready(DataGenerationId),
    /// No migration was admitted.
    NotApplied(NativeFailure),
    /// Persistence is uncertain and startup stays closed.
    Indeterminate,
}

/// Binding to the core-owned exact runtime assessment, supplied by the coordinator.
/// The generic parameter avoids defining a second runtime assessment before the core seed arrives.
pub trait ServerLifecyclePort<RuntimeAssessment>: Send + Sync {
    /// Revalidates the exact assessed instance after native authorization.
    fn revalidate(&self, assessment: &RuntimeAssessment) -> NativeManagementFuture<'_, ()>;
    /// Durably admits quiescence against this original native request.
    fn begin_quiescence(
        &self,
        request: NativeRequestId,
        assessment: &RuntimeAssessment,
    ) -> NativeManagementFuture<'_, QuiescenceProgress>;
    /// Resolves the original operation rather than repeating effects.
    fn resolve_quiescence(
        &self,
        request: NativeRequestId,
    ) -> NativeManagementFuture<'_, QuiescenceProgress>;
}

/// Owning-account migration through the durable store's backup/migration interface.
pub trait ServerMigrationPort: Send + Sync {
    /// Prepares a distinct data generation without listeners or Terminal Sessions.
    fn prepare_generation(
        &self,
        request: NativeRequestId,
        source: DataGenerationId,
        candidate: InstallationId,
    ) -> NativeManagementFuture<'_, MigrationProgress>;
    /// Validates candidate data durably without root parsing account-owned contents.
    fn validate_generation(
        &self,
        request: NativeRequestId,
        candidate: DataGenerationId,
    ) -> NativeManagementFuture<'_, MigrationProgress>;
}

/// Owning-account compatibility inspection. This port cannot prepare or migrate data.
pub trait ServerDataCompatibilityPort: Send + Sync {
    /// Checks the candidate's ability to read the retained generation against the exact assessment.
    /// An unavailable reader or unproved compatibility rejects; no example schema number is used.
    fn revalidate_read_compatibility(
        &self,
        request: NativeRequestId,
        source: DataGenerationId,
        candidate: InstallationId,
        assessment_digest: [u8; 32],
    ) -> NativeManagementFuture<'_, ()>;
}

/// Validates a stopped activation's existing data without migration, credentials or startup.
/// The caller must retain native serialization and durably publish the returned phase before approval.
/// This produces journal facts, not native authorization or an approval commit.
pub async fn validate_stopped_activation_data(
    journal: &ActivationJournalRecord,
    compatibility: &dyn ServerDataCompatibilityPort,
) -> Result<ActivationJournalRecord, NativeFailure> {
    journal.validate()?;
    if journal.operation != NativeOperationKind::Activate
        || journal.desired_state != DesiredServerState::Stopped
        || journal.state != ActivationJournalState::Quiesced
    {
        return Err(NativeFailure::UnsafeNativeState);
    }
    compatibility
        .revalidate_read_compatibility(
            journal.request,
            journal.original_data_generation,
            journal
                .candidate_installation
                .ok_or(NativeFailure::InvalidRecord)?,
            journal
                .assessment_digest
                .ok_or(NativeFailure::InvalidRecord)?,
        )
        .await?;
    let mut ready = journal.clone();
    ready.candidate_data_generation = Some(journal.original_data_generation);
    ready.transition(ActivationJournalState::MigrationReady)
}

/// Checked head proposal. It is not a publication, OS authorization, or execution permit.
pub fn propose_lifecycle(
    head: &RegistrationHeadV2,
    desired_state: DesiredServerState,
) -> Result<RegistrationHeadV2, NativeFailure> {
    validate_registration_head(head)?;
    if head.startup_barrier != StartupBarrier::Open {
        return Err(NativeFailure::OutcomePending);
    }
    let mut next = head.clone();
    next.generation = increment(head.generation)?;
    next.lifecycle_generation = increment(head.lifecycle_generation)?;
    next.desired_state = desired_state;
    Ok(next)
}

/// Proposes staging without opening data, changing lifecycle intent, or selecting a new runtime.
pub fn propose_staging(
    head: &RegistrationHeadV2,
    installation: InstallationId,
    descriptor_sha256: [u8; 32],
) -> Result<RegistrationHeadV2, NativeFailure> {
    validate_registration_head(head)?;
    nonzero(&descriptor_sha256)?;
    if head.startup_barrier != StartupBarrier::Open {
        return Err(NativeFailure::OutcomePending);
    }
    if installation.0.get() <= head.installation_high_water {
        return Err(NativeFailure::UnsafeNativeState);
    }
    let mut next = head.clone();
    next.generation = increment(head.generation)?;
    next.staged_installation = Some(installation);
    next.staged_descriptor_sha256 = Some(descriptor_sha256);
    next.installation_high_water = installation.0.get();
    validate_registration_head(&next)?;
    Ok(next)
}

/// Checks that a publisher never decreases allocations or skips registration generations.
pub fn validate_successor(
    current: &RegistrationHeadV2,
    next: &RegistrationHeadV2,
) -> Result<(), NativeFailure> {
    validate_registration_head(current)?;
    validate_registration_head(next)?;
    if next.key != current.key || next.server_id != current.server_id {
        return Err(NativeFailure::IdentityMismatch);
    }
    if next.generation != increment(current.generation)?
        || !matches!(
            next.lifecycle_generation
                .get()
                .checked_sub(current.lifecycle_generation.get()),
            Some(0 | 1)
        )
        || next.installation_high_water < current.installation_high_water
        || next.data_high_water < current.data_high_water
    {
        return Err(NativeFailure::GenerationMismatch);
    }
    if next.desired_state != current.desired_state
        && next.lifecycle_generation != increment(current.lifecycle_generation)?
    {
        return Err(NativeFailure::GenerationMismatch);
    }
    Ok(())
}

/// Stopped activation has no credential, migration, listener, Session, or process-start effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationEffectPlan {
    /// Revalidate assessed read compatibility, then commit the existing data without starting or migrating.
    CommitWithoutStart,
    /// Quiesce, prepare a separate generation through the owner store, then commit and start.
    QuiesceMigrateCommitStart,
}

/// Rejects stale lifecycle assessment and staging substitution before any effect is admitted.
pub fn plan_activation(
    head: &RegistrationHeadV2,
    journal: &ActivationJournalRecord,
) -> Result<ActivationEffectPlan, NativeFailure> {
    validate_registration_head(head)?;
    journal.validate()?;
    if head.key != journal.key || head.server_id != journal.server_id {
        return Err(NativeFailure::IdentityMismatch);
    }
    if head.generation != journal.expected_registration_generation
        || head.lifecycle_generation != journal.expected_lifecycle_generation
        || head.desired_state != journal.desired_state
    {
        return Err(NativeFailure::StaleAssessment);
    }
    if journal.operation != NativeOperationKind::Activate
        || journal.state != ActivationJournalState::ActivationPrepared
        || journal.original_installation != head.approved_installation
        || journal.original_data_generation != head.approved_data_generation
        || journal.candidate_installation != head.staged_installation
        || head.startup_barrier != StartupBarrier::Open
    {
        return Err(NativeFailure::UnsafeNativeState);
    }
    Ok(match head.desired_state {
        DesiredServerState::Stopped => ActivationEffectPlan::CommitWithoutStart,
        DesiredServerState::Running => ActivationEffectPlan::QuiesceMigrateCommitStart,
    })
}

fn increment(value: std::num::NonZeroU64) -> Result<std::num::NonZeroU64, NativeFailure> {
    value
        .get()
        .checked_add(1)
        .and_then(std::num::NonZeroU64::new)
        .ok_or(NativeFailure::LimitExceeded)
}
