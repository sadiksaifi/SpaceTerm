//! Two-phase bridge over the one physical-repository serializer. All waits are blocking-worker only.
use super::*;
use spaceterm_core::operations::OperationBinding;
use spaceterm_core::worktrees::GitFailure;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Native checks run inside the repository lane immediately before allowing Git to write.
pub trait FinalWriteValidation: Send + Sync + 'static {
    /// Check retained destination identity and the originally approved deletion categories.
    fn validate(
        &self,
        cancellation: &CancellationToken,
        deadline: Instant,
    ) -> Result<(), GitFailure>;
}
/// Prepared native write holding the existing coordinator's physical repository lane.
pub struct PreparedRepositoryWrite {
    binding: OperationBinding,
    ticket: Option<WriteTicket>,
    start: Option<mpsc::Sender<OperationBinding>>,
    completion: mpsc::Receiver<GitWriteOutcome>,
    failure: Arc<Mutex<Option<GitFailure>>>,
    settled: Option<mpsc::Sender<Result<(), IntentError>>>,
}
/// Executed write with a repository lane retained until the final durable settlement callback.
pub struct ExecutedRepositoryWrite {
    ticket: Option<WriteTicket>,
    settled: Option<mpsc::Sender<Result<(), IntentError>>>,
    outcome: GitWriteOutcome,
}
impl PreparedRepositoryWrite {
    /// Authorize the worker only with the original operation and request binding.
    /// Server composition supplies this after its private durable-start permit is established.
    pub fn execute(
        mut self,
        binding: OperationBinding,
    ) -> Result<ExecutedRepositoryWrite, GitFailure> {
        if binding != self.binding {
            return Err(GitFailure::StaleAssessment);
        }
        self.start
            .take()
            .ok_or(GitFailure::WriteFailed)?
            .send(binding)
            .map_err(|_| GitFailure::WriteFailed)?;
        let outcome = self.completion.recv().map_err(|_| {
            self.failure
                .lock()
                .ok()
                .and_then(|f| *f)
                .unwrap_or(GitFailure::ExternalStateUncertain)
        })?;
        Ok(ExecutedRepositoryWrite {
            ticket: self.ticket.take(),
            settled: self.settled.take(),
            outcome,
        })
    }
}
impl Drop for PreparedRepositoryWrite {
    fn drop(&mut self) {
        // Dropping the unstarted preparation refuses its handshake. It cannot cancel an executed write.
        self.start.take();
        self.settled.take();
        if let Some(ticket) = self.ticket.take() {
            let _ = ticket.join();
        }
    }
}
impl ExecutedRepositoryWrite {
    /// Content-free observed external result. This is not a durable acknowledgement.
    pub fn outcome(&self) -> GitWriteOutcome {
        self.outcome
    }
    /// Release the lane only after final durable recording, or explicitly require recovery.
    pub fn finish(
        mut self,
        recording: Result<(), IntentError>,
    ) -> Result<WriteCompletion, WriteError> {
        if let Some(settled) = self.settled.take() {
            let _ = settled.send(recording);
        }
        self.ticket
            .take()
            .ok_or(WriteError::WorkerUnavailable)?
            .join()
    }
}
impl Drop for ExecutedRepositoryWrite {
    fn drop(&mut self) {
        // Process completion is already observed. Never leave the serializer worker stranded.
        self.settled.take();
        if let Some(ticket) = self.ticket.take() {
            let _ = ticket.join();
        }
    }
}
struct Handshake {
    binding: OperationBinding,
    cancellation: CancellationToken,
    deadline: Instant,
    prepared: mpsc::Sender<PendingWriteIntent>,
    start: Mutex<mpsc::Receiver<OperationBinding>>,
    completed: mpsc::Sender<GitWriteOutcome>,
    settled: Mutex<mpsc::Receiver<Result<(), IntentError>>>,
    validation: Arc<dyn FinalWriteValidation>,
    failure: Arc<Mutex<Option<GitFailure>>>,
}
impl WriteIntentHook for Handshake {
    fn record(&self, intent: &PendingWriteIntent) -> Result<(), IntentError> {
        let validation = self.validation.validate(&self.cancellation, self.deadline);
        let validation = if self.cancellation.is_cancelled() {
            Err(GitFailure::CancelledRead)
        } else if Instant::now() >= self.deadline {
            Err(GitFailure::TimedOutRead)
        } else {
            validation
        };
        if let Err(failure) = validation {
            *self.failure.lock().map_err(|_| IntentError)? = Some(failure);
            return Err(IntentError);
        }
        self.prepared
            .send(intent.clone())
            .map_err(|_| IntentError)?;
        let binding = self
            .start
            .lock()
            .map_err(|_| IntentError)?
            .recv()
            .map_err(|_| IntentError)?;
        if binding != self.binding {
            return Err(IntentError);
        }
        // No shutdown/client cancellation is observed here. A durable Started intent must settle.
        if let Err(failure) = self.validation.validate(
            &CancellationToken::default(),
            Instant::now() + Duration::from_secs(30),
        ) {
            *self.failure.lock().map_err(|_| IntentError)? = Some(failure);
            return Err(IntentError);
        }
        Ok(())
    }
    fn finish(&self, _: &PendingWriteIntent, outcome: GitWriteOutcome) -> Result<(), IntentError> {
        self.completed.send(outcome).map_err(|_| IntentError)?;
        self.settled
            .lock()
            .map_err(|_| IntentError)?
            .recv()
            .map_err(|_| IntentError)?
    }
}
impl GitCoordinator {
    /// Prepare a write using this coordinator's existing lane and a worker-to-actor handshake.
    /// This performs no repository mutation. The returned native intent must match the actor's intent.
    pub fn prepare_server_write(
        &self,
        repository: &GitRepository,
        write: GitWrite,
        binding: OperationBinding,
        cancellation: &CancellationToken,
        validation: Arc<dyn FinalWriteValidation>,
    ) -> Result<(PendingWriteIntent, PreparedRepositoryWrite), GitFailure> {
        let cancellation = CancellationToken::linked(cancellation, &CancellationToken::default());
        let began = Instant::now();
        let deadline = began + Duration::from_secs(30);
        let (prepared, observation) = mpsc::channel();
        let (start, decision) = mpsc::channel();
        let (completed, completion) = mpsc::channel();
        let (settled, recording) = mpsc::channel();
        let failure = Arc::new(Mutex::new(None));
        let hook = Arc::new(Handshake {
            binding,
            cancellation: cancellation.clone(),
            deadline,
            prepared,
            start: Mutex::new(decision),
            completed,
            settled: Mutex::new(recording),
            validation,
            failure: Arc::clone(&failure),
        });
        let ticket = self
            .write(repository, write, &cancellation, hook)
            .map_err(write_failure)?;
        let intent = loop {
            match observation.recv_timeout(Duration::from_millis(20)) {
                Ok(intent) => break intent,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let result = ticket.join();
                    return Err(failure.lock().ok().and_then(|f| *f).unwrap_or_else(|| {
                        result
                            .err()
                            .map(write_failure)
                            .unwrap_or(GitFailure::ExternalStateUncertain)
                    }));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancellation.is_cancelled() || began.elapsed() >= Duration::from_secs(30) {
                        cancellation.cancel();
                        drop(start);
                        drop(settled);
                        let _ = ticket.join();
                        return Err(if began.elapsed() >= Duration::from_secs(30) {
                            GitFailure::TimedOutRead
                        } else {
                            GitFailure::CancelledRead
                        });
                    }
                }
            }
        };
        Ok((
            intent,
            PreparedRepositoryWrite {
                binding,
                ticket: Some(ticket),
                start: Some(start),
                completion,
                failure,
                settled: Some(settled),
            },
        ))
    }
}
/// Adapt a payload-free coordinator admission failure to the portable classification.
pub fn write_failure(error: WriteError) -> GitFailure {
    match error {
        WriteError::Cancelled => GitFailure::CancelledRead,
        WriteError::ShuttingDown => GitFailure::Busy,
        WriteError::Capacity => GitFailure::Capacity,
        WriteError::WorkerUnavailable => GitFailure::ToolMissing,
        WriteError::IntentUnconfirmed => GitFailure::StorageFailure,
    }
}

/// Read administrative inventory including bare Main repositories, with hardened execution.
/// The owning service supplies cancellation and blocking-worker lifetime.
pub fn server_inventory(
    git: &LocalWorktreeGit,
    root: &std::path::Path,
    cancellation: &CancellationToken,
) -> Result<WorktreeSnapshot, RepositoryReadError> {
    let inventory = git.administrative_inventory(root, cancellation)?;
    if inventory.worktrees.len() > spaceterm_core::worktrees::MAX_GIT_REPOSITORIES {
        return Err(RepositoryReadError::OutputTooLarge);
    }
    Ok(inventory)
}

/// Normalize only the facts needed by client-local GitHub CLI lookup. Selection
/// follows the existing desktop remote and plain-push policy; no tool runs here.
pub fn pull_request_facts(
    config: &RepositoryConfig,
    head: &RepositoryHead,
) -> Option<spaceterm_core::worktrees::queries::GitPullRequestFacts> {
    use spaceterm_core::organization::BranchName;
    use spaceterm_core::worktrees::queries::GitPullRequestFacts;
    let RepositoryHead::Branch(branch) = head else {
        return None;
    };
    let remote = |name: &str| {
        config
            .remotes
            .iter()
            .find(|(remote, _)| &**remote == name)
            .map(|(_, url)| &**url)
    };
    let base = if remote("upstream").is_some() {
        "upstream"
    } else {
        config.branch_remote.as_deref().unwrap_or("origin")
    };
    let (host, owner, repository) = remote_repository(remote(base)?)?;
    let host = normalized_host(host);
    let head_branch = match config.branch_merge.as_deref() {
        Some(merge) if config.push_to_upstream && !merge.is_empty() => {
            merge.strip_prefix("refs/heads/").unwrap_or(merge)
        }
        _ => branch,
    };
    let head_owner = config
        .branch_push_remote
        .as_deref()
        .or(config.push_default.as_deref())
        .or(config.branch_remote.as_deref())
        .and_then(remote)
        .and_then(remote_repository)
        .and_then(|(host, owner, repository)| {
            // Validate the selected push remote too. An invalid remote does not
            // silently fall through to another remote or disclose its URL.
            GitPullRequestFacts::new(
                normalized_host(host),
                owner.to_owned(),
                repository.to_owned(),
                BranchName::new(head_branch.to_owned()).ok()?,
                None,
            )
            .ok()
        })
        .map(|facts| facts.owner().to_owned());
    GitPullRequestFacts::new(
        host,
        owner.to_owned(),
        repository.to_owned(),
        BranchName::new(head_branch.to_owned()).ok()?,
        head_owner,
    )
    .ok()
}
fn normalized_host(host: &str) -> String {
    let host = host.to_ascii_lowercase();
    if host == "ssh.github.com" {
        "github.com".into()
    } else {
        host
    }
}
fn remote_repository(url: &str) -> Option<(&str, &str, &str)> {
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let (host, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            if !["https", "http", "ssh", "git"]
                .iter()
                .any(|s| scheme.eq_ignore_ascii_case(s))
            {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            let authority = without_userinfo(authority)?;
            let host = match authority.split_once(':') {
                Some((host, port))
                    if !port.is_empty()
                        && port.len() <= 5
                        && port.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    host
                }
                Some(_) => return None,
                None => authority,
            };
            (host, path)
        }
        None => {
            if url.contains("::") {
                return None;
            }
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            (
                without_userinfo(authority)?,
                path.strip_prefix('/').unwrap_or(path),
            )
        }
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repository) = path.split_once('/')?;
    Some((host, owner, repository))
}
fn without_userinfo(authority: &str) -> Option<&str> {
    match authority.rsplit_once('@') {
        Some((user, host)) if !user.is_empty() => Some(host),
        Some(_) => None,
        None => Some(authority),
    }
}

/// Convert hardened native observations without collapsing full aggregate counts
/// to the first 20 display entries. A branch race requires another observation.
pub fn status_facts(
    revision: std::num::NonZeroU64,
    repository: &ProbedRepository,
    summary: &PorcelainSummary,
) -> Result<spaceterm_core::worktrees::queries::GitStatusFacts, GitFailure> {
    use spaceterm_core::organization::BranchName;
    use spaceterm_core::worktrees::queries::*;
    let branch = match &repository.head {
        RepositoryHead::Branch(branch) | RepositoryHead::Unborn(branch) => Some(branch.as_ref()),
        RepositoryHead::Detached(_) => None,
    };
    if branch != summary.headers.branch.as_deref() {
        return Err(GitFailure::GitStateChanged);
    }
    if summary.changes.entries.len() > MAX_GIT_STATUS_CHANGES {
        return Err(GitFailure::Capacity);
    }
    let count = &summary.changes;
    let counts = GitChangeCounts {
        total: match count.total {
            ChangeTotal::Exact(n) => GitChangeTotal::Exact(n),
            ChangeTotal::AtLeast(n) => GitChangeTotal::AtLeast(n),
        },
        staged: count.staged,
        modified: count.modified,
        deleted: count.deleted,
        renamed: count.renamed,
        conflicted: count.conflicted,
        untracked: count.untracked,
    };
    let step = |step: OperationStep| GitOperationStep {
        current: step.current,
        total: step.total,
    };
    let facts = GitStatusFacts {
        revision,
        branch: branch
            .map(|b| BranchName::new(b.to_owned()).map_err(|_| GitFailure::InvalidResponse))
            .transpose()?,
        changes: count
            .entries
            .iter()
            .map(|e| {
                GitChange::new(
                    e.path.as_bytes().to_vec(),
                    match e.kind {
                        ChangeKind::Staged => 1,
                        ChangeKind::Modified => 2,
                        ChangeKind::Deleted => 3,
                        ChangeKind::Renamed => 4,
                        ChangeKind::Conflicted => 5,
                        ChangeKind::Untracked => 6,
                    },
                )
            })
            .collect::<Result<_, _>>()?,
        dirty: counts.total.value() > 0,
        unchecked: false,
        pull_request: pull_request_facts(&repository.config, &repository.head),
        upstream: summary
            .headers
            .upstream
            .as_ref()
            .map(|name| {
                Ok(GitUpstream {
                    name: BranchName::new(name.to_string())
                        .map_err(|_| GitFailure::InvalidResponse)?,
                    divergence: summary.headers.divergence.map(|d| GitDivergence {
                        ahead: d.ahead,
                        behind: d.behind,
                    }),
                })
            })
            .transpose()?,
        operation: repository.operation.map(|operation| match operation {
            RepositoryOperation::Rebasing { step: s } => {
                GitRepositoryOperation::Rebasing { step: s.map(step) }
            }
            RepositoryOperation::Applying { step: s } => {
                GitRepositoryOperation::Applying { step: s.map(step) }
            }
            RepositoryOperation::Merging => GitRepositoryOperation::Merging,
            RepositoryOperation::Reverting => GitRepositoryOperation::Reverting,
            RepositoryOperation::CherryPicking => GitRepositoryOperation::CherryPicking,
            RepositoryOperation::Bisecting => GitRepositoryOperation::Bisecting,
        }),
        counts: Some(counts),
    };
    Ok(facts)
}

/// Compare Git-listed locations through physical parents, including checked absence.
/// This is comparison data only; writes still require retained native identity leases.
pub fn same_location(left: &std::path::Path, right: &std::path::Path) -> bool {
    super::coordinator::same_location(left, right)
}
