//! Repository Status: the read-only git facts SpaceTerm presents for a directory's repository.
//!
//! Git facts, parsing, and local read policy belong to `spaceterm_host::git`. This desktop facade
//! keeps scheduling, presentation, Control Connection adapters, and client-local Pull Request
//! lookup. Host composition supplies native execution and watching by constructor injection.
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
pub(crate) mod testing;
pub(crate) mod tool_check;
pub(crate) mod tools;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use thiserror::Error;

use crate::domain::RemoteDirectory;
use crate::ssh::cancellation::SshCancellationToken;

mod preferences;

pub(crate) use preferences::RepositoryStatusPreferences;

#[cfg(test)]
pub(crate) use spaceterm_host::git::MINIMUM_GIT_VERSION;

#[cfg(any(test, feature = "developer-tools"))]
pub(crate) use spaceterm_host::git::{ChangeEntry, Divergence};

pub(crate) use spaceterm_host::git::{
    ApplyMarkers, ChangeKind, ChangeSummary, ChangeTotal, FsmonitorPolicy, GitToolStatus,
    MAXIMUM_MARKER_BYTES, OperationMarkers, OperationStep, ProbeOutcome, ProbedRepository,
    ProgramError, ProgramExit, ProgramRequest, RemoteMachineKey, RepositoryConfig, RepositoryHead,
    RepositoryKey, RepositoryMachine, RepositoryMarkerReader, RepositoryOperation,
    RepositoryProgramRunner, RepositoryReadError, RepositoryRoot, StepMarkers, ToolVersion,
    Upstream, tool_search_path,
};

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
