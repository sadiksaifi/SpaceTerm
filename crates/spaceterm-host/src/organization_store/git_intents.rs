//! Git comparison records and CAS projections. Recovery never queries Git or
//! reconstructs native directory authority. Ledger/Git owners validate pending
//! progress, result policy and allocation consumption in the complete bundle.

use super::{
    StoreLimits,
    auxiliary::*,
    codec,
    limits::{Collection, SizeBudget},
};
use crate::storage::Transaction;
use rusqlite::params;
use spaceterm_core::identity::RepositoryId;
use spaceterm_core::operation_ledger_contracts::LedgerWrite;
use spaceterm_core::operations::RepositoryScope;
use spaceterm_core::organization::ServerDirectory;
use spaceterm_core::storage::{OperationWrite, RecoveredState, StoreFailure, TransactionParts};
use spaceterm_core::worktrees::intent::*;
use std::collections::BTreeSet;

fn charge(budget: &mut SizeBudget, bytes: usize) -> Result<(), StoreFailure> {
    budget
        .charge(bytes)
        .map_err(|_| StoreFailure::InvalidTransaction)
}
fn scope_bytes(value: &RetainedRemovalScope) -> Result<usize, StoreFailure> {
    let p = value.parts();
    let rows = p
        .tabs
        .len()
        .checked_add(p.panes.len())
        .and_then(|n| n.checked_add(p.fallbacks.len()))
        .ok_or(StoreFailure::InvalidTransaction)?;
    p.fallbacks.iter().try_fold(
        rows.checked_mul(512)
            .ok_or(StoreFailure::InvalidTransaction)?,
        |n, f| {
            n.checked_add(f.directory.bytes().len())
                .ok_or(StoreFailure::InvalidTransaction)
        },
    )
}

pub(super) fn preflight(
    write: &OperationWrite,
    budget: &mut SizeBudget,
) -> Result<usize, StoreFailure> {
    // Covers prepared encodings, decoding temporaries and validator indexes before
    // variable-size encoding or cloning. Domain constructors already bound counts.
    let bytes = match write {
        OperationWrite::GitRepository(GitRepositoryMutation::Put(value)) => {
            512 + value.parts().main_root.bytes().len()
                + value.parts().common_directory.bytes().len()
        }
        OperationWrite::GitIntent(GitIntentMutation::Insert(value)) => {
            let p = value.parts();
            value
                .parts()
                .action
                .validate_scope(value.binding().id.stream().server())
                .map_err(|_| StoreFailure::InvalidTransaction)?;
            32_768usize
                .checked_add(scope_bytes(&p.affected)?)
                .ok_or(StoreFailure::InvalidTransaction)?
        }
        OperationWrite::GitAllocation(GitAllocationMutation::Reserve(value)) => {
            value.parts().fallbacks.iter().try_fold(1024usize, |n, f| {
                n.checked_add(1024 + f.plan.directory.bytes().len())
                    .ok_or(StoreFailure::InvalidTransaction)
            })?
        }
        OperationWrite::GitIntent(value) => {
            value
                .validate()
                .map_err(|_| StoreFailure::InvalidTransaction)?;
            1024
        }
        OperationWrite::GitRepository(_) | OperationWrite::GitAllocation(_) => 1024,
        _ => return Err(StoreFailure::InvalidTransaction),
    };
    charge(
        budget,
        bytes
            .checked_mul(2)
            .ok_or(StoreFailure::InvalidTransaction)?,
    )?;
    Ok(1)
}

pub(super) fn check_duplicates(writes: &[OperationWrite]) -> Result<(), StoreFailure> {
    let mut repositories = BTreeSet::new();
    let mut intents = BTreeSet::new();
    let mut allocations = BTreeSet::new();
    for write in writes {
        let unique = match write {
            OperationWrite::GitRepository(value) => repositories.insert(value.repository()),
            OperationWrite::GitIntent(value) => intents.insert(value.binding().id),
            OperationWrite::GitAllocation(value) => allocations.insert(value.binding().id),
            _ => true,
        };
        if !unique {
            return Err(StoreFailure::InvalidTransaction);
        }
    }
    Ok(())
}

fn completed(
    parts: &TransactionParts,
    original: spaceterm_core::operations::OperationBinding,
) -> bool {
    parts.operation_writes.iter().any(|w| matches!(w, OperationWrite::Ledger(v) if matches!(v.as_ref(), LedgerWrite::CompleteOperation { record, .. } if record.binding == original && record.completed_at == parts.resulting_store_revision)))
}
fn phase(value: GitIntentPhase) -> i64 {
    match value {
        GitIntentPhase::Prepared => 0,
        GitIntentPhase::Started => 1,
        GitIntentPhase::ExternalFinished => 2,
    }
}
fn decode_phase(value: i64) -> Result<GitIntentPhase, StoreFailure> {
    match value {
        0 => Ok(GitIntentPhase::Prepared),
        1 => Ok(GitIntentPhase::Started),
        2 => Ok(GitIntentPhase::ExternalFinished),
        _ => Err(StoreFailure::InvalidDurableState),
    }
}

pub(super) fn apply(
    sql: &Transaction<'_>,
    write: &OperationWrite,
    parts: &TransactionParts,
    previous: &RecoveredState,
) -> Result<usize, StoreFailure> {
    match write {
        OperationWrite::GitRepository(GitRepositoryMutation::Put(value)) => {
            let p = value.parts();
            let id = value.scope().repository;
            if id.server() != parts.server || id.local() > parts.resulting_high_water.repository {
                return Err(StoreFailure::InvalidTransaction);
            }
            if let Some(old) = previous
                .git_repositories
                .iter()
                .find(|v| v.scope().repository == id)
            {
                if old != value {
                    return Err(StoreFailure::InvalidTransaction);
                }
                return Ok(0);
            }
            // A locator may first be discovered for an already retained repository;
            // it is not an organization identity insertion or allocator.
            let known = previous.organization.repositories.iter().any(|v| v.id == id)
                || parts.organization_writes.iter().any(|w| matches!(w, spaceterm_core::organization::OrganizationWrite::PutRepository(v) if v.id == id))
                || previous.git_allocations.iter().any(|v| v.parts().repository == Some(id))
                || parts.operation_writes.iter().any(|w| matches!(w, OperationWrite::GitAllocation(GitAllocationMutation::Reserve(v)) if v.parts().repository == Some(id)));
            if !known {
                return Err(StoreFailure::InvalidTransaction);
            }
            exact(
                sql,
                "INSERT INTO git_repository_locators VALUES (?1, ?2, ?3)",
                params![
                    id.local().to_be_bytes().as_slice(),
                    p.main_root.bytes(),
                    p.common_directory.bytes()
                ],
                1,
            )
        }
        OperationWrite::GitRepository(GitRepositoryMutation::Delete(id)) => {
            if !previous
                .git_repositories
                .iter()
                .any(|v| v.scope().repository == *id)
                || previous
                    .git_intents
                    .iter()
                    .any(|v| v.parts().repository.scope().repository == *id)
            {
                return Err(StoreFailure::InvalidTransaction);
            }
            exact(
                sql,
                "DELETE FROM git_repository_locators WHERE repository_id = ?1",
                [id.local().to_be_bytes().as_slice()],
                1,
            )
        }
        OperationWrite::GitAllocation(value) => {
            let b = value.binding();
            let old = previous
                .git_allocations
                .iter()
                .find(|v| v.binding().id == b.id);
            let key = b.id.stream().key().local().to_be_bytes();
            let sequence = b.id.sequence().get().to_be_bytes();
            match value {
                GitAllocationMutation::Reserve(value) => {
                    if old.is_some()
                        || value.parts().expected_high_water != parts.expected_high_water
                        || value.parts().resulting_high_water != parts.resulting_high_water
                    {
                        return Err(StoreFailure::InvalidTransaction);
                    }
                    let bytes = codec::encode_git_allocation(value).map_err(codec_error)?;
                    exact(
                        sql,
                        "INSERT INTO git_allocation_reservations VALUES (?1, ?2, ?3, ?4)",
                        params![
                            key.as_slice(),
                            sequence.as_slice(),
                            b.digest.bytes().as_slice(),
                            bytes
                        ],
                        1,
                    )
                }
                GitAllocationMutation::Consume { .. } | GitAllocationMutation::Abandon { .. } => {
                    if old.is_none_or(|v| v.binding() != b) || !completed(parts, b) {
                        return Err(StoreFailure::InvalidTransaction);
                    }
                    exact(
                        sql,
                        "DELETE FROM git_allocation_reservations WHERE stream_key = ?1 AND sequence = ?2 AND request_digest = ?3",
                        params![
                            key.as_slice(),
                            sequence.as_slice(),
                            b.digest.bytes().as_slice()
                        ],
                        1,
                    )
                }
            }
        }
        OperationWrite::GitIntent(value) => {
            value
                .validate()
                .map_err(|_| StoreFailure::InvalidTransaction)?;
            let b = value.binding();
            let old = previous.git_intents.iter().find(|v| v.binding().id == b.id);
            let key = b.id.stream().key().local().to_be_bytes();
            let sequence = b.id.sequence().get().to_be_bytes();
            match value {
                GitIntentMutation::Insert(value) => {
                    if old.is_some() {
                        return Err(StoreFailure::InvalidTransaction);
                    }
                    let p = value.parts();
                    let locator_matches = previous.git_repositories.iter().any(|v| v == &p.repository)
                        || parts.operation_writes.iter().any(|w| matches!(w, OperationWrite::GitRepository(GitRepositoryMutation::Put(v)) if v == &p.repository));
                    let allocation_matches = previous.git_allocations.iter().any(|v| v.binding() == b)
                        || parts.operation_writes.iter().any(|w| matches!(w, OperationWrite::GitAllocation(GitAllocationMutation::Reserve(v)) if v.binding() == b));
                    if !locator_matches || !allocation_matches {
                        return Err(StoreFailure::InvalidTransaction);
                    }
                    let action = codec::encode_git_action(&p.action).map_err(codec_error)?;
                    let affected =
                        codec::encode_git_removal_scope(&p.affected).map_err(codec_error)?;
                    exact(
                        sql,
                        "INSERT INTO git_write_intents VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, NULL)",
                        params![
                            key.as_slice(),
                            sequence.as_slice(),
                            b.digest.bytes().as_slice(),
                            p.repository
                                .scope()
                                .repository
                                .local()
                                .to_be_bytes()
                                .as_slice(),
                            action,
                            p.assessment_digest.bytes().as_slice(),
                            affected
                        ],
                        1,
                    )
                }
                GitIntentMutation::Advance {
                    expected_phase,
                    next,
                    result,
                    ..
                } => {
                    if old.is_none_or(|v| v.binding() != b || v.phase() != *expected_phase) {
                        return Err(StoreFailure::InvalidTransaction);
                    }
                    let encoded = result
                        .as_ref()
                        .map(codec::encode_git_external_result)
                        .transpose()
                        .map_err(codec_error)?;
                    exact(
                        sql,
                        "UPDATE git_write_intents SET phase = ?4, external_result = ?5 WHERE stream_key = ?1 AND sequence = ?2 AND request_digest = ?3 AND phase = ?6",
                        params![
                            key.as_slice(),
                            sequence.as_slice(),
                            b.digest.bytes().as_slice(),
                            phase(*next),
                            encoded,
                            phase(*expected_phase)
                        ],
                        1,
                    )
                }
                GitIntentMutation::Complete { .. } => {
                    if old.is_none_or(|v| v.binding() != b || v.phase() == GitIntentPhase::Started)
                        || !completed(parts, b)
                    {
                        return Err(StoreFailure::InvalidTransaction);
                    }
                    exact(
                        sql,
                        "DELETE FROM git_write_intents WHERE stream_key = ?1 AND sequence = ?2 AND request_digest = ?3",
                        params![
                            key.as_slice(),
                            sequence.as_slice(),
                            b.digest.bytes().as_slice()
                        ],
                        1,
                    )
                }
            }
        }
        _ => Err(StoreFailure::InvalidTransaction),
    }
}

pub(super) fn recover(
    sql: &Transaction<'_>,
    recovered: &mut RecoveredState,
    limits: &StoreLimits,
    budget: &mut SizeBudget,
) -> Result<(), StoreFailure> {
    let repositories = inventory(
        sql,
        "git_repository_locators",
        "length(main_root) + length(common_directory)",
        Collection::Repositories,
        limits,
        budget,
    )?;
    let allocations = inventory(
        sql,
        "git_allocation_reservations",
        "length(reservation)",
        Collection::GitIntents,
        limits,
        budget,
    )?;
    let intents = inventory(
        sql,
        "git_write_intents",
        "length(action) + length(affected_scope) + coalesce(length(external_result), 0)",
        Collection::GitIntents,
        limits,
        budget,
    )?;
    invalid(budget.charge_rows(
        intents,
        2 * spaceterm_core::organization::MAX_DIRECTORY_BYTES,
    ))?;
    let server = recovered.organization.server;
    recovered.git_repositories = scan::<8, _>(
        sql,
        "SELECT repository_id, main_root, common_directory FROM git_repository_locators WHERE repository_id > ?1 ORDER BY repository_id LIMIT 1",
        repositories,
        |row| {
            let main = blob(row, 1)?;
            let common = blob(row, 2)?;
            if main.len() > spaceterm_core::organization::MAX_DIRECTORY_BYTES
                || common.len() > spaceterm_core::organization::MAX_DIRECTORY_BYTES
            {
                return Err(StoreFailure::InvalidDurableState);
            }
            invalid(RepositoryLocator::new(RepositoryLocatorParts {
                scope: RepositoryScope {
                    server,
                    repository: invalid(RepositoryId::new(server, counter(row, 0)?))?,
                },
                main_root: invalid(ServerDirectory::new(server, main.to_vec()))?,
                common_directory: invalid(ServerDirectory::new(server, common.to_vec()))?,
            }))
        },
    )?;
    recovered.git_allocations = scan_operations(
        sql,
        "SELECT stream_key, sequence, request_digest, reservation FROM git_allocation_reservations WHERE (stream_key, sequence) > (?1, ?2) ORDER BY stream_key, sequence LIMIT 1",
        allocations,
        |row| {
            let original = binding(
                sql,
                server,
                counter(row, 0)?,
                counter(row, 1)?,
                fixed(row, 2)?,
            )?;
            let value = codec::decode_git_allocation(blob(row, 3)?, server).map_err(codec_error)?;
            if value.binding() != original
                || !recovered
                    .high_water
                    .dominates(value.parts().resulting_high_water)
            {
                return Err(StoreFailure::InvalidDurableState);
            }
            Ok(value)
        },
    )?;
    recovered.git_intents = scan_operations(
        sql,
        "SELECT stream_key, sequence, request_digest, repository_id, action, assessment_digest, phase, affected_scope, external_result FROM git_write_intents WHERE (stream_key, sequence) > (?1, ?2) ORDER BY stream_key, sequence LIMIT 1",
        intents,
        |row| {
            let original = binding(
                sql,
                server,
                counter(row, 0)?,
                counter(row, 1)?,
                fixed(row, 2)?,
            )?;
            let repository = invalid(RepositoryId::new(server, counter(row, 3)?))?;
            let locator = recovered
                .git_repositories
                .iter()
                .find(|v| v.scope().repository == repository)
                .ok_or(StoreFailure::InvalidDurableState)?;
            let result = match invalid(row.get_ref(8))? {
                rusqlite::types::ValueRef::Null => None,
                _ => Some(codec::decode_git_external_result(blob(row, 8)?).map_err(codec_error)?),
            };
            invalid(GitWriteIntent::new(GitWriteIntentParts {
                binding: original,
                repository: locator.clone(),
                action: codec::decode_git_action(blob(row, 4)?, server).map_err(codec_error)?,
                assessment_digest: GitAssessmentDigest::from_bytes(fixed(row, 5)?),
                phase: decode_phase(invalid(row.get(6))?)?,
                affected: codec::decode_git_removal_scope(blob(row, 7)?, server)
                    .map_err(codec_error)?,
                allocation: original,
                external_result: result,
            }))
        },
    )?;
    for value in &recovered.git_intents {
        if !value.fits_marks(recovered.high_water) {
            return Err(StoreFailure::InvalidDurableState);
        }
    }
    Ok(())
}
