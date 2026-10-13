//! Service-owned durable writes. No connection or waiter owns a Started operation's lifetime.
use super::authority::*;
use super::host::*;
use super::reads::{CachedAssessment, ReadReservation};
use spaceterm_core::authority::ServerEpoch;
use spaceterm_core::identity::{ClientId, ServerId};
use spaceterm_core::operations::{OperationBinding, OperationId};
use spaceterm_core::storage::{CommitDisposition, StoreFailure};
use spaceterm_core::worktrees::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;

/// The constructor is private to this service. A recovered reference never creates a permit.
pub struct DurableGitStartPermit {
    binding: OperationBinding,
}
impl DurableGitStartPermit {
    pub fn binding(&self) -> OperationBinding {
        self.binding
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitLimits {
    pub active_writes: usize,
    pub pending_writes: usize,
    pub pending_per_client: usize,
    pub assessments: usize,
    pub assessments_per_client: usize,
}
impl Default for GitLimits {
    fn default() -> Self {
        Self {
            active_writes: 4,
            pending_writes: 32,
            pending_per_client: 8,
            assessments: 24,
            assessments_per_client: 8,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitSubmission {
    Retained(GitMutationOutcome),
    Pending(OperationBinding),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitShutdownReport {
    pub joined_writes: usize,
    pub unresolved: Vec<OperationBinding>,
}
#[derive(Clone)]
pub struct GitService {
    pub(crate) inner: Arc<Inner>,
}
pub(crate) struct Inner {
    pub server: ServerId,
    pub epoch: ServerEpoch,
    pub host: Arc<dyn ServerGitHost>,
    pub organization: Arc<dyn GitOrganizationPort>,
    pub limits: GitLimits,
    pub cancellation: GitReadCancellation,
    pub state: Mutex<State>,
    pub reads: Mutex<super::reads::ReadState>,
    pub read_slots: Arc<Semaphore>,
    pub read_wake: tokio::sync::Notify,
    pub inventory_wake: tokio::sync::Notify,
    writes: Arc<Semaphore>,
    pub(crate) shutdown: tokio::sync::Mutex<()>,
}
pub(crate) struct State {
    pub ready: bool,
    pub closing: bool,
    pub uncertain: bool,
    pub assessments: BTreeMap<[u8; 32], CachedAssessment>,
    jobs: BTreeMap<OperationId, Job>,
    pub deferred: Vec<PendingSettlement>,
}
struct Job {
    binding: OperationBinding,
    client: ClientId,
    handles: Vec<JoinHandle<()>>,
    result: watch::Receiver<Option<Result<GitMutationOutcome, GitFailure>>>,
    admission: watch::Receiver<Option<Result<GitSubmission, GitFailure>>>,
}
pub(crate) struct PendingSettlement {
    pub reservation: GitReservation,
    pub completion: GitCompletionPlan,
    pub native: Option<Box<dyn GitHostSettlement>>,
    pub residency: Option<Arc<ReadReservation>>,
}
impl GitService {
    /// Admission remains closed until `recover` successfully resolves retained intents.
    pub fn new(
        server: ServerId,
        epoch: ServerEpoch,
        host: Arc<dyn ServerGitHost>,
        organization: Arc<dyn GitOrganizationPort>,
        limits: GitLimits,
    ) -> Result<Self, GitFailure> {
        let defaults = GitLimits::default();
        if epoch.get() == 0
            || limits.active_writes == 0
            || limits.active_writes > defaults.active_writes
            || limits.pending_writes == 0
            || limits.pending_writes > defaults.pending_writes
            || limits.pending_per_client == 0
            || limits.pending_per_client > defaults.pending_per_client
            || limits.assessments == 0
            || limits.assessments > defaults.assessments
            || limits.assessments_per_client == 0
            || limits.assessments_per_client > defaults.assessments_per_client
        {
            return Err(GitFailure::Capacity);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                server,
                epoch,
                host,
                organization,
                limits,
                cancellation: GitReadCancellation::default(),
                state: Mutex::new(State {
                    ready: false,
                    closing: false,
                    uncertain: false,
                    assessments: BTreeMap::new(),
                    jobs: BTreeMap::new(),
                    deferred: Vec::new(),
                }),
                reads: Mutex::new(super::reads::ReadState::default()),
                read_slots: Arc::new(Semaphore::new(8)),
                read_wake: tokio::sync::Notify::new(),
                inventory_wake: tokio::sync::Notify::new(),
                writes: Arc::new(Semaphore::new(limits.active_writes)),
                shutdown: tokio::sync::Mutex::new(()),
            }),
        })
    }
    pub(crate) fn check_principal(
        &self,
        principal: AuthenticatedGitPrincipal,
    ) -> Result<(), GitFailure> {
        if principal.server() != self.inner.server {
            return Err(GitFailure::WrongServer);
        }
        if principal.epoch() != self.inner.epoch {
            return Err(GitFailure::StaleAssessment);
        }
        Ok(())
    }
    pub(crate) fn admission(&self) -> Result<(), GitFailure> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        if state.uncertain {
            Err(GitFailure::IndeterminateDurability)
        } else if !state.ready || state.closing {
            Err(GitFailure::Busy)
        } else {
            Ok(())
        }
    }
    pub async fn submit(
        &self,
        principal: AuthenticatedGitPrincipal,
        operation: OperationId,
        command: GitCommand,
    ) -> Result<GitSubmission, GitFailure> {
        self.check_principal(principal)?;
        if operation.stream().server() != self.inner.server {
            return Err(GitFailure::WrongServer);
        }
        if operation.stream().pairing() != principal.pairing()
            || operation.stream().client() != principal.client()
        {
            return Err(GitFailure::Unauthorized);
        }
        self.inner.organization.authorize(principal).await?;
        // Lookup before inspecting even the shape, epoch or expiry of a confirmation token.
        let binding = match self
            .inner
            .organization
            .lookup(principal, operation, &command)
            .await?
        {
            GitOperationLookup::Completed(outcome) => {
                outcome.validate_scope(self.inner.server)?;
                return Ok(GitSubmission::Retained(outcome));
            }
            GitOperationLookup::Pending(binding) => return Ok(GitSubmission::Pending(binding)),
            GitOperationLookup::New(binding) if binding.id == operation => binding,
            GitOperationLookup::New(_) => return Err(GitFailure::InvalidResponse),
        };
        self.admission()?;
        command.validate_stream(operation.stream())?;
        if command.assessment().epoch() != self.inner.epoch {
            return Err(GitFailure::StaleAssessment);
        }
        let (admission_sender, mut admission_receiver) = watch::channel(None);
        let (result_sender, result_receiver) = watch::channel(None);
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| GitFailure::StorageFailure)?;
            state
                .jobs
                .retain(|_, job| !job.handles.iter().all(JoinHandle::is_finished));
            if state.closing || !state.ready || state.uncertain {
                return Err(GitFailure::Busy);
            }
            if let Some(job) = state.jobs.get(&operation) {
                if job.binding != binding {
                    return Err(GitFailure::StaleAssessment);
                }
                admission_receiver = job.admission.clone();
            } else {
                let pending = state.jobs.len() + state.deferred.len();
                let attributable = state
                    .jobs
                    .values()
                    .filter(|job| job.client == principal.client())
                    .count()
                    + state
                        .deferred
                        .iter()
                        .filter(|job| {
                            job.reservation.binding().id.stream().client() == principal.client()
                        })
                        .count();
                if pending >= self.inner.limits.pending_writes
                    || attributable >= self.inner.limits.pending_per_client
                {
                    return Err(GitFailure::Capacity);
                }
                let key = command.assessment().parts().random;
                let assessment = state
                    .assessments
                    .get(&key)
                    .ok_or(GitFailure::ExpiredAssessment)?;
                if assessment.token != *command.assessment() || assessment.principal != principal {
                    return Err(GitFailure::Unauthorized);
                }
                if assessment.created.elapsed() >= Duration::from_secs(120) {
                    return Err(GitFailure::ExpiredAssessment);
                }
                let assessment = state
                    .assessments
                    .remove(&key)
                    .ok_or(GitFailure::ExpiredAssessment)?;
                let service = self.clone();
                let request = ConfirmedGitAdmission {
                    principal,
                    binding,
                    command,
                    plan: assessment.plan.as_ref().clone(),
                };
                let handle = tokio::spawn(async move {
                    let result = service
                        .run(
                            request,
                            assessment.native,
                            assessment.residency,
                            admission_sender,
                        )
                        .await;
                    let _ = result_sender.send(Some(result));
                });
                state.jobs.insert(
                    operation,
                    Job {
                        binding,
                        client: principal.client(),
                        handles: vec![handle],
                        result: result_receiver,
                        admission: admission_receiver.clone(),
                    },
                );
            }
        }
        loop {
            if let Some(result) = admission_receiver.borrow().clone() {
                return result;
            }
            admission_receiver
                .changed()
                .await
                .map_err(|_| GitFailure::StorageFailure)?;
        }
    }
    /// Waiter drop does not cancel the task. Completed retry always uses actor ledger lookup.
    pub async fn wait(&self, operation: OperationId) -> Result<GitMutationOutcome, GitFailure> {
        let mut receiver = self
            .inner
            .state
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?
            .jobs
            .get(&operation)
            .ok_or(GitFailure::TargetGone)?
            .result
            .clone();
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| GitFailure::StorageFailure)?;
        }
    }
    async fn run(
        &self,
        request: ConfirmedGitAdmission,
        native: Arc<dyn GitHostAssessment>,
        residency: Arc<ReadReservation>,
        admitted: watch::Sender<Option<Result<GitSubmission, GitFailure>>>,
    ) -> Result<GitMutationOutcome, GitFailure> {
        let plan = request.plan.clone();
        let reservation = match self.inner.organization.reserve_confirmed(request).await {
            Ok(reservation) => reservation,
            Err(failure) => {
                let _ = admitted.send(Some(Err(failure)));
                return Err(failure);
            }
        };
        let _ = admitted.send(Some(Ok(GitSubmission::Pending(reservation.binding()))));
        if reservation.intent().phase() != GitIntentPhase::Prepared {
            return self
                .reject(reservation, GitFailure::InvalidResponse, None)
                .await;
        }
        if let Err(failure) = plan.validate_intent(reservation.intent()) {
            return self.reject(reservation, failure, None).await;
        }
        let permit = Arc::clone(&self.inner.writes)
            .acquire_owned()
            .await
            .map_err(|_| GitFailure::Busy)?;
        let host = Arc::clone(&self.inner.host);
        let request = HostGitWriteRequest {
            intent: reservation.intent().clone(),
            native,
        };
        let cancellation = self.inner.cancellation.clone();
        let prepared =
            match tokio::task::spawn_blocking(move || host.prepare_write(request, &cancellation))
                .await
            {
                Ok(Ok(prepared)) => prepared,
                Ok(Err(failure)) => return self.reject(reservation, failure, None).await,
                Err(_) => return self.reject(reservation, GitFailure::Unchecked, None).await,
            };
        if prepared.observation().binding != reservation.binding()
            || prepared.observation().assessment_digest
                != reservation.intent().parts().assessment_digest
        {
            return self
                .reject(reservation, GitFailure::StaleAssessment, Some(prepared))
                .await;
        }
        match self.inner.organization.persist_prepared(&reservation).await {
            CommitDisposition::Durable(_) => {}
            CommitDisposition::NotApplied(failure) => {
                return self
                    .reject(reservation, storage_failure(failure), Some(prepared))
                    .await;
            }
            CommitDisposition::Indeterminate => {
                drop_prepared(prepared).await;
                self.defer_unconfirmed(reservation).await;
                return Err(GitFailure::IndeterminateDurability);
            }
        }
        if self.inner.cancellation.is_cancelled()
            || self
                .inner
                .state
                .lock()
                .map_err(|_| GitFailure::StorageFailure)?
                .uncertain
        {
            return self
                .reject(reservation, GitFailure::CancelledRead, Some(prepared))
                .await;
        }
        if let Err(failure) = self
            .inner
            .organization
            .barrier_and_revalidate(&reservation)
            .await
        {
            return self.reject(reservation, failure, Some(prepared)).await;
        }
        match self.inner.organization.persist_started(&reservation).await {
            CommitDisposition::Durable(_) => {}
            CommitDisposition::NotApplied(failure) => {
                return self
                    .reject(reservation, storage_failure(failure), Some(prepared))
                    .await;
            }
            CommitDisposition::Indeterminate => {
                drop_prepared(prepared).await;
                self.defer_unconfirmed(reservation).await;
                return Err(GitFailure::IndeterminateDurability);
            }
        }
        // From this durable boundary onward, do not consult connection, cancellation or quiescence.
        let mut intent_parts = reservation.intent().parts().clone();
        intent_parts.phase = GitIntentPhase::Started;
        let intent = GitWriteIntent::new(intent_parts)?;
        let mut ended = 0;
        if !intent.parts().affected.parts().panes.is_empty() {
            match self.inner.organization.retire_affected(&reservation).await {
                Ok(report) => {
                    let expected: std::collections::BTreeSet<_> = intent
                        .parts()
                        .affected
                        .parts()
                        .panes
                        .iter()
                        .filter_map(|p| p.session)
                        .collect();
                    let actual: std::collections::BTreeSet<_> =
                        report.cleaned.iter().copied().collect();
                    if actual != expected || actual.len() != report.cleaned.len() {
                        drop_prepared(prepared).await;
                        return self
                            .settle(
                                reservation,
                                GitCompletionPlan {
                                    intent,
                                    external: failure_result(GitFailure::Unchecked),
                                    recovery: None,
                                },
                                None,
                                None,
                            )
                            .await;
                    }
                    ended = actual.len() as u32;
                }
                Err(failure) => {
                    drop_prepared(prepared).await;
                    let mut external = failure_result(failure);
                    external.ended_sessions = intent
                        .parts()
                        .affected
                        .parts()
                        .panes
                        .iter()
                        .filter(|p| p.session.is_some())
                        .count() as u32;
                    return self
                        .settle(
                            reservation,
                            GitCompletionPlan {
                                intent,
                                external,
                                recovery: None,
                            },
                            None,
                            None,
                        )
                        .await;
                }
            }
        }
        let durable = DurableGitStartPermit {
            binding: reservation.binding(),
        };
        let mut completion =
            match tokio::task::spawn_blocking(move || prepared.execute(durable)).await {
                Ok(completion) => completion,
                Err(_) => HostGitCompletion {
                    result: RetainedExternalResult {
                        disposition: ExternalDisposition::PartialOrAmbiguous,
                        failure: Some(GitFailure::ExternalStateUncertain),
                        ended_sessions: 0,
                    },
                    settlement: None,
                },
            };
        completion.result.ended_sessions = ended;
        // Keep the server-wide active-write slot until native execution and durable completion finish.
        let result = self
            .settle(
                reservation,
                GitCompletionPlan {
                    intent,
                    external: completion.result,
                    recovery: None,
                },
                completion.settlement,
                Some(residency),
            )
            .await;
        drop(permit);
        result
    }
    async fn reject(
        &self,
        reservation: GitReservation,
        failure: GitFailure,
        prepared: Option<Box<dyn PreparedServerGitWrite>>,
    ) -> Result<GitMutationOutcome, GitFailure> {
        if let Some(prepared) = prepared {
            drop_prepared(prepared).await;
        }
        let completion = GitCompletionPlan {
            intent: reservation.intent().clone(),
            external: failure_result(failure),
            recovery: None,
        };
        self.settle(reservation, completion, None, None).await
    }
    pub(crate) async fn settle(
        &self,
        reservation: GitReservation,
        completion: GitCompletionPlan,
        native: Option<Box<dyn GitHostSettlement>>,
        residency: Option<Arc<ReadReservation>>,
    ) -> Result<GitMutationOutcome, GitFailure> {
        for _ in 0..3 {
            let result = self
                .inner
                .organization
                .complete(&reservation, &completion)
                .await;
            match result.disposition {
                CommitDisposition::Durable(receipt) => {
                    let outcome = result.outcome.filter(|o| {
                        o.server() == self.inner.server
                            && o.revision()
                                .is_none_or(|r| r == receipt.organization_revision)
                            && outcome_matches(&completion, o)
                    });
                    let Some(outcome) = outcome else {
                        self.defer(PendingSettlement {
                            reservation,
                            completion,
                            native,
                            residency,
                        })
                        .await;
                        return Err(GitFailure::IndeterminateDurability);
                    };
                    let recording = finish_native(native, true).await;
                    self.inner.organization.release(reservation).await;
                    recording?;
                    return Ok(outcome);
                }
                CommitDisposition::NotApplied(StoreFailure::RevisionConflict) => continue,
                CommitDisposition::NotApplied(failure) => {
                    let failure = storage_failure(failure);
                    self.store_deferred(PendingSettlement {
                        reservation,
                        completion,
                        native,
                        residency,
                    })?;
                    return Err(failure);
                }
                CommitDisposition::Indeterminate => {
                    self.defer(PendingSettlement {
                        reservation,
                        completion,
                        native,
                        residency,
                    })
                    .await;
                    return Err(GitFailure::IndeterminateDurability);
                }
            }
        }
        self.store_deferred(PendingSettlement {
            reservation,
            completion,
            native,
            residency,
        })?;
        Err(GitFailure::StorageFailure)
    }
    async fn defer_unconfirmed(&self, reservation: GitReservation) {
        let completion = GitCompletionPlan {
            intent: reservation.intent().clone(),
            external: failure_result(GitFailure::IndeterminateDurability),
            recovery: None,
        };
        self.defer(PendingSettlement {
            reservation,
            completion,
            native: None,
            residency: None,
        })
        .await;
    }
    async fn defer(&self, pending: PendingSettlement) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.uncertain = true;
        }
        self.inner.organization.close_mutation_admission().await;
        let _ = self.store_deferred(pending);
    }
    fn store_deferred(&self, pending: PendingSettlement) -> Result<(), GitFailure> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| GitFailure::IndeterminateDurability)?;
        // Every slot was reserved before effects. No eviction or replacement on egress pressure.
        state.deferred.push(pending);
        Ok(())
    }
    /// Retry only final transactions. The retained external result is never executed again.
    pub async fn retry_settlements(&self) -> Result<usize, GitFailure> {
        let receivers = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| GitFailure::StorageFailure)?;
            if state.uncertain {
                return Err(GitFailure::IndeterminateDurability);
            }
            if state.closing {
                return Err(GitFailure::Busy);
            }
            let pending = std::mem::take(&mut state.deferred);
            let mut receivers = Vec::new();
            for pending in pending {
                let binding = pending.reservation.binding();
                let service = self.clone();
                let (sender, receiver) = watch::channel(None);
                let handle = tokio::spawn(async move {
                    let result = service
                        .settle(
                            pending.reservation,
                            pending.completion,
                            pending.native,
                            pending.residency,
                        )
                        .await;
                    let _ = sender.send(Some(result));
                });
                let mut handles = state
                    .jobs
                    .remove(&binding.id)
                    .map(|job| job.handles)
                    .unwrap_or_default();
                handles.push(handle);
                let (_, admission) = watch::channel(Some(Ok(GitSubmission::Pending(binding))));
                state.jobs.insert(
                    binding.id,
                    Job {
                        binding,
                        client: binding.id.stream().client(),
                        handles,
                        result: receiver.clone(),
                        admission,
                    },
                );
                receivers.push(receiver);
            }
            receivers
        };
        let mut completed = 0;
        let mut failure = None;
        for mut receiver in receivers {
            loop {
                if let Some(result) = receiver.borrow().clone() {
                    match result {
                        Ok(_) => completed += 1,
                        Err(error) => failure = Some(error),
                    }
                    break;
                }
                receiver
                    .changed()
                    .await
                    .map_err(|_| GitFailure::StorageFailure)?;
            }
        }
        if let Some(failure) = failure {
            Err(failure)
        } else {
            Ok(completed)
        }
    }
    /// Retained original operations requiring recovery or final persistence, including after shutdown.
    pub fn unresolved_operations(&self) -> Result<Vec<OperationBinding>, GitFailure> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        Ok(state
            .deferred
            .iter()
            .map(|pending| pending.reservation.binding())
            .collect())
    }
    /// Atomically reject new work without cancelling any durable Started settlement.
    pub fn quiesce(&self) -> Result<(), GitFailure> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        state.closing = true;
        state.assessments.clear();
        self.inner.cancellation.cancel();
        self.inner.read_wake.notify_waiters();
        self.inner.inventory_wake.notify_one();
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<GitShutdownReport, GitFailure> {
        self.quiesce()?;
        let _shutdown = self.inner.shutdown.lock().await;
        // All native reads retain service-owned join handles, including dropped callers.
        let read_failure = self.shutdown_reads().await.is_err();
        let jobs = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| GitFailure::StorageFailure)?;
            state.closing = true;
            state.assessments.clear();
            self.inner.cancellation.cancel();
            std::mem::take(&mut state.jobs)
        };
        let joined_writes = jobs.len();
        let mut worker_failure = read_failure;
        for (_, job) in jobs {
            for handle in job.handles {
                if handle.await.is_err() {
                    worker_failure = true;
                }
            }
        }
        let (unresolved, native) = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| GitFailure::StorageFailure)?;
            let unresolved = state
                .deferred
                .iter()
                .map(|p| p.reservation.binding())
                .collect::<Vec<_>>();
            let native = state
                .deferred
                .iter_mut()
                .filter_map(|p| p.native.take().map(|native| (native, p.residency.take())))
                .collect::<Vec<_>>();
            (unresolved, native)
        };
        for (native, residency) in native {
            finish_native(Some(native), false).await?;
            drop(residency);
        }
        let host = Arc::clone(&self.inner.host);
        tokio::task::spawn_blocking(move || host.shutdown())
            .await
            .map_err(|_| GitFailure::ExternalStateUncertain)??;
        if worker_failure {
            return Err(GitFailure::ExternalStateUncertain);
        }
        if !unresolved.is_empty() {
            return Err(GitFailure::IndeterminateDurability);
        }
        Ok(GitShutdownReport {
            joined_writes,
            unresolved,
        })
    }
}
async fn drop_prepared(prepared: Box<dyn PreparedServerGitWrite>) {
    let _ = tokio::task::spawn_blocking(move || drop(prepared)).await;
}
async fn finish_native(
    native: Option<Box<dyn GitHostSettlement>>,
    recorded: bool,
) -> Result<(), GitFailure> {
    if let Some(native) = native {
        tokio::task::spawn_blocking(move || native.finish(recorded))
            .await
            .map_err(|_| GitFailure::ExternalStateUncertain)??;
    }
    Ok(())
}
pub(crate) fn failure_result(failure: GitFailure) -> RetainedExternalResult {
    RetainedExternalResult {
        disposition: ExternalDisposition::NoEffectEstablished,
        failure: Some(failure),
        ended_sessions: 0,
    }
}
fn storage_failure(failure: StoreFailure) -> GitFailure {
    match failure {
        StoreFailure::Locked => GitFailure::StorageLocked,
        StoreFailure::Unavailable => GitFailure::StorageUnavailable,
        StoreFailure::UnsafeStorage => GitFailure::UnsafeStorage,
        StoreFailure::RecoveryRequired => GitFailure::IndeterminateDurability,
        _ => GitFailure::StorageFailure,
    }
}

fn outcome_matches(completion: &GitCompletionPlan, outcome: &GitMutationOutcome) -> bool {
    match outcome.parts() {
        GitMutationOutcomeParts::Created { worktree, .. } => {
            completion.recovery.is_none()
                && completion.external.failure.is_none()
                && completion.external.disposition == ExternalDisposition::DesiredStateObserved
                && matches!(&completion.intent.parts().action, RetainedGitAction::Create { worktree: target, .. } if target == worktree)
        }
        GitMutationOutcomeParts::Removed {
            worktree,
            closed_tabs,
            ended_sessions,
            ..
        } => {
            completion.recovery.is_none()
                && completion.external.failure.is_none()
                && completion.external.disposition == ExternalDisposition::DesiredStateObserved
                && matches!(&completion.intent.parts().action, RetainedGitAction::Remove { worktree: target, .. } if target == worktree)
                && *closed_tabs as usize <= completion.intent.parts().affected.parts().tabs.len()
                && *ended_sessions == completion.external.ended_sessions
        }
        GitMutationOutcomeParts::Failed {
            ended_sessions,
            external,
            ..
        } => {
            completion.recovery.is_none()
                && *external == completion.external.disposition
                && *ended_sessions == completion.external.ended_sessions
        }
        GitMutationOutcomeParts::Recovered { observation, .. } => {
            completion.recovery == Some(*observation)
        }
    }
}
