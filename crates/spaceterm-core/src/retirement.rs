//! Recoverable accepted retirement. The pending ledger entry is the single
//! durable authority. These DTOs never retire a process or prove cleanup.
//! Store and ledger codecs preflight counts before constructing vectors.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::{OrganizationRevision, ServerEpoch, SessionGeneration, SessionKey};
use crate::identity::{PaneId, ServerId, TabId, WorkspaceId, WorktreeId};
use crate::operations::{AssessmentRef, AssessmentTarget, CloseTarget, OperationBinding};
use crate::organization::{FailureClass, RetirementReason};

pub const MAX_RETIREMENT_PANES: usize = 8192;
pub const MAX_RETIREMENT_SESSIONS: usize = 64;
pub const MAX_RETAINED_RETIREMENT_BYTES: usize = 16 * 1024 * 1024;

/// Content-free retirement record failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RetirementRecordError {
    #[error("retirement comparison binding is invalid")]
    InvalidBinding,
    #[error("retirement record exceeds its bound")]
    TooLarge,
    #[error("retirement record contains duplicate targets")]
    Duplicate,
}

/// Checked server/runtime-qualified batch comparison identity. It is not a
/// durable organization allocator and never grants process authority.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "RetirementBatchParts", into = "RetirementBatchParts")]
pub struct RetirementBatchId {
    server: ServerId,
    epoch: ServerEpoch,
    sequence: NonZeroU64,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct RetirementBatchParts {
    server: ServerId,
    epoch: ServerEpoch,
    sequence: u64,
}

impl RetirementBatchId {
    pub fn new(
        server: ServerId,
        epoch: ServerEpoch,
        sequence: u64,
    ) -> Result<Self, RetirementRecordError> {
        if epoch.get() == 0 {
            return Err(RetirementRecordError::InvalidBinding);
        }
        let sequence = NonZeroU64::new(sequence).ok_or(RetirementRecordError::InvalidBinding)?;
        Ok(Self {
            server,
            epoch,
            sequence,
        })
    }
    pub const fn server(self) -> ServerId {
        self.server
    }
    pub const fn epoch(self) -> ServerEpoch {
        self.epoch
    }
    pub const fn sequence(self) -> u64 {
        self.sequence.get()
    }
    pub fn checked_successor(self) -> Option<Self> {
        self.sequence
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(|sequence| Self { sequence, ..self })
    }
}
impl TryFrom<RetirementBatchParts> for RetirementBatchId {
    type Error = RetirementRecordError;
    fn try_from(parts: RetirementBatchParts) -> Result<Self, Self::Error> {
        Self::new(parts.server, parts.epoch, parts.sequence)
    }
}
impl From<RetirementBatchId> for RetirementBatchParts {
    fn from(value: RetirementBatchId) -> Self {
        Self {
            server: value.server,
            epoch: value.epoch,
            sequence: value.sequence.get(),
        }
    }
}

/// Confirmed cleanup classifications require process-session ownership, joined
/// readers/termination workers, and released content/approval resources. Absence
/// of handles after restart is not AlreadyAbsentWithConfirmedOwnership.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ConfirmedCleanupClass {
    ReapedAndReleased,
    AlreadyAbsentWithConfirmedOwnership,
}

/// A timeout, dropped handle, or unavailable cleanup is a failure, never success.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CleanupResult {
    Confirmed(ConfirmedCleanupClass),
    Failed(FailureClass),
}

/// Bounded durable cleanup progress for one exact old Session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RetirementCleanupState {
    AwaitingEvidence,
    Confirmed(ConfirmedCleanupClass),
    Failed(FailureClass),
}

/// The owner's first retirement reason and one cleanup result. Overlapping
/// accepted batches await the same Session evidence without retagging the owner.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "RetiredSessionParts", into = "RetiredSessionParts")]
pub struct RetiredSession {
    session: SessionKey,
    first_reason: RetirementReason,
    cleanup: RetirementCleanupState,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
struct RetiredSessionParts {
    session: SessionKey,
    first_reason: RetirementReason,
    cleanup: RetirementCleanupState,
}
impl RetiredSession {
    pub fn new(
        session: SessionKey,
        first_reason: RetirementReason,
        cleanup: RetirementCleanupState,
    ) -> Result<Self, RetirementRecordError> {
        if session.epoch.get() == 0 || session.generation.get() == 0 {
            return Err(RetirementRecordError::InvalidBinding);
        }
        Ok(Self {
            session,
            first_reason,
            cleanup,
        })
    }
    pub const fn session(self) -> SessionKey {
        self.session
    }
    pub const fn first_reason(self) -> RetirementReason {
        self.first_reason
    }
    pub const fn cleanup(self) -> RetirementCleanupState {
        self.cleanup
    }
}
impl TryFrom<RetiredSessionParts> for RetiredSession {
    type Error = RetirementRecordError;
    fn try_from(parts: RetiredSessionParts) -> Result<Self, Self::Error> {
        Self::new(parts.session, parts.first_reason, parts.cleanup)
    }
}
impl From<RetiredSession> for RetiredSessionParts {
    fn from(value: RetiredSession) -> Self {
        Self {
            session: value.session,
            first_reason: value.first_reason,
            cleanup: value.cleanup,
        }
    }
}

/// Accepted hierarchy comparison and invalidation generation. These identities
/// may already have been deleted. They are not a duplicate live organization.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "RetirementPaneParts", into = "RetirementPaneParts")]
pub struct RetirementPane {
    workspace: WorkspaceId,
    tab: TabId,
    pane: PaneId,
    worktree: Option<WorktreeId>,
    invalidation_generation: SessionGeneration,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
struct RetirementPaneParts {
    workspace: WorkspaceId,
    tab: TabId,
    pane: PaneId,
    worktree: Option<WorktreeId>,
    invalidation_generation: SessionGeneration,
}
impl RetirementPane {
    pub fn new(
        workspace: WorkspaceId,
        tab: TabId,
        pane: PaneId,
        worktree: Option<WorktreeId>,
        invalidation_generation: SessionGeneration,
    ) -> Result<Self, RetirementRecordError> {
        let server = pane.server();
        if workspace.server() != server
            || tab.server() != server
            || worktree.is_some_and(|id| id.server() != server)
        {
            return Err(RetirementRecordError::InvalidBinding);
        }
        Ok(Self {
            workspace,
            tab,
            pane,
            worktree,
            invalidation_generation,
        })
    }
    pub const fn workspace(self) -> WorkspaceId {
        self.workspace
    }
    pub const fn tab(self) -> TabId {
        self.tab
    }
    pub const fn pane(self) -> PaneId {
        self.pane
    }
    pub const fn worktree(self) -> Option<WorktreeId> {
        self.worktree
    }
    pub const fn invalidation_generation(self) -> SessionGeneration {
        self.invalidation_generation
    }
}
impl TryFrom<RetirementPaneParts> for RetirementPane {
    type Error = RetirementRecordError;
    fn try_from(parts: RetirementPaneParts) -> Result<Self, Self::Error> {
        Self::new(
            parts.workspace,
            parts.tab,
            parts.pane,
            parts.worktree,
            parts.invalidation_generation,
        )
    }
}
impl From<RetirementPane> for RetirementPaneParts {
    fn from(value: RetirementPane) -> Self {
        Self {
            workspace: value.workspace,
            tab: value.tab,
            pane: value.pane,
            worktree: value.worktree,
            invalidation_generation: value.invalidation_generation,
        }
    }
}

/// Compact eventual organization result. Git cleanup alone cannot complete its
/// operation or choose the final membership transaction's outcome revision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RetirementCompletion {
    AppliedAtAcceptanceRevision,
    ContinueGitSettlement,
}

/// Bounded codec input for a pending ledger retirement descriptor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetirementDescriptorParts {
    pub batch: RetirementBatchId,
    pub binding: OperationBinding,
    pub assessment: AssessmentRef,
    pub acceptance_revision: OrganizationRevision,
    pub target: AssessmentTarget,
    pub panes: Vec<RetirementPane>,
    pub sessions: Vec<RetiredSession>,
    pub completion: RetirementCompletion,
    pub eventual_result: crate::operation_ledger_contracts::CompactOutcome,
}

/// Accepted request identity/digest, original publication revision, exact old
/// Sessions, reason, barrier, cleanup progress, and compact completion policy.
/// The pending ledger entry alone persists this descriptor. Reducer recovery
/// receives a validated projection of it, not a separately stored operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "RetirementDescriptorParts",
    into = "RetirementDescriptorParts"
)]
pub struct RetirementDescriptor(RetirementDescriptorParts);

impl RetirementDescriptor {
    pub fn new(parts: RetirementDescriptorParts) -> Result<Self, RetirementRecordError> {
        let server = parts.batch.server();
        if parts.panes.len() > MAX_RETIREMENT_PANES
            || parts.sessions.len() > MAX_RETIREMENT_SESSIONS
        {
            return Err(RetirementRecordError::TooLarge);
        }
        if parts.binding.id.stream().server() != server
            || parts.assessment.operation.stream() != parts.binding.id.stream()
            || parts.target.validate_scope(server).is_err()
            || matches!(parts.target, AssessmentTarget::RemoveWorktree { .. })
                != matches!(
                    parts.completion,
                    RetirementCompletion::ContinueGitSettlement
                )
        {
            return Err(RetirementRecordError::InvalidBinding);
        }
        let mut panes = BTreeMap::new();
        let mut previous_pane = None;
        for pane in &parts.panes {
            if pane.pane().server() != server {
                return Err(RetirementRecordError::InvalidBinding);
            }
            if previous_pane.is_some_and(|id| id >= pane.pane()) {
                return Err(RetirementRecordError::Duplicate);
            }
            previous_pane = Some(pane.pane());
            let target_matches = match parts.target {
                AssessmentTarget::Close(CloseTarget::Pane(id)) => pane.pane() == id,
                AssessmentTarget::Close(CloseTarget::Tab(id)) => pane.tab() == id,
                AssessmentTarget::Close(CloseTarget::Workspace(id))
                | AssessmentTarget::KillWorkspace(id) => pane.workspace() == id,
                AssessmentTarget::Close(CloseTarget::WorktreeTabs {
                    workspace,
                    worktree,
                }) => pane.workspace() == workspace && pane.worktree() == Some(worktree),
                AssessmentTarget::EndAllSessions(_) => true,
                // Git's exact intent also includes directory-contained work whose
                // saved tag differs. The reducer compares that complete hierarchy.
                AssessmentTarget::RemoveWorktree { .. } => true,
            };
            if !target_matches || panes.insert(pane.pane(), *pane).is_some() {
                return Err(RetirementRecordError::InvalidBinding);
            }
        }
        let mut seen_sessions = BTreeSet::new();
        let mut previous_session = None;
        for session in &parts.sessions {
            let key = session.session();
            if key.epoch != parts.batch.epoch()
                || previous_session.is_some_and(|previous| previous >= key)
                || !seen_sessions.insert(key.pane)
            {
                return Err(RetirementRecordError::InvalidBinding);
            }
            previous_session = Some(key);
            let pane = panes
                .get(&key.pane)
                .ok_or(RetirementRecordError::InvalidBinding)?;
            if key.generation >= pane.invalidation_generation() {
                return Err(RetirementRecordError::InvalidBinding);
            }
        }
        let value = Self(parts);
        value.value_bytes()?;
        Ok(value)
    }

    pub fn parts(&self) -> &RetirementDescriptorParts {
        &self.0
    }
    pub const fn batch(&self) -> RetirementBatchId {
        self.0.batch
    }
    pub const fn binding(&self) -> OperationBinding {
        self.0.binding
    }
    pub const fn assessment(&self) -> AssessmentRef {
        self.0.assessment
    }
    pub const fn acceptance_revision(&self) -> OrganizationRevision {
        self.0.acceptance_revision
    }
    pub const fn target(&self) -> AssessmentTarget {
        self.0.target
    }
    pub fn panes(&self) -> &[RetirementPane] {
        &self.0.panes
    }
    pub fn sessions(&self) -> &[RetiredSession] {
        &self.0.sessions
    }
    pub const fn completion(&self) -> RetirementCompletion {
        self.0.completion
    }

    pub const fn reason(&self) -> RetirementReason {
        match self.0.target {
            AssessmentTarget::Close(_) => RetirementReason::ConfirmedClose,
            AssessmentTarget::KillWorkspace(_) => RetirementReason::KillWorkspace,
            AssessmentTarget::EndAllSessions(_) => RetirementReason::EndAllSessions,
            AssessmentTarget::RemoveWorktree { .. } => RetirementReason::WorktreeRemoval,
        }
    }

    pub const fn server_wide_barrier(&self) -> bool {
        matches!(self.0.target, AssessmentTarget::EndAllSessions(_))
    }

    /// Per-descriptor charge; reducer policy also bounds the aggregate at 16 MiB
    /// and the number of concurrent accepted batches at 64.
    pub fn value_bytes(&self) -> Result<usize, RetirementRecordError> {
        let panes = self
            .0
            .panes
            .len()
            .checked_mul(112)
            .ok_or(RetirementRecordError::TooLarge)?;
        let sessions = self
            .0
            .sessions
            .len()
            .checked_mul(64)
            .ok_or(RetirementRecordError::TooLarge)?;
        let bytes = (320usize + self.0.eventual_result.encoded_len())
            .checked_add(panes)
            .and_then(|v| v.checked_add(sessions))
            .ok_or(RetirementRecordError::TooLarge)?;
        if bytes > MAX_RETAINED_RETIREMENT_BYTES {
            return Err(RetirementRecordError::TooLarge);
        }
        Ok(bytes)
    }

    /// Comparison predicate only. Composition still validates actual evidence,
    /// applies exact-generation fences, and durably records completion.
    pub fn cleanup_confirmed(&self) -> bool {
        self.0
            .sessions
            .iter()
            .all(|session| matches!(session.cleanup(), RetirementCleanupState::Confirmed(_)))
    }
}
impl TryFrom<RetirementDescriptorParts> for RetirementDescriptor {
    type Error = RetirementRecordError;
    fn try_from(parts: RetirementDescriptorParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<RetirementDescriptor> for RetirementDescriptorParts {
    fn from(value: RetirementDescriptor) -> Self {
        value.0
    }
}

/// Server-internal cleanup report from the exact owner's first retirement.
/// It is deliberately not serializable as a client request or native proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionCleanupObservation {
    session: SessionKey,
    first_reason: RetirementReason,
    result: CleanupResult,
}
impl SessionCleanupObservation {
    pub fn new(
        session: SessionKey,
        first_reason: RetirementReason,
        result: CleanupResult,
    ) -> Result<Self, RetirementRecordError> {
        if session.epoch.get() == 0 || session.generation.get() == 0 {
            return Err(RetirementRecordError::InvalidBinding);
        }
        Ok(Self {
            session,
            first_reason,
            result,
        })
    }
    pub const fn session(self) -> SessionKey {
        self.session
    }
    pub const fn first_reason(self) -> RetirementReason {
        self.first_reason
    }
    pub const fn result(self) -> CleanupResult {
        self.result
    }
}

/// Native acceptance has no AssessmentRef, pairing or Client-bound operation.
/// The native provider first durably admits its real management request. The
/// actor commits this descriptor and ended-state publication in OrganizationStore.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeRetirementDescriptorParts {
    pub request: crate::native_management::NativeManagementRequestRef,
    pub batch: RetirementBatchId,
    pub action: crate::native_management::NativeRetirementAction,
    pub acceptance_revision: OrganizationRevision,
    pub panes: Vec<RetirementPane>,
    pub sessions: Vec<RetiredSession>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "NativeRetirementDescriptorParts",
    into = "NativeRetirementDescriptorParts"
)]
pub struct NativeRetirementDescriptor(NativeRetirementDescriptorParts);
impl NativeRetirementDescriptor {
    pub fn new(parts: NativeRetirementDescriptorParts) -> Result<Self, RetirementRecordError> {
        if parts.request.server() != parts.batch.server() {
            return Err(RetirementRecordError::InvalidBinding);
        }
        if parts.panes.len() > MAX_RETIREMENT_PANES
            || parts.sessions.len() > MAX_RETIREMENT_SESSIONS
        {
            return Err(RetirementRecordError::TooLarge);
        }
        let mut panes = BTreeMap::new();
        let mut previous_pane = None;
        for pane in &parts.panes {
            if pane.pane().server() != parts.batch.server()
                || previous_pane.is_some_and(|id| id >= pane.pane())
            {
                return Err(RetirementRecordError::InvalidBinding);
            }
            previous_pane = Some(pane.pane());
            panes.insert(pane.pane(), pane);
        }
        let mut seen_panes = BTreeSet::new();
        let mut previous_session = None;
        for session in &parts.sessions {
            let key = session.session();
            let pane = panes
                .get(&key.pane)
                .ok_or(RetirementRecordError::InvalidBinding)?;
            if key.epoch != parts.batch.epoch()
                || key.generation >= pane.invalidation_generation()
                || !seen_panes.insert(key.pane)
                || previous_session.is_some_and(|previous| previous >= key)
            {
                return Err(RetirementRecordError::InvalidBinding);
            }
            previous_session = Some(key);
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &NativeRetirementDescriptorParts {
        &self.0
    }
    pub const fn request(&self) -> crate::native_management::NativeManagementRequestRef {
        self.0.request
    }
    pub const fn batch(&self) -> RetirementBatchId {
        self.0.batch
    }
    pub const fn reason(&self) -> RetirementReason {
        self.0.action.reason()
    }
    pub const fn acceptance_revision(&self) -> OrganizationRevision {
        self.0.acceptance_revision
    }
    pub fn panes(&self) -> &[RetirementPane] {
        &self.0.panes
    }
    pub fn sessions(&self) -> &[RetiredSession] {
        &self.0.sessions
    }
    pub const fn server_wide_barrier(&self) -> bool {
        true
    }
    pub fn cleanup_confirmed(&self) -> bool {
        self.0
            .sessions
            .iter()
            .all(|session| matches!(session.cleanup(), RetirementCleanupState::Confirmed(_)))
    }
}
impl TryFrom<NativeRetirementDescriptorParts> for NativeRetirementDescriptor {
    type Error = RetirementRecordError;
    fn try_from(parts: NativeRetirementDescriptorParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<NativeRetirementDescriptor> for NativeRetirementDescriptorParts {
    fn from(value: NativeRetirementDescriptor) -> Self {
        value.0
    }
}
/// CAS of the original native request/digest and batch. Only complete borrowed
/// validation can admit these writes. Completion requires trusted cleanup; the
/// native management journal keeps a reference, not a duplicate descriptor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NativeRetirementMutation {
    Accept(Box<NativeRetirementDescriptor>),
    Progress {
        expected: Box<NativeRetirementDescriptor>,
        next: Box<NativeRetirementDescriptor>,
    },
    Complete {
        request: crate::native_management::NativeManagementRequestRef,
        batch: RetirementBatchId,
    },
}
/// Borrowed owner request. Every batch waits on the owner's first exact Session
/// cleanup evidence. A second batch never issues another process effect.
#[derive(Clone, Copy, Debug)]
pub enum RetirementRequest<'a> {
    Operation(&'a RetirementDescriptor),
    Native(&'a NativeRetirementDescriptor),
}
impl<'a> RetirementRequest<'a> {
    pub fn batch(self) -> RetirementBatchId {
        match self {
            Self::Operation(v) => v.batch(),
            Self::Native(v) => v.batch(),
        }
    }
    pub fn reason(self) -> RetirementReason {
        match self {
            Self::Operation(v) => v.reason(),
            Self::Native(v) => v.reason(),
        }
    }
    pub fn sessions(self) -> &'a [RetiredSession] {
        match self {
            Self::Operation(v) => v.sessions(),
            Self::Native(v) => v.sessions(),
        }
    }
}

pub const MAX_NATIVE_RETIREMENTS: usize = 64;
impl NativeRetirementMutation {
    /// Structural comparison only. DurabilityValidator additionally checks
    /// committed previous state, native-provider admission and cleanup evidence.
    pub fn validate(&self) -> Result<(), RetirementRecordError> {
        match self {
            Self::Accept(_) => Ok(()),
            Self::Complete { request, batch } if request.server() == batch.server() => Ok(()),
            Self::Complete { .. } => Err(RetirementRecordError::InvalidBinding),
            Self::Progress { expected, next } => {
                let a = expected.parts();
                let b = next.parts();
                if a.request != b.request
                    || a.batch != b.batch
                    || a.action != b.action
                    || a.acceptance_revision != b.acceptance_revision
                    || a.panes != b.panes
                    || a.sessions.len() != b.sessions.len()
                    || a.sessions.iter().zip(&b.sessions).any(|(old, new)| {
                        old.session() != new.session()
                            || old.first_reason() != new.first_reason()
                            || (old.cleanup() != RetirementCleanupState::AwaitingEvidence
                                && old.cleanup() != new.cleanup())
                    })
                {
                    return Err(RetirementRecordError::InvalidBinding);
                }
                Ok(())
            }
        }
    }
    pub fn request(&self) -> crate::native_management::NativeManagementRequestRef {
        match self {
            Self::Accept(value) => value.request(),
            Self::Progress { next, .. } => next.request(),
            Self::Complete { request, .. } => *request,
        }
    }
}
