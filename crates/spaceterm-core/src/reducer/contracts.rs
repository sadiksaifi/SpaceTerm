//! Adapters for the frozen shared declarations. No competing stamps, defaults,
//! grant provenance, or retirement projections are defined here.

pub use crate::authority::CloseWorkGeneration;
use crate::authority::*;
use crate::identity::*;
pub use crate::observations::{
    CloseWorkObservation, CloseWorkState, ConfirmedDirectoryObservation, DirectoryPresentation,
    DisplayDirectoryObservation, MonotonicTick as ReducerTick, RuntimeObservationSequence,
};
use crate::operations::*;
use crate::organization::*;
use crate::organization_retained::*;
use crate::retirement::*;
use crate::storage::{OperationWrite, TransactionParts};
use crate::worktrees::intent::{GitAllocationReservation, GitIntentPhase};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationPrincipal {
    pub pairing: PairingId,
    pub client: ClientId,
}

#[derive(Clone, Debug)]
pub struct ReductionContext {
    pub principal: MutationPrincipal,
    pub operation: OperationId,
    pub request_digest: RequestDigest,
    pub now: ReducerTick,
    pub facts: ReductionFacts,
}
impl ReductionContext {
    pub fn binding(&self) -> OperationBinding {
        OperationBinding {
            id: self.operation,
            digest: self.request_digest,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ReductionFacts {
    pub directories: Option<DirectoryValidation>,
    pub retirement_outcome: Option<AppliedRetirementOutcome>,
}

/// The actor supplies the ledger owner's encoding before accepting external
/// retirement. No default codec or shape-only bytes become completion authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedRetirementOutcome(crate::operation_ledger_contracts::CompactOutcome);
impl AppliedRetirementOutcome {
    pub fn new(
        codec: &dyn crate::operation_ledger_contracts::LedgerCodec,
    ) -> Result<Self, crate::operation_ledger_contracts::LedgerFailure> {
        crate::operation_ledger_contracts::CompactOutcome::new(&Outcome::Applied, codec).map(Self)
    }
    pub fn compact(&self) -> &crate::operation_ledger_contracts::CompactOutcome {
        &self.0
    }
}

/// Relevant state only. Unrelated organization edits do not stale these facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryBasis {
    pub workspace: Option<WorkspaceId>,
    pub pin: Option<Versioned<Option<ServerDirectory>>>,
    pub source: Option<SourceDirectoryBasis>,
    pub worktree: Option<WorktreeDirectoryBasis>,
    pub requested: Option<ServerDirectory>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceDirectoryBasis {
    pub pane: PaneId,
    pub tab: TabId,
    pub generation: SessionGeneration,
    pub directory_version: FieldVersion,
    pub observation: Option<RuntimeObservationSequence>,
    pub directory: ServerDirectory,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeDirectoryBasis {
    pub worktree: WorktreeRecord,
    pub inventory: WorktreeInventoryFacts,
    pub repository_version: RepositoryInventoryVersion,
    pub membership_version: Option<TabMembershipVersion>,
}

/// Content-free validation result plus the exact value checked by the native
/// adapter. Physical containment is an adapter fact, never a lexical prefix test.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryFact {
    pub directory: ServerDirectory,
    pub available: bool,
    pub physically_inside: Option<WorktreeId>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryValidation {
    pub binding: OperationBinding,
    pub basis: DirectoryBasis,
    pub home: DirectoryFact,
    pub pin: Option<DirectoryFact>,
    pub source: Option<DirectoryFact>,
    pub requested: Option<DirectoryFact>,
    pub worktree: Option<DirectoryFact>,
    pub worktree_bare: bool,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DurableInvariant {
    #[error("shared organization graph is invalid")]
    Graph,
    #[error("required retained comparison facts are absent")]
    RetainedFacts,
    #[error("identity high-water marks do not cover records")]
    HighWater,
    #[error("field version is invalid")]
    FieldVersion,
    #[error("retained Worktree Location grammar is invalid")]
    WorktreeLocation,
    #[error("retained automatic names collide")]
    Names,
    #[error("recovered retirement comparison is invalid")]
    Retirement,
    #[error("recovered allocation reservation is invalid")]
    Reservation,
    #[error("organization exceeds configured limits")]
    Limits,
}
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ReducerFailure {
    #[error("request belongs to another Server or principal")]
    WrongServer,
    #[error("target no longer exists")]
    TargetNotFound,
    #[error("target membership is invalid")]
    WrongMembership,
    #[error("internal validation facts are invalid")]
    InvalidFacts,
    #[error("organization quota is exhausted")]
    QuotaExceeded,
    #[error("comparison counter is exhausted")]
    CounterExhausted,
    #[error("invalid durable organization: {0}")]
    InvalidDurableState(DurableInvariant),
    #[error("validated recovery is required")]
    RecoveryRequired,
    #[error("prepared reduction is stale")]
    StalePreparation,
    #[error("commit receipt does not match the proposal")]
    InvalidCommitReceipt,
    #[error("operation belongs to another policy owner")]
    WrongReducer,
}

#[derive(Clone, Debug)]
pub enum OrganizationEvent {
    DisplayDirectoryObserved(DisplayDirectoryObservation),
    RestorationDirectoryConfirmed(ConfirmedDirectoryObservation),
    CloseWorkObserved(CloseWorkObservation),
    StartupReserved(StartupReservation),
    RuntimeStarted {
        session: SessionKey,
    },
    StartupCancelled {
        session: SessionKey,
    },
    OrdinaryExit {
        session: SessionKey,
        class: OrdinaryExitClass,
    },
    RuntimeFailed {
        session: SessionKey,
        class: FailureClass,
    },
    DirectoryUnavailable {
        pane: PaneId,
        expected_generation: SessionGeneration,
    },
    ControlChanged {
        pane: PaneId,
        expected: ControlRecord,
        replacement: ControlRecord,
    },
    RetirementCleanup {
        batch: RetirementBatchId,
        session: SessionKey,
        result: CleanupResult,
    },
    WorktreeOrganizationChanged(Box<WorktreeOrganizationPatch>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryExitClass {
    Success,
    Nonzero,
    Signal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrganizationEffect {
    RegisterCreatedPane {
        pane: PaneId,
        initial_grant: ControlGrant,
    },
    InvalidatePaneRuntime {
        pane: PaneId,
        generation: SessionGeneration,
        clear_content: bool,
        clear_approvals: bool,
    },
    CancelStartupIntents {
        pane: PaneId,
    },
    RetireSession {
        batch: RetirementBatchId,
        session: SessionKey,
        reason: RetirementReason,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetirementPlan {
    pub descriptor: RetirementDescriptor,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolution {
    Completed(Outcome),
    AwaitingRetirement(Box<RetirementPlan>),
    /// Cleanup progress for every overlapping batch. The actor merges all
    /// operation descriptors into their original pending ledger entries.
    RetirementProgress {
        operations: Vec<RetirementDescriptor>,
        native: Vec<NativeRetirementDescriptor>,
    },
    /// Final ledger completion uses this descriptor's compact result and original
    /// acceptance revision, not the current Organization Revision.
    RetirementCompleted(Box<RetirementPlan>),
    /// Git failed or reconciled without membership insertion. The Git owner
    /// supplies the final intent/result projection in the same commit.
    GitRetirementReleased(Box<RetirementPlan>),
    AwaitingNativeRetirement(Box<NativeRetirementDescriptor>),
    NativeRetirementCompleted(Box<NativeRetirementDescriptor>),
    AllocationReserved(Box<GitAllocationReservation>),
    AllocationAbandoned(OperationBinding),
}

/// Immutable typed projection. The caller merges this with real ledger writes
/// before the shared DurabilityValidator checks the complete transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrganizationWriteSet {
    pub(crate) server: ServerId,
    pub(crate) expected_store: StoreRevision,
    pub(crate) base: OrganizationRevision,
    pub(crate) revision: OrganizationRevision,
    pub(crate) epoch: ServerEpoch,
    pub(crate) expected_marks: IdentityHighWater,
    pub(crate) resulting_marks: IdentityHighWater,
    pub(crate) organization: Vec<OrganizationWrite>,
    pub(crate) operations: Vec<OperationWrite>,
}
impl OrganizationWriteSet {
    pub fn organization_writes(&self) -> &[OrganizationWrite] {
        &self.organization
    }
    pub fn operation_writes(&self) -> &[OperationWrite] {
        &self.operations
    }
    pub fn expected_high_water(&self) -> IdentityHighWater {
        self.expected_marks
    }
    pub fn resulting_high_water(&self) -> IdentityHighWater {
        self.resulting_marks
    }
    pub fn append_to(&self, parts: &mut TransactionParts) -> Result<(), ReducerFailure> {
        if parts.server != self.server
            || parts.expected_store_revision != self.expected_store
            || self.expected_store.checked_successor() != Some(parts.resulting_store_revision)
            || parts.expected_organization_revision != self.base
            || parts.resulting_organization_revision != self.revision
            || parts.expected_epoch != self.epoch
            || parts.resulting_epoch != self.epoch
            || parts.expected_high_water != self.expected_marks
            || parts.resulting_high_water != self.resulting_marks
            || !parts.organization_writes.is_empty()
            || parts.operation_writes.iter().any(|write| {
                matches!(
                    write,
                    OperationWrite::ReductionBasis(_) | OperationWrite::GitAllocation(_)
                )
            })
        {
            return Err(ReducerFailure::InvalidFacts);
        }
        parts
            .organization_writes
            .extend_from_slice(&self.organization);
        parts.operation_writes.extend_from_slice(&self.operations);
        Ok(())
    }
}

/// Actor-admitted Git metadata patch. This is not a wire command or an external
/// execution permit. Inventory failures must not invent membership deletions.
#[derive(Clone, Debug)]
pub struct WorktreeOrganizationPatch {
    pub binding: OperationBinding,
    pub scope: RepositoryScope,
    pub expected_inventory_version: Option<FieldVersion>,
    pub repositories: Vec<RepositoryRecord>,
    pub worktrees: Vec<(WorktreeRecord, bool)>,
    pub memberships: Vec<WorktreeMembershipEdit>,
    pub settlement: Option<GitOrganizationSettlement>,
}
#[derive(Clone, Copy, Debug)]
pub struct WorktreeMembershipEdit {
    pub tab: TabId,
    pub expected_version: FieldVersion,
    pub worktree: Option<WorktreeId>,
}
/// A recorded, already-Started intent may finish during a creation barrier. The
/// actor supplies the real durable intent and revocation decision, never the wire.
#[derive(Clone, Debug)]
pub struct GitOrganizationSettlement {
    pub reservation: GitAllocationReservation,
    pub phase: GitIntentPhase,
    pub remove_tabs: Vec<TabRecord>,
    pub creator_still_authorized: bool,
}

/// Owning-server inventory facts, not a Git write intent or execution permit.
/// `None` requests a reducer-issued identity in the insertion transaction; supplied
/// identities must already exist. Omitted Worktrees and their memberships survive.
#[derive(Clone, Debug)]
pub struct GitInventoryPatch {
    pub repository: Option<RepositoryId>,
    pub directory: ServerDirectory,
    pub expected_inventory_version: Option<FieldVersion>,
    pub worktrees: Vec<GitInventoryWorktree>,
}
#[derive(Clone, Debug)]
pub struct GitInventoryWorktree {
    pub id: Option<WorktreeId>,
    pub directory: ServerDirectory,
    pub main: bool,
    pub missing: bool,
    pub listed: bool,
}

/// Allocation request after real ledger admission, before irreversible Git work.
#[derive(Clone, Debug)]
pub struct GitAllocationRequest {
    pub repository: bool,
    pub worktree: bool,
    pub fallbacks: Vec<crate::worktrees::intent::WorkspaceFallbackPlan>,
}

/// Content-free, exact comparison slots for Job B's bounded assessment registry.
/// Capturing and revalidating these records is assessment policy, not constructor authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceAssessmentBasis {
    pub workspace: WorkspaceId,
    pub root_tab: TabId,
    pub tabs: Vec<TabId>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneAssessmentBasis {
    pub pane: PaneId,
    pub tab: TabId,
    pub runtime: PaneRuntimeRecord,
    pub close_work: Option<CloseWorkObservation>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssessmentRecord {
    pub reference: AssessmentRef,
    pub principal: MutationPrincipal,
    pub epoch: ServerEpoch,
    pub target: AssessmentTarget,
    pub captured_at: ReducerTick,
    pub expires_at: ReducerTick,
    pub workspaces: Vec<WorkspaceAssessmentBasis>,
    pub tabs: Vec<TabRecord>,
    pub tab_panes: Vec<(TabId, Vec<PaneId>)>,
    pub panes: Vec<PaneAssessmentBasis>,
    pub retiring_panes: Vec<RetirementPane>,
    pub retiring_sessions: Vec<RetiredSession>,
    pub requires_confirmation: bool,
    pub value_bytes: usize,
}
