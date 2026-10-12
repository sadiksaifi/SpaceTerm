//! Bounded read-side Git presentation. These pages are not removal authority or
//! organization mutations. The actor owns query execution and cached cursor binding.

use super::*;
use crate::authority::{
    CloseWorkGeneration, ControlGeneration, FieldVersion, OrganizationRevision, SessionGeneration,
};
use crate::identity::{PaneId, ServerId, TabId, WorkspaceId, WorktreeId};
use crate::operations::RepositoryScope;
use crate::organization::{BranchName, ServerDirectory};
use std::collections::BTreeSet;
use std::num::NonZeroU64;

pub const MAX_GIT_PAGE_RECORDS: usize = 128;
pub const MAX_GIT_PAGE_BYTES: usize = 48 * 1024;
pub const MAX_GIT_STATUS_CHANGES: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitCorrelation {
    pub request: NonZeroU64,
    pub generation: NonZeroU64,
}
impl GitCorrelation {
    pub fn new(request: u64, generation: u64) -> Result<Self, GitFailure> {
        Ok(Self {
            request: NonZeroU64::new(request).ok_or(GitFailure::InvalidResponse)?,
            generation: NonZeroU64::new(generation).ok_or(GitFailure::InvalidResponse)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitQueryRequest {
    pub correlation: GitCorrelation,
    pub query: GitQuery,
    /// An optional comparison assertion, never discovery or filesystem authority.
    pub scope: Option<RepositoryScope>,
    pub subscribe: bool,
}
impl GitQueryRequest {
    pub fn new(
        correlation: GitCorrelation,
        query: GitQuery,
        scope: Option<RepositoryScope>,
        subscribe: bool,
    ) -> Result<Self, GitFailure> {
        let mut value = Self {
            correlation,
            query,
            scope,
            subscribe,
        };
        value.validate()?;
        if matches!(
            value.query,
            GitQuery::Branches { .. } | GitQuery::PreviewCreate { .. }
        ) {
            value.scope = None;
        }
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), GitFailure> {
        let server = self.query.server();
        self.query.validate_scope(server)?;
        if let Some(scope) = self.scope {
            scope.validate().map_err(|_| GitFailure::WrongServer)?;
            if scope.server != server {
                return Err(GitFailure::WrongServer);
            }
        }
        let expected = match &self.query {
            GitQuery::Branches { repository, .. } => Some(*repository),
            GitQuery::PreviewCreate { request } => Some(request.parts().repository),
            _ => None,
        };
        if expected.zip(self.scope).is_some_and(|(a, b)| a != b) {
            return Err(GitFailure::InvalidResponse);
        }
        if self.subscribe && !matches!(self.query, GitQuery::Status { .. }) {
            return Err(GitFailure::InvalidResponse);
        }
        let cursor = match &self.query {
            GitQuery::Branches { cursor, .. }
            | GitQuery::Inventory { cursor, .. }
            | GitQuery::AssessmentPage { cursor, .. } => cursor.as_ref(),
            _ => None,
        };
        if cursor
            .zip(self.scope)
            .is_some_and(|(c, s)| c.parts().scope != s)
        {
            return Err(GitFailure::StaleAssessment);
        }
        Ok(())
    }
    pub fn repository_scope(&self) -> Option<RepositoryScope> {
        match &self.query {
            GitQuery::Branches { repository, .. } => Some(*repository),
            GitQuery::PreviewCreate { request } => Some(request.parts().repository),
            _ => self.scope,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitInventoryRecord {
    pub worktree: WorktreeId,
    pub directory: ServerDirectory,
    pub main: bool,
    pub missing: bool,
    pub listed: bool,
    pub version: FieldVersion,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitInventoryPage {
    pub records: Vec<GitInventoryRecord>,
    pub next: Option<GitPageCursor>,
    pub version: OrganizationRevision,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitBranchRecord {
    pub name: BranchName,
    pub remote: bool,
    pub checked_out: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitBranchPage {
    pub records: Vec<GitBranchRecord>,
    pub next: Option<GitPageCursor>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitAffectedTab {
    pub workspace: WorkspaceId,
    pub tab: TabId,
    pub worktree: Option<WorktreeId>,
    pub membership_version: FieldVersion,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitAffectedPane {
    pub pane: PaneId,
    pub tab: TabId,
    pub session_generation: SessionGeneration,
    pub control_generation: ControlGeneration,
    pub work_generation: CloseWorkGeneration,
    pub starting: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitFallback {
    pub workspace: WorkspaceId,
    pub worktree: Option<WorktreeId>,
    pub directory: ServerDirectory,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitAssessmentPage {
    pub token: GitAssessmentToken,
    pub tabs: Vec<GitAffectedTab>,
    pub panes: Vec<GitAffectedPane>,
    pub fallbacks: Vec<GitFallback>,
    pub next: Option<GitPageCursor>,
    pub requires_confirmation: bool,
    pub creation_preview: Option<WorktreeCreationPreview>,
    /// Bound to this page's token and repository scope. Absent in legacy responses.
    /// A client must not confirm removal without the target and risk summary.
    pub removal: Option<GitRemovalSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRemovalSummary {
    pub worktree: WorktreeId,
    pub directory: ServerDirectory,
    pub branch: Option<BranchName>,
    pub risk: ApprovedRemoval,
}

#[derive(Clone, Eq, PartialEq)]
pub struct GitChange {
    path: Vec<u8>,
    /// The frozen profile's presentation code, not executable Git arguments.
    pub kind: u32,
}
impl GitChange {
    pub fn new(path: Vec<u8>, kind: u32) -> Result<Self, GitFailure> {
        if path.is_empty() || path.len() > 4096 || path.contains(&0) {
            return Err(GitFailure::InvalidResponse);
        }
        Ok(Self { path, kind })
    }
    pub fn path(&self) -> &[u8] {
        &self.path
    }
}
impl std::fmt::Debug for GitChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitChange")
            .field("path_bytes", &self.path.len())
            .field("kind", &self.kind)
            .finish()
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusFacts {
    /// Independent, epoch-qualified by the authenticated connection. Not an organization revision.
    pub revision: NonZeroU64,
    pub branch: Option<BranchName>,
    pub changes: Vec<GitChange>,
    pub dirty: bool,
    pub unchecked: bool,
    pub pull_request: Option<GitPullRequestFacts>,
    pub upstream: Option<GitUpstream>,
    pub operation: Option<GitRepositoryOperation>,
    /// None means the peer did not supply aggregate facts, not an exact count of displayed rows.
    pub counts: Option<GitChangeCounts>,
}

/// Normalized inputs for client-local GitHub CLI lookup. Raw remote URLs never cross this boundary.
#[derive(Clone, Eq, PartialEq)]
pub struct GitPullRequestFacts {
    host: String,
    owner: String,
    repository: String,
    head_branch: BranchName,
    head_owner: Option<String>,
}
impl GitPullRequestFacts {
    pub fn new(
        host: String,
        owner: String,
        repository: String,
        head_branch: BranchName,
        head_owner: Option<String>,
    ) -> Result<Self, GitFailure> {
        if host.is_empty()
            || host.len() > 253
            || host != host.to_ascii_lowercase()
            || host == "ssh.github.com"
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            || !github_segment(&owner)
            || !github_segment(&repository)
            || head_owner
                .as_ref()
                .is_some_and(|owner| !github_segment(owner))
            || head_branch.as_str().is_empty()
        {
            return Err(GitFailure::InvalidResponse);
        }
        Ok(Self {
            host,
            owner,
            repository,
            head_branch,
            head_owner,
        })
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn owner(&self) -> &str {
        &self.owner
    }
    pub fn repository(&self) -> &str {
        &self.repository
    }
    pub fn head_branch(&self) -> &BranchName {
        &self.head_branch
    }
    pub fn head_owner(&self) -> Option<&str> {
        self.head_owner.as_deref()
    }
    fn bytes(&self) -> usize {
        self.host.len()
            + self.owner.len()
            + self.repository.len()
            + self.head_branch.as_str().len()
            + self.head_owner.as_ref().map_or(0, String::len)
    }
}
impl std::fmt::Debug for GitPullRequestFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitPullRequestFacts([redacted])")
    }
}
fn github_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value != "."
        && value != ".."
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitDivergence {
    pub ahead: u32,
    pub behind: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitUpstream {
    pub name: BranchName,
    pub divergence: Option<GitDivergence>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitOperationStep {
    pub current: u32,
    pub total: u32,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitRepositoryOperation {
    Rebasing { step: Option<GitOperationStep> },
    Applying { step: Option<GitOperationStep> },
    Merging,
    Reverting,
    CherryPicking,
    Bisecting,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitChangeTotal {
    Exact(u32),
    AtLeast(u32),
}
impl GitChangeTotal {
    pub fn value(self) -> u32 {
        match self {
            Self::Exact(value) | Self::AtLeast(value) => value,
        }
    }
}
/// Category counts share the completeness of total and may overlap for one path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitChangeCounts {
    pub total: GitChangeTotal,
    pub staged: u32,
    pub modified: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub conflicted: u32,
    pub untracked: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitQueryResult {
    Inventory(GitInventoryPage),
    Branches(GitBranchPage),
    Status(GitStatusFacts),
    Assessment(Box<GitAssessmentPage>),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitQueryResponse {
    pub correlation: GitCorrelation,
    pub scope: Option<RepositoryScope>,
    pub result: Result<GitQueryResult, GitFailure>,
}
impl GitQueryResponse {
    pub fn validate(&self, server: ServerId) -> Result<(), GitFailure> {
        match &self.result {
            Ok(result) => result.validate(self.scope, server),
            Err(_) => {
                if let Some(scope) = self.scope {
                    scope.validate().map_err(|_| GitFailure::WrongServer)?;
                    if scope.server != server {
                        return Err(GitFailure::WrongServer);
                    }
                }
                Ok(())
            }
        }
    }
}
impl GitQueryResult {
    pub fn validate(
        &self,
        scope: Option<RepositoryScope>,
        server: ServerId,
    ) -> Result<(), GitFailure> {
        if let Some(scope) = scope {
            scope.validate().map_err(|_| GitFailure::WrongServer)?;
            if scope.server != server {
                return Err(GitFailure::WrongServer);
            }
        }
        let result = self;
        let mut bytes = 256;
        let cursor = match result {
            GitQueryResult::Inventory(page) => {
                count(page.records.len())?;
                let mut ids = BTreeSet::new();
                for record in &page.records {
                    if record.worktree.server() != server || record.directory.server() != server {
                        return Err(GitFailure::WrongServer);
                    }
                    if !ids.insert(record.worktree) {
                        return Err(GitFailure::InvalidResponse);
                    }
                    bytes += 128 + record.directory.bytes().len();
                }
                if page
                    .next
                    .as_ref()
                    .is_some_and(|c| c.parts().observation != page.version.get())
                {
                    return Err(GitFailure::StaleAssessment);
                }
                page.next.as_ref()
            }
            GitQueryResult::Branches(page) => {
                count(page.records.len())?;
                let mut names = BTreeSet::new();
                for record in &page.records {
                    if !names.insert((record.remote, record.name.as_str())) {
                        return Err(GitFailure::InvalidResponse);
                    }
                    bytes += 128 + record.name.as_str().len();
                }
                page.next.as_ref()
            }
            GitQueryResult::Status(facts) => {
                if facts.changes.len() > MAX_GIT_STATUS_CHANGES {
                    return Err(GitFailure::Capacity);
                }
                bytes += facts.branch.as_ref().map_or(0, |b| b.as_str().len());
                if let Some(query) = &facts.pull_request {
                    if facts.branch.is_none() {
                        return Err(GitFailure::InvalidResponse);
                    }
                    bytes += 128 + query.bytes();
                }
                if let Some(upstream) = &facts.upstream {
                    if upstream.name.as_str().is_empty() {
                        return Err(GitFailure::InvalidResponse);
                    }
                    bytes += 64 + upstream.name.as_str().len();
                }
                if let Some(
                    GitRepositoryOperation::Rebasing { step: Some(step) }
                    | GitRepositoryOperation::Applying { step: Some(step) },
                ) = facts.operation
                    && (step.current == 0 || step.current > step.total)
                {
                    return Err(GitFailure::InvalidResponse);
                }
                if let Some(counts) = facts.counts {
                    let total = counts.total.value();
                    if total < facts.changes.len() as u32
                        || [
                            counts.staged,
                            counts.modified,
                            counts.deleted,
                            counts.renamed,
                            counts.conflicted,
                            counts.untracked,
                        ]
                        .into_iter()
                        .any(|count| count > total)
                        || (total > 0 && !facts.dirty)
                        || (counts.total == GitChangeTotal::Exact(0) && facts.dirty)
                    {
                        return Err(GitFailure::InvalidResponse);
                    }
                    bytes += 64;
                }
                for change in &facts.changes {
                    bytes += 64 + change.path.len();
                }
                None
            }
            GitQueryResult::Assessment(page) => {
                if page.token.server() != server {
                    return Err(GitFailure::WrongServer);
                }
                // One page cannot exceed the combined management record budget.
                count(page.tabs.len() + page.panes.len() + page.fallbacks.len())?;
                let mut tabs = BTreeSet::new();
                let mut panes = BTreeSet::new();
                let mut fallbacks = BTreeSet::new();
                for tab in &page.tabs {
                    if tab.workspace.server() != server
                        || tab.tab.server() != server
                        || tab.worktree.is_some_and(|w| w.server() != server)
                    {
                        return Err(GitFailure::WrongServer);
                    }
                    if !tabs.insert(tab.tab) {
                        return Err(GitFailure::InvalidResponse);
                    }
                    bytes += 192;
                }
                for pane in &page.panes {
                    if pane.pane.server() != server || pane.tab.server() != server {
                        return Err(GitFailure::WrongServer);
                    }
                    if !panes.insert(pane.pane) {
                        return Err(GitFailure::InvalidResponse);
                    }
                    bytes += 192;
                }
                for fallback in &page.fallbacks {
                    if fallback.workspace.server() != server
                        || fallback.directory.server() != server
                        || fallback.worktree.is_some_and(|w| w.server() != server)
                    {
                        return Err(GitFailure::WrongServer);
                    }
                    if !fallbacks.insert(fallback.workspace) {
                        return Err(GitFailure::InvalidResponse);
                    }
                    bytes += 128 + fallback.directory.bytes().len();
                }
                if let Some(removal) = &page.removal {
                    if page.token.kind() != GitAssessmentKind::Remove
                        || !page.requires_confirmation
                        || page.creation_preview.is_some()
                    {
                        return Err(GitFailure::InvalidResponse);
                    }
                    if removal.worktree.server() != server || removal.directory.server() != server {
                        return Err(GitFailure::WrongServer);
                    }
                    bytes += 128
                        + removal.directory.bytes().len()
                        + removal
                            .branch
                            .as_ref()
                            .map_or(0, |branch| branch.as_str().len());
                }
                if let Some(preview) = &page.creation_preview {
                    if page.token.kind() != GitAssessmentKind::Create
                        || preview.parts().draft.parts().repository.server != server
                        || scope != Some(preview.parts().draft.parts().repository)
                    {
                        return Err(GitFailure::InvalidResponse);
                    }
                    bytes += 4096 + preview.parts().directory.bytes().len();
                }
                if page
                    .next
                    .as_ref()
                    .is_some_and(|c| c.parts().epoch != page.token.epoch())
                {
                    return Err(GitFailure::StaleAssessment);
                }
                page.next.as_ref()
            }
        };
        if let Some(cursor) = cursor {
            if scope != Some(cursor.parts().scope) {
                return Err(GitFailure::StaleAssessment);
            }
            bytes += 256;
        }
        if bytes > MAX_GIT_PAGE_BYTES {
            return Err(GitFailure::Capacity);
        }
        if scope.is_none()
            && match result {
                GitQueryResult::Inventory(page) => !page.records.is_empty() || page.next.is_some(),
                GitQueryResult::Branches(_) | GitQueryResult::Assessment(_) => true,
                GitQueryResult::Status(facts) => {
                    facts.branch.is_some()
                        || !facts.changes.is_empty()
                        || facts.dirty
                        || facts.pull_request.is_some()
                        || facts.upstream.is_some()
                        || facts.operation.is_some()
                        || facts.counts.is_some()
                }
            }
        {
            return Err(GitFailure::InvalidResponse);
        }
        Ok(())
    }
}
fn count(count: usize) -> Result<(), GitFailure> {
    if count > MAX_GIT_PAGE_RECORDS {
        Err(GitFailure::Capacity)
    } else {
        Ok(())
    }
}
