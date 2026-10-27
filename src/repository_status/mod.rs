//! Repository Status: the read-only git facts SpaceTerm presents for a directory's repository.
//!
//! This module is portable product policy. It owns the facts, their parsing, scheduling, and
//! presentation. Running programs, reading files, and watching directories happen only through the
//! narrow adapter traits below, which host composition supplies by constructor injection.
//!
//! Values here can carry repository paths, branch names, and remote URLs for presentation. Errors
//! never do: every error type is a payload-free classification.

pub(crate) mod discovery;
mod display_text;
pub(crate) mod local_read;
pub(crate) mod operation;
pub(crate) mod porcelain;
pub(crate) mod presentation;
pub(crate) mod pull_request;
pub(crate) mod pull_request_lookup;
pub(crate) mod remote_url;
pub(crate) mod scheduler;
#[cfg(test)]
pub(crate) mod testing;
pub(crate) mod tool_check;
pub(crate) mod tools;

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use thiserror::Error;

use crate::cancellation::CancellationToken;

mod preferences;

pub(crate) use preferences::RepositoryStatusPreferences;

/// The machine whose filesystem a repository lives on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RepositoryMachine {
    Local,
    Remote(RemoteMachineKey),
}

/// Opaque identity of one remote machine and account.
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct RemoteMachineKey(Arc<str>);

impl RemoteMachineKey {
    pub(crate) fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }
}

impl fmt::Debug for RemoteMachineKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteMachineKey(<redacted>)")
    }
}

/// The work tree root of a repository on its machine.
///
/// A remote root is an opaque remote spelling. It never becomes a local path.
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) enum RepositoryRoot {
    Local(PathBuf),
    Remote(Arc<str>),
}

impl RepositoryRoot {
    /// The root spelling for presentation only.
    pub(crate) fn display(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Local(path) => path.to_string_lossy(),
            Self::Remote(path) => std::borrow::Cow::Borrowed(path),
        }
    }
}

impl fmt::Debug for RepositoryRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Local(_) => "RepositoryRoot::Local(<redacted>)",
            Self::Remote(_) => "RepositoryRoot::Remote(<redacted>)",
        })
    }
}

/// One repository: the cache key shared by every Pane, Workspace, and window that shows it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RepositoryKey {
    pub(crate) machine: RepositoryMachine,
    pub(crate) root: RepositoryRoot,
}

/// Raw presence and contents of the operation marker files in a git directory.
///
/// Contents are at most [`MAXIMUM_MARKER_BYTES`] and are parsed, never displayed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct OperationMarkers {
    /// `rebase-merge/` with its `msgnum` and `end` files.
    pub(crate) rebase_merge: Option<StepMarkers>,
    /// `rebase-apply/` with its `next` and `last` files and its mode files.
    pub(crate) rebase_apply: Option<ApplyMarkers>,
    pub(crate) merge_head: bool,
    pub(crate) revert_head: bool,
    pub(crate) cherry_pick_head: bool,
    pub(crate) bisect_log: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StepMarkers {
    pub(crate) current: Option<Vec<u8>>,
    pub(crate) total: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ApplyMarkers {
    pub(crate) step: StepMarkers,
    /// The `rebasing` file exists.
    pub(crate) rebasing: bool,
    /// The `applying` file exists.
    pub(crate) applying: bool,
}

/// The largest operation marker file content read.
pub(crate) const MAXIMUM_MARKER_BYTES: usize = 32;

/// `# branch.*` headers from `git status --porcelain=v2 --branch`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StatusHeaders {
    /// The full commit id, or `None` for an unborn branch.
    pub(crate) oid: Option<Arc<str>>,
    /// The branch name, or `None` when `HEAD` is detached.
    pub(crate) branch: Option<Arc<str>>,
    pub(crate) upstream: Option<Arc<str>>,
    pub(crate) divergence: Option<Divergence>,
}

/// One parsed `git status --porcelain=v2 --branch -z` stream.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PorcelainSummary {
    pub(crate) headers: StatusHeaders,
    pub(crate) changes: ChangeSummary,
}

/// The repository configuration Repository Status and Pull Request lookup need.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RepositoryConfig {
    pub(crate) fsmonitor: FsmonitorPolicy,
    /// `remote.<name>.url` pairs in configuration order.
    pub(crate) remotes: Vec<(Arc<str>, Arc<str>)>,
    pub(crate) push_default: Option<Arc<str>>,
    /// `push.default` is `upstream` or `tracking`, so a plain `git push` publishes the branch to
    /// its merge target rather than to its own name.
    pub(crate) push_to_upstream: bool,
    pub(crate) branch_remote: Option<Arc<str>>,
    pub(crate) branch_merge: Option<Arc<str>>,
    pub(crate) branch_push_remote: Option<Arc<str>>,
}

/// A repository found by a probe, without its change counts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProbedRepository {
    pub(crate) root: RepositoryRoot,
    /// Per-worktree git directory and shared common directory, for watching.
    pub(crate) git_directory: RepositoryRoot,
    pub(crate) common_directory: RepositoryRoot,
    pub(crate) head: RepositoryHead,
    /// Abbreviated commit id, or `None` for an unborn branch.
    pub(crate) commit: Option<Arc<str>>,
    pub(crate) upstream: Option<Upstream>,
    pub(crate) operation: Option<RepositoryOperation>,
    pub(crate) config: RepositoryConfig,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProbeOutcome {
    NotRepository,
    /// A repository Repository Status deliberately does not show: bare, inside a git directory,
    /// or rooted at home while the directory is below home.
    Hidden,
    Repository(Box<ProbedRepository>),
}

/// An open or draft GitHub pull request for a branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PullRequest {
    pub(crate) number: u32,
    /// Sanitized for display.
    pub(crate) title: Arc<str>,
    pub(crate) draft: bool,
    /// An `https` URL on the queried host.
    pub(crate) url: Arc<str>,
    pub(crate) head: Arc<str>,
    pub(crate) base: Arc<str>,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// Content-free Pull Request lookup failure.
pub(crate) enum PullRequestError {
    #[error("the GitHub CLI is unavailable")]
    ToolMissing,
    #[error("the GitHub CLI is not logged in")]
    NotLoggedIn,
    #[error("the Pull Request lookup failed")]
    Unavailable,
    #[error("the GitHub CLI returned an invalid response")]
    InvalidResponse,
    #[error("the Pull Request lookup was cancelled")]
    Cancelled,
}

/// The oldest git with `--no-optional-locks`.
pub(crate) const MINIMUM_GIT_VERSION: ToolVersion = ToolVersion {
    major: 2,
    minor: 15,
    patch: 0,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum GitHubCliStatus {
    #[default]
    Unknown,
    SignedIn,
    SignedOut,
    NotFound,
    Unavailable,
}

/// Change counts for one repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ChangeState {
    /// No count has finished, and none is pending long enough to announce.
    NotCounted,
    /// A first count has run longer than the counting indicator delay.
    Counting,
    Known(ChangeSummary),
    /// Branch and upstream facts remain current when change counting fails.
    Unavailable(RepositoryReadError),
}

/// Whether the presented facts are current or retained from an earlier read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Freshness {
    Current,
    /// Presented dimmed after a failed read, stopped Pane or lost server connection.
    LastKnown {
        as_of: Instant,
    },
}

/// Everything Repository Status presents for one repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepositoryStatus {
    pub(crate) key: RepositoryKey,
    pub(crate) head: RepositoryHead,
    pub(crate) commit: Option<Arc<str>>,
    pub(crate) upstream: Option<Upstream>,
    pub(crate) operation: Option<RepositoryOperation>,
    pub(crate) changes: ChangeState,
    pub(crate) freshness: Freshness,
    pub(crate) read_at: Instant,
    pub(crate) pull_request: Option<PullRequest>,
    /// The most recent read failure, cleared by the next successful read.
    pub(crate) read_failure: Option<RepositoryReadError>,
}

/// What one Pane or sidebar row presents.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum RepositoryView {
    #[default]
    Hidden,
    Repository(Arc<RepositoryStatus>),
}

// Native boundary -------------------------------------------------------------------------------

/// One program run. Policy chooses every argument and environment entry; the runner adds
/// nothing from the ambient environment.
#[derive(Clone)]
pub(crate) struct ProgramRequest {
    pub(crate) executable: PathBuf,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) directory: PathBuf,
    pub(crate) environment: Vec<(OsString, OsString)>,
    /// `None` streams without a byte limit.
    pub(crate) stdout_limit: Option<usize>,
    pub(crate) deadline: Option<Instant>,
    /// After a normal exit, leave the process group alone so a daemon the program started, such
    /// as Git's built-in fsmonitor, survives. Cancellation and timeout still end the group.
    pub(crate) keep_process_group_on_exit: bool,
}

impl fmt::Debug for ProgramRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProgramRequest(<redacted>)")
    }
}

/// `PATH` for a program run: the program's own directory, then the system directories.
pub(crate) fn tool_search_path(executable: &Path) -> OsString {
    let mut path = OsString::new();
    if let Some(directory) = executable
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
    {
        path.push(directory.as_os_str());
        path.push(":");
    }
    path.push("/usr/bin:/bin");
    path
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProgramExit {
    pub(crate) code: Option<i32>,
}

impl ProgramExit {
    pub(crate) fn success(self) -> bool {
        self.code == Some(0)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum ProgramError {
    #[error("the program was not found")]
    NotFound,
    #[error("the program run was cancelled")]
    Cancelled,
    #[error("the program run exceeded its deadline")]
    TimedOut,
    #[error("the program output exceeded its limit")]
    OutputTooLarge,
    #[error("the program could not be run")]
    Failed,
}

/// Runs programs for Repository Status. Blocking; callers use a dedicated thread.
pub(crate) trait RepositoryProgramRunner: Send + Sync + 'static {
    /// Runs one program with a cleared environment in a private process group, feeding stdout to
    /// `stdout` as it arrives. Stderr is discarded. Cancellation and the deadline end the whole
    /// process group before returning.
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &CancellationToken,
    ) -> Result<ProgramExit, ProgramError>;
}

/// The program paths host discovery found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ToolInventory {
    pub(crate) git: Option<PathBuf>,
    pub(crate) github_cli: Option<PathBuf>,
}

/// Finds git and the GitHub CLI without running them.
pub(crate) trait RepositoryToolDiscovery: Send + Sync + 'static {
    fn discover(&self) -> ToolInventory;
}

/// Reads operation marker files inside a git directory that a local probe reported.
pub(crate) trait RepositoryMarkerReader: Send + Sync + 'static {
    /// Reads the markers below `git_directory`, never following symbolic links.
    fn read(&self, git_directory: &Path) -> OperationMarkers;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WatchDirectory {
    pub(crate) path: PathBuf,
    pub(crate) recursive: bool,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("the repository could not be watched")]
pub(crate) struct RepositoryWatchError;

/// Dropping the value stops watching.
pub(crate) trait RepositoryWatch: Send {}

/// Watches local git directories, never a work tree.
pub(crate) trait RepositoryWatcher: Send + Sync + 'static {
    /// Calls `changed` after any change, and after any watch error, below `directories`.
    fn watch(
        &self,
        directories: Vec<WatchDirectory>,
        changed: Box<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn RepositoryWatch>, RepositoryWatchError>;
}

/// Called with the paths a watch set saw change. An empty list means the paths are unknown, as
/// after a watch error.
pub(crate) type ChangedPaths = Box<dyn Fn(&[PathBuf]) + Send + Sync>;

/// One watcher, and one event thread, for every directory its owner follows. Dropping it stops
/// every watch.
pub(crate) trait WatchSet: Send {
    fn watch(&mut self, directory: &WatchDirectory) -> Result<(), RepositoryWatchError>;
    fn unwatch(&mut self, path: &Path);
}

/// Starts watch sets. A server follows every repository its subscribers see through one set.
pub(crate) trait WatchSets: Send + Sync + 'static {
    fn start(&self, changed: ChangedPaths) -> Result<Box<dyn WatchSet>, RepositoryWatchError>;
}

pub(crate) use spaceterm_server::repository::{
    ChangeEntry, ChangeKind, ChangeSummary, ChangeTotal, Divergence, FsmonitorPolicy,
    GitHubRepository, GitToolStatus, MAXIMUM_CHANGE_ENTRIES, OperationStep, RepositoryHead,
    RepositoryOperation, RepositoryReadError, ToolVersion, Upstream,
};
