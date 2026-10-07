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
pub(crate) mod remote_read;
pub(crate) mod remote_url;
pub(crate) mod scheduler;
#[cfg(test)]
mod testing;
pub(crate) mod tool_check;
pub(crate) mod tools;

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use thiserror::Error;

use crate::domain::RemoteDirectory;
use crate::ssh::cancellation::SshCancellationToken;

mod preferences;

pub(crate) use preferences::RepositoryStatusPreferences;

/// The machine whose filesystem a repository lives on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RepositoryMachine {
    Local,
    Remote(RemoteMachineKey),
}

/// Opaque identity of one remote machine and account, as reached through a Control Connection.
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

/// What `HEAD` names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RepositoryHead {
    Branch(Arc<str>),
    /// A detached `HEAD`, carrying the abbreviated commit id.
    Detached(Arc<str>),
    /// A branch with no commits yet.
    Unborn(Arc<str>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Divergence {
    pub(crate) ahead: u32,
    pub(crate) behind: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Upstream {
    /// The upstream's short name, such as `origin/main`.
    pub(crate) name: Arc<str>,
    /// `None` when the configured upstream no longer exists.
    pub(crate) divergence: Option<Divergence>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OperationStep {
    pub(crate) current: u32,
    pub(crate) total: u32,
}

/// An unfinished multi-step git operation, in Starship's `git_state` precedence order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RepositoryOperation {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChangeKind {
    Staged,
    Modified,
    Deleted,
    Renamed,
    Conflicted,
    Untracked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChangeEntry {
    pub(crate) kind: ChangeKind,
    /// Repository-relative path, sanitized for display.
    pub(crate) path: Arc<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChangeTotal {
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
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ChangeSummary {
    pub(crate) total: ChangeTotal,
    pub(crate) staged: u32,
    pub(crate) modified: u32,
    pub(crate) deleted: u32,
    pub(crate) renamed: u32,
    pub(crate) conflicted: u32,
    pub(crate) untracked: u32,
    /// The first [`MAXIMUM_CHANGE_ENTRIES`] records, in git's order.
    pub(crate) entries: Vec<ChangeEntry>,
}

/// The most changed paths retained for the Repository Status popover.
pub(crate) const MAXIMUM_CHANGE_ENTRIES: usize = 20;

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

/// Whether a read may use Git's built-in fsmonitor daemon.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum FsmonitorPolicy {
    #[default]
    Disabled,
    Builtin,
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

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// Content-free Repository Status read failure.
pub(crate) enum RepositoryReadError {
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
    #[error("the Control Connection is unavailable")]
    ConnectionUnavailable,
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

/// A GitHub repository named by a remote URL: the Pull Request lookup's `--repo` target.
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct GitHubRepository {
    /// Lowercase host name, such as `github.com` or an Enterprise host.
    pub(crate) host: Arc<str>,
    pub(crate) owner: Arc<str>,
    pub(crate) name: Arc<str>,
}

impl fmt::Debug for GitHubRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GitHubRepository(<redacted>)")
    }
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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ToolVersion {
    pub(crate) major: u16,
    pub(crate) minor: u16,
    pub(crate) patch: u16,
}

/// The oldest git with `--no-optional-locks`.
pub(crate) const MINIMUM_GIT_VERSION: ToolVersion = ToolVersion {
    major: 2,
    minor: 15,
    patch: 0,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum GitToolStatus {
    #[default]
    Unknown,
    Ready(ToolVersion),
    TooOld(ToolVersion),
    NotFound,
}

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
}

/// Whether the presented facts are current or retained from an earlier read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Freshness {
    Current,
    /// Presented dimmed: a read failed, the Pane stopped, or the Control Connection is down.
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
        cancellation: &SshCancellationToken,
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

/// Raw, bounded results of the remote repository probe script, parsed by the portable parsers.
#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct RemoteRepositoryProbe {
    pub(crate) outcome: RemoteProbeOutcome,
    /// Raw `git --version` output, for the version-dependent fsmonitor policy and tool status.
    pub(crate) git_version: Vec<u8>,
    /// The discovery `git rev-parse` exited with status 0. Inside a git directory or in a bare
    /// repository it exits non-zero after printing its first lines.
    pub(crate) discovery_succeeded: bool,
    /// `git rev-parse` discovery output, in the same layout the local probe uses.
    pub(crate) discovery: Vec<u8>,
    /// The remote account's physical home directory.
    pub(crate) physical_home: Vec<u8>,
    /// Headers-only `git status --porcelain=v2 --branch -z` output.
    pub(crate) status_headers: Vec<u8>,
    /// `git config -z` records (`key\nvalue\0`), in the `--get-regexp` layout. Only whole
    /// records are kept.
    pub(crate) config: Vec<u8>,
    pub(crate) markers: OperationMarkers,
}

impl fmt::Debug for RemoteRepositoryProbe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteRepositoryProbe")
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RemoteProbeOutcome {
    #[default]
    Repository,
    NotRepository,
    GitMissing,
    DirectoryUnavailable,
}

/// Raw, bounded remote `git status --porcelain=v2 --branch -z` output.
#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct RemoteRepositoryCount {
    pub(crate) status: Vec<u8>,
    /// The output was cut at the remote field limit.
    pub(crate) truncated: bool,
}

impl fmt::Debug for RemoteRepositoryCount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteRepositoryCount")
            .field("truncated", &self.truncated)
            .finish_non_exhaustive()
    }
}

/// Reads repositories on one remote machine through its Control Connection. Blocking.
pub(crate) trait RemoteRepositoryReader: Send + Sync + 'static {
    fn probe(
        &self,
        directory: &RemoteDirectory,
        cancellation: &SshCancellationToken,
    ) -> Result<RemoteRepositoryProbe, RepositoryReadError>;

    fn count(
        &self,
        root: &str,
        fsmonitor: FsmonitorPolicy,
        cancellation: &SshCancellationToken,
    ) -> Result<RemoteRepositoryCount, RepositoryReadError>;
}
