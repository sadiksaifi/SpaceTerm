//! Server-wide Git worker ownership, durable intent ordering, and physical-repository lanes.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use thiserror::Error;

use super::filesystem::native::Directory;
use super::filesystem::{DirectoryError, RetainedDirectory, WorktreeFilesystem};
use super::listing::WorktreeHead;
use super::{
    BranchList, CancellationToken, FsmonitorPolicy, LocalRepositoryReader, LocalWorktreeGit,
    PorcelainSummary, ProbeOutcome, RemovalCheck, RemovalExpectation, RepositoryReadError,
    WorktreeBranch, WorktreeCreateError, WorktreeRemoveError, WorktreeSnapshot,
};

const MAX_WRITES: usize = 64;
const QUEUE_POLL: Duration = Duration::from_millis(20);

/// A checked repository with retained main root and common-directory identities.
/// The main root is the bare repository directory when its Worktrees are all linked.
///
/// Location aliases and linked Worktrees use the same physical common-directory write lane.
/// This value is host-only authority, not a wire or durable representation.
#[derive(Clone)]
pub struct GitRepository {
    main: Arc<Directory>,
    common: PhysicalRepository,
}

impl fmt::Debug for GitRepository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GitRepository(<redacted>)")
    }
}

#[derive(Clone)]
struct PhysicalRepository(Arc<Directory>);

impl PartialEq for PhysicalRepository {
    fn eq(&self, other: &Self) -> bool {
        self.0.identity == other.0.identity
    }
}

impl Eq for PhysicalRepository {}

impl Hash for PhysicalRepository {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.identity.hash(state);
    }
}

/// A validated operation request. Removal retains the exact confirmation's native identity.
#[derive(Clone)]
pub enum GitWrite {
    /// Create a Worktree after validating its branch and current repository state.
    Create {
        /// The server-host destination selected by the caller.
        path: PathBuf,
        /// The branch selected by the caller.
        branch: WorktreeBranch,
    },
    /// Remove a Worktree without deleting its branch.
    Remove {
        /// The exact server-host location that was confirmed.
        path: PathBuf,
        /// Explicit permission to discard changes or remove checked-out submodules.
        force: bool,
        /// The physical identity and cleanliness policy confirmed by the caller.
        expected: RemovalExpectation,
    },
}

impl fmt::Debug for GitWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Create { .. } => "GitWrite::Create(<redacted>)",
            Self::Remove { .. } => "GitWrite::Remove(<redacted>)",
        })
    }
}

/// External effects to retain with the caller's original durable operation identity.
///
/// Native confirmation proofs are deliberately absent. After a forced stop they cannot be
/// reconstructed or treated as fresh confirmation. Reconciliation only inspects external state.
#[derive(Clone, Eq, PartialEq)]
pub enum DurableGitWrite {
    /// Intended creation and branch checkout.
    Create {
        /// Server-host destination.
        path: PathBuf,
        /// Requested local branch and starting point.
        branch: WorktreeBranch,
    },
    /// Intended removal with its original force decision.
    Remove {
        /// Server-host location selected for removal.
        path: PathBuf,
        /// Whether removal was explicitly forced.
        force: bool,
    },
}

impl fmt::Debug for DurableGitWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Create { .. } => "DurableGitWrite::Create(<redacted>)",
            Self::Remove { .. } => "DurableGitWrite::Remove(<redacted>)",
        })
    }
}

/// Durable intent data, supplied to the caller before the external write starts.
#[derive(Clone, Eq, PartialEq)]
pub struct PendingWriteIntent {
    /// Main Worktree or bare repository location on the server host.
    pub main_root: PathBuf,
    /// Shared Git directory location on the server host.
    pub common_directory: PathBuf,
    /// The intended external effects, without native authority.
    pub write: DurableGitWrite,
}

impl fmt::Debug for PendingWriteIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingWriteIntent")
            .field("write", &self.write)
            .finish_non_exhaustive()
    }
}

/// Content-free failure to durably record an intent or outcome.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("the Git operation record commit was not confirmed")]
pub struct IntentError;

/// The server's durable-operation boundary. Callbacks run on the write worker.
///
/// `record` must commit pending intent before returning success. `finish` must retain the final
/// outcome under the same caller-bound operation identity. Neither callback may report raw
/// storage errors. A failed intent prevents execution. A failed outcome leaves the durable
/// outcome unconfirmed for reconciliation; it must not cause a retry of the external write.
/// A callback error is not proof of rollback. Preserve the original operation identity and
/// resolve its durable record before admitting more effects under that identity.
/// The server must deduplicate operation identities before calling `write`. It must also
/// revalidate affected Workspace/Session generations when `record` runs in the write lane,
/// and retain its destructive-assessment reservation through the final outcome. Checking only
/// before queuing is insufficient because another operation can run while the write waits.
pub trait WriteIntentHook: Send + Sync + 'static {
    /// Durably record intent. No mutating external Git command has run yet.
    fn record(&self, intent: &PendingWriteIntent) -> Result<(), IntentError>;
    /// Durably record the observed final outcome while the repository lane is still held.
    fn finish(
        &self,
        intent: &PendingWriteIntent,
        outcome: GitWriteOutcome,
    ) -> Result<(), IntentError>;
}

/// Content-free final write outcome. A rejected or uncertain write can have partial effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitWriteOutcome {
    /// Git successfully created the Worktree.
    Created,
    /// Git successfully removed the Worktree.
    Removed,
    /// Creation did not complete successfully; reconciliation may still be required.
    CreateRejected(WorktreeCreateError),
    /// Removal did not complete successfully; reconciliation may still be required.
    RemoveRejected(WorktreeRemoveError),
    /// The retained repository or its Git mapping changed before execution.
    RepositoryChanged,
    /// The repository could not be rechecked. No mutating Git command was started.
    RepositoryUnavailable,
    /// A worker failed without establishing a final external outcome.
    Uncertain,
}

/// Final external outcome and whether its durable recording succeeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteCompletion {
    /// The result observed from Git, never terminal output or a native error.
    pub outcome: GitWriteOutcome,
    /// Failure here means the durable outcome is unconfirmed, not permission to execute again.
    pub recording: Result<(), IntentError>,
}

/// Content-free failure before an external write starts.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WriteError {
    /// Ordinary cancellation was observed before this write started.
    #[error("the queued Git write was cancelled")]
    Cancelled,
    /// The coordinator is shutting down or has stopped.
    #[error("Git write admission is closed")]
    ShuttingDown,
    /// The server's bounded pending-write capacity is exhausted.
    #[error("Git write capacity is exhausted")]
    Capacity,
    /// The host could not create a worker thread.
    #[error("the Git write worker is unavailable")]
    WorkerUnavailable,
    /// Intent commit was not confirmed. No external write was started.
    #[error("the Git write intent commit was not confirmed")]
    IntentUnconfirmed,
}

/// Join an admitted write even after the requesting connection or client has gone away.
///
/// Dropping a ticket does not cancel its write. The coordinator retains worker ownership.
pub struct WriteTicket(mpsc::Receiver<Result<WriteCompletion, WriteError>>);

impl WriteTicket {
    /// Wait for completion and the final durable-outcome callback.
    pub fn join(self) -> Result<WriteCompletion, WriteError> {
        self.0.recv().unwrap_or(Ok(WriteCompletion {
            outcome: GitWriteOutcome::Uncertain,
            recording: Err(IntentError),
        }))
    }
}

/// A read-only inspection after forced termination. None of these values authorizes a retry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reconciliation {
    /// Current Git and filesystem state matches the intended result.
    ObservedApplied,
    /// Current state shows no requested creation or new branch.
    ObservedNotApplied,
    /// State is partial, replaced, or cannot distinguish the original operation.
    NeedsReview,
}

/// One coordinator per SpaceTerm Server, shared across every Workspace and connection.
///
/// Reads do not acquire write lanes. Writes serialize by retained physical common-directory
/// identity and remain owned through their final durable-outcome callback. Call `shutdown`
/// during orderly server shutdown, including before an async runtime stops its blocking workers.
#[derive(Clone)]
pub struct GitCoordinator(Arc<Inner>);

struct Inner {
    worktrees: Arc<LocalWorktreeGit>,
    repositories: Arc<LocalRepositoryReader>,
    filesystem: Arc<dyn WorktreeFilesystem>,
    cancellation: CancellationToken,
    state: Mutex<State>,
    drained: Condvar,
}

#[derive(Default)]
struct State {
    closing: bool,
    draining: bool,
    stopped: bool,
    reads: usize,
    writes: usize,
    workers: Vec<JoinHandle<()>>,
    lanes: HashMap<PhysicalRepository, Weak<Lane>>,
}

#[derive(Default)]
struct Lane {
    busy: Mutex<bool>,
    changed: Condvar,
}

struct LaneLease(Arc<Lane>);

impl Drop for LaneLease {
    fn drop(&mut self) {
        *lock(&self.0.busy) = false;
        self.0.changed.notify_all();
    }
}

struct ReadLease(Arc<Inner>);

impl Drop for ReadLease {
    fn drop(&mut self) {
        lock(&self.0.state).reads -= 1;
        self.0.drained.notify_all();
    }
}

impl GitCoordinator {
    /// Construct the server's shared coordinator with injected execution and filesystem adapters.
    pub fn new(
        worktrees: Arc<LocalWorktreeGit>,
        repositories: Arc<LocalRepositoryReader>,
        filesystem: Arc<dyn WorktreeFilesystem>,
    ) -> Self {
        Self(Arc::new(Inner {
            worktrees,
            repositories,
            filesystem,
            cancellation: CancellationToken::default(),
            state: Mutex::default(),
            drained: Condvar::new(),
        }))
    }

    fn read<T>(
        &self,
        cancellation: &CancellationToken,
        run: impl FnOnce(&CancellationToken) -> Result<T, RepositoryReadError>,
    ) -> Result<T, RepositoryReadError> {
        {
            let mut state = lock(&self.0.state);
            if state.closing || cancellation.is_cancelled() {
                return Err(RepositoryReadError::Cancelled);
            }
            state.reads += 1;
            state.lanes.retain(|_, lane| lane.strong_count() != 0);
        }
        let _lease = ReadLease(Arc::clone(&self.0));
        run(&CancellationToken::linked(
            cancellation,
            &self.0.cancellation,
        ))
    }

    /// Discover and retain a repository from any of its Worktrees or path aliases.
    pub fn repository(
        &self,
        directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<GitRepository, RepositoryReadError> {
        self.read(cancellation, |cancellation| {
            let snapshot = self
                .0
                .worktrees
                .list(directory, cancellation)?
                .ok_or(RepositoryReadError::Unavailable)?;
            let main = Directory::open(&snapshot.main_root)
                .map_err(|_| RepositoryReadError::Unavailable)?;
            let common = Directory::open(&snapshot.common_directory)
                .map_err(|_| RepositoryReadError::Unavailable)?;
            Ok(GitRepository {
                main: Arc::new(main),
                common: PhysicalRepository(Arc::new(common)),
            })
        })
    }

    /// Read Worktree inventory concurrently with writes and other reads.
    pub fn list(
        &self,
        directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Option<WorktreeSnapshot>, RepositoryReadError> {
        self.read(cancellation, |cancellation| {
            self.0.worktrees.list(directory, cancellation)
        })
    }

    /// Inspect a retained administrative root, including a bare Main repository.
    /// Keep the same cancellation and shutdown ownership as presentation discovery.
    pub fn administrative_inventory(
        &self,
        root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<WorktreeSnapshot, RepositoryReadError> {
        self.read(cancellation, |cancellation| {
            super::server::server_inventory(&self.0.worktrees, root, cancellation)
        })
    }

    /// Read branches without taking a write lane.
    pub fn branches(
        &self,
        root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<BranchList, RepositoryReadError> {
        self.read(cancellation, |cancellation| {
            self.0.worktrees.branches(root, cancellation)
        })
    }

    /// Probe Repository Status without taking a write lane.
    pub fn probe(
        &self,
        directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProbeOutcome, RepositoryReadError> {
        self.read(cancellation, |cancellation| {
            self.0.repositories.probe(directory, cancellation)
        })
    }

    /// Stream bounded Repository Status facts without taking a write lane.
    pub fn count(
        &self,
        root: &Path,
        fsmonitor: FsmonitorPolicy,
        cancellation: &CancellationToken,
    ) -> Result<PorcelainSummary, RepositoryReadError> {
        self.read(cancellation, |cancellation| {
            self.0.repositories.count(root, fsmonitor, cancellation)
        })
    }

    /// Retain the removal assessment's native identity before asking for confirmation.
    pub fn check_removal(
        &self,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<RemovalCheck, WorktreeRemoveError> {
        self.read(cancellation, |cancellation| {
            self.0
                .worktrees
                .check_removal(path, &self.0.filesystem, cancellation)
                .map_err(|_| RepositoryReadError::Unavailable)
        })
        .map_err(|_| WorktreeRemoveError::Unchecked)
    }

    /// Admit a bounded, owned write. Ordinary cancellation affects only its queued phase.
    ///
    /// The caller must revalidate server-wide affected Workspace/Session generations before
    /// admission and supply the exact original force confirmation. The native removal proof is
    /// checked again within the write lane. No server-authority decisions are delegated to Git.
    pub fn write(
        &self,
        repository: &GitRepository,
        write: GitWrite,
        cancellation: &CancellationToken,
        hook: Arc<dyn WriteIntentHook>,
    ) -> Result<WriteTicket, WriteError> {
        let mut state = lock(&self.0.state);
        if state.closing {
            return Err(WriteError::ShuttingDown);
        }
        if cancellation.is_cancelled() {
            return Err(WriteError::Cancelled);
        }
        if state.writes >= MAX_WRITES {
            return Err(WriteError::Capacity);
        }
        state.workers.retain(|worker| !worker.is_finished());
        state.lanes.retain(|_, lane| lane.strong_count() != 0);
        let lane = state
            .lanes
            .get(&repository.common)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let lane = Arc::new(Lane::default());
                state
                    .lanes
                    .insert(repository.common.clone(), Arc::downgrade(&lane));
                lane
            });
        let repository = repository.clone();
        let cancellation = cancellation.clone();
        let inner = Arc::clone(&self.0);
        let (sender, receiver) = mpsc::channel();
        state.writes += 1;
        match thread::Builder::new()
            .name("git-write".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_write(&inner, lane, &repository, write, &cancellation, &*hook)
                }))
                .unwrap_or(Ok(WriteCompletion {
                    outcome: GitWriteOutcome::Uncertain,
                    recording: Err(IntentError),
                }));
                let _ = sender.send(result);
                lock(&inner.state).writes -= 1;
                inner.drained.notify_all();
            }) {
            Ok(worker) => state.workers.push(worker),
            Err(_) => {
                state.writes -= 1;
                return Err(WriteError::WorkerUnavailable);
            }
        }
        Ok(WriteTicket(receiver))
    }

    /// Close admissions, cancel and reap reads, and join every admitted write worker.
    ///
    /// Started writes ignore client cancellation and the shutdown token. Queued writes stop
    /// before committing intent. Concurrent shutdown callers all wait for active work to drain.
    /// No deadline is imposed on a normally finishing external write.
    pub fn shutdown(&self) {
        let workers = {
            let mut state = lock(&self.0.state);
            if state.draining {
                while !state.stopped {
                    state = self
                        .0
                        .drained
                        .wait(state)
                        .unwrap_or_else(|error| error.into_inner());
                }
                return;
            }
            state.draining = true;
            state.closing = true;
            self.0.cancellation.cancel();
            while state.reads != 0 || state.writes != 0 {
                state = self
                    .0
                    .drained
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
            state.lanes.clear();
            std::mem::take(&mut state.workers)
        };
        for worker in workers {
            let _ = worker.join();
        }
        lock(&self.0.state).stopped = true;
        self.0.drained.notify_all();
    }

    /// Inspect Git and filesystem state after forced termination without issuing any write.
    ///
    /// An observed matching state is not proof that this operation caused it. A replacement or
    /// partial write requires review. The caller resolves the original durable operation identity;
    /// it must never automatically resubmit this intent, even when no effect is observed.
    pub fn reconcile(
        &self,
        intent: &PendingWriteIntent,
        cancellation: &CancellationToken,
    ) -> Result<Reconciliation, RepositoryReadError> {
        self.read(cancellation, |cancellation| {
            let snapshot = self.0.worktrees.administrative_inventory(&intent.main_root, cancellation)?;
            if !same_location(&snapshot.common_directory, &intent.common_directory) {
                return Ok(Reconciliation::NeedsReview);
            }
            let (path, branch) = match &intent.write {
                DurableGitWrite::Create { path, branch } => (path, Some(branch)),
                DurableGitWrite::Remove { path, .. } => (path, None),
            };
            let listed = snapshot.worktrees.iter().find(|record| same_location(&record.root, path));
            let absent = match self.0.filesystem.is_absent(path) {
                Ok(absent) => absent,
                Err(_) => return Ok(Reconciliation::NeedsReview),
            };
            let Some(branch) = branch else {
                return Ok(if listed.is_none() && absent {
                    Reconciliation::ObservedApplied
                } else {
                    Reconciliation::NeedsReview
                });
            };
            let name = match branch {
                WorktreeBranch::New { name, .. } | WorktreeBranch::Existing { name }
                | WorktreeBranch::Remote { name, .. } => name,
            };
            if listed.is_some_and(|record| {
                !record.missing && !record.locked
                    && matches!(&record.head, WorktreeHead::Branch(head) if head == name)
            }) && self.0.filesystem.validate_directory(path).is_ok() {
                // A stale worktree-list record and a replacement directory are insufficient.
                // Ask Git at the target to prove that it currently maps to this checkout.
                if let ProbeOutcome::Repository(probed) = self.0.repositories.probe(path, cancellation)?
                    && let (super::RepositoryRoot::Local(root), super::RepositoryRoot::Local(common))
                        = (&probed.root, &probed.common_directory)
                    && same_location(root, path)
                    && same_location(common, &snapshot.common_directory)
                    && matches!(&probed.head, super::RepositoryHead::Branch(head) if &**head == name)
                {
                    return Ok(Reconciliation::ObservedApplied);
                }
            }
            if listed.is_none() && absent {
                let branches = self.0.worktrees.branches(&intent.main_root, cancellation)?;
                if matches!(branch, WorktreeBranch::Existing { .. })
                    || !branches.local.contains(name)
                {
                    return Ok(Reconciliation::ObservedNotApplied);
                }
            }
            Ok(Reconciliation::NeedsReview)
        })
    }
}

fn run_write(
    inner: &Arc<Inner>,
    lane: Arc<Lane>,
    repository: &GitRepository,
    write: GitWrite,
    cancellation: &CancellationToken,
    hook: &dyn WriteIntentHook,
) -> Result<WriteCompletion, WriteError> {
    let _lane = {
        let mut busy = lock(&lane.busy);
        loop {
            if lock(&inner.state).closing {
                return Err(WriteError::ShuttingDown);
            }
            if cancellation.is_cancelled() {
                return Err(WriteError::Cancelled);
            }
            if !*busy {
                // Admission closure and the queued-to-started transition are ordered by the
                // same lock. Cancellation after this point cannot stop the durable operation.
                let state = lock(&inner.state);
                if state.closing {
                    return Err(WriteError::ShuttingDown);
                }
                *busy = true;
                break;
            }
            busy = lane
                .changed
                .wait_timeout(busy, QUEUE_POLL)
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
        drop(busy);
        LaneLease(lane)
    };
    let durable_write = match &write {
        GitWrite::Create { path, branch } => DurableGitWrite::Create {
            path: path.clone(),
            branch: branch.clone(),
        },
        GitWrite::Remove { path, force, .. } => DurableGitWrite::Remove {
            path: path.clone(),
            force: *force,
        },
    };
    let intent = PendingWriteIntent {
        main_root: repository.main.path().to_owned(),
        common_directory: repository.common.0.path().to_owned(),
        write: durable_write,
    };
    hook.record(&intent)
        .map_err(|_| WriteError::IntentUnconfirmed)?;
    // This token is deliberately unrelated to connection lifetime and orderly shutdown.
    let uncancelled = CancellationToken::default();
    let outcome = match revalidate_repository(inner, repository, &uncancelled) {
        Err(outcome) => outcome,
        Ok(()) => match write {
            GitWrite::Create { path, branch } => {
                match inner
                    .worktrees
                    .create(repository.main.path(), &path, &branch, &uncancelled)
                {
                    Ok(()) => GitWriteOutcome::Created,
                    Err(error) => GitWriteOutcome::CreateRejected(error),
                }
            }
            GitWrite::Remove {
                path,
                force,
                expected,
            } => match inner.worktrees.remove(
                repository.main.path(),
                &path,
                force,
                &expected,
                &uncancelled,
            ) {
                Ok(()) => GitWriteOutcome::Removed,
                Err(error) => GitWriteOutcome::RemoveRejected(error),
            },
        },
    };
    Ok(WriteCompletion {
        outcome,
        recording: hook.finish(&intent, outcome),
    })
}

fn revalidate_repository(
    inner: &Inner,
    repository: &GitRepository,
    cancellation: &CancellationToken,
) -> Result<(), GitWriteOutcome> {
    repository
        .main
        .revalidate()
        .map_err(directory_validation_outcome)?;
    repository
        .common
        .0
        .revalidate()
        .map_err(directory_validation_outcome)?;
    let snapshot = inner
        .worktrees
        .administrative_inventory(repository.main.path(), cancellation)
        .map_err(|_| GitWriteOutcome::RepositoryUnavailable)?;
    // Reuse retained authority instead of opening more leases from the admission budget.
    repository
        .common
        .0
        .revalidate_at(&snapshot.common_directory)
        .map_err(directory_validation_outcome)?;
    repository
        .main
        .revalidate_at(&snapshot.main_root)
        .map_err(directory_validation_outcome)
}

fn directory_validation_outcome(error: DirectoryError) -> GitWriteOutcome {
    match error {
        DirectoryError::Missing
        | DirectoryError::NotDirectory
        | DirectoryError::IdentityChanged => GitWriteOutcome::RepositoryChanged,
        DirectoryError::Capacity | DirectoryError::Unavailable => {
            GitWriteOutcome::RepositoryUnavailable
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

// Missing Worktrees still need parent-alias normalization (for example /var on macOS).
// Do not equate failed observations or follow a dangling final link.
pub(super) fn same_location(left: &Path, right: &Path) -> bool {
    match (physical_location(left), physical_location(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn physical_location(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    match std::fs::canonicalize(path) {
        Ok(path) => Some(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling link is a present replacement, not an absent target.
            if std::fs::symlink_metadata(path).is_ok() {
                return None;
            }
            let name = path.file_name()?;
            Some(physical_location(path.parent()?)?.join(name))
        }
        Err(_) => None,
    }
}
