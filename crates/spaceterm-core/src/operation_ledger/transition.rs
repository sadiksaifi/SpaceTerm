//! Durable progress, completion, cumulative compaction and commit disposition.
use std::collections::BTreeSet;
use std::sync::Arc;

use super::model::*;
use super::recovery::{progress_retirement, validate_progress_record};
use crate::authority::OrganizationRevision;
use crate::operation_ledger_contracts::*;
use crate::operations::{OperationId, Outcome, RequestDigest};
use crate::retirement::{RetirementCleanupState, RetirementDescriptor};
use crate::storage::CommitDisposition;

impl OperationLedger {
    /// Notify the ledger of the actor's checked non-ledger commit. No operation
    /// ownership, reservation or historical result changes in this transition.
    pub fn observe_store_commit(
        &mut self,
        receipt: crate::storage::CommitReceipt,
    ) -> Result<(), LedgerFailure> {
        if self.prepared.is_some()
            || self.recovery_required
            || receipt.store_revision <= self.known_store
            || receipt.organization_revision < self.known_organization
        {
            return Err(failure(LedgerFailureClass::InvalidTransition));
        }
        self.known_store = receipt.store_revision;
        self.known_organization = receipt.organization_revision;
        Ok(())
    }

    pub fn prepare_external_progress(
        &mut self,
        binding: &StreamBinding,
        operation: OperationId,
        digest: RequestDigest,
        progress: PendingExternalProgress,
        context: TransactionContext,
    ) -> Result<PreparedLedgerTransition, LedgerFailure> {
        self.ready(context)?;
        let old = self.pending(binding, operation, digest)?;
        let next_store = context
            .expected_store()
            .checked_successor()
            .ok_or_else(|| failure(LedgerFailureClass::CounterExhausted))?;
        let record = PendingOperationRecord {
            binding: old.binding,
            kind: old.kind,
            reservation: old.reservation,
            admitted_at: old.admitted_at,
            admission_organization_revision: old.admission_organization_revision,
            progress_recorded_at: next_store,
            external_progress: Some(progress),
        };
        validate_progress_record(
            &record,
            record
                .external_progress
                .as_ref()
                .ok_or_else(|| failure(LedgerFailureClass::InvalidExternalProgress))?,
        )?;
        validate_progress_transition(self, old, &record, context)?;
        let previous = old.progress_recorded_at;
        self.prepare(
            context,
            vec![LedgerWrite::RecordExternalProgress {
                previous_progress_store_revision: previous,
                record,
            }],
            PreparedResolution::Progress,
        )
    }
    pub fn prepare_completion(
        &mut self,
        binding: &StreamBinding,
        operation: OperationId,
        digest: RequestDigest,
        outcome: CompactOutcome,
        outcome_revision: OrganizationRevision,
        context: TransactionContext,
    ) -> Result<PreparedLedgerTransition, LedgerFailure> {
        self.ready(context)?;
        let old = self.pending(binding, operation, digest)?;
        let decoded = outcome.outcome(&super::canonical::CanonicalCodec)?;
        decoded
            .validate_scope(binding.stream())
            .map_err(|_| failure(LedgerFailureClass::ScopeMismatch))?;
        let basis = completion_basis(old, &decoded, outcome_revision, context)?;
        let record = CompletedOperationRecord {
            binding: old.binding,
            reservation: old.reservation,
            admitted_at: old.admitted_at,
            completed_at: context
                .expected_store()
                .checked_successor()
                .ok_or_else(|| failure(LedgerFailureClass::CounterExhausted))?,
            outcome,
            outcome_revision,
        };
        self.prepare(
            context,
            vec![LedgerWrite::CompleteOperation { record, basis }],
            PreparedResolution::Completed,
        )
    }
    pub fn prepare_acknowledgement(
        &mut self,
        binding: &StreamBinding,
        through: u64,
        context: TransactionContext,
    ) -> Result<Option<PreparedLedgerTransition>, LedgerFailure> {
        self.ready(context)?;
        Self::store_only(context)?;
        let header = self.header(binding)?;
        if through <= header.acknowledged_through {
            return Ok(None);
        }
        if through > header.admitted_through {
            return Err(failure(LedgerFailureClass::AcknowledgementAhead));
        }
        for (_, record) in self.state.operations.range(
            crate::operations::OperationId::new(
                binding.stream(),
                crate::operations::OperationSequence::new(
                    header
                        .acknowledged_through
                        .checked_add(1)
                        .ok_or_else(|| failure(LedgerFailureClass::CounterExhausted))?,
                )
                .map_err(|_| failure(LedgerFailureClass::CounterExhausted))?,
            )
                ..=crate::operations::OperationId::new(
                    binding.stream(),
                    crate::operations::OperationSequence::new(through)
                        .map_err(|_| failure(LedgerFailureClass::InvalidTransition))?,
                ),
        ) {
            if matches!(record, RecoveredOperationRecord::Pending(_)) {
                return Err(failure(LedgerFailureClass::AcknowledgementGap));
            }
        }
        let previous = header.acknowledged_through;
        Ok(Some(self.prepare(
            context,
            vec![LedgerWrite::AcknowledgePrefix {
                binding: *binding,
                previous,
                through,
            }],
            PreparedResolution::Acknowledged(through),
        )?))
    }
    pub fn prepare_retirement(
        &mut self,
        bindings: &[StreamBinding],
        reason: StreamRetirementReason,
        context: TransactionContext,
    ) -> Result<PreparedLedgerTransition, LedgerFailure> {
        self.ready(context)?;
        Self::store_only(context)?;
        if bindings.is_empty() || bindings.len() > self.state.limits.max_streams() as usize {
            return Err(failure(LedgerFailureClass::InvalidTransition));
        }
        let mut seen = BTreeSet::new();
        let mut writes = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let header = self.header(binding)?;
            if !seen.insert(binding.stream().key()) {
                return Err(failure(LedgerFailureClass::InvalidTransition));
            }
            let reason = match header.status {
                StreamStatus::Active => reason,
                StreamStatus::Retired(original) => original,
            };
            if reason == StreamRetirementReason::SequenceExhausted
                && header.admitted_through != u64::MAX
            {
                return Err(failure(LedgerFailureClass::InvalidTransition));
            }
            writes.push(LedgerWrite::RetireStream {
                binding: *binding,
                reason,
            });
        }
        self.prepare(context, writes, PreparedResolution::Retired)
    }
    /// Pairing can register dedicated capacity or retire streams in the same
    /// physical completion transaction. Additions grant no extra execution permit.
    pub fn extend_prepared(
        &mut self,
        prepared: &mut PreparedLedgerTransition,
        additions: Vec<LedgerWrite>,
    ) -> Result<(), LedgerFailure> {
        if self.recovery_required {
            return Err(failure(LedgerFailureClass::RecoveryRequired));
        }
        self.check_token(prepared)?;
        if matches!(prepared.resolution, PreparedResolution::Admitted(_, _)) {
            return Err(failure(LedgerFailureClass::InvalidTransition));
        }
        if additions.iter().any(|w| {
            !matches!(
                w,
                LedgerWrite::RegisterStream(_) | LedgerWrite::RetireStream { .. }
            )
        }) {
            return Err(failure(LedgerFailureClass::InvalidTransition));
        }
        let mut writes = prepared.projection.writes().to_vec();
        writes.extend(additions);
        let projection = LedgerProjection::new(prepared.projection.context(), writes)?;
        super::projection::validate_projection(self, &projection)?;
        prepared.projection = projection;
        Ok(())
    }
    pub fn resolve_commit(
        &mut self,
        prepared: PreparedLedgerTransition,
        disposition: CommitDisposition,
    ) -> Result<TransitionResolution, LedgerFailure> {
        if self.recovery_required {
            return Err(failure(LedgerFailureClass::RecoveryRequired));
        }
        self.check_token(&prepared)?;
        self.prepared = None;
        match disposition {
            CommitDisposition::NotApplied(error) => Ok(TransitionResolution::NotApplied(error)),
            CommitDisposition::Indeterminate => {
                self.recovery_required = true;
                self.uncertain = Some(prepared.projection);
                Ok(TransitionResolution::RecoveryRequired)
            }
            CommitDisposition::Durable(receipt) => {
                if receipt.store_revision != prepared.projection.resulting_store()
                    || receipt.organization_revision != prepared.projection.resulting_organization()
                {
                    self.recovery_required = true;
                    self.uncertain = Some(prepared.projection);
                    return Err(failure(LedgerFailureClass::InvalidDurableState));
                }
                super::projection::apply(&mut self.state, prepared.projection.writes());
                self.known_store = receipt.store_revision;
                self.known_organization = receipt.organization_revision;
                Ok(match prepared.resolution {
                    PreparedResolution::Registered => TransitionResolution::StreamRegistered,
                    PreparedResolution::Admitted(binding, kind) => {
                        self.executing.insert(binding.id);
                        TransitionResolution::Admitted(ExecutionPermit { binding, kind })
                    }
                    PreparedResolution::Progress => TransitionResolution::ExternalProgressRecorded,
                    PreparedResolution::Completed => {
                        for write in prepared.projection.writes() {
                            if let LedgerWrite::CompleteOperation { record, .. } = write {
                                self.executing.remove(&record.binding.id);
                                self.resumable.remove(&record.binding.id);
                            }
                        }
                        TransitionResolution::Completed
                    }
                    PreparedResolution::Acknowledged(through) => {
                        TransitionResolution::Acknowledged { through }
                    }
                    PreparedResolution::Retired => TransitionResolution::StreamRetired,
                })
            }
        }
    }
    fn check_token(&self, prepared: &PreparedLedgerTransition) -> Result<(), LedgerFailure> {
        if !Arc::ptr_eq(&self.owner, &prepared.owner)
            || self
                .prepared
                .as_ref()
                .is_none_or(|v| !Arc::ptr_eq(&v.token, &prepared.token))
        {
            return Err(failure(LedgerFailureClass::InvalidTransition));
        }
        Ok(())
    }
}

pub(super) fn validate_progress_transition(
    ledger: &OperationLedger,
    old: &PendingOperationRecord,
    new: &PendingOperationRecord,
    context: TransactionContext,
) -> Result<(), LedgerFailure> {
    let invalid = || failure(LedgerFailureClass::InvalidExternalProgress);
    if old.binding != new.binding
        || old.kind != new.kind
        || old.reservation != new.reservation
        || old.admitted_at != new.admitted_at
        || old.admission_organization_revision != new.admission_organization_revision
        || new.progress_recorded_at <= old.progress_recorded_at
    {
        return Err(invalid());
    }
    let old_progress = old.external_progress.as_ref().ok_or_else(invalid)?;
    let new_progress = new.external_progress.as_ref().ok_or_else(invalid)?;
    validate_progress_record(new, new_progress)?;
    // Quiescence bars new durable prerequisites, not cleanup or settlement of
    // work whose prerequisite committed before the barrier.
    let starts_work = matches!(old_progress.state(), ExternalProgressState::AwaitingIntent)
        || (progress_retirement(old_progress).is_none()
            && progress_retirement(new_progress).is_some()
            && !matches!(old_progress.state(), ExternalProgressState::RecordedIntent { reference, .. }
                if reference.phase() == ExternalIntentPhase::Started))
        || matches!(
            (old_progress.state(), new_progress.state()),
            (
                ExternalProgressState::RecordedIntent { reference: previous, .. },
                ExternalProgressState::RecordedIntent { reference: next, .. }
            ) if previous.phase() == ExternalIntentPhase::Prepared
                && next.phase() == ExternalIntentPhase::Started
        );
    if starts_work {
        if ledger.closed {
            return Err(failure(LedgerFailureClass::AdmissionsClosed));
        }
        if ledger
            .header(&StreamBinding::from_stream(old.binding.id.stream()))?
            .status
            != StreamStatus::Active
        {
            return Err(failure(LedgerFailureClass::Retired));
        }
    }
    match (old_progress.state(), new_progress.state()) {
        (
            ExternalProgressState::AwaitingIntent,
            ExternalProgressState::AcceptedRetirement(retirement),
        ) => {
            if retirement.acceptance_revision() != context.resulting_organization()
                || context
                    .operation_basis()
                    .is_none_or(|v| v.binding() != old.binding)
            {
                return Err(invalid());
            }
        }
        (
            ExternalProgressState::AwaitingIntent,
            ExternalProgressState::RecordedIntent {
                reference,
                retirement: None,
            },
        ) => {
            if reference.phase() != ExternalIntentPhase::Prepared
                || reference.recorded_at() != new.progress_recorded_at
            {
                return Err(invalid());
            }
        }
        (
            ExternalProgressState::AcceptedRetirement(old),
            ExternalProgressState::AcceptedRetirement(new),
        ) => validate_retirement_update(old, new)?,
        (
            ExternalProgressState::RecordedIntent {
                reference: previous,
                retirement: old_retirement,
            },
            ExternalProgressState::RecordedIntent {
                reference: next,
                retirement: new_retirement,
            },
        ) => {
            if previous.scope() != next.scope()
                || next.recorded_at() < previous.recorded_at()
                || next.recorded_at() > new.progress_recorded_at
            {
                return Err(invalid());
            }
            if !matches!(
                (previous.phase(), next.phase()),
                (ExternalIntentPhase::Prepared, ExternalIntentPhase::Started)
                    | (
                        ExternalIntentPhase::Started,
                        ExternalIntentPhase::ExternalFinished
                    )
            ) && previous != next
            {
                return Err(invalid());
            }
            if previous.phase() != next.phase() && next.recorded_at() != new.progress_recorded_at {
                return Err(invalid());
            }
            match (old_retirement, new_retirement) {
                (Some(old), Some(new)) => validate_retirement_update(old, new)?,
                (None, Some(new)) if next.phase() == ExternalIntentPhase::Started => {
                    if new.acceptance_revision() != context.resulting_organization()
                        || context
                            .operation_basis()
                            .is_none_or(|v| v.binding() != old.binding)
                    {
                        return Err(invalid());
                    }
                }
                (None, None) => {}
                _ => return Err(invalid()),
            }
        }
        _ => return Err(invalid()),
    }
    if context.resulting_organization() != context.base_organization()
        && context
            .operation_basis()
            .is_none_or(|v| v.binding() != old.binding)
    {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn validate_retirement_update(
    old: &RetirementDescriptor,
    new: &RetirementDescriptor,
) -> Result<(), LedgerFailure> {
    if old.batch() != new.batch()
        || old.binding() != new.binding()
        || old.assessment() != new.assessment()
        || old.acceptance_revision() != new.acceptance_revision()
        || old.target() != new.target()
        || old.panes() != new.panes()
        || old.completion() != new.completion()
        || old.parts().eventual_result != new.parts().eventual_result
        || old.sessions().len() != new.sessions().len()
    {
        return Err(failure(LedgerFailureClass::InvalidExternalProgress));
    }
    for (old, new) in old.sessions().iter().zip(new.sessions()) {
        if old.session() != new.session()
            || old.first_reason() != new.first_reason()
            || (old.cleanup() != RetirementCleanupState::AwaitingEvidence
                && old.cleanup() != new.cleanup())
        {
            return Err(failure(LedgerFailureClass::InvalidExternalProgress));
        }
    }
    Ok(())
}
pub(super) fn completion_basis(
    record: &PendingOperationRecord,
    outcome: &Outcome,
    revision: OrganizationRevision,
    context: TransactionContext,
) -> Result<CompletionRevisionBasis, LedgerFailure> {
    let invalid = || failure(LedgerFailureClass::OutcomeRevisionMismatch);
    if revision > context.resulting_organization() {
        return Err(invalid());
    }
    if let Outcome::Git(result) = outcome
        && result.revision().is_some_and(|v| v != revision)
    {
        return Err(invalid());
    }
    if let Some(progress) = &record.external_progress {
        if let Some(retirement) = progress_retirement(progress)
            && matches!(
                progress.state(),
                ExternalProgressState::AcceptedRetirement(_)
            )
        {
            if revision != retirement.acceptance_revision()
                || context.base_organization() != context.resulting_organization()
                || context.operation_basis().is_some()
            {
                return Err(invalid());
            }
            let eventual = retirement
                .parts()
                .eventual_result
                .outcome(&super::canonical::CanonicalCodec)?;
            if retirement
                .sessions()
                .iter()
                .any(|v| v.cleanup() == RetirementCleanupState::AwaitingEvidence)
                || (!matches!(outcome, Outcome::Failed(_))
                    && (!retirement.cleanup_confirmed() || outcome != &eventual))
            {
                return Err(failure(LedgerFailureClass::InvalidExternalProgress));
            }
            return Ok(CompletionRevisionBasis::RecordedProgress);
        }
        match progress.state() {
            ExternalProgressState::AwaitingIntent if !is_unstarted_cancellation(outcome) => {
                return Err(failure(LedgerFailureClass::InvalidExternalProgress));
            }
            ExternalProgressState::RecordedIntent { reference, .. } => {
                match reference.phase() {
                    ExternalIntentPhase::Prepared if !is_unstarted_cancellation(outcome) => {
                        return Err(failure(LedgerFailureClass::InvalidExternalProgress));
                    }
                    ExternalIntentPhase::Started => {
                        return Err(failure(LedgerFailureClass::InvalidExternalProgress));
                    }
                    _ => {}
                }
                if let Some(retirement) = progress_retirement(progress)
                    && (retirement
                        .sessions()
                        .iter()
                        .any(|v| v.cleanup() == RetirementCleanupState::AwaitingEvidence)
                        || (!retirement.cleanup_confirmed()
                            && !is_external_failure_result(outcome)))
                {
                    return Err(failure(LedgerFailureClass::InvalidExternalProgress));
                }
            }
            _ => {}
        }
    }
    let basis = context.operation_basis().ok_or_else(invalid)?;
    if basis.binding() != record.binding || basis.resulting() != revision {
        return Err(invalid());
    }
    Ok(CompletionRevisionBasis::OwnedReduction(basis))
}
fn is_unstarted_cancellation(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Failed(_) => true,
        Outcome::Git(v) => matches!(
            v.parts(),
            crate::worktrees::types::GitMutationOutcomeParts::Recovered { .. }
                | crate::worktrees::types::GitMutationOutcomeParts::Failed {
                    external: crate::worktrees::types::ExternalDisposition::NoEffectEstablished,
                    ..
                }
        ),
        _ => false,
    }
}

fn is_external_failure_result(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Failed(_) => true,
        Outcome::Git(v) => matches!(
            v.parts(),
            crate::worktrees::types::GitMutationOutcomeParts::Failed { .. }
        ),
        _ => false,
    }
}
