//! Two-phase bridge over the one physical-repository serializer. All waits are blocking-worker only.
use super::*;
use spaceterm_core::operations::OperationBinding;
use spaceterm_core::worktrees::GitFailure;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Native checks run inside the repository lane immediately before allowing Git to write.
pub trait FinalWriteValidation: Send + Sync + 'static {
    /// Check retained destination identity and the originally approved deletion categories.
    fn validate(&self) -> Result<(), GitFailure>;
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
    prepared: mpsc::Sender<PendingWriteIntent>,
    start: Mutex<mpsc::Receiver<OperationBinding>>,
    completed: mpsc::Sender<GitWriteOutcome>,
    settled: Mutex<mpsc::Receiver<Result<(), IntentError>>>,
    validation: Arc<dyn FinalWriteValidation>,
    failure: Arc<Mutex<Option<GitFailure>>>,
}
impl WriteIntentHook for Handshake {
    fn record(&self, intent: &PendingWriteIntent) -> Result<(), IntentError> {
        if let Err(failure) = self.validation.validate() {
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
        if let Err(failure) = self.validation.validate() {
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
        let (prepared, observation) = mpsc::channel();
        let (start, decision) = mpsc::channel();
        let (completed, completion) = mpsc::channel();
        let (settled, recording) = mpsc::channel();
        let failure = Arc::new(Mutex::new(None));
        let hook = Arc::new(Handshake {
            binding,
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
        let began = Instant::now();
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
