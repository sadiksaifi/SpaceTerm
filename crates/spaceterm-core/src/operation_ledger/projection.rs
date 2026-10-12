//! Validation and composition of the single typed ledger write inventory.
//! Validation overlays borrowed operation records and small stream headers. It
//! never clones live compact outcomes or invokes execution callbacks.
use std::collections::{BTreeMap, BTreeSet};

use super::model::*;
use super::recovery::{external_phase, validate_progress_record};
use super::transition::{completion_basis, validate_progress_transition};
use crate::operation_ledger_contracts::*;
use crate::storage::{OperationWrite, TransactionParts};
use crate::worktrees::intent::GitIntentMutation;

/// Validates the ledger portion against committed state. Reducer, pairing, Git
/// and native owners must validate their other slots at the same boundary.
pub fn validate_transaction_projection(
    ledger: &OperationLedger,
    parts: &TransactionParts,
) -> Result<(), LedgerFailure> {
    if ledger.requires_recovery() {
        return Err(failure(LedgerFailureClass::RecoveryRequired));
    }
    if parts.server != ledger.server || parts.operation_writes.len() > MAX_LEDGER_PROJECTION_WRITES
    {
        return Err(failure(LedgerFailureClass::InvalidTransition));
    }
    if parts.operation_writes.iter().any(|w| {
        matches!(
            w,
            OperationWrite::Put(_)
                | OperationWrite::SetStream(_)
                | OperationWrite::RetireAcknowledgedPrefix { .. }
        )
    }) {
        return Err(failure(LedgerFailureClass::InvalidTransition));
    }
    let writes: Vec<_> = parts
        .operation_writes
        .iter()
        .filter_map(|v| match v {
            OperationWrite::Ledger(v) => Some(v.as_ref()),
            _ => None,
        })
        .collect();
    if writes.is_empty() {
        return Ok(());
    }
    let bases: Vec<_> = parts
        .operation_writes
        .iter()
        .filter_map(|v| match v {
            OperationWrite::ReductionBasis(v) => Some(*v),
            _ => None,
        })
        .collect();
    if bases.len() > 1 {
        return Err(failure(LedgerFailureClass::InvalidTransition));
    }
    let context = TransactionContext::new(
        parts.expected_store_revision,
        parts.expected_organization_revision,
        parts.resulting_organization_revision,
        bases.first().copied(),
    )?;
    if parts.resulting_store_revision
        != parts
            .expected_store_revision
            .checked_successor()
            .ok_or_else(|| failure(LedgerFailureClass::CounterExhausted))?
    {
        return Err(failure(LedgerFailureClass::InvalidTransition));
    }
    validate_writes(ledger, context, &writes)?;
    for write in &writes {
        let binding = match write {
            LedgerWrite::RegisterStream(v) => v.binding,
            LedgerWrite::AdmitOperation(v)
            | LedgerWrite::RecordExternalProgress { record: v, .. } => {
                StreamBinding::from_stream(v.binding.id.stream())
            }
            LedgerWrite::CompleteOperation { record, .. } => {
                StreamBinding::from_stream(record.binding.id.stream())
            }
            LedgerWrite::AcknowledgePrefix { binding, .. }
            | LedgerWrite::RetireStream { binding, .. } => *binding,
        };
        let marks = parts.resulting_high_water;
        if binding.stream().key().local() > marks.operation_stream
            || binding.pairing().local() > marks.pairing
            || binding.client().local() > marks.client
        {
            return Err(failure(LedgerFailureClass::InvalidDurableState));
        }
        if let LedgerWrite::CompleteOperation { record, .. } = write
            && !super::recovery::outcome_fits_marks(
                &record.outcome.outcome(&super::canonical::CanonicalCodec)?,
                marks,
            )
        {
            return Err(failure(LedgerFailureClass::InvalidDurableState));
        }
        if let LedgerWrite::CompleteOperation { record, .. } = write {
            let old = ledger.pending(
                &StreamBinding::from_stream(record.binding.id.stream()),
                record.binding.id,
                record.binding.digest,
            )?;
            if matches!(old.external_progress.as_ref().map(|p| p.state()), Some(ExternalProgressState::RecordedIntent { reference, .. }) if matches!(reference.scope(), ExternalIntentScope::Git(_)))
            {
                let completion_count = parts.operation_writes.iter().filter(|w| matches!(w, OperationWrite::GitIntent(GitIntentMutation::Complete { binding }) if *binding == record.binding)).count();
                if completion_count != 1 {
                    return Err(failure(LedgerFailureClass::InvalidExternalProgress));
                }
            }
        }
        match write {
            LedgerWrite::AdmitOperation(_) => {
                if !parts.organization_writes.is_empty()
                    || parts.expected_high_water != parts.resulting_high_water
                    || parts.expected_epoch != parts.resulting_epoch
                    || parts.operation_writes.len() != 1
                {
                    return Err(failure(LedgerFailureClass::InvalidTransition));
                }
            }
            LedgerWrite::CompleteOperation {
                basis: CompletionRevisionBasis::OwnedReduction(basis),
                ..
            } => {
                if bases.as_slice() != [*basis] {
                    return Err(failure(LedgerFailureClass::OutcomeRevisionMismatch));
                }
            }
            LedgerWrite::RecordExternalProgress { record, .. } => {
                if let Some(progress) = &record.external_progress
                    && let ExternalProgressState::RecordedIntent { reference, .. } =
                        progress.state()
                {
                    match reference.scope() {
                        ExternalIntentScope::Git(scope) => {
                            let old = ledger.pending(
                                &crate::operation_ledger_contracts::StreamBinding::from_stream(
                                    record.binding.id.stream(),
                                ),
                                record.binding.id,
                                record.binding.digest,
                            )?;
                            let old_reference =
                                old.external_progress
                                    .as_ref()
                                    .and_then(|p| match p.state() {
                                        ExternalProgressState::RecordedIntent {
                                            reference, ..
                                        } => Some(reference),
                                        _ => None,
                                    });
                            let mutations: Vec<_> = parts
                                .operation_writes
                                .iter()
                                .filter_map(|w| match w {
                                    OperationWrite::GitIntent(v)
                                        if v.binding().id == record.binding.id =>
                                    {
                                        Some(v)
                                    }
                                    _ => None,
                                })
                                .collect();
                            if old_reference == Some(reference) {
                                if !mutations.is_empty() {
                                    return Err(failure(
                                        LedgerFailureClass::InvalidExternalProgress,
                                    ));
                                }
                            } else if mutations.len() != 1
                                || !match mutations[0] {
                                    GitIntentMutation::Insert(v) => {
                                        old_reference.is_none()
                                            && v.binding() == record.binding
                                            && v.parts().repository.scope() == scope
                                            && external_phase(v.phase()) == reference.phase()
                                    }
                                    GitIntentMutation::Advance {
                                        binding,
                                        expected_phase,
                                        next,
                                        ..
                                    } => {
                                        *binding == record.binding
                                            && old_reference.is_some_and(|r| {
                                                r.phase() == external_phase(*expected_phase)
                                            })
                                            && external_phase(*next) == reference.phase()
                                    }
                                    GitIntentMutation::Complete { .. } => false,
                                }
                            {
                                return Err(failure(LedgerFailureClass::InvalidExternalProgress));
                            }
                        }
                        ExternalIntentScope::Management => {
                            return Err(failure(LedgerFailureClass::InvalidExternalProgress));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn validate_projection(
    ledger: &OperationLedger,
    projection: &LedgerProjection,
) -> Result<(), LedgerFailure> {
    validate_writes(
        ledger,
        projection.context(),
        &projection.writes().iter().collect::<Vec<_>>(),
    )
}
fn validate_writes(
    ledger: &OperationLedger,
    context: TransactionContext,
    writes: &[&LedgerWrite],
) -> Result<(), LedgerFailure> {
    if writes.is_empty()
        || writes.len() > MAX_LEDGER_PROJECTION_WRITES
        || context.expected_store() < ledger.known_store
        || context.base_organization() < ledger.known_organization
    {
        return Err(failure(LedgerFailureClass::InvalidTransition));
    }
    let next_store = context
        .expected_store()
        .checked_successor()
        .ok_or_else(|| failure(LedgerFailureClass::CounterExhausted))?;
    let mut streams = ledger.state.streams.clone();
    let mut operations: BTreeMap<_, _> = ledger
        .state
        .operations
        .iter()
        .map(|(id, v)| (*id, v))
        .collect();
    // Newly written pending/completed records are borrowed from the write list.
    let mut new_pending = BTreeMap::new();
    let mut new_completed = BTreeMap::new();
    let mut changed_headers = BTreeSet::new();
    let mut changed_operations = BTreeSet::new();
    let mut owns_revision = false;
    for write in writes {
        match write {
            LedgerWrite::RegisterStream(record) => {
                record.validate_shape()?;
                let key = record.binding.stream().key();
                if record.binding.server() != ledger.server || !changed_headers.insert(key) {
                    return Err(failure(LedgerFailureClass::InvalidTransition));
                }
                if let Some(old) = streams.get(&key) {
                    if old != record {
                        return Err(failure(LedgerFailureClass::InvalidTransition));
                    }
                } else {
                    if record.admitted_through != 0
                        || record.acknowledged_through != 0
                        || record.status != StreamStatus::Active
                    {
                        return Err(failure(LedgerFailureClass::InvalidTransition));
                    }
                    streams.insert(key, record.clone());
                }
            }
            LedgerWrite::AdmitOperation(record) => {
                OperationLedger::store_only(context)?;
                record.validate_shape()?;
                let header = streams
                    .get_mut(&record.binding.id.stream().key())
                    .ok_or_else(|| failure(LedgerFailureClass::UnknownStream))?;
                if header.binding.stream() != record.binding.id.stream()
                    || header.status != StreamStatus::Active
                    || record.binding.id.sequence().get()
                        != header
                            .admitted_through
                            .checked_add(1)
                            .ok_or_else(|| failure(LedgerFailureClass::CounterExhausted))?
                    || !changed_operations.insert(record.binding.id)
                    || record.admitted_at != next_store
                    || record.progress_recorded_at != next_store
                    || record.admission_organization_revision != context.base_organization()
                    || !matches!(
                        record.external_progress.as_ref().map(|v| v.state()),
                        None | Some(ExternalProgressState::AwaitingIntent)
                    )
                {
                    return Err(failure(LedgerFailureClass::InvalidTransition));
                }
                header.admitted_through = record.binding.id.sequence().get();
                header.dormant_reservation = None;
                new_pending.insert(record.binding.id, record);
            }
            LedgerWrite::RecordExternalProgress {
                previous_progress_store_revision,
                record,
            } => {
                let old = ledger.pending(
                    &StreamBinding::from_stream(record.binding.id.stream()),
                    record.binding.id,
                    record.binding.digest,
                )?;
                if *previous_progress_store_revision != old.progress_recorded_at
                    || record.progress_recorded_at != next_store
                    || !changed_operations.insert(record.binding.id)
                {
                    return Err(failure(LedgerFailureClass::InvalidTransition));
                }
                validate_progress_transition(ledger, old, record, context)?;
                owns_revision |= context.operation_basis().is_some();
                operations.remove(&record.binding.id);
                new_pending.insert(record.binding.id, record);
            }
            LedgerWrite::CompleteOperation { record, basis } => {
                let old = ledger.pending(
                    &StreamBinding::from_stream(record.binding.id.stream()),
                    record.binding.id,
                    record.binding.digest,
                )?;
                record.validate_shape(&super::canonical::CanonicalCodec)?;
                let outcome = record.outcome.outcome(&super::canonical::CanonicalCodec)?;
                if record.reservation != old.reservation
                    || record.admitted_at != old.admitted_at
                    || record.completed_at != next_store
                    || !changed_operations.insert(record.binding.id)
                    || completion_basis(old, &outcome, record.outcome_revision, context)? != *basis
                {
                    return Err(failure(LedgerFailureClass::InvalidTransition));
                }
                owns_revision |= matches!(basis, CompletionRevisionBasis::OwnedReduction(_));
                operations.remove(&record.binding.id);
                new_completed.insert(record.binding.id, record);
            }
            LedgerWrite::AcknowledgePrefix {
                binding,
                previous,
                through,
            } => {
                let header = streams
                    .get_mut(&binding.stream().key())
                    .ok_or_else(|| failure(LedgerFailureClass::UnknownStream))?;
                if header.binding != *binding
                    || header.acknowledged_through != *previous
                    || *through <= *previous
                    || *through > header.admitted_through
                    || !changed_headers.insert(binding.stream().key())
                {
                    return Err(failure(LedgerFailureClass::InvalidTransition));
                }
                if operations.values().any(|v| {
                    record_binding(v).id.stream() == binding.stream()
                        && record_binding(v).id.sequence().get() <= *through
                        && matches!(v, RecoveredOperationRecord::Pending(_))
                }) || new_pending
                    .keys()
                    .any(|id| id.stream() == binding.stream() && id.sequence().get() <= *through)
                {
                    return Err(failure(LedgerFailureClass::AcknowledgementGap));
                }
                operations.retain(|id, _| {
                    id.stream() != binding.stream() || id.sequence().get() > *through
                });
                new_completed.retain(|id, _| {
                    id.stream() != binding.stream() || id.sequence().get() > *through
                });
                header.acknowledged_through = *through;
            }
            LedgerWrite::RetireStream { binding, reason } => {
                let header = streams
                    .get_mut(&binding.stream().key())
                    .ok_or_else(|| failure(LedgerFailureClass::UnknownStream))?;
                if header.binding != *binding
                    || !changed_headers.insert(binding.stream().key())
                    || (header.status != StreamStatus::Active
                        && header.status != StreamStatus::Retired(*reason))
                    || (*reason == StreamRetirementReason::SequenceExhausted
                        && header.admitted_through != u64::MAX)
                {
                    return Err(failure(LedgerFailureClass::InvalidTransition));
                }
                header.status = StreamStatus::Retired(*reason);
            }
        }
    }
    if context.base_organization() != context.resulting_organization() && !owns_revision {
        return Err(failure(LedgerFailureClass::OutcomeRevisionMismatch));
    }
    let limits = ledger.state.limits;
    if streams.len() > limits.max_streams() as usize {
        return Err(failure(LedgerFailureClass::StreamLimit));
    }
    let mut clients = BTreeMap::new();
    let mut totals = (0u32, 0u32);
    let mut usage = BTreeMap::new();
    let mut counts = BTreeMap::new();
    for header in streams.values() {
        header.validate_shape()?;
        let count = clients
            .entry((header.binding.pairing(), header.binding.client()))
            .or_insert(0u32);
        *count += 1;
        if *count > limits.max_streams_per_client_binding() {
            return Err(failure(LedgerFailureClass::StreamLimit));
        }
        if let Some(reserved) = header.dormant_reservation {
            add_usage(&mut totals, &mut usage, header.binding.stream(), reserved);
        }
    }
    for (id, reserved) in operations
        .iter()
        .map(|(id, v)| (*id, reservation(v)))
        .chain(new_pending.iter().map(|(id, v)| (*id, v.reservation)))
        .chain(new_completed.iter().map(|(id, v)| (*id, v.reservation)))
    {
        let header = streams
            .get(&id.stream().key())
            .ok_or_else(|| failure(LedgerFailureClass::InvalidDurableState))?;
        if header.binding.stream() != id.stream()
            || id.sequence().get() <= header.acknowledged_through
            || id.sequence().get() > header.admitted_through
            || !match header.purpose {
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
            }
        {
            return Err(failure(LedgerFailureClass::PurposeMismatch));
        }
        *counts.entry(id.stream()).or_insert(0u64) += 1;
        add_usage(&mut totals, &mut usage, id.stream(), reserved);
    }
    if totals.0 > limits.global_slots()
        || totals.1 > limits.ordinary_global_slots()
        || usage.values().any(|(total, ordinary)| {
            *total > limits.per_stream_slots() || *ordinary > limits.ordinary_stream_slots()
        })
    {
        return Err(failure(LedgerFailureClass::QuotaExceeded));
    }
    for header in streams.values() {
        if counts.get(&header.binding.stream()).copied().unwrap_or(0)
            != header.admitted_through - header.acknowledged_through
        {
            return Err(failure(LedgerFailureClass::InvalidDurableState));
        }
    }
    let mut retirement_count = 0usize;
    let mut retirement_bytes = 0usize;
    let mut retirement_batches = BTreeSet::new();
    for pending in operations
        .values()
        .filter_map(|v| match v {
            RecoveredOperationRecord::Pending(p) => Some(p.as_ref()),
            _ => None,
        })
        .chain(new_pending.values().copied())
    {
        if let Some(retirement) = pending
            .external_progress
            .as_ref()
            .and_then(super::recovery::progress_retirement)
        {
            if !retirement_batches.insert(retirement.batch()) {
                return Err(failure(LedgerFailureClass::InvalidDurableState));
            }
            retirement_count += 1;
            retirement_bytes = retirement_bytes
                .checked_add(
                    retirement
                        .value_bytes()
                        .map_err(|_| failure(LedgerFailureClass::InvalidExternalProgress))?,
                )
                .ok_or_else(|| failure(LedgerFailureClass::QuotaExceeded))?;
        }
    }
    if retirement_count > 64 || retirement_bytes > crate::retirement::MAX_RETAINED_RETIREMENT_BYTES
    {
        return Err(failure(LedgerFailureClass::QuotaExceeded));
    }
    // Each reservation is exactly 64 KiB. Slot validation therefore validates
    // both byte ceilings without relying on small encoded outcome lengths.
    for record in new_pending.values() {
        if let Some(progress) = &record.external_progress {
            validate_progress_record(record, progress)?;
        }
    }
    Ok(())
}
fn add_usage(
    totals: &mut (u32, u32),
    usage: &mut BTreeMap<crate::identity::OperationStreamId, (u32, u32)>,
    stream: crate::identity::OperationStreamId,
    reserved: OutcomeReservation,
) {
    totals.0 += 1;
    let entry = usage.entry(stream).or_default();
    entry.0 += 1;
    if reserved.class() == ReservationClass::Ordinary {
        totals.1 += 1;
        entry.1 += 1;
    }
}

pub(super) fn apply(state: &mut LedgerState, writes: &[LedgerWrite]) {
    for write in writes {
        match write {
            LedgerWrite::RegisterStream(record) => {
                state
                    .streams
                    .entry(record.binding.stream().key())
                    .or_insert_with(|| record.clone());
            }
            LedgerWrite::AdmitOperation(record) => {
                if let Some(header) = state.streams.get_mut(&record.binding.id.stream().key()) {
                    header.admitted_through = record.binding.id.sequence().get();
                    header.dormant_reservation = None;
                }
                state.operations.insert(
                    record.binding.id,
                    RecoveredOperationRecord::Pending(Box::new(record.clone())),
                );
            }
            LedgerWrite::RecordExternalProgress { record, .. } => {
                state.operations.insert(
                    record.binding.id,
                    RecoveredOperationRecord::Pending(Box::new(record.clone())),
                );
            }
            LedgerWrite::CompleteOperation { record, .. } => {
                state.operations.insert(
                    record.binding.id,
                    RecoveredOperationRecord::Completed(record.clone()),
                );
            }
            LedgerWrite::AcknowledgePrefix {
                binding, through, ..
            } => {
                state.operations.retain(|id, _| {
                    id.stream() != binding.stream() || id.sequence().get() > *through
                });
                if let Some(header) = state.streams.get_mut(&binding.stream().key()) {
                    header.acknowledged_through = *through;
                }
            }
            LedgerWrite::RetireStream { binding, reason } => {
                if let Some(header) = state.streams.get_mut(&binding.stream().key()) {
                    header.status = StreamStatus::Retired(*reason);
                }
            }
        }
    }
}

/// Checked composition rejects legacy or competing ledger writes before mutation.
/// The frozen LedgerProjection::append_to supplies revision/size checks; callers
/// use this stricter helper when combining independently supplied owner proposals.
pub fn append_to(
    projection: &LedgerProjection,
    parts: &mut TransactionParts,
) -> Result<(), LedgerFailure> {
    if parts.operation_writes.iter().any(|v| {
        matches!(
            v,
            OperationWrite::Ledger(_)
                | OperationWrite::Put(_)
                | OperationWrite::SetStream(_)
                | OperationWrite::RetireAcknowledgedPrefix { .. }
        )
    }) {
        return Err(failure(LedgerFailureClass::InvalidTransition));
    }
    projection.append_to(parts)
}
