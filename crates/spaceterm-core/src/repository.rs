//! Bounded Git facts from the owning server. Paths describe that server and grant no local authority.
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

/// What `HEAD` names.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RepositoryHead {
    Branch(Arc<str>),
    /// A detached `HEAD`, carrying the abbreviated commit id.
    Detached(Arc<str>),
    /// A branch with no commits yet.
    Unborn(Arc<str>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Divergence {
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Upstream {
    /// The upstream's short name, such as `origin/main`.
    pub name: Arc<str>,
    /// `None` when the configured upstream no longer exists.
    pub divergence: Option<Divergence>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationStep {
    pub current: u32,
    pub total: u32,
}

/// An unfinished multi-step git operation, in Starship's `git_state` precedence order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RepositoryOperation {
    Rebasing {
        step: Option<OperationStep>,
    },
    /// `git am`.
    Applying {
        step: Option<OperationStep>,
    },
    Merging,
    Reverting,
    CherryPicking,
    Bisecting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ChangeKind {
    Staged,
    Modified,
    Deleted,
    Renamed,
    Conflicted,
    Untracked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangeEntry {
    pub kind: ChangeKind,
    /// Repository-relative path, sanitized for display.
    pub path: Arc<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ChangeTotal {
    Exact(u32),
    /// The output was truncated after this many records.
    AtLeast(u32),
}

impl Default for ChangeTotal {
    fn default() -> Self {
        Self::Exact(0)
    }
}

/// Changed paths by kind. A path counts once in `total` and once per kind it has.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangeSummary {
    pub total: ChangeTotal,
    pub staged: u32,
    pub modified: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub conflicted: u32,
    pub untracked: u32,
    /// The first [`MAXIMUM_CHANGE_ENTRIES`] records, in git's order.
    pub entries: Vec<ChangeEntry>,
}

/// The most changed paths retained for the Repository Status popover.
pub const MAXIMUM_CHANGE_ENTRIES: usize = 20;

/// Whether a read may use Git's built-in fsmonitor daemon.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum FsmonitorPolicy {
    #[default]
    Disabled,
    Builtin,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq, Serialize, Deserialize)]
/// Content-free Repository Status read failure.
pub enum RepositoryReadError {
    #[error("git is unavailable")]
    ToolMissing,
    #[error("the repository could not be read")]
    Unavailable,
    #[error("the repository read exceeded its deadline")]
    TimedOut,
    #[error("the repository read was cancelled")]
    Cancelled,
    #[error("the repository read exceeded its output limit")]
    OutputTooLarge,
    #[error("the repository read returned an invalid response")]
    InvalidResponse,
}

/// A GitHub repository named by a remote URL: the Pull Request lookup's `--repo` target.
#[derive(Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct GitHubRepository {
    /// Lowercase host name, such as `github.com` or an Enterprise host.
    pub host: Arc<str>,
    pub owner: Arc<str>,
    pub name: Arc<str>,
}

impl fmt::Debug for GitHubRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GitHubRepository(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ToolVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum GitToolStatus {
    #[default]
    Unknown,
    Ready(ToolVersion),
    TooOld(ToolVersion),
    NotFound,
}

/// What a Worktree has checked out.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum WorktreeHead {
    /// A branch's short name, such as `feature/login`.
    Branch(String),
    /// A detached commit's short object name.
    Detached(String),
    /// A bare repository has no work tree and no checkout.
    Bare,
}

/// One entry of `git worktree list`.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorktreeRecord {
    /// The work tree root as git reports it.
    pub root: PathBuf,
    pub head: WorktreeHead,
    pub locked: bool,
    /// Git reports the work tree as prunable: its directory no longer exists.
    pub missing: bool,
}

impl std::fmt::Debug for WorktreeRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorktreeRecord")
            .field("root", &"<redacted>")
            .field("locked", &self.locked)
            .field("missing", &self.missing)
            .finish_non_exhaustive()
    }
}

/// One Pull Request lookup: the base repository, the head branch, and the account that owns the
/// head when it lives in a fork.
#[derive(Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct PullRequestQuery {
    pub repository: GitHubRepository,
    pub head_branch: Arc<str>,
    /// The push remote's owner. When known, every pull request must come from it.
    pub head_owner: Option<Arc<str>>,
}

impl fmt::Debug for PullRequestQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PullRequestQuery(<redacted>)")
    }
}

/// Facts for one validated directory. Failures preserve the last successful facts as stale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryFacts {
    pub directory: PathBuf,
    pub repository: Option<Repository>,
    pub worktrees: Option<WorktreeInventory>,
    pub failure: Option<RepositoryReadError>,
    /// The directory is inside a work tree repository's Git directory, such as `.git/hooks`.
    /// Repository Status shows nothing there, but the directory is inside its repository.
    pub inside_git_directory: bool,
}
impl DirectoryFacts {
    /// Whether a successful read placed the directory outside any repository. Only then does an
    /// unpinned Root Tab leave its Worktree (ADR 0020).
    pub fn outside_repository(&self) -> bool {
        self.failure.is_none() && self.repository.is_none() && !self.inside_git_directory
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub root: PathBuf,
    pub head: RepositoryHead,
    pub commit: Option<Arc<str>>,
    pub upstream: Option<Upstream>,
    pub operation: Option<RepositoryOperation>,
    /// Change counting can fail independently of branch, upstream, and Worktree discovery.
    pub changes: Result<ChangeSummary, RepositoryReadError>,
    /// A pending count carries the last successful summary, or an unavailable placeholder.
    /// Readers that understand progress ignore that placeholder until the count finishes.
    pub counting: Option<ChangeCounting>,
    /// Parsed target only. Raw remote URLs and Git configuration stay on the server.
    pub pull_request: Option<PullRequestQuery>,
}

/// Progress for an incomplete change count; the last successful summary can remain visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeCounting {
    /// The count has not run long enough to announce progress.
    Pending,
    /// The count has exceeded the presentation delay.
    Counting,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInventory {
    pub main_root: PathBuf,
    pub current: Option<usize>,
    pub worktrees: Vec<WorktreeRecord>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Server boot that owns this revision sequence.
    pub boot: crate::hierarchy::Id,
    pub revision: u64,
    pub git: GitToolStatus,
    pub directories: Vec<DirectoryFacts>,
    /// Listed Worktree roots that physically contain a Workspace's Pinned Directory.
    /// The owning server resolves symbolic links; clients grant no local file authority.
    pub pinned_worktrees: Vec<PathBuf>,
    pub pin_check: PinCheckState,
}

impl Snapshot {
    /// The facts the server read for one directory it reported.
    pub fn facts(&self, directory: &Path) -> Option<&DirectoryFacts> {
        self.directories
            .iter()
            .find(|facts| facts.directory == directory)
    }

    /// The Worktrees of the repository containing one directory, as git on the server lists them.
    ///
    /// A failed read keeps the directory's last listing, which a removed Worktree would keep
    /// forever. Another directory in the same repository that the server still reads lists its
    /// current Worktrees instead. Without one, the last listing stays.
    pub fn directory_worktrees(&self, directory: &Path) -> Option<&WorktreeInventory> {
        let facts = self.facts(directory)?;
        let last = facts.worktrees.as_ref()?;
        if facts.failure.is_none() {
            return Some(last);
        }
        self.current_worktrees(&last.main_root).or(Some(last))
    }

    /// The Worktrees of the repository whose Main Worktree is `main_root`.
    ///
    /// The Main Worktree's own reading comes first. A bare Main Worktree has no work tree, so the
    /// server reads no Worktrees there; any directory whose read lists its repository does.
    pub fn repository_worktrees(&self, main_root: &Path) -> Option<&WorktreeInventory> {
        self.directory_worktrees(main_root)
            .or_else(|| self.current_worktrees(main_root))
    }

    /// The listing of the repository whose Main Worktree is `main_root` from a directory whose
    /// latest read succeeded. The first by path is chosen so every reading picks the same one.
    fn current_worktrees(&self, main_root: &Path) -> Option<&WorktreeInventory> {
        self.directories
            .iter()
            .filter(|facts| facts.failure.is_none())
            .filter_map(|facts| Some((&facts.directory, facts.worktrees.as_ref()?)))
            .filter(|(_, inventory)| inventory.main_root == main_root)
            .min_by_key(|(directory, _)| *directory)
            .map(|(_, inventory)| inventory)
    }
}

/// Physical removal eligibility is resolved independently of Git and the terminal request lock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PinCheckState {
    #[default]
    Ready,
    Checking,
    Unavailable,
}
