//! Atomic durability boundary. SQL, native file authority and synchronization
//! belong to the host; organization and ledger policy belong to core reducers.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::*;
use crate::identity::*;
use crate::operation_ledger_contracts::{LedgerRecovery, LedgerWrite, OperationReductionBasis};
use crate::operations::*;
use crate::organization::*;
use crate::pairing::{PairingRecovery, PairingWrite};
use crate::worktrees::intent::{
    GitAllocationMutation, GitAllocationReservation, GitIntentMutation, GitRepositoryMutation,
    GitWriteIntent, RepositoryLocator,
};

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
    #[error("proposed storage transaction is invalid")]
    InvalidTransaction,
    #[error("storage identity comparison failed")]
    IdentityMismatch,
    #[error("storage recovery is required")]
    RecoveryRequired,
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
    Ledger(Box<LedgerWrite>),
    Pairing(Box<PairingWrite>),
    GitRepository(GitRepositoryMutation),
    GitIntent(GitIntentMutation),
    GitAllocation(GitAllocationMutation),
    /// Same-operation reducer projection, including a no-change conflict.
    ReductionBasis(OperationReductionBasis),
    NativeRetirement(crate::retirement::NativeRetirementMutation),
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
        let value = Self::check_shape(parts)?;
        if value.requires_domain_validation() {
            return Err(StoreFailure::RecoveryRequired);
        }
        Ok(value)
    }
    /// Constructs a Wave 2 proposal only after complete borrowed validation.
    /// The physical store still compares the proposal against committed state.
    pub fn new_with_validation(
        parts: TransactionParts,
        validator: &dyn DurabilityValidator,
    ) -> Result<Self, StoreFailure> {
        let value = Self::check_shape(parts)?;
        validator.validate_transaction_projection(value.parts())?;
        Ok(value)
    }
    fn requires_domain_validation(&self) -> bool {
        self.0.operation_writes.iter().any(|write| match write {
            OperationWrite::Put(record) => legacy_record_requires_domain_validation(record),
            OperationWrite::SetStream(_) | OperationWrite::RetireAcknowledgedPrefix { .. } => false,
            _ => true,
        }) || self
            .0
            .organization_writes
            .iter()
            .any(|write| matches!(write, OrganizationWrite::Retained(_)))
    }
    fn check_shape(parts: TransactionParts) -> Result<Self, StoreFailure> {
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
        if parts
            .operation_writes
            .iter()
            .any(|write| matches!(write, OperationWrite::Ledger(_)))
            && parts.operation_writes.iter().any(|write| {
                matches!(
                    write,
                    OperationWrite::Put(_)
                        | OperationWrite::SetStream(_)
                        | OperationWrite::RetireAcknowledgedPrefix { .. }
                )
            })
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
                // Owner validators check these complete typed projections.
                OperationWrite::Ledger(_)
                | OperationWrite::Pairing(_)
                | OperationWrite::GitRepository(_)
                | OperationWrite::GitIntent(_)
                | OperationWrite::GitAllocation(_)
                | OperationWrite::ReductionBasis(_) => {}
                OperationWrite::NativeRetirement(write) => {
                    if write.request().server() != parts.server || write.validate().is_err() {
                        return Err(StoreFailure::InvalidDurableState);
                    }
                }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<LedgerRecovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing: Option<PairingRecovery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub git_repositories: Vec<RepositoryLocator>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub git_intents: Vec<GitWriteIntent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub git_allocations: Vec<GitAllocationReservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_retirements: Vec<crate::retirement::NativeRetirementDescriptor>,
}
impl RecoveredState {
    /// Legacy validation never silently accepts a new-format durable bundle.
    pub fn validate(&self) -> Result<(), StoreFailure> {
        self.validate_shape()?;
        if self.requires_domain_validation() {
            return Err(StoreFailure::RecoveryRequired);
        }
        Ok(())
    }
    /// Org-store calls this boundary before exposing new-format recovery.
    pub fn validate_with(&self, validator: &dyn DurabilityValidator) -> Result<(), StoreFailure> {
        self.validate_shape()?;
        validator.validate_recovery(self)
    }
    pub fn ledger_recovery(&self) -> Option<&LedgerRecovery> {
        self.ledger.as_ref()
    }
    pub fn pairing_recovery(&self) -> Option<&PairingRecovery> {
        self.pairing.as_ref()
    }
    pub fn git_repositories(&self) -> &[RepositoryLocator] {
        &self.git_repositories
    }
    pub fn git_intents(&self) -> &[GitWriteIntent] {
        &self.git_intents
    }
    pub fn git_allocations(&self) -> &[GitAllocationReservation] {
        &self.git_allocations
    }
    fn requires_domain_validation(&self) -> bool {
        self.operations
            .iter()
            .any(legacy_record_requires_domain_validation)
            || self.ledger.is_some()
            || self.pairing.is_some()
            || self.organization.retained.is_some()
            || !self.git_repositories.is_empty()
            || !self.git_intents.is_empty()
            || !self.git_allocations.is_empty()
            || !self.native_retirements.is_empty()
    }
    fn validate_shape(&self) -> Result<(), StoreFailure> {
        self.organization
            .validate()
            .map_err(|_| StoreFailure::InvalidDurableState)?;
        let server = self.organization.server;
        let marks = self.high_water;
        let mut native_requests = std::collections::BTreeSet::new();
        let mut native_batches = std::collections::BTreeSet::new();
        if self.native_retirements.len() > crate::retirement::MAX_NATIVE_RETIREMENTS
            || self.native_retirements.iter().any(|value| {
                value.request().server() != server
                    || value.batch().epoch() > self.last_epoch
                    || value.acceptance_revision() > self.organization.revision
                    || !native_requests.insert(value.request().id_bytes())
                    || !native_batches.insert(value.batch())
                    || value.panes().iter().any(|pane| {
                        pane.pane().local() > marks.pane
                            || pane.tab().local() > marks.tab
                            || pane.workspace().local() > marks.workspace
                            || pane
                                .worktree()
                                .is_some_and(|worktree| worktree.local() > marks.worktree)
                    })
            })
        {
            return Err(StoreFailure::InvalidDurableState);
        }

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
        if too_high
            || self
                .organization
                .retained
                .as_ref()
                .is_some_and(|v| !v.fits_high_water(marks))
        {
            return Err(StoreFailure::InvalidDurableState);
        }
        if self.ledger.is_some() && (!self.streams.is_empty() || !self.operations.is_empty()) {
            return Err(StoreFailure::InvalidDurableState);
        }
        if let Some(pairing) = &self.pairing {
            pairing
                .validate_for(server, self.store_revision, marks)
                .map_err(|_| StoreFailure::InvalidDurableState)?;
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
            if matches!(result, Outcome::Git(value) if value.revision().is_some_and(|owned| owned != *publication_revision))
                || *publication_revision > revision
                || !outcome_fits_marks(result, marks)
            {
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
        OrganizationWrite::Retained(value) => value.fits_high_water(marks),
    }
}

fn outcome_fits_marks(outcome: &Outcome, marks: IdentityHighWater) -> bool {
    match outcome {
        Outcome::CreatedWithControl(value) => value.fits_high_water(marks),
        Outcome::ControlGranted(value) => {
            value.grant().pane().local() <= marks.pane
                && value.grant().client().local() <= marks.client
        }
        Outcome::Assessment(value) => {
            stream_fits_marks(value.reference().operation.stream(), marks)
                && value.summary().target().fits_high_water(marks)
        }
        Outcome::Pairing(value) => value.fits_marks(marks),
        Outcome::Git(value) => value
            .worktree()
            .is_none_or(|id| id.local() <= marks.worktree),
        Outcome::Conflict(FieldConflict::RemoteAccess { .. }) => true,
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

/// Complete pure validation supplied by the domain owners. It borrows the one
/// transaction/recovery bundle, decodes compact outcomes with the ledger codec,
/// verifies exact signed receipt bytes, and checks cross-projection agreement.
/// No default implementation can authorize Wave 2 durable state.
pub trait DurabilityValidator: Send + Sync {
    fn validate_transaction_projection(&self, parts: &TransactionParts)
    -> Result<(), StoreFailure>;
    fn validate_recovery(&self, recovered: &RecoveredState) -> Result<(), StoreFailure>;
}

// Legacy retained variants keep their old validation. New domain payloads cannot
// acquire a default-validation bypass by entering the old operation slot.
fn legacy_record_requires_domain_validation(record: &DurableOperationRecord) -> bool {
    match &record.state {
        DurableOperationState::Pending { command } => matches!(
            command,
            Command::CompleteMeasuredStartup(_)
                | Command::AssessKillWorkspace { .. }
                | Command::SetRemoteAccess { .. }
                | Command::Pairing(_)
                | Command::Worktree(_)
                | Command::AssessClose {
                    target: CloseTarget::WorktreeTabs { .. }
                }
                | Command::CloseConfirmed {
                    target: CloseTarget::WorktreeTabs { .. },
                    ..
                }
                | Command::CreateWorkspace {
                    name: NameDraft::Blank,
                    ..
                }
                | Command::RenameWorkspace {
                    draft: NameDraft::Blank,
                    ..
                }
        ),
        DurableOperationState::Completed { result, .. } => matches!(
            result,
            Outcome::CreatedWithControl(_)
                | Outcome::ControlGranted(_)
                | Outcome::Assessment(_)
                | Outcome::Pairing(_)
                | Outcome::Git(_)
                | Outcome::Conflict(FieldConflict::RemoteAccess { .. })
        ),
    }
}
