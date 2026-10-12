//! Worktrees: the git worktrees of the repository a Workspace reads, and the git commands that
//! list, create, and remove them.
//!
//! Git policy and parsers belong to `spaceterm_host::git`. This desktop facade keeps its catalog,
//! Workspace-scoped repository identities, and Local Filesystem Authority. Values can carry paths
//! and branch names for presentation; errors never do.

pub(crate) mod catalog;
pub(crate) mod git;
pub(crate) mod listing;
pub(crate) mod path_template;
pub(crate) mod ref_format;

use std::fmt;
use std::path::PathBuf;

use crate::domain::RepositoryIdentity;
use listing::WorktreeRecord;

/// One read of a repository's Worktrees from a directory inside it.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct WorktreeSnapshot {
    pub(crate) repository: RepositoryIdentity,
    /// The index in `worktrees` of the Worktree containing the read directory.
    pub(crate) current: Option<usize>,
    /// The shared git directory, watched for Worktree changes.
    pub(crate) common_directory: PathBuf,
    /// The Main Worktree first, then linked Worktrees in git's order.
    pub(crate) worktrees: Vec<WorktreeRecord>,
}

impl fmt::Debug for WorktreeSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorktreeSnapshot")
            .field("repository", &self.repository)
            .field("current", &self.current)
            .field("worktrees", &self.worktrees)
            .finish_non_exhaustive()
    }
}
