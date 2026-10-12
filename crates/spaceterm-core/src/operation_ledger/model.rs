//! Committed ledger state and process-local, non-cloneable execution ownership.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::authority::{OrganizationRevision, StoreRevision};
use crate::identity::{OperationStreamKey, ServerId};
use crate::operation_ledger_contracts::*;
use crate::operations::{
    OperationBinding, OperationId, OperationLookup, OperationSequence, RequestDigest,
};
use crate::storage::StoreFailure;

pub struct OperationLedger {
    pub(super) server: ServerId,
    pub(super) state: LedgerState,
    pub(super) owner: Arc<()>,
    pub(super) prepared: Option<OutstandingPreparation>,
    pub(super) closed: bool,
    pub(super) recovery_required: bool,
    pub(super) executing: BTreeSet<OperationId>,
    pub(super) resumable: BTreeSet<OperationId>,
    pub(super) uncertain: Option<LedgerProjection>,
    pub(super) known_store: StoreRevision,
    pub(super) known_organization: OrganizationRevision,
}

pub(super) struct LedgerState {
    pub limits: LedgerLimits,
    pub streams: BTreeMap<OperationStreamKey, StreamRecord>,
    pub operations: BTreeMap<OperationId, RecoveredOperationRecord>,
}

pub(super) struct OutstandingPreparation {
    pub token: Arc<()>,
    pub acknowledgement: Option<AcknowledgementProposal>,
}
pub(super) struct AcknowledgementProposal {
    pub binding: StreamBinding,
    pub previous: u64,
    pub through: u64,
}

pub enum AdmissionPreparation {
    Prepared(PreparedLedgerTransition),
    Existing(OperationLookup),
}

/// Grants one process-local execution ownership. External admission initially
/// awaits a separately durable prerequisite and cannot itself authorize effects.
pub struct ExecutionPermit {
    pub(super) binding: OperationBinding,
    pub(super) kind: PendingKind,
}
impl ExecutionPermit {
    pub const fn binding(&self) -> OperationBinding {
        self.binding
    }
    pub const fn kind(&self) -> PendingKind {
        self.kind
    }
    pub const fn permits_external_effects(&self) -> bool {
        false
    }
}

pub enum TransitionResolution {
    StreamRegistered,
    Admitted(ExecutionPermit),
    ExternalProgressRecorded,
    Completed,
    Acknowledged { through: u64 },
    StreamRetired,
    NotApplied(StoreFailure),
    RecoveryRequired,
}

/// Dropping this proposal does not silently roll back a possibly submitted store
/// transaction. The ledger stays busy until resolution or validated recovery.
pub struct PreparedLedgerTransition {
    pub(super) owner: Arc<()>,
    pub(super) token: Arc<()>,
    pub(super) projection: LedgerProjection,
    pub(super) resolution: PreparedResolution,
}
impl PreparedLedgerTransition {
    pub fn projection(&self) -> &LedgerProjection {
        &self.projection
    }
}
pub(super) enum PreparedResolution {
    Registered,
    Admitted(OperationBinding, PendingKind),
    Progress,
    Completed,
    Acknowledged(u64),
    Retired,
}

pub enum RecoveryTask<'a> {
    AwaitOriginalAtomicRequest(&'a PendingOperationRecord),
    ReconcileExternal(&'a PendingOperationRecord),
}

pub(super) fn failure(class: LedgerFailureClass) -> LedgerFailure {
    LedgerFailure::new(class)
}
pub(super) fn record_binding(record: &RecoveredOperationRecord) -> OperationBinding {
    match record {
        RecoveredOperationRecord::Pending(v) => v.binding,
        RecoveredOperationRecord::Completed(v) => v.binding,
    }
}
pub(super) fn reservation(record: &RecoveredOperationRecord) -> OutcomeReservation {
    match record {
        RecoveredOperationRecord::Pending(v) => v.reservation,
        RecoveredOperationRecord::Completed(v) => v.reservation,
    }
}

impl OperationLedger {
    pub fn empty(server: ServerId, limits: LedgerLimits) -> Result<Self, LedgerFailure> {
        Self::recover(server, LedgerRecovery::empty(limits))
    }
    pub fn lookup(
        &self,
        binding: &StreamBinding,
        operation: OperationId,
        digest: Option<RequestDigest>,
    ) -> Result<OperationLookup, LedgerFailure> {
        let header = self.header(binding)?;
        binding.check_operation(operation)?;
        let n = operation.sequence().get();
        if n <= header.acknowledged_through {
            return Ok(OperationLookup::AcknowledgedRetired);
        }
        if n <= header.admitted_through {
            let record = self
                .state
                .operations
                .get(&operation)
                .ok_or_else(|| failure(LedgerFailureClass::InvalidDurableState))?;
            if digest.is_some_and(|v| v != record_binding(record).digest) {
                return Ok(OperationLookup::IdentityReusedWithDifferentContents);
            }
            return match record {
                RecoveredOperationRecord::Pending(_) => Ok(OperationLookup::Pending),
                RecoveredOperationRecord::Completed(v) => Ok(OperationLookup::Completed {
                    result: v.outcome.outcome(&super::canonical::CanonicalCodec)?,
                    publication_revision: v.outcome_revision,
                }),
            };
        }
        if header.status != StreamStatus::Active {
            return Ok(OperationLookup::AcknowledgedRetired);
        }
        let next = header
            .admitted_through
            .checked_add(1)
            .ok_or_else(|| failure(LedgerFailureClass::CounterExhausted))?;
        if n == next {
            Ok(OperationLookup::NeverAdmitted)
        } else {
            Ok(OperationLookup::SequenceGap {
                expected: OperationSequence::new(next)
                    .map_err(|_| failure(LedgerFailureClass::CounterExhausted))?,
            })
        }
    }
    pub fn close_admissions(&mut self) {
        self.closed = true;
    }
    pub fn requires_recovery(&self) -> bool {
        self.recovery_required
    }
    pub fn limits(&self) -> LedgerLimits {
        self.state.limits
    }
    pub fn recovery_tasks(&self) -> impl Iterator<Item = RecoveryTask<'_>> {
        self.state
            .operations
            .values()
            .filter_map(|record| match record {
                RecoveredOperationRecord::Pending(v) => Some(match v.kind {
                    PendingKind::AtomicMutation => RecoveryTask::AwaitOriginalAtomicRequest(v),
                    PendingKind::External { .. } => RecoveryTask::ReconcileExternal(v),
                }),
                RecoveredOperationRecord::Completed(_) => None,
            })
    }
    /// A bounded read projection, never an execution permit. Store validators use
    /// the borrowed inventory instead of cloning this export for each write.
    pub fn retained(&self) -> LedgerRecovery {
        LedgerRecovery {
            format_version: LEDGER_FORMAT_VERSION,
            quota_policy_version: LEDGER_QUOTA_POLICY_VERSION,
            limits: self.state.limits,
            streams: self.state.streams.values().cloned().collect(),
            operations: self.state.operations.values().cloned().collect(),
        }
    }
    pub fn stream(&self, binding: &StreamBinding) -> Result<&StreamRecord, LedgerFailure> {
        self.header(binding)
    }
    pub fn operation(
        &self,
        binding: &StreamBinding,
        operation: OperationId,
    ) -> Result<Option<&RecoveredOperationRecord>, LedgerFailure> {
        self.header(binding)?;
        binding.check_operation(operation)?;
        Ok(self.state.operations.get(&operation))
    }
    pub(super) fn header(&self, binding: &StreamBinding) -> Result<&StreamRecord, LedgerFailure> {
        if binding.server() != self.server {
            return Err(failure(LedgerFailureClass::ScopeMismatch));
        }
        let header = self
            .state
            .streams
            .get(&binding.stream().key())
            .ok_or_else(|| failure(LedgerFailureClass::UnknownStream))?;
        if header.binding != *binding {
            return Err(failure(LedgerFailureClass::ScopeMismatch));
        }
        Ok(header)
    }
    pub(super) fn pending(
        &self,
        binding: &StreamBinding,
        operation: OperationId,
        digest: RequestDigest,
    ) -> Result<&PendingOperationRecord, LedgerFailure> {
        self.header(binding)?;
        binding.check_operation(operation)?;
        let record = self
            .state
            .operations
            .get(&operation)
            .ok_or_else(|| failure(LedgerFailureClass::InvalidTransition))?;
        if record_binding(record).digest != digest {
            return Err(failure(LedgerFailureClass::IdentityReused));
        }
        match record {
            RecoveredOperationRecord::Pending(v) => Ok(v),
            RecoveredOperationRecord::Completed(_) => {
                Err(failure(LedgerFailureClass::InvalidTransition))
            }
        }
    }
    pub(super) fn ready(&self, context: TransactionContext) -> Result<(), LedgerFailure> {
        if self.recovery_required {
            return Err(failure(LedgerFailureClass::RecoveryRequired));
        }
        if self.prepared.is_some() {
            return Err(failure(LedgerFailureClass::PreparationBusy));
        }
        if context.expected_store() < self.known_store
            || context.base_organization() < self.known_organization
        {
            return Err(failure(LedgerFailureClass::InvalidTransition));
        }
        Ok(())
    }
    pub(super) fn store_only(context: TransactionContext) -> Result<(), LedgerFailure> {
        if context.base_organization() != context.resulting_organization()
            || context.operation_basis().is_some()
        {
            return Err(failure(LedgerFailureClass::InvalidTransition));
        }
        Ok(())
    }
    pub(super) fn prepare(
        &mut self,
        context: TransactionContext,
        writes: Vec<LedgerWrite>,
        resolution: PreparedResolution,
    ) -> Result<PreparedLedgerTransition, LedgerFailure> {
        let projection = LedgerProjection::new(context, writes)?;
        super::projection::validate_projection(self, &projection)?;
        let token = Arc::new(());
        // A dropped token can follow a durable store commit. Retain the exact
        // compaction proposal until resolution or validated recovery.
        self.prepared = Some(OutstandingPreparation {
            token: Arc::clone(&token),
            acknowledgement: projection.writes().iter().find_map(|write| match write {
                LedgerWrite::AcknowledgePrefix {
                    binding,
                    previous,
                    through,
                } => Some(AcknowledgementProposal {
                    binding: *binding,
                    previous: *previous,
                    through: *through,
                }),
                _ => None,
            }),
        });
        Ok(PreparedLedgerTransition {
            owner: Arc::clone(&self.owner),
            token,
            projection,
            resolution,
        })
    }
}

macro_rules! redacted_debug {
    ($($ty:ty),+ $(,)?) => { $(impl fmt::Debug for $ty {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(concat!(stringify!($ty), "([redacted])")) }
    })+ };
}
redacted_debug!(
    OperationLedger,
    AdmissionPreparation,
    ExecutionPermit,
    PreparedLedgerTransition,
    TransitionResolution,
    RecoveryTask<'_>
);
