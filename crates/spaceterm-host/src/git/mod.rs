//! Hardened, headless Git reads and Worktree writes on the owning host.
//!
//! GitHub CLI authentication and Pull Request lookup belong to the desktop client.
//! Blocking calls run on dedicated threads, not an async executor's workers.

mod cancellation;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod coordinator;
pub mod discovery;
pub mod display_text;
pub mod filesystem;
pub mod listing;
pub mod local_read;
mod model;
pub mod operation;
pub mod path_template;
pub mod porcelain;
pub mod ref_format;
pub mod tools;
mod worktrees;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use coordinator::*;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod markers;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod process;
#[cfg(test)]
mod testing;

pub use cancellation::CancellationToken;
pub use local_read::LocalRepositoryReader;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use markers::UnixRepositoryMarkerReader;
pub use model::*;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use process::UnixRepositoryProgramRunner;
pub use worktrees::*;

use listing::WorktreeRecord;
use std::fmt;
use std::path::PathBuf;

/// One read of a repository's Worktrees from a directory inside it.
#[derive(Clone, Eq, PartialEq)]
pub struct WorktreeSnapshot {
    /// The Main Worktree's root as Git reports it.
    pub main_root: PathBuf,
    /// Index of the Worktree containing the read directory.
    pub current: Option<usize>,
    /// The shared Git directory.
    pub common_directory: PathBuf,
    /// Main Worktree first, then linked Worktrees in Git's order.
    pub worktrees: Vec<WorktreeRecord>,
}

impl fmt::Debug for WorktreeSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorktreeSnapshot")
            .field("current", &self.current)
            .field("worktrees", &self.worktrees)
            .finish_non_exhaustive()
    }
}
