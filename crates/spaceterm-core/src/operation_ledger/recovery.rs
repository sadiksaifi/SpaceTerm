//! Borrowed validation before installation. Interval cardinality, not iteration
//! from zero to a persisted high-water, bounds recovery work.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::model::*;
use crate::identity::{OperationStreamId, ServerId};
use crate::operation_ledger_contracts::*;
use crate::operations::{Command, OperationBinding, OperationId};
use crate::storage::{RecoveredState, StoreFailure};
use crate::worktrees::intent::{GitIntentPhase, GitIntentReference, GitRecoveryLedger};

/// Validates ledger-owned invariants without cloning retained results. Owning
/// intent, pairing and reducer validation additionally uses the complete bundle.
pub fn validate_recovery(
    server: ServerId,
    recovered: &LedgerRecovery,
) -> Result<(), LedgerFailure> {
    if recovered.format_version != LEDGER_FORMAT_VERSION
        || recovered.quota_policy_version != LEDGER_QUOTA_POLICY_VERSION
    {
        return Err(failure(LedgerFailureClass::UnsupportedFormat));
    }
    let limits = recovered.limits;
    if recovered.streams.len() > limits.max_streams() as usize
        || recovered.operations.len() > limits.global_slots() as usize
    {
        return Err(failure(LedgerFailureClass::QuotaExceeded));
    }
    let mut streams = BTreeMap::new();
    let mut clients = BTreeMap::new();
    let mut usage = BTreeMap::<OperationStreamId, (u32, u32, u64)>::new();
    let mut total = 0u32;
    let mut ordinary = 0u32;
    let mut bytes = 0u64;
    for header in &recovered.streams {
        header.validate_shape()?;
        if header.binding.server() != server
            || streams
                .insert(header.binding.stream().key(), header)
                .is_some()
        {
            return Err(failure(LedgerFailureClass::InvalidDurableState));
        }
        let count = clients
            .entry((header.binding.pairing(), header.binding.client()))
            .or_insert(0u32);
        *count += 1;
        if *count > limits.max_streams_per_client_binding() {
            return Err(failure(LedgerFailureClass::StreamLimit));
        }
        if header.admitted_through - header.acknowledged_through
            > u64::from(limits.per_stream_slots())
        {
            return Err(failure(LedgerFailureClass::QuotaExceeded));
        }
        if let Some(reserved) = header.dormant_reservation {
            charge(
                &mut usage,
                &mut total,
                &mut ordinary,
                &mut bytes,
                header.binding.stream(),
                reserved,
            )?;
        }
    }
    let mut seen = BTreeSet::new();
    let mut counts = BTreeMap::new();
    let mut retirement_bytes = 0usize;
    let mut retirement_count = 0usize;
    let mut retirement_batches = BTreeSet::new();
    for record in &recovered.operations {
        let binding = record_binding(record);
        let header = streams
            .get(&binding.id.stream().key())
            .ok_or_else(|| failure(LedgerFailureClass::InvalidDurableState))?;
        let n = binding.id.sequence().get();
        if header.binding.stream() != binding.id.stream()
            || n <= header.acknowledged_through
            || n > header.admitted_through
            || !seen.insert(binding.id)
        {
            return Err(failure(LedgerFailureClass::InvalidDurableState));
        }
        let count = counts.entry(binding.id.stream()).or_insert(0u64);
        *count += 1;
        let reserved = reservation(record);
        let class_valid = match header.purpose {
            StreamPurpose::Ordinary => matches!(
                reserved.class(),
                ReservationClass::Ordinary | ReservationClass::Safety
            ),
            StreamPurpose::SelfRevocation => {
                reserved.class() == ReservationClass::DedicatedSelfRevocation
            }
            StreamPurpose::Enrollment { .. } => {
                reserved.class() == ReservationClass::DedicatedEnrollment
            }
        };
        if !class_valid {
            return Err(failure(LedgerFailureClass::PurposeMismatch));
        }
        match record {
            RecoveredOperationRecord::Pending(v) => {
                v.validate_shape()?;
                if let Some(progress) = &v.external_progress {
                    validate_progress_record(v, progress)?;
                    if let Some(retirement) = progress_retirement(progress) {
                        if !retirement_batches.insert(retirement.batch()) {
                            return Err(failure(LedgerFailureClass::InvalidDurableState));
                        }
                        retirement_count += 1;
                        retirement_bytes = retirement_bytes
                            .checked_add(retirement.value_bytes().map_err(|_| {
                                failure(LedgerFailureClass::InvalidExternalProgress)
                            })?)
                            .ok_or_else(|| failure(LedgerFailureClass::QuotaExceeded))?;
                    }
                }
            }
            RecoveredOperationRecord::Completed(v) => {
                v.validate_shape(&super::canonical::CanonicalCodec)?;
                if let crate::operations::Outcome::Git(result) =
                    v.outcome.outcome(&super::canonical::CanonicalCodec)?
                    && result.revision().is_some_and(|r| r != v.outcome_revision)
                {
                    return Err(failure(LedgerFailureClass::OutcomeRevisionMismatch));
                }
            }
        }
        charge(
            &mut usage,
            &mut total,
            &mut ordinary,
            &mut bytes,
            binding.id.stream(),
            reserved,
        )?;
    }
    for header in &recovered.streams {
        if counts.get(&header.binding.stream()).copied().unwrap_or(0)
            != header.admitted_through - header.acknowledged_through
        {
            return Err(failure(LedgerFailureClass::InvalidDurableState));
        }
    }
    if total > limits.global_slots()
        || ordinary > limits.ordinary_global_slots()
        || bytes > limits.global_reserved_byte_ceiling()
        || usage.values().any(|(count, ordinary, bytes)| {
            *count > limits.per_stream_slots()
                || *ordinary > limits.ordinary_stream_slots()
                || *bytes > limits.per_stream_reserved_byte_ceiling()
        })
        || retirement_count > 64
        || retirement_bytes > crate::retirement::MAX_RETAINED_RETIREMENT_BYTES
    {
        return Err(failure(LedgerFailureClass::QuotaExceeded));
    }
    Ok(())
}
fn charge(
    usage: &mut BTreeMap<OperationStreamId, (u32, u32, u64)>,
    total: &mut u32,
    ordinary: &mut u32,
    bytes: &mut u64,
    stream: OperationStreamId,
    reserved: OutcomeReservation,
) -> Result<(), LedgerFailure> {
    let error = || failure(LedgerFailureClass::QuotaExceeded);
    *total = total.checked_add(1).ok_or_else(error)?;
    *bytes = bytes
        .checked_add(u64::from(reserved.bytes()))
        .ok_or_else(error)?;
    let (count, ordinary_count, stream_bytes) = usage.entry(stream).or_default();
    *count = count.checked_add(1).ok_or_else(error)?;
    *stream_bytes = stream_bytes
        .checked_add(u64::from(reserved.bytes()))
        .ok_or_else(error)?;
    if reserved.class() == ReservationClass::Ordinary {
        *ordinary = ordinary.checked_add(1).ok_or_else(error)?;
        *ordinary_count = ordinary_count.checked_add(1).ok_or_else(error)?;
    }
    Ok(())
}
pub(super) fn progress_retirement(
    progress: &PendingExternalProgress,
) -> Option<&crate::retirement::RetirementDescriptor> {
    match progress.state() {
        ExternalProgressState::AcceptedRetirement(v) => Some(v),
        ExternalProgressState::RecordedIntent { retirement, .. } => retirement.as_deref(),
        ExternalProgressState::AwaitingIntent => None,
    }
}
pub(super) fn validate_progress_record(
    record: &PendingOperationRecord,
    progress: &PendingExternalProgress,
) -> Result<(), LedgerFailure> {
    if progress.binding() != record.binding
        || record.kind
            != (PendingKind::External {
                effect: progress.effect(),
            })
    {
        return Err(failure(LedgerFailureClass::InvalidExternalProgress));
    }
    if let ExternalProgressState::RecordedIntent { reference, .. } = progress.state()
        && (reference.recorded_at() < record.admitted_at
            || reference.recorded_at() > record.progress_recorded_at)
    {
        return Err(failure(LedgerFailureClass::InvalidExternalProgress));
    }
    if matches!(progress.state(), ExternalProgressState::AwaitingIntent)
        && record.progress_recorded_at != record.admitted_at
    {
        return Err(failure(LedgerFailureClass::InvalidExternalProgress));
    }
    if let Some(retirement) = progress_retirement(progress) {
        if retirement.acceptance_revision() < record.admission_organization_revision {
            return Err(failure(LedgerFailureClass::OutcomeRevisionMismatch));
        }
        retirement
            .parts()
            .eventual_result
            .outcome(&super::canonical::CanonicalCodec)?
            .validate_scope(record.binding.id.stream())
            .map_err(|_| failure(LedgerFailureClass::InvalidCompactOutcome))?;
    }
    Ok(())
}

/// Ledger's portion of the complete recovery validator. It does not verify
/// signatures or reducer state; those owners compose their validators alongside it.
pub fn validate_recovered_state(recovered: &RecoveredState) -> Result<(), LedgerFailure> {
    let Some(ledger) = &recovered.ledger else {
        if !recovered.git_intents.is_empty()
            || recovered.pairing.as_ref().is_some_and(|p| {
                !p.parts().authorizations.is_empty() || !p.parts().enrollments.is_empty()
            })
        {
            return Err(failure(LedgerFailureClass::InvalidDurableState));
        }
        return Ok(());
    };
    if recovered.git_intents.len() > crate::worktrees::types::MAX_GIT_INTENTS {
        return Err(failure(LedgerFailureClass::QuotaExceeded));
    }
    if !recovered.streams.is_empty() || !recovered.operations.is_empty() {
        return Err(failure(LedgerFailureClass::InvalidDurableState));
    }
    validate_recovery(recovered.organization.server, ledger)?;
    let marks = recovered.high_water;
    for header in &ledger.streams {
        let stream = header.binding.stream();
        if stream.key().local() > marks.operation_stream
            || stream.pairing().local() > marks.pairing
            || stream.client().local() > marks.client
        {
            return Err(failure(LedgerFailureClass::InvalidDurableState));
        }
    }
    for record in &ledger.operations {
        match record {
            RecoveredOperationRecord::Pending(v) => {
                if v.progress_recorded_at > recovered.store_revision
                    || v.admission_organization_revision > recovered.organization.revision
                {
                    return Err(failure(LedgerFailureClass::InvalidDurableState));
                }
                if let Some(progress) = &v.external_progress {
                    if let Some(retirement) = progress_retirement(progress)
                        && (retirement.acceptance_revision() > recovered.organization.revision
                            || retirement.batch().epoch() > recovered.last_epoch
                            || retirement.panes().iter().any(|p| {
                                p.pane().local() > marks.pane
                                    || p.tab().local() > marks.tab
                                    || p.workspace().local() > marks.workspace
                                    || p.worktree().is_some_and(|w| w.local() > marks.worktree)
                            }))
                    {
                        return Err(failure(LedgerFailureClass::InvalidDurableState));
                    }
                    match progress.state() {
                        ExternalProgressState::RecordedIntent { reference, .. } => match reference
                            .scope()
                        {
                            ExternalIntentScope::Git(scope) => {
                                let intent = recovered
                                    .git_intents
                                    .iter()
                                    .find(|v| v.binding() == v_binding(reference))
                                    .ok_or_else(|| {
                                        failure(LedgerFailureClass::InvalidExternalProgress)
                                    })?;
                                if intent.parts().repository.scope() != scope
                                    || external_phase(intent.phase()) != reference.phase()
                                {
                                    return Err(failure(
                                        LedgerFailureClass::InvalidExternalProgress,
                                    ));
                                }
                                if let Some(retirement) = progress_retirement(progress) {
                                    let affected = intent.parts().affected.parts();
                                    let target_matches = matches!(retirement.target(), crate::operations::AssessmentTarget::RemoveWorktree { scope: target, worktree }
                                        if target == scope && worktree == intent.parts().action.worktree());
                                    let hierarchy_matches = retirement.panes().len()
                                        == affected.panes.len()
                                        && retirement.panes().iter().all(|pane| {
                                            affected.panes.iter().any(|old| {
                                                let hierarchy = old.hierarchy;
                                                let tag = affected
                                                    .tabs
                                                    .iter()
                                                    .find(|v| v.id == hierarchy.tab)
                                                    .and_then(|v| v.worktree);
                                                pane.pane() == hierarchy.pane
                                                    && pane.tab() == hierarchy.tab
                                                    && pane.workspace() == hierarchy.workspace
                                                    && pane.worktree() == tag
                                                    && pane.invalidation_generation()
                                                        > hierarchy.session_generation
                                            })
                                        });
                                    let sessions: std::collections::BTreeSet<_> =
                                        affected.panes.iter().filter_map(|v| v.session).collect();
                                    let retired: std::collections::BTreeSet<_> =
                                        retirement.sessions().iter().map(|v| v.session()).collect();
                                    if !target_matches || !hierarchy_matches || sessions != retired
                                    {
                                        return Err(failure(
                                            LedgerFailureClass::InvalidExternalProgress,
                                        ));
                                    }
                                }
                            }
                            // H0 has no pairing-bound management intent slot. Native
                            // management has its own identity and is never fabricated.
                            ExternalIntentScope::Management => {
                                return Err(failure(LedgerFailureClass::InvalidExternalProgress));
                            }
                        },
                        ExternalProgressState::AwaitingIntent => {
                            if recovered
                                .git_intents
                                .iter()
                                .any(|i| i.binding().id == v.binding.id)
                            {
                                return Err(failure(LedgerFailureClass::InvalidExternalProgress));
                            }
                        }
                        ExternalProgressState::AcceptedRetirement(_) => {}
                    }
                }
            }
            RecoveredOperationRecord::Completed(v) => {
                if v.completed_at > recovered.store_revision
                    || v.outcome_revision > recovered.organization.revision
                    || !outcome_fits_marks(
                        &v.outcome.outcome(&super::canonical::CanonicalCodec)?,
                        marks,
                    )
                {
                    return Err(failure(LedgerFailureClass::InvalidDurableState));
                }
            }
        }
    }
    for intent in &recovered.git_intents {
        let record = ledger
            .operations
            .iter()
            .find(|v| record_binding(v) == intent.binding())
            .ok_or_else(|| failure(LedgerFailureClass::InvalidExternalProgress))?;
        if !matches!(record, RecoveredOperationRecord::Pending(v) if matches!(v.external_progress.as_ref().map(|p| p.state()), Some(ExternalProgressState::RecordedIntent { reference, .. }) if reference.scope() == ExternalIntentScope::Git(intent.parts().repository.scope()) && reference.phase() == external_phase(intent.phase())))
        {
            return Err(failure(LedgerFailureClass::InvalidExternalProgress));
        }
    }
    if let Some(pairing) = &recovered.pairing {
        pairing
            .validate_for(
                recovered.organization.server,
                recovered.store_revision,
                marks,
            )
            .map_err(|_| failure(LedgerFailureClass::InvalidDurableState))?;
        validate_pairing_purposes(ledger, pairing)?;
    } else if ledger
        .streams
        .iter()
        .any(|v| v.purpose != StreamPurpose::Ordinary)
    {
        return Err(failure(LedgerFailureClass::PurposeMismatch));
    }
    Ok(())
}
fn v_binding(reference: &ExternalIntentReference) -> OperationBinding {
    reference.binding()
}
pub(super) fn external_phase(phase: GitIntentPhase) -> ExternalIntentPhase {
    match phase {
        GitIntentPhase::Prepared => ExternalIntentPhase::Prepared,
        GitIntentPhase::Started => ExternalIntentPhase::Started,
        GitIntentPhase::ExternalFinished => ExternalIntentPhase::ExternalFinished,
    }
}
fn validate_pairing_purposes(
    ledger: &LedgerRecovery,
    pairing: &crate::pairing::PairingRecovery,
) -> Result<(), LedgerFailure> {
    let mut required = BTreeMap::new();
    for authorization in &pairing.parts().authorizations {
        for client in &authorization.parts().clients {
            required.insert(client.parts().ordinary, StreamPurpose::Ordinary);
            required.insert(
                client.parts().self_revocation,
                StreamPurpose::SelfRevocation,
            );
        }
    }
    for enrollment in &pairing.parts().enrollments {
        required.insert(
            enrollment.parts().enrollment_stream,
            StreamPurpose::Enrollment {
                claim_digest: enrollment.parts().claim_digest,
            },
        );
    }
    for (stream, purpose) in &required {
        if !ledger
            .streams
            .iter()
            .any(|v| v.binding.stream() == *stream && v.purpose == *purpose)
        {
            return Err(failure(LedgerFailureClass::PurposeMismatch));
        }
    }
    for authorization in &pairing.parts().authorizations {
        if matches!(
            authorization.parts().record.state,
            crate::pairing::PairingState::Revoked { .. }
        ) {
            for client in &authorization.parts().clients {
                for stream in [client.parts().ordinary, client.parts().self_revocation] {
                    if ledger
                        .streams
                        .iter()
                        .any(|v| v.binding.stream() == stream && v.status == StreamStatus::Active)
                    {
                        return Err(failure(LedgerFailureClass::InvalidDurableState));
                    }
                }
            }
        }
    }
    for enrollment in &pairing.parts().enrollments {
        let header = ledger
            .streams
            .iter()
            .find(|v| v.binding.stream() == enrollment.parts().enrollment_stream)
            .ok_or_else(|| failure(LedgerFailureClass::PurposeMismatch))?;
        if let Some(record) = ledger
            .operations
            .iter()
            .find(|v| record_binding(v).id.stream() == enrollment.parts().enrollment_stream)
            && record_binding(record).digest != enrollment.parts().enrollment_request_digest
        {
            return Err(failure(LedgerFailureClass::IdentityReused));
        }
        match enrollment.parts().state {
            crate::pairing::EnrollmentReservationState::Consumed { .. }
                if header.admitted_through != 1 =>
            {
                return Err(failure(LedgerFailureClass::InvalidDurableState));
            }
            crate::pairing::EnrollmentReservationState::Invalidated
                if header.status == StreamStatus::Active =>
            {
                return Err(failure(LedgerFailureClass::InvalidDurableState));
            }
            _ => {}
        }
    }
    for header in &ledger.streams {
        if header.purpose != StreamPurpose::Ordinary
            && required.get(&header.binding.stream()) != Some(&header.purpose)
        {
            return Err(failure(LedgerFailureClass::PurposeMismatch));
        }
    }
    Ok(())
}

impl OperationLedger {
    /// Installs ledger-owned facts only. Composition validates owning pairing,
    /// reducer and intent facts through validate_recovered_state and their owner
    /// validators before exposing the complete OrganizationStore recovery.
    pub fn recover(server: ServerId, recovered: LedgerRecovery) -> Result<Self, LedgerFailure> {
        validate_recovery(server, &recovered)?;
        let known_store = recovered
            .operations
            .iter()
            .map(|v| match v {
                RecoveredOperationRecord::Pending(p) => p.progress_recorded_at,
                RecoveredOperationRecord::Completed(c) => c.completed_at,
            })
            .max()
            .unwrap_or_default();
        let known_organization = recovered
            .operations
            .iter()
            .map(|v| match v {
                RecoveredOperationRecord::Pending(p) => p
                    .external_progress
                    .as_ref()
                    .and_then(progress_retirement)
                    .map_or(p.admission_organization_revision, |r| {
                        r.acceptance_revision()
                            .max(p.admission_organization_revision)
                    }),
                RecoveredOperationRecord::Completed(c) => c.outcome_revision,
            })
            .max()
            .unwrap_or_default();
        let resumable = recovered
            .operations
            .iter()
            .filter_map(|v| match v {
                RecoveredOperationRecord::Pending(p) if p.kind == PendingKind::AtomicMutation => {
                    Some(p.binding.id)
                }
                _ => None,
            })
            .collect();
        Ok(Self {
            server,
            state: LedgerState {
                limits: recovered.limits,
                streams: recovered
                    .streams
                    .into_iter()
                    .map(|v| (v.binding.stream().key(), v))
                    .collect(),
                operations: recovered
                    .operations
                    .into_iter()
                    .map(|v| (record_binding(&v).id, v))
                    .collect(),
            },
            owner: Arc::new(()),
            prepared: None,
            closed: false,
            recovery_required: false,
            executing: BTreeSet::new(),
            resumable,
            uncertain: None,
            known_store,
            known_organization,
        })
    }
    pub fn install_recovery(&mut self, recovered: LedgerRecovery) -> Result<(), LedgerFailure> {
        self.recovery_required = true;
        let mut candidate = Self::recover(self.server, recovered)?;
        // Validated recovery cannot undo previously verified durable identity,
        // acknowledgement, retirement or completed-result facts.
        for old in self.state.streams.values() {
            let new = candidate
                .state
                .streams
                .get(&old.binding.stream().key())
                .ok_or_else(|| failure(LedgerFailureClass::InvalidDurableState))?;
            if new.binding != old.binding
                || new.purpose != old.purpose
                || new.admitted_through < old.admitted_through
                || new.acknowledged_through < old.acknowledged_through
                || (old.status != StreamStatus::Active && new.status != old.status)
            {
                return Err(failure(LedgerFailureClass::InvalidDurableState));
            }
            // Only the exact prepared or uncertain acknowledgement can authorize
            // compaction. Monotonic prefixes alone do not prove consent.
            let prepared_acknowledgement = self
                .prepared
                .as_ref()
                .and_then(|prepared| prepared.acknowledgement.as_ref())
                .is_some_and(|proposal| {
                    proposal.binding == old.binding
                        && proposal.previous == old.acknowledged_through
                        && proposal.through == new.acknowledged_through
                });
            let uncertain_acknowledgement = self.uncertain.as_ref().is_some_and(|projection| {
                projection.writes().iter().any(|write| {
                    matches!(write, LedgerWrite::AcknowledgePrefix { binding, previous, through }
                        if *binding == old.binding
                            && *previous == old.acknowledged_through
                            && *through == new.acknowledged_through)
                })
            });
            if new.acknowledged_through != old.acknowledged_through
                && !prepared_acknowledgement
                && !uncertain_acknowledgement
            {
                return Err(failure(LedgerFailureClass::InvalidDurableState));
            }
        }
        for (id, old) in &self.state.operations {
            let header = candidate.header(&StreamBinding::from_stream(id.stream()))?;
            if id.sequence().get() <= header.acknowledged_through {
                if matches!(old, RecoveredOperationRecord::Pending(_)) {
                    return Err(failure(LedgerFailureClass::InvalidDurableState));
                }
                continue;
            }
            let new = candidate
                .state
                .operations
                .get(id)
                .ok_or_else(|| failure(LedgerFailureClass::InvalidDurableState))?;
            if record_binding(old) != record_binding(new)
                || reservation(old) != reservation(new)
                || matches!((old, new), (RecoveredOperationRecord::Completed(_), _) if old != new)
            {
                return Err(failure(LedgerFailureClass::InvalidDurableState));
            }
            if let RecoveredOperationRecord::Pending(old) = old {
                match new {
                    RecoveredOperationRecord::Pending(new) => {
                        if old.kind != new.kind
                            || old.admitted_at != new.admitted_at
                            || old.admission_organization_revision
                                != new.admission_organization_revision
                            || new.progress_recorded_at < old.progress_recorded_at
                        {
                            return Err(failure(LedgerFailureClass::InvalidDurableState));
                        }
                        if new.progress_recorded_at == old.progress_recorded_at && old != new {
                            return Err(failure(LedgerFailureClass::InvalidDurableState));
                        }
                        if let (Some(previous), Some(next)) =
                            (&old.external_progress, &new.external_progress)
                        {
                            let rank = |value: &PendingExternalProgress| match value.state() {
                                ExternalProgressState::AwaitingIntent => 0,
                                ExternalProgressState::AcceptedRetirement(_) => 1,
                                ExternalProgressState::RecordedIntent { reference, .. } => {
                                    match reference.phase() {
                                        ExternalIntentPhase::Prepared => 1,
                                        ExternalIntentPhase::Started => 2,
                                        ExternalIntentPhase::ExternalFinished => 3,
                                    }
                                }
                            };
                            if rank(next) < rank(previous) {
                                return Err(failure(LedgerFailureClass::InvalidDurableState));
                            }
                            if let ExternalProgressState::RecordedIntent {
                                reference: before, ..
                            } = previous.state()
                                && !matches!(next.state(), ExternalProgressState::RecordedIntent { reference: after, .. } if before.scope() == after.scope() && before.recorded_at() <= after.recorded_at())
                            {
                                return Err(failure(LedgerFailureClass::InvalidDurableState));
                            }
                            if let Some(before) = progress_retirement(previous) {
                                let after = progress_retirement(next).ok_or_else(|| {
                                    failure(LedgerFailureClass::InvalidDurableState)
                                })?;
                                super::transition::validate_retirement_update(before, after)
                                    .map_err(|_| {
                                        failure(LedgerFailureClass::InvalidDurableState)
                                    })?;
                            }
                        }
                    }
                    RecoveredOperationRecord::Completed(new) => {
                        if new.admitted_at != old.admitted_at
                            || new.completed_at <= old.progress_recorded_at
                        {
                            return Err(failure(LedgerFailureClass::InvalidDurableState));
                        }
                    }
                }
            }
        }
        if let Some(uncertain) = &self.uncertain {
            for write in uncertain.writes() {
                match write {
                    LedgerWrite::RegisterStream(record) => {
                        if candidate.state.streams.get(&record.binding.stream().key()).is_some_and(|v| v.binding != record.binding || v.purpose != record.purpose) { return Err(failure(LedgerFailureClass::InvalidDurableState)); }
                    },
                    LedgerWrite::AdmitOperation(record) => {
                        if candidate.state.operations.get(&record.binding.id).is_some_and(|v| !matches!(v, RecoveredOperationRecord::Pending(new) if new.as_ref() == record)) { return Err(failure(LedgerFailureClass::InvalidDurableState)); }
                    },
                    LedgerWrite::CompleteOperation { record, .. } => {
                        let valid = match candidate.state.operations.get(&record.binding.id) {
                            Some(RecoveredOperationRecord::Completed(new)) => new == record,
                            Some(RecoveredOperationRecord::Pending(new)) => matches!(
                                self.state.operations.get(&record.binding.id),
                                Some(RecoveredOperationRecord::Pending(old)) if old == new
                            ),
                            None => false,
                        };
                        if !valid {
                            return Err(failure(LedgerFailureClass::InvalidDurableState));
                        }
                    },
                    LedgerWrite::RecordExternalProgress { record, .. } => {
                        if let Some(RecoveredOperationRecord::Pending(new)) = candidate.state.operations.get(&record.binding.id)
                            && new.progress_recorded_at == record.progress_recorded_at && new.as_ref() != record { return Err(failure(LedgerFailureClass::InvalidDurableState)); }
                    },
                    LedgerWrite::AcknowledgePrefix { binding, previous, through } => {
                        let new = candidate.header(binding)?;
                        if new.acknowledged_through != *previous && new.acknowledged_through != *through { return Err(failure(LedgerFailureClass::InvalidDurableState)); }
                    },
                    LedgerWrite::RetireStream { binding, reason } => {
                        let new = candidate.header(binding)?;
                        if new.status != StreamStatus::Active && new.status != StreamStatus::Retired(*reason) { return Err(failure(LedgerFailureClass::InvalidDurableState)); }
                    },
                }
                let binding = match write {
                    LedgerWrite::AdmitOperation(v)
                    | LedgerWrite::RecordExternalProgress { record: v, .. } => Some(v.binding),
                    LedgerWrite::CompleteOperation { record, .. } => Some(record.binding),
                    _ => None,
                };
                if let Some(binding) = binding
                    && candidate
                        .state
                        .operations
                        .get(&binding.id)
                        .is_some_and(|v| record_binding(v) != binding)
                {
                    return Err(failure(LedgerFailureClass::InvalidDurableState));
                }
            }
        }
        candidate.closed = self.closed;
        candidate.known_store = candidate.known_store.max(self.known_store);
        candidate.known_organization = candidate.known_organization.max(self.known_organization);
        // Running service-owned external effects are not cancelled by recovery.
        candidate.executing = std::mem::take(&mut self.executing);
        candidate.executing.retain(|id| {
            matches!(
                candidate.state.operations.get(id),
                Some(RecoveredOperationRecord::Pending(_))
            )
        });
        candidate
            .resumable
            .retain(|id| !candidate.executing.contains(id));
        *self = candidate;
        Ok(())
    }
    pub fn resume_recovered_atomic(
        &mut self,
        binding: &StreamBinding,
        operation: OperationId,
        command: &Command,
    ) -> Result<ExecutionPermit, LedgerFailure> {
        if self.recovery_required {
            return Err(failure(LedgerFailureClass::RecoveryRequired));
        }
        if self.prepared.is_some() {
            return Err(failure(LedgerFailureClass::PreparationBusy));
        }
        if self.closed {
            return Err(failure(LedgerFailureClass::AdmissionsClosed));
        }
        if self.header(binding)?.status != StreamStatus::Active {
            return Err(failure(LedgerFailureClass::Retired));
        }
        let digest = super::canonical::canonical_request_digest(binding, operation, command)?;
        let record = self.pending(binding, operation, digest)?;
        if record.kind != PendingKind::AtomicMutation
            || super::admission::command_pending_kind(command) != PendingKind::AtomicMutation
            || super::admission::reservation_class(self.header(binding)?.purpose, command)?
                != record.reservation.class()
            || !self.resumable.contains(&operation)
            || self.executing.contains(&operation)
        {
            return Err(failure(LedgerFailureClass::InvalidTransition));
        }
        let permit = ExecutionPermit {
            binding: record.binding,
            kind: record.kind,
        };
        self.resumable.remove(&operation);
        self.executing.insert(operation);
        Ok(permit)
    }
}
impl GitRecoveryLedger for OperationLedger {
    fn pending_git_progress(
        &self,
        binding: OperationBinding,
    ) -> Result<Option<GitIntentReference>, StoreFailure> {
        let record = self
            .operation(&StreamBinding::from_stream(binding.id.stream()), binding.id)
            .map_err(|_| StoreFailure::InvalidDurableState)?;
        if record.is_some_and(|r| record_binding(r) != binding) {
            return Err(StoreFailure::InvalidDurableState);
        }
        Ok(match record {
            Some(RecoveredOperationRecord::Pending(v)) => {
                match v.external_progress.as_ref().map(|p| p.state()) {
                    Some(ExternalProgressState::RecordedIntent { reference, .. })
                        if matches!(reference.scope(), ExternalIntentScope::Git(_)) =>
                    {
                        Some(GitIntentReference::new(
                            binding,
                            match reference.phase() {
                                ExternalIntentPhase::Prepared => GitIntentPhase::Prepared,
                                ExternalIntentPhase::Started => GitIntentPhase::Started,
                                ExternalIntentPhase::ExternalFinished => {
                                    GitIntentPhase::ExternalFinished
                                }
                            },
                        ))
                    }
                    _ => None,
                }
            }
            _ => None,
        })
    }
    fn is_awaiting_git_intent(&self, binding: OperationBinding) -> Result<bool, StoreFailure> {
        Ok(
            matches!(self.operation(&StreamBinding::from_stream(binding.id.stream()), binding.id).map_err(|_| StoreFailure::InvalidDurableState)?, Some(RecoveredOperationRecord::Pending(v)) if v.binding == binding && v.kind == (PendingKind::External { effect: ExternalEffectKind::GitWrite }) && matches!(v.external_progress.as_ref().map(|p| p.state()), Some(ExternalProgressState::AwaitingIntent))),
        )
    }
    fn has_completed_git_result(&self, operation: OperationId) -> Result<bool, StoreFailure> {
        Ok(
            matches!(self.operation(&StreamBinding::from_stream(operation.stream()), operation).map_err(|_| StoreFailure::InvalidDurableState)?, Some(RecoveredOperationRecord::Completed(v)) if matches!(v.outcome.outcome(&super::canonical::CanonicalCodec).map_err(|_| StoreFailure::InvalidDurableState)?, crate::operations::Outcome::Git(_))),
        )
    }
}

/// Historical identity checks never require current target existence.
pub fn outcome_fits_marks(
    outcome: &crate::operations::Outcome,
    marks: crate::identity::IdentityHighWater,
) -> bool {
    use crate::operations::{FieldConflict, Outcome};
    use crate::organization::ControlRecord;
    let stream_fits = |stream: crate::identity::OperationStreamId| {
        stream.key().local() <= marks.operation_stream
            && stream.pairing().local() <= marks.pairing
            && stream.client().local() <= marks.client
    };
    match outcome {
        Outcome::CreatedWithControl(v) => v.fits_high_water(marks),
        Outcome::ControlGranted(v) => {
            v.grant().pane().local() <= marks.pane && v.grant().client().local() <= marks.client
        }
        Outcome::Assessment(v) => {
            stream_fits(v.reference().operation.stream())
                && v.summary().target().fits_high_water(marks)
        }
        Outcome::Pairing(v) => v.fits_marks(marks),
        Outcome::Git(v) => v.worktree().is_none_or(|id| id.local() <= marks.worktree),
        Outcome::CreatedWorkspace {
            workspace,
            tab,
            pane,
        } => {
            workspace.local() <= marks.workspace
                && tab.local() <= marks.tab
                && pane.local() <= marks.pane
        }
        Outcome::CreatedTab { tab, pane } => tab.local() <= marks.tab && pane.local() <= marks.pane,
        Outcome::CreatedPane { pane } => pane.local() <= marks.pane,
        Outcome::CreatedWorktree { worktree } => worktree.local() <= marks.worktree,
        Outcome::ControlAcquired(v) => {
            v.pane().local() <= marks.pane && v.client().local() <= marks.client
        }
        Outcome::ControlUnavailable(ControlRecord::Held { client, .. }) => {
            client.local() <= marks.client
        }
        Outcome::StartupAwaitingMeasurement { intent } => stream_fits(intent.stream()),
        Outcome::SessionStarted(session) => session.pane.local() <= marks.pane,
        Outcome::Assessed { assessment, .. } => stream_fits(assessment.operation.stream()),
        Outcome::Conflict(
            FieldConflict::WorkspaceName { workspace, .. }
            | FieldConflict::PinnedDirectory { workspace, .. },
        ) => workspace.local() <= marks.workspace,
        Outcome::Conflict(FieldConflict::PaneDirectory { pane, .. }) => pane.local() <= marks.pane,
        Outcome::Applied
        | Outcome::Failed(_)
        | Outcome::ControlUnavailable(ControlRecord::Unclaimed)
        | Outcome::Conflict(
            FieldConflict::WorktreeLocation { .. } | FieldConflict::RemoteAccess { .. },
        ) => true,
    }
}
