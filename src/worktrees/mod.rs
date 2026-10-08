//! Worktrees: the git worktrees of the repository a Workspace reads, and the git commands that
//! list, create, and remove them.
//!
//! This module is portable product policy. Programs run only through the Repository Status
//! [`RepositoryProgramRunner`](crate::repository_status::RepositoryProgramRunner) adapter with the
//! same hardening its reads use. Values can carry paths and branch names for presentation; errors
//! never do.

pub(crate) mod catalog;
pub(crate) mod git;
pub(crate) mod listing;

use std::fmt;
use std::path::{Path, PathBuf};

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

/// A path without trailing separators, for comparing git's spellings of one directory.
fn fixed(path: &Path) -> &Path {
    let text = path.as_os_str().to_str().unwrap_or_default();
    match text.trim_end_matches('/') {
        "" => path,
        trimmed => Path::new(trimmed),
    }
}
