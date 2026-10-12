//! Adapter seams for the reducer and ledger owners. No domain algorithm or
//! production fallback lives here. Every staged candidate belongs to its port
//! until the actor reports a checked store receipt or definite non-application.

use crate::contracts::ServerFailure;
use spaceterm_core::{
    authority::*, control::MeasuredStartupReservation, control_policy::ControlTransition,
    identity::*, observations::ConfirmedDirectoryObservation, operation_ledger_contracts::*,
    operations::*, organization::*, retirement::*, runtime_authority::SessionTerminalResult,
    storage::*, worktrees::intent::*,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommitCursor {
    pub server: ServerId,
    pub store: StoreRevision,
    pub organization: OrganizationRevision,
    pub epoch: ServerEpoch,
    pub high_water: IdentityHighWater,
}
impl CommitCursor {
    pub fn from_recovered(value: &RecoveredState) -> Self {
        Self {
            server: value.organization.server,
            store: value.store_revision,
            organization: value.organization.revision,
            epoch: value.last_epoch,
            high_water: value.high_water,
        }
    }
    pub fn parts(self) -> Result<TransactionParts, StoreFailure> {
        Ok(TransactionParts {
            server: self.server,
            expected_store_revision: self.store,
            resulting_store_revision: self
                .store
                .checked_successor()
                .ok_or(StoreFailure::Exhausted)?,
            expected_organization_revision: self.organization,
            resulting_organization_revision: self.organization,
            expected_epoch: self.epoch,
            resulting_epoch: self.epoch,
            expected_high_water: self.high_water,
            resulting_high_water: self.high_water,
            organization_writes: Vec::new(),
            operation_writes: Vec::new(),
        })
    }
    pub fn context(self) -> Result<TransactionContext, LedgerFailure> {
        TransactionContext::new(self.store, self.organization, self.organization, None)
    }
    pub fn install(
        &mut self,
        parts: &TransactionParts,
        receipt: CommitReceipt,
    ) -> Result<(), ServerFailure> {
        if parts.server != self.server
            || parts.expected_store_revision != self.store
            || parts.expected_organization_revision != self.organization
            || parts.expected_epoch != self.epoch
            || parts.expected_high_water != self.high_water
            || receipt.store_revision != parts.resulting_store_revision
            || receipt.organization_revision != parts.resulting_organization_revision
        {
            return Err(crate::failure::failure(
                crate::contracts::ServerFailureClass::RecoveryRequired,
            ));
        }
        self.store = receipt.store_revision;
        self.organization = receipt.organization_revision;
        self.epoch = parts.resulting_epoch;
        self.high_water = parts.resulting_high_water;
        Ok(())
    }
}

/// A projection of the reducer's staged candidate, not another hierarchy owner.
/// Auxiliary writes remain core-owned OperationWrite values in the same commit.
pub struct PreparedReduction {
    pub snapshot: Arc<OrganizationSnapshot>,
    pub delta: Option<Arc<OrganizationDelta>>,
    pub high_water: IdentityHighWater,
    pub organization_writes: Vec<OrganizationWrite>,
    pub auxiliary_writes: Vec<OperationWrite>,
    pub outcome: Outcome,
    pub retirement: Option<RetirementDescriptor>,
    pub native_retirement: Option<NativeRetirementDescriptor>,
}
impl PreparedReduction {
    pub fn append_to(&self, parts: &mut TransactionParts) -> Result<(), ServerFailure> {
        if self.snapshot.state().server != parts.server
            || self.snapshot.epoch() != parts.resulting_epoch
            || !self.high_water.dominates(parts.expected_high_water)
            || self.snapshot.state().revision < parts.expected_organization_revision
            || (self.snapshot.state().revision != parts.expected_organization_revision
                && parts.expected_organization_revision.checked_successor()
                    != Some(self.snapshot.state().revision))
        {
            return Err(crate::failure::failure(
                crate::contracts::ServerFailureClass::RecoveryRequired,
            ));
        }
        parts.resulting_organization_revision = self.snapshot.state().revision;
        parts.resulting_high_water = self.high_water;
        parts
            .organization_writes
            .extend(self.organization_writes.iter().cloned());
        parts
            .operation_writes
            .extend(self.auxiliary_writes.iter().cloned());
        Ok(())
    }
}

pub enum ReductionRequest<'a> {
    Command(&'a CanonicalOperationRequest),
    Rejected {
        binding: OperationBinding,
        failure: OperationFailure,
    },
    Control {
        binding: OperationBinding,
        transition: &'a ControlTransition,
    },
    StartupPreparationFailed {
        binding: OperationBinding,
        intent: &'a spaceterm_core::control::StartupIntent,
        failure: OperationFailure,
    },
    StartupReserved {
        binding: OperationBinding,
        reservation: &'a MeasuredStartupReservation,
    },
    RuntimeStarted(SessionKey),
    RuntimeFinished(SessionTerminalResult),
    RestorationDirectoryConfirmed(&'a ConfirmedDirectoryObservation),
    RetirementCompleted {
        descriptor: &'a RetirementDescriptor,
        cleanup: &'a [SessionCleanupObservation],
    },
    NativeRetirementAccepted {
        request: spaceterm_core::native_management::NativeManagementRequestRef,
        action: spaceterm_core::native_management::NativeRetirementAction,
        sessions: &'a [RetiredSession],
    },
    NativeRetirementCompleted {
        descriptor: &'a NativeRetirementDescriptor,
        cleanup: &'a [SessionCleanupObservation],
    },
    ReleaseClients(&'a [ClientId]),
    /// Stage allocation and control release without activating any pairing or
    /// launching a process. The extension supplies its matching core facts later
    /// in this same actor-owned transaction.
    PairingCompletion {
        binding: OperationBinding,
        allocation: crate::extensions::PairingAllocationRequest,
        revoked_clients: &'a [ClientId],
    },
    /// Check the original Started intent against current hierarchy without
    /// deleting membership before the external Git result.
    StartedGitRetirement {
        intent: &'a GitWriteIntent,
        descriptor: &'a RetirementDescriptor,
    },
    ReserveGitAllocation {
        binding: OperationBinding,
        repository: bool,
        worktree: bool,
        fallbacks: &'a [WorkspaceFallbackPlan],
    },
    /// Consume only the original operation's allocation against current
    /// membership, release revoked grants in the same candidate, and never
    /// supply executable shell-start authority.
    SettleStartedGit {
        intent: &'a GitWriteIntent,
        result: &'a RetainedExternalResult,
        revoked_clients: &'a [ClientId],
    },
}

pub trait ReducerPort: Send {
    fn recover(
        &mut self,
        recovered: &RecoveredState,
        epoch: ServerEpoch,
        cleanup: &[SessionCleanupObservation],
    ) -> Result<PreparedReduction, ServerFailure>;
    fn snapshot(&self) -> Arc<OrganizationSnapshot>;
    /// Retain the producer's original stamp and tick. Assessment policy, including
    /// stale-to-Unknown classification, stays in the reducer owner.
    fn observe_close_work(
        &mut self,
        observation: spaceterm_core::observations::CloseWorkObservation,
        now: spaceterm_core::observations::MonotonicTick,
    ) -> Result<(), ServerFailure>;
    fn prepare(
        &mut self,
        request: ReductionRequest<'_>,
        cursor: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure>;
    /// Includes store-only receipts, so the next reduction has the current CAS.
    fn committed(&mut self, receipt: CommitReceipt) -> Result<(), ServerFailure>;
    fn not_applied(&mut self);
}

pub trait LedgerPort: Send {
    fn recover(&mut self, recovered: &RecoveredState) -> Result<(), ServerFailure>;
    /// Interrupt unexecuted starts and reconcile accepted retirement using only
    /// exact trusted old-instance cleanup. Atomic requests remain retriable.
    fn prepare_recovery(
        &mut self,
        recovered: &RecoveredState,
        cleanup: &[SessionCleanupObservation],
        context: TransactionContext,
    ) -> Result<Option<LedgerProjection>, ServerFailure>;
    fn lookup(&self, binding: OperationBinding) -> Result<OperationLookup, ServerFailure>;
    /// Only matching atomic pending work can resume reduction. External progress
    /// and AwaitingIntent never confer a resumed executable-effect permit.
    fn resume_atomic(&mut self, request: &CanonicalOperationRequest)
    -> Result<bool, ServerFailure>;
    fn prepare_admission(
        &mut self,
        request: &CanonicalOperationRequest,
        kind: PendingKind,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure>;
    fn prepare_external_progress(
        &mut self,
        binding: OperationBinding,
        progress: PendingExternalProgress,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure>;
    fn prepare_completion(
        &mut self,
        binding: OperationBinding,
        outcome: &Outcome,
        outcome_revision: OrganizationRevision,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure>;
    /// One staged ledger candidate reserves every new purpose-bound stream,
    /// retires revoked ordinary streams, and completes the original operation.
    /// A quota failure applies none of those changes. Separate prepare calls must
    /// not overwrite one another's staged ledger candidate.
    fn prepare_pairing_completion(
        &mut self,
        binding: OperationBinding,
        streams: &[StreamRecord],
        retired_streams: &[StreamBinding],
        outcome: &Outcome,
        outcome_revision: OrganizationRevision,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure>;
    fn prepare_acknowledgement(
        &mut self,
        binding: StreamBinding,
        through: OperationSequence,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure>;
    /// Must accept every durable receipt, including non-ledger commits.
    fn committed(&mut self, receipt: CommitReceipt) -> Result<(), ServerFailure>;
    fn not_applied(&mut self);
}

pub struct CorePorts {
    pub reducer: Box<dyn ReducerPort>,
    pub ledger: Box<dyn LedgerPort>,
    pub codec: Arc<dyn LedgerCodec>,
    pub validator: Arc<dyn DurabilityValidator>,
}
