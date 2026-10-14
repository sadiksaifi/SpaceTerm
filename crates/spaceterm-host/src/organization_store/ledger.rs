//! Canonical ledger SQL projection inside the organization authority transaction.

use super::{
    OperationProjection, StoreLimits, auxiliary, database_adapter::classify, limits::SizeBudget,
    pairing::PairingReceiptReader,
};
use crate::storage::Transaction;
use rusqlite::{Row, params};
use spaceterm_core::authority::{OrganizationRevision, StoreRevision};
use spaceterm_core::identity::*;
use spaceterm_core::operation_ledger::{
    canonical::CanonicalCodec, model::OperationLedger, projection,
};
use spaceterm_core::operation_ledger_contracts::*;
use spaceterm_core::operations::{OperationBinding, OperationId, OperationSequence, RequestDigest};
use spaceterm_core::storage::*;

pub(super) struct LedgerSql(pub std::sync::Arc<dyn PairingReceiptReader>);
fn invalid(_: impl Sized) -> StoreFailure {
    StoreFailure::InvalidDurableState
}
fn counter(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    super::codec::decode_u64(row.get_ref(index)?.as_blob()?)
        .map_err(|_| rusqlite::Error::InvalidQuery)
}
fn class(value: ReservationClass) -> i64 {
    match value {
        ReservationClass::Ordinary => 0,
        ReservationClass::Safety => 1,
        ReservationClass::DedicatedSelfRevocation => 2,
        ReservationClass::DedicatedEnrollment => 3,
    }
}
fn decode_class(value: i64) -> Result<ReservationClass, StoreFailure> {
    match value {
        0 => Ok(ReservationClass::Ordinary),
        1 => Ok(ReservationClass::Safety),
        2 => Ok(ReservationClass::DedicatedSelfRevocation),
        3 => Ok(ReservationClass::DedicatedEnrollment),
        _ => Err(StoreFailure::InvalidDurableState),
    }
}
fn reason(value: StreamRetirementReason) -> i64 {
    match value {
        StreamRetirementReason::ExplicitRetirement => 0,
        StreamRetirementReason::Expired => 1,
        StreamRetirementReason::PairingRevoked => 2,
        StreamRetirementReason::SequenceExhausted => 3,
    }
}
fn decode_reason(value: i64) -> Result<StreamRetirementReason, StoreFailure> {
    match value {
        0 => Ok(StreamRetirementReason::ExplicitRetirement),
        1 => Ok(StreamRetirementReason::Expired),
        2 => Ok(StreamRetirementReason::PairingRevoked),
        3 => Ok(StreamRetirementReason::SequenceExhausted),
        _ => Err(StoreFailure::InvalidDurableState),
    }
}
fn exact(rows: usize) -> Result<usize, StoreFailure> {
    if rows == 1 {
        Ok(rows)
    } else {
        Err(StoreFailure::InvalidTransaction)
    }
}

fn encode_progress(
    value: Option<&PendingExternalProgress>,
) -> Result<Option<Vec<u8>>, StoreFailure> {
    value
        .map(super::codec::encode_external_progress)
        .transpose()
        .map_err(super::auxiliary::codec_error)
}
fn decode_progress(
    bytes: &[u8],
    binding: OperationBinding,
) -> Result<PendingExternalProgress, StoreFailure> {
    let value = super::codec::decode_external_progress(bytes, binding.id.stream().server())
        .map_err(super::auxiliary::codec_error)?;
    if value.binding() != binding {
        return Err(StoreFailure::InvalidDurableState);
    }
    Ok(value)
}
impl OperationProjection for LedgerSql {
    fn preflight(
        &self,
        writes: &[OperationWrite],
        budget: &mut SizeBudget,
    ) -> Result<usize, StoreFailure> {
        let mut rows = auxiliary::preflight(writes, budget)?;
        for write in writes {
            budget
                .charge(1024)
                .map_err(|_| StoreFailure::InvalidTransaction)?;
            let changed = match write {
                OperationWrite::Ledger(value) => match value.as_ref() {
                    LedgerWrite::CompleteOperation { record, .. } => {
                        record
                            .validate_shape(&CanonicalCodec)
                            .map_err(|_| StoreFailure::InvalidTransaction)?;
                        budget
                            .charge(record.outcome.encoded_len())
                            .map_err(|_| StoreFailure::InvalidTransaction)?;
                        1
                    }
                    LedgerWrite::AcknowledgePrefix {
                        previous, through, ..
                    } => usize::try_from(
                        through
                            .checked_sub(*previous)
                            .ok_or(StoreFailure::InvalidTransaction)?,
                    )
                    .map_err(|_| StoreFailure::InvalidTransaction)?
                    .checked_add(1)
                    .ok_or(StoreFailure::InvalidTransaction)?,
                    LedgerWrite::AdmitOperation(record)
                    | LedgerWrite::RecordExternalProgress { record, .. } => {
                        record
                            .validate_shape()
                            .map_err(|_| StoreFailure::InvalidTransaction)?;
                        if let Some(progress) = &record.external_progress {
                            let bytes = super::codec::external_progress_size(progress)
                                .map_err(|_| StoreFailure::InvalidTransaction)?;
                            budget
                                .charge(bytes)
                                .map_err(|_| StoreFailure::InvalidTransaction)?;
                        }
                        2
                    }
                    LedgerWrite::RegisterStream(header) => {
                        header
                            .validate_shape()
                            .map_err(|_| StoreFailure::InvalidTransaction)?;
                        1
                    }
                    LedgerWrite::RetireStream { .. } => 1,
                },
                _ => 0,
            };
            rows = rows
                .checked_add(changed)
                .ok_or(StoreFailure::InvalidTransaction)?;
        }
        Ok(rows)
    }
    fn apply(
        &self,
        sql: &Transaction<'_>,
        parts: &TransactionParts,
        previous: &RecoveredState,
    ) -> Result<usize, StoreFailure> {
        // The owner transaction validator requires its live state. Inventory
        // accounting reserves eight payload copies and per-row index headroom;
        // this temporary copy is dropped before the resulting snapshot is read.
        let ledger = OperationLedger::recover(
            parts.server,
            previous
                .ledger
                .clone()
                .ok_or(StoreFailure::InvalidDurableState)?,
        )
        .map_err(invalid)?;
        projection::validate_transaction_projection(&ledger, parts)
            .map_err(|_| StoreFailure::InvalidTransaction)?;
        let mut changed = 0;
        for write in &parts.operation_writes {
            let OperationWrite::Ledger(write) = write else {
                continue;
            };
            match write.as_ref() {
                LedgerWrite::RegisterStream(header) => {
                    // The owner validator above permits an exact repeat. Do not
                    // turn that idempotent registration into a uniqueness error.
                    if previous
                        .ledger
                        .as_ref()
                        .is_some_and(|v| v.streams.iter().any(|old| old == header))
                    {
                        continue;
                    }
                    let zero = 0_u64.to_be_bytes();
                    let purpose = match header.purpose {
                        StreamPurpose::Ordinary => 0,
                        StreamPurpose::SelfRevocation => 1,
                        StreamPurpose::Enrollment { .. } => 2,
                    };
                    let claim = match header.purpose {
                        StreamPurpose::Enrollment { claim_digest } => Some(claim_digest.bytes()),
                        _ => None,
                    };
                    changed += exact(sql.execute("INSERT INTO operation_streams VALUES (?1, ?2, ?3, ?4, ?11, 0, NULL, ?5, ?5, ?6, ?7, ?8, ?9, ?10)", params![header.binding.stream().key().local().to_be_bytes().as_slice(), header.binding.pairing().local().to_be_bytes().as_slice(), header.binding.client().local().to_be_bytes().as_slice(), purpose, zero.as_slice(), header.dormant_reservation.map(|r| class(r.class())), header.dormant_reservation.map(|r| u64::from(r.bytes()).to_be_bytes().to_vec()), header.request_format_version, header.outcome_format_version, header.quota_policy_version, claim.as_ref().map(|v| v.as_slice())]).map_err(classify)?)?;
                }
                LedgerWrite::AdmitOperation(record) => {
                    let id = record.binding.id;
                    let stream = id.stream().key().local().to_be_bytes();
                    let sequence = id.sequence().get().to_be_bytes();
                    let progress = encode_progress(record.external_progress.as_ref())?;
                    changed += exact(sql.execute("UPDATE operation_streams SET admitted_through = ?2, dormant_class = NULL, dormant_bytes = NULL WHERE stream_key = ?1", params![stream.as_slice(), sequence.as_slice()]).map_err(classify)?)?;
                    changed += exact(sql.execute("INSERT INTO operation_entries VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, ?6, NULL, ?8, ?9, NULL, NULL)", params![stream.as_slice(), sequence.as_slice(), record.binding.digest.bytes().as_slice(), class(record.reservation.class()), u64::from(record.reservation.bytes()).to_be_bytes().as_slice(), record.admitted_at.get().to_be_bytes().as_slice(), record.admission_organization_revision.get().to_be_bytes().as_slice(), i64::from(progress.is_some()), progress]).map_err(classify)?)?;
                }
                LedgerWrite::RecordExternalProgress {
                    previous_progress_store_revision,
                    record,
                } => {
                    changed += exact(sql.execute("UPDATE operation_entries SET progress_recorded_at = ?3, external_progress = ?4 WHERE stream_key = ?1 AND sequence = ?2 AND progress_recorded_at = ?5", params![record.binding.id.stream().key().local().to_be_bytes().as_slice(), record.binding.id.sequence().get().to_be_bytes().as_slice(), record.progress_recorded_at.get().to_be_bytes().as_slice(), encode_progress(record.external_progress.as_ref())?, previous_progress_store_revision.get().to_be_bytes().as_slice()]).map_err(classify)?)?;
                }
                LedgerWrite::CompleteOperation { record, .. } => {
                    changed += exact(sql.execute("UPDATE operation_entries SET state = 1, admission_organization_revision = NULL, progress_recorded_at = NULL, completed_at = ?3, pending_kind = NULL, external_progress = NULL, compact_outcome = ?4, publication_revision = ?5 WHERE stream_key = ?1 AND sequence = ?2", params![record.binding.id.stream().key().local().to_be_bytes().as_slice(), record.binding.id.sequence().get().to_be_bytes().as_slice(), record.completed_at.get().to_be_bytes().as_slice(), record.outcome.as_encoded(), record.outcome_revision.get().to_be_bytes().as_slice()]).map_err(classify)?)?;
                }
                LedgerWrite::AcknowledgePrefix {
                    binding, through, ..
                } => {
                    let stream = binding.stream().key().local().to_be_bytes();
                    let through = through.to_be_bytes();
                    changed += sql.execute("DELETE FROM operation_entries WHERE stream_key = ?1 AND sequence <= ?2", params![stream.as_slice(), through.as_slice()]).map_err(classify)?;
                    changed += exact(sql.execute("UPDATE operation_streams SET acknowledged_through = ?2 WHERE stream_key = ?1", params![stream.as_slice(), through.as_slice()]).map_err(classify)?)?;
                }
                LedgerWrite::RetireStream {
                    binding,
                    reason: why,
                } => {
                    changed += exact(sql.execute("UPDATE operation_streams SET status = 1, retirement_reason = ?2 WHERE stream_key = ?1", params![binding.stream().key().local().to_be_bytes().as_slice(), reason(*why)]).map_err(classify)?)?;
                }
            }
        }
        Ok(changed + auxiliary::apply(sql, parts, previous, self.0.as_ref())?)
    }
    fn recover(
        &self,
        sql: &Transaction<'_>,
        recovered: &mut RecoveredState,
        limits: &StoreLimits,
        budget: &mut SizeBudget,
    ) -> Result<(), StoreFailure> {
        let server = recovered.organization.server;
        let mut ledger = LedgerRecovery {
            format_version: LEDGER_FORMAT_VERSION,
            quota_policy_version: LEDGER_QUOTA_POLICY_VERSION,
            limits: LedgerLimits::production_v1(),
            streams: vec![],
            operations: vec![],
        };
        let stream_count = auxiliary::inventory(
            sql,
            "operation_streams",
            "0",
            super::limits::Collection::OperationStreams,
            limits,
            budget,
        )?;
        ledger.streams = auxiliary::scan::<8, _>(
            sql,
            "SELECT stream_key, pairing_id, client_id, purpose, status, retirement_reason, admitted_through, acknowledged_through, dormant_class, dormant_bytes, request_format_version, outcome_format_version, quota_policy_version, enrollment_claim_digest FROM operation_streams WHERE stream_key > ?1 ORDER BY stream_key LIMIT 1",
            stream_count,
            |r| {
                let stream = OperationStreamId::new(
                    OperationStreamKey::new(server, counter(r, 0).map_err(invalid)?)
                        .map_err(invalid)?,
                    PairingId::new(server, counter(r, 1).map_err(invalid)?).map_err(invalid)?,
                    ClientId::new(server, counter(r, 2).map_err(invalid)?).map_err(invalid)?,
                )
                .map_err(invalid)?;
                let purpose = match r.get::<_, i64>(3).map_err(invalid)? {
                    0 => {
                        super::recovery::require_nulls(r, &[13]).map_err(invalid)?;
                        StreamPurpose::Ordinary
                    }
                    1 => {
                        super::recovery::require_nulls(r, &[13]).map_err(invalid)?;
                        StreamPurpose::SelfRevocation
                    }
                    2 => StreamPurpose::Enrollment {
                        claim_digest: RequestDigest::from_bytes(auxiliary::fixed::<32>(r, 13)?),
                    },
                    _ => return Err(StoreFailure::InvalidDurableState),
                };
                let status = match r.get::<_, i64>(4).map_err(invalid)? {
                    0 => {
                        super::recovery::require_nulls(r, &[5]).map_err(invalid)?;
                        StreamStatus::Active
                    }
                    1 => StreamStatus::Retired(decode_reason(r.get(5).map_err(invalid)?)?),
                    _ => return Err(StoreFailure::InvalidDurableState),
                };
                let dormant_reservation = r
                    .get::<_, Option<i64>>(8)
                    .map_err(invalid)?
                    .map(|c| {
                        OutcomeReservation::new(
                            decode_class(c)?,
                            u32::try_from(counter(r, 9).map_err(invalid)?).map_err(invalid)?,
                        )
                        .map_err(invalid)
                    })
                    .transpose()?;
                if dormant_reservation.is_none() {
                    super::recovery::require_nulls(r, &[9]).map_err(invalid)?;
                }
                Ok(StreamRecord {
                    binding: StreamBinding::from_stream(stream),
                    purpose,
                    status,
                    admitted_through: counter(r, 6).map_err(invalid)?,
                    acknowledged_through: counter(r, 7).map_err(invalid)?,
                    dormant_reservation,
                    request_format_version: r.get(10).map_err(invalid)?,
                    outcome_format_version: r.get(11).map_err(invalid)?,
                    quota_policy_version: r.get(12).map_err(invalid)?,
                })
            },
        )?;
        let count = auxiliary::inventory(
            sql,
            "operation_entries",
            "coalesce(length(compact_outcome), 0) + coalesce(length(external_progress), 0)",
            super::limits::Collection::OperationEntries,
            limits,
            budget,
        )?;
        ledger.operations = auxiliary::scan_operations(
            sql,
            "SELECT stream_key, sequence, request_digest, state, reservation_class, reservation_bytes, admitted_at, admission_organization_revision, progress_recorded_at, completed_at, pending_kind, external_progress, compact_outcome, publication_revision FROM operation_entries WHERE (stream_key, sequence) > (?1, ?2) ORDER BY stream_key, sequence LIMIT 1",
            count,
            |r| {
                let key = counter(r, 0).map_err(invalid)?;
                let header = ledger
                    .streams
                    .iter()
                    .find(|h| h.binding.stream().key().local() == key)
                    .ok_or(StoreFailure::InvalidDurableState)?;
                let id = OperationId::new(
                    header.binding.stream(),
                    OperationSequence::new(counter(r, 1).map_err(invalid)?).map_err(invalid)?,
                );
                let binding = OperationBinding {
                    id,
                    digest: RequestDigest::from_bytes(auxiliary::fixed::<32>(r, 2)?),
                };
                let reservation = OutcomeReservation::new(
                    decode_class(r.get(4).map_err(invalid)?)?,
                    u32::try_from(counter(r, 5).map_err(invalid)?).map_err(invalid)?,
                )
                .map_err(invalid)?;
                let admitted_at = StoreRevision::new(counter(r, 6).map_err(invalid)?);
                match r.get::<_, i64>(3).map_err(invalid)? {
                    0 => {
                        super::recovery::require_nulls(r, &[9, 12, 13]).map_err(invalid)?;
                        let (kind, external_progress) =
                            match r.get::<_, i64>(10).map_err(invalid)? {
                                0 => {
                                    super::recovery::require_nulls(r, &[11]).map_err(invalid)?;
                                    (PendingKind::AtomicMutation, None)
                                }
                                1 => {
                                    let progress =
                                        decode_progress(auxiliary::blob(r, 11)?, binding)?;
                                    (
                                        PendingKind::External {
                                            effect: progress.effect(),
                                        },
                                        Some(progress),
                                    )
                                }
                                _ => return Err(StoreFailure::InvalidDurableState),
                            };
                        Ok(RecoveredOperationRecord::Pending(Box::new(
                            PendingOperationRecord {
                                binding,
                                kind,
                                reservation,
                                admitted_at,
                                admission_organization_revision: OrganizationRevision::new(
                                    counter(r, 7).map_err(invalid)?,
                                ),
                                progress_recorded_at: StoreRevision::new(
                                    counter(r, 8).map_err(invalid)?,
                                ),
                                external_progress,
                            },
                        )))
                    }
                    1 => {
                        super::recovery::require_nulls(r, &[7, 8, 10, 11]).map_err(invalid)?;
                        Ok(RecoveredOperationRecord::Completed(
                            CompletedOperationRecord {
                                binding,
                                reservation,
                                admitted_at,
                                completed_at: StoreRevision::new(counter(r, 9).map_err(invalid)?),
                                outcome: CompactOutcome::from_encoded(r.get(12).map_err(invalid)?)
                                    .map_err(invalid)?,
                                outcome_revision: OrganizationRevision::new(
                                    counter(r, 13).map_err(invalid)?,
                                ),
                            },
                        ))
                    }
                    _ => Err(StoreFailure::InvalidDurableState),
                }
            },
        )?;
        recovered.ledger = Some(ledger);
        auxiliary::recover(sql, recovered, limits, budget, self.0.as_ref())
    }
}
