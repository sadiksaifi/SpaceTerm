//! Shared Git durable facts. Recovery obtains fresh native authority and never replays a write.

use super::types::*;
use crate::authority::{CloseWorkGeneration, CreationOrdinal, FieldVersion, SessionKey};
use crate::identity::*;
use crate::operations::{AssessedPane, OperationBinding, OperationId, RepositoryScope};
use crate::organization::{BranchName, ServerDirectory, TabRecord};
use crate::organization_retained::TabMembershipVersion;
use crate::storage::StoreFailure;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// Locator bytes are comparison data. The host rediscovers retained repository identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryLocatorParts {
    pub scope: RepositoryScope,
    pub main_root: ServerDirectory,
    pub common_directory: ServerDirectory,
}
/// A checked locator for the existing core RepositoryId, not another repository identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "RepositoryLocatorParts", into = "RepositoryLocatorParts")]
pub struct RepositoryLocator(RepositoryLocatorParts);
impl RepositoryLocator {
    pub fn new(parts: RepositoryLocatorParts) -> Result<Self, GitFailure> {
        if parts.scope.validate().is_err()
            || parts.main_root.server() != parts.scope.server
            || parts.common_directory.server() != parts.scope.server
        {
            return Err(GitFailure::WrongServer);
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &RepositoryLocatorParts {
        &self.0
    }
    pub const fn scope(&self) -> RepositoryScope {
        self.0.scope
    }
}
impl TryFrom<RepositoryLocatorParts> for RepositoryLocator {
    type Error = GitFailure;
    fn try_from(parts: RepositoryLocatorParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<RepositoryLocator> for RepositoryLocatorParts {
    fn from(value: RepositoryLocator) -> Self {
        value.0
    }
}

/// Repository auxiliary mutation in the same OrganizationStore transaction.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GitRepositoryMutation {
    Put(RepositoryLocator),
    Delete(RepositoryId),
}
impl GitRepositoryMutation {
    pub fn repository(&self) -> RepositoryId {
        match self {
            Self::Put(v) => v.scope().repository,
            Self::Delete(v) => *v,
        }
    }
}

/// Local versus already-fetched remote-tracking branch operand; never an arbitrary revision expression.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BranchReference {
    Local(BranchName),
    Remote(BranchName),
}
/// Bounded branch choice. Native Git validates every literal operand again before writing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum WorktreeBranchChoice {
    New {
        name: BranchName,
        base: BranchReference,
    },
    Existing {
        name: BranchName,
    },
    Remote {
        remote: BranchName,
        name: BranchName,
    },
}

/// Hash of the complete assessment binding, not terminal or file contents.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitAssessmentDigest([u8; 32]);
impl GitAssessmentDigest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}
impl fmt::Debug for GitAssessmentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GitAssessmentDigest([redacted])")
    }
}

/// Git object-format comparison. Debug omits the object value.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub enum GitObjectId {
    Sha1([u8; 20]),
    Sha256([u8; 32]),
}
impl fmt::Debug for GitObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GitObjectId([redacted])")
    }
}
/// Bounded head facts retained for reconciliation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RetainedGitHead {
    Branch {
        name: BranchName,
        commit: GitObjectId,
    },
    Detached(GitObjectId),
    Unborn(BranchName),
}
/// Destructive categories observed and explicitly confirmed by the server assessment.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ApprovedRemoval {
    Clean,
    DiscardChanges,
    DeleteSubmoduleCheckouts,
    DiscardChangesAndDeleteSubmoduleCheckouts,
    Missing,
}
impl ApprovedRemoval {
    pub const fn force(self) -> bool {
        matches!(
            self,
            Self::DiscardChanges
                | Self::DeleteSubmoduleCheckouts
                | Self::DiscardChangesAndDeleteSubmoduleCheckouts
        )
    }
    pub const fn require_clean(self) -> bool {
        matches!(self, Self::Clean | Self::DeleteSubmoduleCheckouts)
    }
}
/// Comparison category for a create destination. Native identity stays in the assessment cache.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CreationTargetObservation {
    Absent,
    EmptyDirectory,
}
/// Bounded external action. Commands, environment, processes and native proofs are absent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RetainedGitAction {
    Create {
        worktree: WorktreeId,
        directory: ServerDirectory,
        branch: WorktreeBranchChoice,
        target: CreationTargetObservation,
        template_version: Option<FieldVersion>,
    },
    Remove {
        worktree: WorktreeId,
        directory: ServerDirectory,
        head: Option<RetainedGitHead>,
        approval: ApprovedRemoval,
    },
}
impl RetainedGitAction {
    pub fn worktree(&self) -> WorktreeId {
        match self {
            Self::Create { worktree, .. } | Self::Remove { worktree, .. } => *worktree,
        }
    }
    pub fn directory(&self) -> &ServerDirectory {
        match self {
            Self::Create { directory, .. } | Self::Remove { directory, .. } => directory,
        }
    }
    pub fn validate_scope(&self, server: ServerId) -> Result<(), GitFailure> {
        if self.worktree().server() != server || self.directory().server() != server {
            Err(GitFailure::WrongServer)
        } else {
            Ok(())
        }
    }
}

/// Exact content-free work comparison using the reducer/runtime's canonical generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetainedAffectedPane {
    pub hierarchy: AssessedPane,
    pub session: Option<SessionKey>,
    pub startup_reserved: bool,
    pub work_generation: CloseWorkGeneration,
}
/// Fallback policy selects an existing surviving Worktree and starts no Session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceFallbackPlan {
    pub workspace: WorkspaceId,
    pub worktree: WorktreeId,
    pub directory: ServerDirectory,
}
/// Raw bounded-scope input. A complete checked scope is required for a durable intent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetainedRemovalScopeParts {
    pub server: ServerId,
    pub tabs: Vec<TabRecord>,
    /// Empty only for legacy records without comparison stamps. Recovery may read
    /// them, but a fresh assessment must obtain every affected Tab's version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub membership_versions: Vec<TabMembershipVersion>,
    pub panes: Vec<RetainedAffectedPane>,
    pub fallbacks: Vec<WorkspaceFallbackPlan>,
}
/// Exact affected hierarchy, independent of attachments and personal order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "RetainedRemovalScopeParts",
    into = "RetainedRemovalScopeParts"
)]
pub struct RetainedRemovalScope(RetainedRemovalScopeParts);
impl RetainedRemovalScope {
    pub fn new(parts: RetainedRemovalScopeParts) -> Result<Self, GitFailure> {
        if parts.tabs.len() > MAX_GIT_TABS
            || parts.membership_versions.len() > MAX_GIT_TABS
            || parts.panes.len() > MAX_GIT_PANES
            || parts.fallbacks.len() > parts.tabs.len()
        {
            return Err(GitFailure::Capacity);
        }
        let mut tabs = BTreeSet::new();
        let mut panes = BTreeSet::new();
        let mut workspaces = BTreeSet::new();
        for tab in &parts.tabs {
            if tab.id.server() != parts.server
                || tab.workspace.server() != parts.server
                || tab.root_pane.server() != parts.server
                || tab.worktree.is_some_and(|v| v.server() != parts.server)
            {
                return Err(GitFailure::WrongServer);
            }
            if !tabs.insert(tab.id) {
                return Err(GitFailure::InvalidResponse);
            }
            workspaces.insert(tab.workspace);
        }
        let mut memberships = BTreeSet::new();
        for version in &parts.membership_versions {
            if version.id().server() != parts.server {
                return Err(GitFailure::WrongServer);
            }
            if !tabs.contains(&version.id()) || !memberships.insert(version.id()) {
                return Err(GitFailure::InvalidResponse);
            }
        }
        if !memberships.is_empty() && memberships != tabs {
            return Err(GitFailure::InvalidResponse);
        }
        let mut session_epoch = None;
        for pane in &parts.panes {
            let h = pane.hierarchy;
            if h.pane.server() != parts.server
                || h.tab.server() != parts.server
                || h.workspace.server() != parts.server
            {
                return Err(GitFailure::WrongServer);
            }
            if !panes.insert(h.pane)
                || !parts
                    .tabs
                    .iter()
                    .any(|t| t.id == h.tab && t.workspace == h.workspace)
            {
                return Err(GitFailure::InvalidResponse);
            }
            if let Some(session) = pane.session {
                if session.pane != h.pane
                    || session.epoch.get() == 0
                    || session.generation.get() == 0
                    || session.generation != h.session_generation
                    || pane.startup_reserved
                    || session_epoch.is_some_and(|epoch| epoch != session.epoch)
                {
                    return Err(GitFailure::InvalidResponse);
                }
                session_epoch = Some(session.epoch);
            }
        }
        if parts.tabs.iter().any(|t| {
            !parts
                .panes
                .iter()
                .any(|p| p.hierarchy.pane == t.root_pane && p.hierarchy.tab == t.id)
        }) {
            return Err(GitFailure::InvalidResponse);
        }
        let mut fallback_workspaces = BTreeSet::new();
        let mut bytes = parts
            .tabs
            .len()
            .checked_mul(128)
            .and_then(|n| n.checked_add(parts.membership_versions.len().checked_mul(32)?))
            .and_then(|n| n.checked_add(parts.panes.len().checked_mul(256)?))
            .ok_or(GitFailure::Capacity)?;
        for fallback in &parts.fallbacks {
            if fallback.workspace.server() != parts.server
                || fallback.worktree.server() != parts.server
                || fallback.directory.server() != parts.server
            {
                return Err(GitFailure::WrongServer);
            }
            if !workspaces.contains(&fallback.workspace)
                || !fallback_workspaces.insert(fallback.workspace)
            {
                return Err(GitFailure::InvalidResponse);
            }
            bytes = bytes
                .checked_add(128 + fallback.directory.bytes().len())
                .ok_or(GitFailure::Capacity)?;
        }
        if bytes > MAX_GIT_SCOPE_BYTES {
            return Err(GitFailure::Capacity);
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &RetainedRemovalScopeParts {
        &self.0
    }
    pub fn empty(server: ServerId) -> Self {
        Self(RetainedRemovalScopeParts {
            server,
            tabs: Vec::new(),
            membership_versions: Vec::new(),
            panes: Vec::new(),
            fallbacks: Vec::new(),
        })
    }
    pub fn fits_marks(&self, marks: IdentityHighWater) -> bool {
        self.0.tabs.iter().all(|t| {
            t.id.local() <= marks.tab
                && t.workspace.local() <= marks.workspace
                && t.root_pane.local() <= marks.pane
                && t.creation_ordinal.get() <= marks.creation_ordinal
                && t.worktree.is_none_or(|w| w.local() <= marks.worktree)
        }) && self
            .0
            .panes
            .iter()
            .all(|p| p.hierarchy.pane.local() <= marks.pane)
            && self.0.fallbacks.iter().all(|f| {
                f.workspace.local() <= marks.workspace && f.worktree.local() <= marks.worktree
            })
    }
}
impl TryFrom<RetainedRemovalScopeParts> for RetainedRemovalScope {
    type Error = GitFailure;
    fn try_from(parts: RetainedRemovalScopeParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<RetainedRemovalScope> for RetainedRemovalScopeParts {
    fn from(value: RetainedRemovalScope) -> Self {
        value.0
    }
}

/// Exact metadata-only fallback IDs allocated by the reducer after durable admission.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitReservedFallback {
    pub plan: WorkspaceFallbackPlan,
    pub tab: TabId,
    pub pane: PaneId,
    pub tab_creation_ordinal: CreationOrdinal,
    pub pane_creation_ordinal: CreationOrdinal,
}
/// Untrusted reservation input. This DTO validates allocation, but allocates nothing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitAllocationReservationParts {
    pub binding: OperationBinding,
    pub expected_high_water: IdentityHighWater,
    pub resulting_high_water: IdentityHighWater,
    pub repository: Option<RepositoryId>,
    pub worktree: Option<WorktreeId>,
    pub fallbacks: Vec<GitReservedFallback>,
}
/// Operation-bound exception to no-resurrection, consumed in the final insertion transaction.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    try_from = "GitAllocationReservationParts",
    into = "GitAllocationReservationParts"
)]
pub struct GitAllocationReservation(GitAllocationReservationParts);
impl GitAllocationReservation {
    pub fn new(parts: GitAllocationReservationParts) -> Result<Self, GitFailure> {
        if parts.fallbacks.len() > MAX_GIT_TABS {
            return Err(GitFailure::Capacity);
        }
        let server = parts.binding.id.stream().server();
        let old = parts.expected_high_water;
        let mut expected = old;
        let count = parts.fallbacks.len() as u64;
        expected.tab = old.tab.checked_add(count).ok_or(GitFailure::Capacity)?;
        expected.pane = old.pane.checked_add(count).ok_or(GitFailure::Capacity)?;
        expected.creation_ordinal = old
            .creation_ordinal
            .checked_add(count.checked_mul(2).ok_or(GitFailure::Capacity)?)
            .ok_or(GitFailure::Capacity)?;
        if let Some(id) = parts.repository {
            expected.repository = old.repository.checked_add(1).ok_or(GitFailure::Capacity)?;
            if id.server() != server || id.local() != expected.repository {
                return Err(GitFailure::InvalidResponse);
            }
        }
        if let Some(id) = parts.worktree {
            expected.worktree = old.worktree.checked_add(1).ok_or(GitFailure::Capacity)?;
            if id.server() != server || id.local() != expected.worktree {
                return Err(GitFailure::InvalidResponse);
            }
        }
        if parts.resulting_high_water != expected {
            return Err(GitFailure::InvalidResponse);
        }
        let mut tabs = BTreeSet::new();
        let mut panes = BTreeSet::new();
        let mut ordinals = BTreeSet::new();
        let mut workspaces = BTreeSet::new();
        for fallback in &parts.fallbacks {
            if fallback.tab.server() != server
                || fallback.pane.server() != server
                || fallback.plan.workspace.server() != server
                || fallback.plan.worktree.server() != server
                || fallback.plan.directory.server() != server
            {
                return Err(GitFailure::WrongServer);
            }
            if fallback.plan.workspace.local() > old.workspace
                || fallback.plan.worktree.local() > old.worktree
                || !workspaces.insert(fallback.plan.workspace)
            {
                return Err(GitFailure::InvalidResponse);
            }
            if !(old.tab < fallback.tab.local()
                && fallback.tab.local() <= expected.tab
                && old.pane < fallback.pane.local()
                && fallback.pane.local() <= expected.pane)
                || !tabs.insert(fallback.tab)
                || !panes.insert(fallback.pane)
            {
                return Err(GitFailure::InvalidResponse);
            }
            for ordinal in [
                fallback.tab_creation_ordinal,
                fallback.pane_creation_ordinal,
            ] {
                if !(old.creation_ordinal < ordinal.get()
                    && ordinal.get() <= expected.creation_ordinal)
                    || !ordinals.insert(ordinal)
                {
                    return Err(GitFailure::InvalidResponse);
                }
            }
            if fallback.tab_creation_ordinal >= fallback.pane_creation_ordinal {
                return Err(GitFailure::InvalidResponse);
            }
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &GitAllocationReservationParts {
        &self.0
    }
    pub const fn binding(&self) -> OperationBinding {
        self.0.binding
    }
}
impl TryFrom<GitAllocationReservationParts> for GitAllocationReservation {
    type Error = GitFailure;
    fn try_from(parts: GitAllocationReservationParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<GitAllocationReservation> for GitAllocationReservationParts {
    fn from(value: GitAllocationReservation) -> Self {
        value.0
    }
}

/// Persist before any retirement or external Git effect.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GitIntentPhase {
    Prepared,
    Started,
    ExternalFinished,
}
/// Typed observed external result. A failed Git invocation may have partial effects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetainedExternalResult {
    pub disposition: ExternalDisposition,
    pub failure: Option<GitFailure>,
    pub ended_sessions: u32,
}
/// Raw retained-intent input, validated without reconstructing native authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitWriteIntentParts {
    pub binding: OperationBinding,
    pub repository: RepositoryLocator,
    pub action: RetainedGitAction,
    pub phase: GitIntentPhase,
    pub assessment_digest: GitAssessmentDigest,
    pub affected: RetainedRemovalScope,
    pub allocation: OperationBinding,
    pub external_result: Option<RetainedExternalResult>,
}
/// One original durable write intent. Restart can reconcile it, never execute it again.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "GitWriteIntentParts", into = "GitWriteIntentParts")]
pub struct GitWriteIntent(GitWriteIntentParts);
impl GitWriteIntent {
    pub fn new(parts: GitWriteIntentParts) -> Result<Self, GitFailure> {
        let server = parts.binding.id.stream().server();
        if parts.repository.scope().server != server
            || parts.affected.parts().server != server
            || parts.allocation != parts.binding
        {
            return Err(GitFailure::WrongServer);
        }
        parts.action.validate_scope(server)?;
        // Directory-contained work can belong to Tabs tagged with another Worktree.
        // Retain their original membership and exact Pane generations. The actor
        // revalidates this complete assessed scope before retirement, not just target tags.
        if (parts.phase == GitIntentPhase::ExternalFinished) != parts.external_result.is_some()
            || parts
                .external_result
                .as_ref()
                .is_some_and(|r| r.ended_sessions as usize > MAX_GIT_PANES)
        {
            return Err(GitFailure::InvalidResponse);
        }
        if matches!(parts.action, RetainedGitAction::Create { .. })
            && (!parts.affected.parts().tabs.is_empty()
                || !parts.affected.parts().panes.is_empty()
                || !parts.affected.parts().fallbacks.is_empty())
        {
            return Err(GitFailure::InvalidResponse);
        }
        if parts
            .affected
            .parts()
            .fallbacks
            .iter()
            .any(|f| f.worktree == parts.action.worktree())
        {
            return Err(GitFailure::InvalidResponse);
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &GitWriteIntentParts {
        &self.0
    }
    pub const fn binding(&self) -> OperationBinding {
        self.0.binding
    }
    pub const fn phase(&self) -> GitIntentPhase {
        self.0.phase
    }
    pub fn reference(&self) -> GitIntentReference {
        GitIntentReference {
            binding: self.binding(),
            phase: self.phase(),
        }
    }
    pub fn fits_marks(&self, marks: IdentityHighWater) -> bool {
        self.0.repository.scope().repository.local() <= marks.repository
            && self.0.action.worktree().local() <= marks.worktree
            && self.0.affected.fits_marks(marks)
    }
}
impl TryFrom<GitWriteIntentParts> for GitWriteIntent {
    type Error = GitFailure;
    fn try_from(parts: GitWriteIntentParts) -> Result<Self, Self::Error> {
        Self::new(parts)
    }
}
impl From<GitWriteIntent> for GitWriteIntentParts {
    fn from(value: GitWriteIntent) -> Self {
        value.0
    }
}

/// The ledger's typed Git external-progress reference. It grants no execution permit.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitIntentReference {
    binding: OperationBinding,
    phase: GitIntentPhase,
}
impl GitIntentReference {
    pub const fn new(binding: OperationBinding, phase: GitIntentPhase) -> Self {
        Self { binding, phase }
    }
    pub const fn binding(self) -> OperationBinding {
        self.binding
    }
    pub const fn phase(self) -> GitIntentPhase {
        self.phase
    }
}

/// CAS projection. The store compares the previous phase and original digest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GitIntentMutation {
    Insert(Box<GitWriteIntent>),
    Advance {
        binding: OperationBinding,
        expected_phase: GitIntentPhase,
        next: GitIntentPhase,
        result: Option<RetainedExternalResult>,
    },
    Complete {
        binding: OperationBinding,
    },
}
impl GitIntentMutation {
    pub fn binding(&self) -> OperationBinding {
        match self {
            Self::Insert(v) => v.binding(),
            Self::Advance { binding, .. } | Self::Complete { binding } => *binding,
        }
    }
    pub fn validate(&self) -> Result<(), GitFailure> {
        match self {
            Self::Insert(v) if v.phase() != GitIntentPhase::Prepared => {
                Err(GitFailure::InvalidResponse)
            }
            Self::Advance {
                expected_phase,
                next,
                result,
                ..
            } => {
                if !matches!(
                    (*expected_phase, *next),
                    (GitIntentPhase::Prepared, GitIntentPhase::Started)
                        | (GitIntentPhase::Started, GitIntentPhase::ExternalFinished)
                ) || (*next == GitIntentPhase::ExternalFinished) != result.is_some()
                    || result
                        .as_ref()
                        .is_some_and(|r| r.ended_sessions as usize > MAX_GIT_PANES)
                {
                    Err(GitFailure::InvalidResponse)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }
}
/// Consume or abandon never lowers high-water marks or makes identities reusable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GitAllocationMutation {
    Reserve(Box<GitAllocationReservation>),
    Consume { binding: OperationBinding },
    Abandon { binding: OperationBinding },
}
impl GitAllocationMutation {
    pub fn binding(&self) -> OperationBinding {
        match self {
            Self::Reserve(v) => v.binding(),
            Self::Consume { binding } | Self::Abandon { binding } => *binding,
        }
    }
}

/// Borrowed ledger facts for Git's recovery validator. A retired stream can still settle Started work.
/// No method supplies a resumed external execution permit.
pub trait GitRecoveryLedger {
    fn pending_git_progress(
        &self,
        binding: OperationBinding,
    ) -> Result<Option<GitIntentReference>, StoreFailure>;
    fn is_awaiting_git_intent(&self, binding: OperationBinding) -> Result<bool, StoreFailure>;
    fn has_completed_git_result(&self, operation: OperationId) -> Result<bool, StoreFailure>;
}

/// Core structural checks shared by transaction and recovery admission.
pub fn validate_git_binding(
    binding: OperationBinding,
    server: ServerId,
    marks: IdentityHighWater,
) -> Result<(), StoreFailure> {
    let stream = binding.id.stream();
    if stream.server() != server
        || stream.key().local() > marks.operation_stream
        || stream.pairing().local() > marks.pairing
        || stream.client().local() > marks.client
    {
        Err(StoreFailure::InvalidDurableState)
    } else {
        Ok(())
    }
}
