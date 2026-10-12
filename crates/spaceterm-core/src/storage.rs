//! Atomic durability boundary. SQL, native file authority and synchronization
//! belong to the host; organization and ledger policy belong to core reducers.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::*;
use crate::identity::*;
use crate::operations::*;
use crate::organization::*;

/// Typed, content-free storage classifications. Locked or unavailable storage is
/// never treated as missing organization or permission to replace an identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Error, PartialEq, Serialize)]
pub enum StoreFailure {
    #[error("storage is locked")]
    Locked,
    #[error("storage is unavailable")]
    Unavailable,
    #[error("storage authority is unsafe")]
    UnsafeStorage,
    #[error("storage schema is unsupported")]
    UnknownSchema,
    #[error("durable state is invalid")]
    InvalidDurableState,
    #[error("storage I/O failed")]
    Io,
    #[error("store revision comparison failed")]
    RevisionConflict,
    #[error("durable counters are exhausted")]
    Exhausted,
}

/// A host may implement this through synchronous SQLite on a blocking worker.
/// Commit metadata, field versions, marks, ledger and revisions atomically before
/// publication or acknowledgement. Never reopen a live SQLite inode by path.
/// Store full-width u64 counters losslessly, for example as eight-byte big-endian
/// BLOBs. Never cast arbitrary u64 values into SQLite signed INTEGER fields.
pub trait OrganizationStore: Send {
    fn recover(&mut self) -> Result<RecoveredState, StoreFailure>;
    fn commit(&mut self, transaction: &DurableTransaction) -> CommitDisposition;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommitReceipt {
    pub store_revision: StoreRevision,
    pub organization_revision: OrganizationRevision,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitDisposition {
    Durable(CommitReceipt),
    /// The host knows that no durable effects were applied.
    NotApplied(StoreFailure),
    /// Mutation admission must close until validated recovery resolves original
    /// identities. Neither success nor clean rollback can be claimed. A later
    /// successful fsync alone does not prove that the failed writes survived.
    Indeterminate,
}

/// Compact durable state. A pending external write is reconciled against native
/// Git/filesystem state after recovery, never blindly executed a second time.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DurableOperationState {
    Pending {
        command: Command,
    },
    Completed {
        result: Outcome,
        publication_revision: OrganizationRevision,
    },
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DurableOperationRecord {
    pub binding: OperationBinding,
    pub state: DurableOperationState,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DurableStreamState {
    pub id: OperationStreamId,
    pub admitted_through: u64,
    /// Only a client that durably recorded results may acknowledge this prefix.
    pub acknowledged_through: u64,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum OperationWrite {
    Put(Box<DurableOperationRecord>),
    SetStream(DurableStreamState),
    /// Compaction retains a durable prefix marker. An old ID never becomes new.
    RetireAcknowledgedPrefix {
        stream: OperationStreamId,
        through: OperationSequence,
    },
}

/// A transaction proposal, not a commit receipt. Expected values must be compared
/// inside the host transaction; constructor checks cannot establish durability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionParts {
    pub server: ServerId,
    pub expected_store_revision: StoreRevision,
    pub resulting_store_revision: StoreRevision,
    pub expected_organization_revision: OrganizationRevision,
    pub resulting_organization_revision: OrganizationRevision,
    pub expected_epoch: ServerEpoch,
    pub resulting_epoch: ServerEpoch,
    pub expected_high_water: IdentityHighWater,
    pub resulting_high_water: IdentityHighWater,
    pub organization_writes: Vec<OrganizationWrite>,
    pub operation_writes: Vec<OperationWrite>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableTransaction(TransactionParts);
impl DurableTransaction {
    pub fn new(parts: TransactionParts) -> Result<Self, StoreFailure> {
        let next_store = parts
            .expected_store_revision
            .checked_successor()
            .ok_or(StoreFailure::Exhausted)?;
        if parts.resulting_store_revision != next_store {
            return Err(StoreFailure::InvalidDurableState);
        }
        let organization_unchanged =
            parts.resulting_organization_revision == parts.expected_organization_revision;
        if (!organization_unchanged
            && parts.expected_organization_revision.checked_successor()
                != Some(parts.resulting_organization_revision))
            || (organization_unchanged && !parts.organization_writes.is_empty())
            || !parts
                .resulting_high_water
                .dominates(parts.expected_high_water)
            || (parts.resulting_epoch != parts.expected_epoch
                && parts.expected_epoch.checked_successor() != Some(parts.resulting_epoch))
        {
            return Err(StoreFailure::InvalidDurableState);
        }
        for write in &parts.organization_writes {
            write
                .validate_scope(parts.server)
                .map_err(|_| StoreFailure::InvalidDurableState)?;
            if !write_fits_marks(write, parts.resulting_high_water) {
                return Err(StoreFailure::InvalidDurableState);
            }
        }
        for write in &parts.operation_writes {
            match write {
                OperationWrite::Put(record) => validate_operation(
                    record,
                    parts.server,
                    parts.resulting_organization_revision,
                    parts.resulting_high_water,
                )?,
                OperationWrite::SetStream(stream) => {
                    validate_stream(stream, parts.server)?;
                    if !stream_fits_marks(stream.id, parts.resulting_high_water) {
                        return Err(StoreFailure::InvalidDurableState);
                    }
                }
                OperationWrite::RetireAcknowledgedPrefix { stream, .. }
                    if stream.server() != parts.server
                        || !stream_fits_marks(*stream, parts.resulting_high_water) =>
                {
                    return Err(StoreFailure::InvalidDurableState);
                }
                OperationWrite::RetireAcknowledgedPrefix { .. } => {}
            }
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &TransactionParts {
        &self.0
    }
    pub fn accepts_receipt(&self, receipt: CommitReceipt) -> bool {
        receipt.store_revision == self.0.resulting_store_revision
            && receipt.organization_revision == self.0.resulting_organization_revision
    }
}

/// Live processes, content, grants and native proofs are absent from recovery.
/// Server composition publishes fresh NotStarted/unclaimed runtime facts in a
/// new durably advanced epoch. High-water marks include deleted entities.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveredState {
    pub organization: OrganizationState,
    pub store_revision: StoreRevision,
    pub last_epoch: ServerEpoch,
    pub high_water: IdentityHighWater,
    pub streams: Vec<DurableStreamState>,
    pub operations: Vec<DurableOperationRecord>,
}
impl RecoveredState {
    pub fn validate(&self) -> Result<(), StoreFailure> {
        self.organization
            .validate()
            .map_err(|_| StoreFailure::InvalidDurableState)?;
        let server = self.organization.server;
        let marks = self.high_water;
        let too_high =
            self.organization.workspaces.iter().any(|v| {
                v.id.local() > marks.workspace || v.creation_ordinal.get() > marks.creation_ordinal
            }) || self.organization.tabs.iter().any(|v| {
                v.id.local() > marks.tab || v.creation_ordinal.get() > marks.creation_ordinal
            }) || self.organization.panes.iter().any(|v| {
                v.id.local() > marks.pane || v.creation_ordinal.get() > marks.creation_ordinal
            }) || self
                .organization
                .worktrees
                .iter()
                .any(|v| v.id.local() > marks.worktree)
                || self
                    .organization
                    .repositories
                    .iter()
                    .any(|v| v.id.local() > marks.repository);
        if too_high {
            return Err(StoreFailure::InvalidDurableState);
        }
        let mut streams = BTreeMap::new();
        let mut stream_keys = BTreeSet::new();
        for stream in &self.streams {
            validate_stream(stream, server)?;
            if !stream_fits_marks(stream.id, marks)
                || !stream_keys.insert(stream.id.key())
                || streams.insert(stream.id, stream).is_some()
            {
                return Err(StoreFailure::InvalidDurableState);
            }
        }
        let mut seen = BTreeSet::new();
        let mut counts = BTreeMap::<OperationStreamId, u64>::new();
        for record in &self.operations {
            validate_operation(record, server, self.organization.revision, marks)?;
            let id = record.binding.id;
            let stream = streams
                .get(&id.stream())
                .ok_or(StoreFailure::InvalidDurableState)?;
            if id.sequence().get() <= stream.acknowledged_through
                || id.sequence().get() > stream.admitted_through
                || !seen.insert(id)
            {
                return Err(StoreFailure::InvalidDurableState);
            }
            let count = counts.entry(id.stream()).or_default();
            *count = count
                .checked_add(1)
                .ok_or(StoreFailure::InvalidDurableState)?;
        }
        // Unique, in-range sequences with the exact cardinality cannot have gaps.
        for stream in streams.values() {
            if counts.get(&stream.id).copied().unwrap_or(0)
                != stream.admitted_through - stream.acknowledged_through
            {
                return Err(StoreFailure::InvalidDurableState);
            }
        }
        Ok(())
    }
}
fn validate_stream(stream: &DurableStreamState, server: ServerId) -> Result<(), StoreFailure> {
    if stream.id.server() != server || stream.acknowledged_through > stream.admitted_through {
        Err(StoreFailure::InvalidDurableState)
    } else {
        Ok(())
    }
}
fn validate_operation(
    record: &DurableOperationRecord,
    server: ServerId,
    revision: OrganizationRevision,
    marks: IdentityHighWater,
) -> Result<(), StoreFailure> {
    if record.binding.id.stream().server() != server
        || !stream_fits_marks(record.binding.id.stream(), marks)
    {
        return Err(StoreFailure::InvalidDurableState);
    }
    match &record.state {
        DurableOperationState::Pending { command } => {
            let request = OperationRequest::new(record.binding.id, command.clone())
                .map_err(|_| StoreFailure::InvalidDurableState)?;
            request
                .verify_binding(record.binding)
                .map_err(|_| StoreFailure::InvalidDurableState)
        }
        DurableOperationState::Completed {
            result,
            publication_revision,
        } => {
            if *publication_revision > revision || !outcome_fits_marks(result, marks) {
                return Err(StoreFailure::InvalidDurableState);
            }
            result
                .validate_scope(record.binding.id.stream())
                .map_err(|_| StoreFailure::InvalidDurableState)
        }
    }
}

fn stream_fits_marks(stream: OperationStreamId, marks: IdentityHighWater) -> bool {
    stream.key().local() <= marks.operation_stream
        && stream.pairing().local() <= marks.pairing
        && stream.client().local() <= marks.client
}

fn write_fits_marks(write: &OrganizationWrite, marks: IdentityHighWater) -> bool {
    match write {
        OrganizationWrite::PutWorkspace(v) => {
            v.id.local() <= marks.workspace
                && v.root_tab.local() <= marks.tab
                && v.creation_ordinal.get() <= marks.creation_ordinal
        }
        OrganizationWrite::DeleteWorkspace(v) => v.local() <= marks.workspace,
        OrganizationWrite::PutTab(v) => {
            v.id.local() <= marks.tab
                && v.workspace.local() <= marks.workspace
                && v.root_pane.local() <= marks.pane
                && v.worktree.is_none_or(|id| id.local() <= marks.worktree)
                && v.creation_ordinal.get() <= marks.creation_ordinal
        }
        OrganizationWrite::DeleteTab(v) => v.local() <= marks.tab,
        OrganizationWrite::PutPane(v) => {
            v.id.local() <= marks.pane
                && v.tab.local() <= marks.tab
                && v.creation_ordinal.get() <= marks.creation_ordinal
                && !matches!(v.creation, PaneCreation::SplitOf { source, .. } if source.local() > marks.pane)
        }
        OrganizationWrite::DeletePane(v) => v.local() <= marks.pane,
        OrganizationWrite::PutRepository(v) => v.id.local() <= marks.repository,
        OrganizationWrite::DeleteRepository(v) => v.local() <= marks.repository,
        OrganizationWrite::PutWorktree(v) => {
            v.id.local() <= marks.worktree && v.repository.local() <= marks.repository
        }
        OrganizationWrite::DeleteWorktree(v) => v.local() <= marks.worktree,
        OrganizationWrite::SetServerSettings(_) => true,
    }
}

fn outcome_fits_marks(outcome: &Outcome, marks: IdentityHighWater) -> bool {
    match outcome {
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
        Outcome::ControlAcquired(grant) => {
            grant.pane().local() <= marks.pane && grant.client().local() <= marks.client
        }
        Outcome::ControlUnavailable(ControlRecord::Held { client, .. }) => {
            client.local() <= marks.client
        }
        Outcome::StartupAwaitingMeasurement { intent } => stream_fits_marks(intent.stream(), marks),
        Outcome::SessionStarted(session) => session.pane.local() <= marks.pane,
        Outcome::Assessed { assessment, .. } => {
            stream_fits_marks(assessment.operation.stream(), marks)
        }
        Outcome::Conflict(
            FieldConflict::WorkspaceName { workspace, .. }
            | FieldConflict::PinnedDirectory { workspace, .. },
        ) => workspace.local() <= marks.workspace,
        Outcome::Conflict(FieldConflict::PaneDirectory { pane, .. }) => pane.local() <= marks.pane,
        Outcome::Applied
        | Outcome::Failed(_)
        | Outcome::ControlUnavailable(ControlRecord::Unclaimed)
        | Outcome::Conflict(FieldConflict::WorktreeLocation { .. }) => true,
    }
}
