//! Repository facts and content-free execution contracts.

use super::CancellationToken;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

/// The machine whose filesystem a repository lives on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RepositoryMachine {
    /// A repository or branch on the owning host.
    Local,
    /// An opaque remote machine, path, or fetched branch spelling.
    Remote(RemoteMachineKey),
}

/// Opaque identity of one remote machine and account, as reached through a Control Connection.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct RemoteMachineKey(Arc<str>);

impl RemoteMachineKey {
    /// Construct an opaque remote machine identity.
    pub fn new(value: impl Into<Arc<str>>) -> Self {
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
pub enum RepositoryRoot {
    /// A repository or branch on the owning host.
    Local(PathBuf),
    /// An opaque remote machine, path, or fetched branch spelling.
    Remote(Arc<str>),
}

impl RepositoryRoot {
    /// The root spelling for presentation only.
    pub fn display(&self) -> std::borrow::Cow<'_, str> {
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
pub struct RepositoryKey {
    /// The machine whose filesystem owns this repository.
    pub machine: RepositoryMachine,
    /// The repository root on that machine.
    pub root: RepositoryRoot,
}

/// What `HEAD` names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepositoryHead {
    /// The checked-out local branch.
    Branch(Arc<str>),
    /// A detached `HEAD`, carrying the abbreviated commit id.
    Detached(Arc<str>),
    /// A branch with no commits yet.
    Unborn(Arc<str>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Local and upstream commit counts.
pub struct Divergence {
    /// Commits present locally but not upstream.
    pub ahead: u32,
    /// Commits present upstream but not locally.
    pub behind: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// The configured upstream branch and its divergence.
pub struct Upstream {
    /// The upstream's short name, such as `origin/main`.
    pub name: Arc<str>,
    /// `None` when the configured upstream no longer exists.
    pub divergence: Option<Divergence>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Progress of an unfinished Git operation.
pub struct OperationStep {
    /// Current operation step or marker contents.
    pub current: u32,
    /// Total operation steps or changed paths.
    pub total: u32,
}

/// An unfinished multi-step git operation, in Starship's `git_state` precedence order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryOperation {
    /// A rebase is unfinished.
    Rebasing {
        /// Step facts, when both marker files contain valid numbers.
        step: Option<OperationStep>,
    },
    /// `git am`.
    Applying {
        /// Step facts, when both marker files contain valid numbers.
        step: Option<OperationStep>,
    },
    /// A merge is unfinished.
    Merging,
    /// A revert is unfinished.
    Reverting,
    /// A cherry-pick is unfinished.
    CherryPicking,
    /// A bisect is unfinished.
    Bisecting,
}

/// Raw presence and contents of the operation marker files in a git directory.
///
/// Contents are at most [`MAXIMUM_MARKER_BYTES`] and are parsed, never displayed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OperationMarkers {
    /// `rebase-merge/` with its `msgnum` and `end` files.
    pub rebase_merge: Option<StepMarkers>,
    /// `rebase-apply/` with its `next` and `last` files and its mode files.
    pub rebase_apply: Option<ApplyMarkers>,
    /// A MERGE_HEAD marker exists.
    pub merge_head: bool,
    /// A REVERT_HEAD marker exists.
    pub revert_head: bool,
    /// A CHERRY_PICK_HEAD marker exists.
    pub cherry_pick_head: bool,
    /// A BISECT_LOG marker exists.
    pub bisect_log: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// Bounded raw progress marker contents.
pub struct StepMarkers {
    /// Current operation step or marker contents.
    pub current: Option<Vec<u8>>,
    /// Total operation steps or changed paths.
    pub total: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// Progress and mode markers for rebase-apply.
pub struct ApplyMarkers {
    /// Step facts, when both marker files contain valid numbers.
    pub step: StepMarkers,
    /// The `rebasing` file exists.
    pub rebasing: bool,
    /// The `applying` file exists.
    pub applying: bool,
}

/// The largest operation marker file content read.
pub const MAXIMUM_MARKER_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The kind of change Git reported.
pub enum ChangeKind {
    /// A path has staged changes.
    Staged,
    /// A tracked path has unstaged changes.
    Modified,
    /// A tracked path was deleted.
    Deleted,
    /// A tracked path was renamed.
    Renamed,
    /// A path has unresolved conflicts.
    Conflicted,
    /// A path is untracked.
    Untracked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One bounded changed-path presentation entry.
pub struct ChangeEntry {
    /// The kind of change.
    pub kind: ChangeKind,
    /// Repository-relative path, sanitized for display.
    pub path: Arc<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// An exact or lower-bound changed-path count.
pub enum ChangeTotal {
    /// The complete changed-path count.
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
pub struct ChangeSummary {
    /// Total operation steps or changed paths.
    pub total: ChangeTotal,
    /// Number of paths with staged changes.
    pub staged: u32,
    /// Number of modified paths.
    pub modified: u32,
    /// Number of deleted paths.
    pub deleted: u32,
    /// Number of renamed paths.
    pub renamed: u32,
    /// Number of conflicted paths.
    pub conflicted: u32,
    /// Number of untracked paths.
    pub untracked: u32,
    /// The first [`MAXIMUM_CHANGE_ENTRIES`] records, in git's order.
    pub entries: Vec<ChangeEntry>,
}

/// The most changed paths retained for the Repository Status popover.
pub const MAXIMUM_CHANGE_ENTRIES: usize = 20;

/// `# branch.*` headers from `git status --porcelain=v2 --branch`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StatusHeaders {
    /// The full commit id, or `None` for an unborn branch.
    pub oid: Option<Arc<str>>,
    /// The branch name, or `None` when `HEAD` is detached.
    pub branch: Option<Arc<str>>,
    /// The configured upstream branch.
    pub upstream: Option<Arc<str>>,
    /// Commits ahead of and behind the upstream.
    pub divergence: Option<Divergence>,
}

/// One parsed `git status --porcelain=v2 --branch -z` stream.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PorcelainSummary {
    /// Branch and upstream facts.
    pub headers: StatusHeaders,
    /// Bounded change counts and entries.
    pub changes: ChangeSummary,
}

/// Whether a read may use Git's built-in fsmonitor daemon.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FsmonitorPolicy {
    #[default]
    /// Never run an fsmonitor hook or daemon.
    Disabled,
    /// Allow only Git's built-in fsmonitor daemon.
    Builtin,
}

/// The repository configuration Repository Status and Pull Request lookup need.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepositoryConfig {
    /// The explicitly selected fsmonitor policy.
    pub fsmonitor: FsmonitorPolicy,
    /// `remote.<name>.url` pairs in configuration order.
    pub remotes: Vec<(Arc<str>, Arc<str>)>,
    /// Default remote selected for push operations.
    pub push_default: Option<Arc<str>>,
    /// `push.default` is `upstream` or `tracking`, so a plain `git push` publishes the branch to
    /// its merge target rather than to its own name.
    pub push_to_upstream: bool,
    /// Remote configured for the current branch.
    pub branch_remote: Option<Arc<str>>,
    /// Full ref configured as the current branch's merge target.
    pub branch_merge: Option<Arc<str>>,
    /// Push remote configured for the current branch.
    pub branch_push_remote: Option<Arc<str>>,
}

/// A repository found by a probe, without its change counts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbedRepository {
    /// The repository root on that machine.
    pub root: RepositoryRoot,
    /// Per-worktree git directory and shared common directory, for watching.
    pub git_directory: RepositoryRoot,
    /// The shared administrative directory for all Worktrees.
    pub common_directory: RepositoryRoot,
    /// The branch or detached commit currently checked out.
    pub head: RepositoryHead,
    /// Abbreviated commit id, or `None` for an unborn branch.
    pub commit: Option<Arc<str>>,
    /// The configured upstream branch.
    pub upstream: Option<Upstream>,
    /// The unfinished Git operation, if any.
    pub operation: Option<RepositoryOperation>,
    /// Bounded configuration used by Repository Status.
    pub config: RepositoryConfig,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Repository discovery classification.
pub enum ProbeOutcome {
    /// Git found no repository at the directory.
    NotRepository,
    /// A repository Repository Status deliberately does not show: bare, inside a git directory,
    /// or rooted at home while the directory is below home.
    Hidden,
    /// Git found a visible work tree repository.
    Repository(Box<ProbedRepository>),
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// Content-free Repository Status read failure.
pub enum RepositoryReadError {
    #[error("git is unavailable")]
    /// Git is unavailable.
    ToolMissing,
    #[error("the repository could not be read")]
    /// The repository could not be read.
    Unavailable,
    #[error("the repository read exceeded its deadline")]
    /// The repository read exceeded its deadline.
    TimedOut,
    #[error("the repository read was cancelled")]
    /// The repository read was cancelled.
    Cancelled,
    #[error("the repository read exceeded its output limit")]
    /// The repository read exceeded its output limit.
    OutputTooLarge,
    #[error("the repository read returned an invalid response")]
    /// The repository read returned an invalid response.
    InvalidResponse,
    #[error("the Control Connection is unavailable")]
    /// The Control Connection is unavailable.
    ConnectionUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// A parsed program version.
pub struct ToolVersion {
    /// Major version number.
    pub major: u16,
    /// Minor version number.
    pub minor: u16,
    /// Patch version number.
    pub patch: u16,
}

/// The oldest git with `--no-optional-locks`.
pub const MINIMUM_GIT_VERSION: ToolVersion = ToolVersion {
    major: 2,
    minor: 15,
    patch: 0,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Git availability and version compatibility.
pub enum GitToolStatus {
    #[default]
    /// No tool check has completed.
    Unknown,
    /// Git meets the minimum supported version.
    Ready(ToolVersion),
    /// Git is installed but older than the supported version.
    TooOld(ToolVersion),
    /// The program is not installed or executable.
    NotFound,
}

/// One program run. Policy chooses every argument and environment entry; the runner adds
/// nothing from the ambient environment.
#[derive(Clone)]
pub struct ProgramRequest {
    /// Discovered executable to run directly.
    pub executable: PathBuf,
    /// Validated command-line arguments, without a shell.
    pub arguments: Vec<OsString>,
    /// Host-local working directory.
    pub directory: PathBuf,
    /// The complete environment; ambient values are not inherited.
    pub environment: Vec<(OsString, OsString)>,
    /// `None` streams without a byte limit.
    pub stdout_limit: Option<usize>,
    /// Latest allowed completion time, absent for an unlimited run.
    pub deadline: Option<Instant>,
    /// After a normal exit, leave the process group alone so a daemon the program started, such
    /// as Git's built-in fsmonitor, survives. Cancellation and timeout still end the group.
    pub keep_process_group_on_exit: bool,
}

impl fmt::Debug for ProgramRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProgramRequest(<redacted>)")
    }
}

/// `PATH` for a program run: the program's own directory, then the system directories.
pub fn tool_search_path(executable: &Path) -> OsString {
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
/// Content-free program exit status.
pub struct ProgramExit {
    /// Exit code, absent when terminated by a signal.
    pub code: Option<i32>,
}

impl ProgramExit {
    /// Whether the program exited with status zero.
    pub fn success(self) -> bool {
        self.code == Some(0)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
/// Content-free failure to run a program.
pub enum ProgramError {
    #[error("the program was not found")]
    /// The program is not installed or executable.
    NotFound,
    #[error("the program run was cancelled")]
    /// The program run was cancelled.
    Cancelled,
    #[error("the program run exceeded its deadline")]
    /// The program run exceeded its deadline.
    TimedOut,
    #[error("the program output exceeded its limit")]
    /// The program output exceeded its limit.
    OutputTooLarge,
    #[error("the program could not be run")]
    /// The program could not be run.
    Failed,
}

/// Runs programs for Repository Status. Blocking; callers use a dedicated thread.
pub trait RepositoryProgramRunner: Send + Sync + 'static {
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

/// Reads operation marker files inside a git directory that a local probe reported.
pub trait RepositoryMarkerReader: Send + Sync + 'static {
    /// Reads the markers below `git_directory`, never following symbolic links.
    fn read(&self, git_directory: &Path) -> OperationMarkers;
}
