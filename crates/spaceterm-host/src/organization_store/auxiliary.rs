//! Borrowed auxiliary SQL projection. The transaction owner composes these helpers
//! with the ledger owner's projection and complete domain validator. None of these
//! helpers opens a database, commits, signs, launches, or executes an external effect.
//! Ledger pending progress and pairing-bound retirement stay in the ledger owner's
//! operation_entries codec, never in a competing auxiliary table.

use super::{
    StoreLimits, codec,
    database_adapter::classify,
    limits::{Collection, SizeBudget},
};
use crate::storage::Transaction;
use rusqlite::{Params, Row};
use spaceterm_core::identity::{
    ClientId, OperationStreamId, OperationStreamKey, PairingId, ServerId,
};
use spaceterm_core::operations::{OperationBinding, OperationId, OperationSequence, RequestDigest};
use spaceterm_core::storage::{OperationWrite, RecoveredState, StoreFailure, TransactionParts};

/// Charges and validates only the auxiliary arms. Ledger and reduction-basis arms
/// must also be preflighted by their owner. Legacy operation arms are rejected.
pub fn preflight(
    writes: &[OperationWrite],
    budget: &mut SizeBudget,
) -> Result<usize, StoreFailure> {
    if writes.len() > super::limits::MAX_AUXILIARY_WRITES {
        return Err(StoreFailure::InvalidTransaction);
    }
    let mut rows = 0usize;
    for write in writes {
        let bound = match write {
            OperationWrite::Pairing(value) => super::pairing::preflight(value, budget)?,
            OperationWrite::GitRepository(_)
            | OperationWrite::GitIntent(_)
            | OperationWrite::GitAllocation(_) => super::git_intents::preflight(write, budget)?,
            OperationWrite::NativeRetirement(value) => native_preflight(value, budget)?,
            OperationWrite::Ledger(_) | OperationWrite::ReductionBasis(_) => 0,
            _ => return Err(StoreFailure::RecoveryRequired),
        };
        rows = rows
            .checked_add(bound)
            .filter(|n| *n <= super::limits::MAX_CHANGED_ROWS)
            .ok_or(StoreFailure::InvalidTransaction)?;
    }
    super::pairing::check_duplicates(writes)?;
    super::git_intents::check_duplicates(writes)?;
    Ok(rows)
}

/// Applies pairing and Git CAS projections inside the caller's sole authority
/// transaction. The caller must apply ledger writes and validate the complete
/// resulting recovery bundle before returning its receipt.
pub fn apply(
    sql: &Transaction<'_>,
    parts: &TransactionParts,
    previous: &RecoveredState,
    receipts: &dyn super::pairing::PairingReceiptReader,
) -> Result<usize, StoreFailure> {
    let mut rows = 0usize;
    // Compare all writes to the same committed candidate. Duplicate targets cannot
    // manufacture sequential CAS steps inside one proposal.
    super::pairing::check_duplicates(&parts.operation_writes)?;
    super::git_intents::check_duplicates(&parts.operation_writes)?;
    let mut native_requests = std::collections::BTreeSet::new();
    for write in &parts.operation_writes {
        if let OperationWrite::NativeRetirement(value) = write {
            let request = match value {
                spaceterm_core::retirement::NativeRetirementMutation::Accept(v) => v.request(),
                spaceterm_core::retirement::NativeRetirementMutation::Progress {
                    expected, ..
                } => expected.request(),
                spaceterm_core::retirement::NativeRetirementMutation::Complete {
                    request, ..
                } => *request,
            };
            if !native_requests.insert(request.id_bytes()) {
                return Err(StoreFailure::InvalidTransaction);
            }
        }
    }
    for write in &parts.operation_writes {
        let changed = match write {
            OperationWrite::Pairing(value) => {
                super::pairing::apply(sql, value, parts, previous, receipts)?
            }
            OperationWrite::GitRepository(_)
            | OperationWrite::GitIntent(_)
            | OperationWrite::GitAllocation(_) => {
                super::git_intents::apply(sql, write, parts, previous)?
            }
            OperationWrite::NativeRetirement(value) => native_apply(sql, value, parts, previous)?,
            OperationWrite::Ledger(_) | OperationWrite::ReductionBasis(_) => 0,
            _ => return Err(StoreFailure::RecoveryRequired),
        };
        rows = rows
            .checked_add(changed)
            .filter(|n| *n <= super::limits::MAX_CHANGED_ROWS)
            .ok_or(StoreFailure::InvalidTransaction)?;
    }
    Ok(rows)
}

/// Decodes all owned auxiliary tables in the existing bounded snapshot. Signed
/// receipts require the pairing owner's exact-byte decoder/verifier. No pending
/// comparison descriptor supplies cleanup evidence or a resumed effect permit.
pub fn recover(
    sql: &Transaction<'_>,
    recovered: &mut RecoveredState,
    limits: &StoreLimits,
    budget: &mut SizeBudget,
    receipts: &dyn super::pairing::PairingReceiptReader,
) -> Result<(), StoreFailure> {
    super::pairing::recover(sql, recovered, limits, budget, receipts)?;
    super::git_intents::recover(sql, recovered, limits, budget)?;
    native_recover(sql, recovered, limits, budget)?;
    Ok(())
}

pub(super) fn exact<P: Params>(
    sql: &Transaction<'_>,
    statement: &str,
    params: P,
    expected: usize,
) -> Result<usize, StoreFailure> {
    let count = sql
        .execute(statement, params)
        .map_err(|error| super::transaction::proposal_failure(classify(error)))?;
    if count != expected {
        return Err(StoreFailure::InvalidTransaction);
    }
    Ok(count)
}
pub(super) fn invalid<T, E>(result: Result<T, E>) -> Result<T, StoreFailure> {
    result.map_err(|_| StoreFailure::InvalidDurableState)
}
pub(super) fn counter(row: &Row<'_>, index: usize) -> Result<u64, StoreFailure> {
    invalid(super::recovery::counter(row, index))
}
pub(super) fn fixed<const N: usize>(row: &Row<'_>, index: usize) -> Result<[u8; N], StoreFailure> {
    invalid(invalid(row.get_ref(index))?.as_blob())?
        .try_into()
        .map_err(|_| StoreFailure::InvalidDurableState)
}
pub(super) fn blob<'a>(row: &'a Row<'_>, index: usize) -> Result<&'a [u8], StoreFailure> {
    invalid(invalid(row.get_ref(index))?.as_blob())
}
pub(super) fn codec_error(error: codec::CodecFailure) -> StoreFailure {
    match error {
        codec::CodecFailure::UnsupportedVersion => StoreFailure::UnknownSchema,
        _ => StoreFailure::InvalidDurableState,
    }
}
pub(super) fn binding(
    sql: &Transaction<'_>,
    server: ServerId,
    key: u64,
    sequence: u64,
    digest: [u8; 32],
) -> Result<OperationBinding, StoreFailure> {
    let stream = sql
        .query_row(
            "SELECT pairing_id, client_id FROM operation_streams WHERE stream_key = ?1",
            [key.to_be_bytes().as_slice()],
            |row| {
                Ok((
                    super::recovery::counter(row, 0)?,
                    super::recovery::counter(row, 1)?,
                ))
            },
        )
        .map_err(classify)?;
    let stream = invalid(OperationStreamId::new(
        invalid(OperationStreamKey::new(server, key))?,
        invalid(PairingId::new(server, stream.0))?,
        invalid(ClientId::new(server, stream.1))?,
    ))?;
    Ok(OperationBinding {
        id: OperationId::new(stream, invalid(OperationSequence::new(sequence))?),
        digest: RequestDigest::from_bytes(digest),
    })
}
pub(super) fn inventory(
    sql: &Transaction<'_>,
    table: &str,
    payload: &str,
    collection: Collection,
    limits: &StoreLimits,
    budget: &mut SizeBudget,
) -> Result<u64, StoreFailure> {
    // Identifiers are static local schema names, not remotely supplied SQL.
    let (count, bytes): (u64, u64) = sql
        .query_row(
            &format!("SELECT count(*), coalesce(sum({payload}), 0) FROM {table}"),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(classify)?;
    invalid(limits.check_count(collection, count))?;
    invalid(budget.charge_rows(count, 1024))?;
    invalid(
        budget.charge(
            usize::try_from(bytes)
                .ok()
                .and_then(|n| n.checked_mul(8))
                .ok_or(StoreFailure::InvalidDurableState)?,
        ),
    )?;
    Ok(count)
}
pub(super) fn reserve<T>(count: u64) -> Result<Vec<T>, StoreFailure> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(usize::try_from(count).map_err(|_| StoreFailure::InvalidDurableState)?)
        .map_err(|_| StoreFailure::Unavailable)?;
    Ok(values)
}
pub(super) fn scan<const N: usize, T>(
    sql: &Transaction<'_>,
    query: &str,
    count: u64,
    decode: impl Fn(&Row<'_>) -> Result<T, StoreFailure>,
) -> Result<Vec<T>, StoreFailure> {
    let mut values = reserve(count)?;
    let mut key = [0; N];
    for _ in 0..count {
        let (next, value) = sql
            .query_row(query, [key.as_slice()], |row| {
                Ok((fixed::<N>(row, 0), decode(row)))
            })
            .map_err(classify)?;
        let next = next?;
        if next <= key {
            return Err(StoreFailure::InvalidDurableState);
        }
        key = next;
        values.push(value?);
    }
    Ok(values)
}
pub(super) fn scan_operations<T>(
    sql: &Transaction<'_>,
    query: &str,
    count: u64,
    decode: impl Fn(&Row<'_>) -> Result<T, StoreFailure>,
) -> Result<Vec<T>, StoreFailure> {
    let mut values = reserve(count)?;
    let mut key = [0_u8; 8];
    let mut sequence = [0_u8; 8];
    for _ in 0..count {
        let (next, seq, value) = sql
            .query_row(
                query,
                rusqlite::params![key.as_slice(), sequence.as_slice()],
                |row| Ok((fixed::<8>(row, 0), fixed::<8>(row, 1), decode(row))),
            )
            .map_err(classify)?;
        let (next, seq) = (next?, seq?);
        if (next, seq) <= (key, sequence) {
            return Err(StoreFailure::InvalidDurableState);
        }
        key = next;
        sequence = seq;
        values.push(value?);
    }
    Ok(values)
}

fn native_preflight(
    value: &spaceterm_core::retirement::NativeRetirementMutation,
    budget: &mut SizeBudget,
) -> Result<usize, StoreFailure> {
    use spaceterm_core::retirement::NativeRetirementMutation::*;
    let descriptors: &[&spaceterm_core::retirement::NativeRetirementDescriptor] = match value {
        Accept(v) => &[v],
        Progress { expected, next } => &[expected, next],
        Complete { .. } => &[],
    };
    budget
        .charge(1024)
        .map_err(|_| StoreFailure::InvalidTransaction)?;
    for descriptor in descriptors {
        let count = descriptor
            .panes()
            .len()
            .checked_add(descriptor.parts().sessions.len())
            .ok_or(StoreFailure::InvalidTransaction)?;
        budget
            .charge_rows(count as u64, 1024)
            .map_err(|_| StoreFailure::InvalidTransaction)?;
    }
    Ok(1)
}
fn native_action(value: spaceterm_core::native_management::NativeRetirementAction) -> i64 {
    use spaceterm_core::native_management::NativeRetirementAction::*;
    match value {
        Stop => 0,
        RestartApproved => 1,
        ActivateUpdate => 2,
    }
}
fn native_apply(
    sql: &Transaction<'_>,
    value: &spaceterm_core::retirement::NativeRetirementMutation,
    parts: &TransactionParts,
    previous: &RecoveredState,
) -> Result<usize, StoreFailure> {
    use spaceterm_core::retirement::{NativeRetirementMutation::*, RetirementCleanupState};
    match value {
        Accept(value) => {
            if previous.native_retirements.iter().any(|v| {
                v.request().id_bytes() == value.request().id_bytes() || v.batch() == value.batch()
            }) || value.request().server() != parts.server
                || value.batch().epoch() != parts.resulting_epoch
                || value.acceptance_revision() != parts.resulting_organization_revision
                || value
                    .parts()
                    .sessions
                    .iter()
                    .any(|s| s.cleanup() != RetirementCleanupState::AwaitingEvidence)
            {
                return Err(StoreFailure::InvalidTransaction);
            }
            let bytes = codec::encode_native_retirement(value).map_err(codec_error)?;
            exact(
                sql,
                "INSERT INTO native_retirements VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    value.request().id_bytes().as_slice(),
                    value.request().digest().bytes().as_slice(),
                    value.batch().epoch().get().to_be_bytes().as_slice(),
                    value.batch().sequence().to_be_bytes().as_slice(),
                    native_action(value.parts().action),
                    value.acceptance_revision().get().to_be_bytes().as_slice(),
                    bytes
                ],
                1,
            )
        }
        Progress { expected, next } => {
            let old = previous
                .native_retirements
                .iter()
                .find(|v| v.request().id_bytes() == expected.request().id_bytes())
                .ok_or(StoreFailure::InvalidTransaction)?;
            if old != expected.as_ref()
                || expected.request() != next.request()
                || expected.batch() != next.batch()
                || expected.parts().action != next.parts().action
                || expected.acceptance_revision() != next.acceptance_revision()
                || expected.panes() != next.panes()
                || expected.parts().sessions.len() != next.parts().sessions.len()
                || expected
                    .parts()
                    .sessions
                    .iter()
                    .zip(&next.parts().sessions)
                    .any(|(a, b)| {
                        a.session() != b.session()
                            || a.first_reason() != b.first_reason()
                            || (a.cleanup() != RetirementCleanupState::AwaitingEvidence
                                && a.cleanup() != b.cleanup())
                    })
            {
                return Err(StoreFailure::InvalidTransaction);
            }
            let bytes = codec::encode_native_retirement(next).map_err(codec_error)?;
            exact(
                sql,
                "UPDATE native_retirements SET descriptor = ?3 WHERE request_id = ?1 AND request_digest = ?2",
                rusqlite::params![
                    next.request().id_bytes().as_slice(),
                    next.request().digest().bytes().as_slice(),
                    bytes
                ],
                1,
            )
        }
        Complete { request, batch } => {
            let old = previous
                .native_retirements
                .iter()
                .find(|v| v.request().id_bytes() == request.id_bytes())
                .ok_or(StoreFailure::InvalidTransaction)?;
            if old.request() != *request
                || old.batch() != *batch
                || old
                    .parts()
                    .sessions
                    .iter()
                    .any(|s| !matches!(s.cleanup(), RetirementCleanupState::Confirmed(_)))
            {
                return Err(StoreFailure::InvalidTransaction);
            }
            exact(
                sql,
                "DELETE FROM native_retirements WHERE request_id = ?1 AND request_digest = ?2",
                rusqlite::params![
                    request.id_bytes().as_slice(),
                    request.digest().bytes().as_slice()
                ],
                1,
            )
        }
    }
}
fn native_recover(
    sql: &Transaction<'_>,
    recovered: &mut RecoveredState,
    limits: &StoreLimits,
    budget: &mut SizeBudget,
) -> Result<(), StoreFailure> {
    let count = inventory(
        sql,
        "native_retirements",
        "length(descriptor)",
        Collection::NativeRetirements,
        limits,
        budget,
    )?;
    let server = recovered.organization.server;
    recovered.native_retirements = scan::<16, _>(
        sql,
        "SELECT request_id, request_digest, batch_epoch, batch_sequence, action, acceptance_revision, descriptor FROM native_retirements WHERE request_id > ?1 ORDER BY request_id LIMIT 1",
        count,
        |row| {
            let value =
                codec::decode_native_retirement(blob(row, 6)?, server).map_err(codec_error)?;
            if value.request().id_bytes() != fixed::<16>(row, 0)?
                || value.request().digest().bytes() != fixed::<32>(row, 1)?
                || value.batch().epoch().get() != counter(row, 2)?
                || value.batch().sequence() != counter(row, 3)?
                || native_action(value.parts().action) != invalid(row.get::<_, i64>(4))?
                || value.acceptance_revision().get() != counter(row, 5)?
            {
                return Err(StoreFailure::InvalidDurableState);
            }
            Ok(value)
        },
    )?;
    Ok(())
}
