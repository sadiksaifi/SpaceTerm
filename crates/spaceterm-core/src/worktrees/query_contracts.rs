//! Frozen engine-free Git read selectors. Discovery uses qualified hierarchy
//! identities, never a server-global active target or a guessed RepositoryId.
//! Git-server owns caching, authorization, page production and query execution.

use super::{GitAssessmentToken, GitFailure, WorktreeCreateDraft};
use crate::authority::ServerEpoch;
use crate::identity::{PaneId, ServerId, WorkspaceId, WorktreeId};
use crate::operations::RepositoryScope;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitStatusTarget {
    Pane(PaneId),
    Workspace(WorkspaceId),
    Worktree(WorktreeId),
}
impl GitStatusTarget {
    pub const fn server(self) -> ServerId {
        match self {
            Self::Pane(id) => id.server(),
            Self::Workspace(id) => id.server(),
            Self::Worktree(id) => id.server(),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct GitPageCursorParts {
    pub scope: RepositoryScope,
    pub epoch: ServerEpoch,
    pub observation: u64,
    pub random: [u8; 32],
}
/// Server-issued comparison input. Shape checks do not prove cached principal,
/// query or observation binding; the service must compare the original request.
#[derive(Clone, Eq, PartialEq)]
pub struct GitPageCursor(GitPageCursorParts);
impl GitPageCursor {
    pub fn new(parts: GitPageCursorParts) -> Result<Self, GitFailure> {
        parts
            .scope
            .validate()
            .map_err(|_| GitFailure::WrongServer)?;
        if parts.epoch.get() == 0 || parts.observation == 0 || parts.random == [0; 32] {
            return Err(GitFailure::InvalidResponse);
        }
        Ok(Self(parts))
    }
    pub fn parts(&self) -> &GitPageCursorParts {
        &self.0
    }
}
impl std::fmt::Debug for GitPageCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitPageCursor")
            .field("scope", &self.0.scope)
            .field("epoch", &self.0.epoch)
            .field("observation", &self.0.observation)
            .finish_non_exhaustive()
    }
}

/// Repository scope is required only by repository-scoped operations. An
/// unpinned Pane can request discovery without knowing a repository identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitQuery {
    Status {
        target: GitStatusTarget,
    },
    Inventory {
        workspace: WorkspaceId,
        cursor: Option<GitPageCursor>,
    },
    Branches {
        repository: RepositoryScope,
        cursor: Option<GitPageCursor>,
    },
    PreviewCreate {
        request: WorktreeCreateDraft,
    },
    AssessRemove {
        worktree: WorktreeId,
    },
    AssessmentPage {
        assessment: GitAssessmentToken,
        cursor: Option<GitPageCursor>,
    },
}
impl GitQuery {
    pub fn server(&self) -> ServerId {
        match self {
            Self::Status { target } => target.server(),
            Self::Inventory { workspace, .. } => workspace.server(),
            Self::Branches { repository, .. } => repository.server,
            Self::PreviewCreate { request } => request.parts().repository.server,
            Self::AssessRemove { worktree } => worktree.server(),
            Self::AssessmentPage { assessment, .. } => assessment.server(),
        }
    }
    /// Structural scope checks only. The authenticated server resolves membership
    /// and cached cursor/assessment binding before executing a query.
    pub fn validate_scope(&self, server: ServerId) -> Result<(), GitFailure> {
        if self.server() != server {
            return Err(GitFailure::WrongServer);
        }
        let cursor = match self {
            Self::Inventory { cursor, .. } | Self::AssessmentPage { cursor, .. } => cursor.as_ref(),
            Self::Branches { repository, cursor } => {
                repository.validate().map_err(|_| GitFailure::WrongServer)?;
                if cursor
                    .as_ref()
                    .is_some_and(|value| value.parts().scope != *repository)
                {
                    return Err(GitFailure::StaleAssessment);
                }
                cursor.as_ref()
            }
            _ => None,
        };
        if let Some(cursor) = cursor {
            if cursor.parts().scope.server != server {
                return Err(GitFailure::WrongServer);
            }
            if let Self::AssessmentPage { assessment, .. } = self
                && cursor.parts().epoch != assessment.epoch()
            {
                return Err(GitFailure::StaleAssessment);
            }
        }
        Ok(())
    }
}
