//! One physical organization and operation commit owner.

use super::{
    StoreIdentity, StoreLimits, StoreServices,
    database_adapter::classify,
    limits::{MAX_CHANGED_ROWS, SizeBudget},
    organization, recovery,
};
use crate::storage::Transaction;
use rusqlite::{Params, params};
use spaceterm_core::storage::{CommitReceipt, OperationWrite, StoreFailure, TransactionParts};

pub(super) fn proposal_failure(error: StoreFailure) -> StoreFailure {
    match error {
        StoreFailure::InvalidDurableState => StoreFailure::InvalidTransaction,
        _ => error,
    }
}

pub(super) fn preflight(
    parts: &TransactionParts,
    limits: &StoreLimits,
    services: &StoreServices,
    budget: &mut SizeBudget,
) -> Result<(), StoreFailure> {
    limits
        .check_transaction_counts(
            parts.organization_writes.len(),
            parts.operation_writes.len(),
        )
        .map_err(|_| StoreFailure::InvalidTransaction)?;
    if parts.expected_store_revision.checked_successor() != Some(parts.resulting_store_revision)
        || !parts
            .resulting_high_water
            .dominates(parts.expected_high_water)
        || (parts.expected_epoch != parts.resulting_epoch
            && parts.expected_epoch.checked_successor() != Some(parts.resulting_epoch))
        || (parts.expected_organization_revision != parts.resulting_organization_revision
            && parts.expected_organization_revision.checked_successor()
                != Some(parts.resulting_organization_revision))
        || (parts.organization_writes.is_empty()
            != (parts.expected_organization_revision == parts.resulting_organization_revision))
    {
        return Err(StoreFailure::InvalidTransaction);
    }
    // Schema v1 is a distinct canonical ledger format. Legacy command-bearing
    // records are not silently reencoded under new digests or reservations.
    if parts.operation_writes.iter().any(|v| {
        matches!(
            v,
            OperationWrite::Put(_)
                | OperationWrite::SetStream(_)
                | OperationWrite::RetireAcknowledgedPrefix { .. }
        )
    }) {
        return Err(StoreFailure::RecoveryRequired);
    }
    organization::preflight(parts, budget)?;
    let operation_rows = services
        .operations
        .preflight(&parts.operation_writes, budget)?;
    limits
        .check_transaction_counts(operation_rows, parts.operation_writes.len())
        .map_err(|_| StoreFailure::InvalidTransaction)?;
    services
        .validator
        .validate_transaction_projection(parts)
        .map_err(proposal_failure)
}

pub(super) struct SqlWriter<'a, 'b> {
    pub sql: &'a Transaction<'b>,
    changed: usize,
}
impl SqlWriter<'_, '_> {
    pub(super) fn execute<P: Params>(
        &mut self,
        sql: &str,
        parameters: P,
    ) -> Result<usize, StoreFailure> {
        let rows = self
            .sql
            .execute(sql, parameters)
            .map_err(|error| proposal_failure(classify(error)))?;
        self.charge(rows)?;
        Ok(rows)
    }
    pub(super) fn exact<P: Params>(
        &mut self,
        sql: &str,
        parameters: P,
        expected: usize,
    ) -> Result<(), StoreFailure> {
        if self.execute(sql, parameters)? != expected {
            return Err(StoreFailure::InvalidTransaction);
        }
        Ok(())
    }
    fn charge(&mut self, rows: usize) -> Result<(), StoreFailure> {
        self.changed = self
            .changed
            .checked_add(rows)
            .filter(|rows| *rows <= MAX_CHANGED_ROWS)
            .ok_or(StoreFailure::InvalidTransaction)?;
        Ok(())
    }
}

pub(super) fn apply(
    sql: &Transaction<'_>,
    parts: &TransactionParts,
    identity: StoreIdentity,
    limits: &StoreLimits,
    services: &StoreServices,
) -> Result<CommitReceipt, StoreFailure> {
    // Compare the caller first so an ordinary stale request is not corruption.
    let metadata = recovery::metadata(sql, identity)?;
    if parts.server != identity.server {
        return Err(StoreFailure::IdentityMismatch);
    }
    if metadata.store_revision != parts.expected_store_revision
        || metadata.organization_revision != parts.expected_organization_revision
        || metadata.epoch != parts.expected_epoch
        || recovery::high_water(sql)? != parts.expected_high_water
    {
        return Err(StoreFailure::RevisionConflict);
    }
    let previous = recovery::load(sql, identity, limits, services)?;
    organization::compare(parts, &previous)?;
    let mut budget = limits.transaction_budget();
    let operation_bound = services
        .operations
        .preflight(&parts.operation_writes, &mut budget)?;
    let bound = organization::row_bound(sql, parts)?
        .checked_add(operation_bound)
        .ok_or(StoreFailure::InvalidTransaction)?;
    limits
        .check_transaction_counts(bound, parts.operation_writes.len())
        .map_err(|_| StoreFailure::InvalidTransaction)?;
    let receipt = CommitReceipt {
        store_revision: parts.resulting_store_revision,
        organization_revision: parts.resulting_organization_revision,
    };
    let mut writer = SqlWriter { sql, changed: 0 };
    organization::apply(&mut writer, parts)?;
    let rows = services
        .operations
        .apply(sql, parts, &previous)
        .map_err(proposal_failure)?;
    if rows > operation_bound {
        return Err(StoreFailure::InvalidTransaction);
    }
    writer.charge(rows)?;
    // Release the previous candidate before allocating the resulting one.
    drop(previous);
    let marks = parts.resulting_high_water;
    writer.exact("UPDATE identity_high_water SET workspace = ?1, tab = ?2, pane = ?3, worktree = ?4, repository = ?5, pairing = ?6, client = ?7, operation_stream = ?8, creation_ordinal = ?9 WHERE singleton = 1", params![marks.workspace.to_be_bytes().as_slice(), marks.tab.to_be_bytes().as_slice(), marks.pane.to_be_bytes().as_slice(), marks.worktree.to_be_bytes().as_slice(), marks.repository.to_be_bytes().as_slice(), marks.pairing.to_be_bytes().as_slice(), marks.client.to_be_bytes().as_slice(), marks.operation_stream.to_be_bytes().as_slice(), marks.creation_ordinal.to_be_bytes().as_slice()], 1)?;
    writer.exact("UPDATE store_metadata SET store_revision = ?1, organization_revision = ?2, last_epoch = ?3 WHERE singleton = 1", params![parts.resulting_store_revision.get().to_be_bytes().as_slice(), parts.resulting_organization_revision.get().to_be_bytes().as_slice(), parts.resulting_epoch.get().to_be_bytes().as_slice()], 1)?;
    recovery::load(sql, identity, limits, services).map_err(proposal_failure)?;
    // All encodings, candidate validation and receipt preparation precede the
    // authority commit. No serialization or additional sync follows its receipt.
    Ok(receipt)
}
