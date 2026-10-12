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
                    facts.branch.is_some() || !facts.changes.is_empty() || facts.dirty
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
