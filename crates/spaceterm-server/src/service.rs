//! One authoritative actor owns reducer, ledger, ordered commits and runtime
//! registry. Awaited database, process, launch and transport work runs outside
//! this loop. Lifecycle requests have independent reserved ingress.

mod git_actor;
#[path = "pairing/actor.rs"]
pub(crate) mod pairing_actor;
pub(crate) use git_actor::{GitRequest, git_failure as classify_git_failure};

use crate::{
    adapters::{
        core::*,
        store::StoreWorker,
        terminal::{LaunchGate, PreparedLaunchPort, RetainedRuntimeObservations},
    },
    api::*,
    contracts::*,
    control::ControlRegistry,
    extensions::{
        PairingAllocationRequest, PairingCompletionContext, PairingCompletionRequest,
        PreparedPairingCompletion,
    },
    failure::{failure, ledger_failure, store_failure},
    runtime_registry::{OwnedRetirement, RuntimeRegistry},
    shutdown::*,
    startup_intents::StartupIntents,
};
use spaceterm_core::{
    authority::*, control::*, identity::*, native_management::*, observations::*,
    operation_ledger_contracts::*, operations::*, organization::*, pairing::*, retirement::*,
    runtime_authority::*, startup_policy::StartupContext, storage::*, worktrees::intent::*,
};
use spaceterm_terminal::runtime_contracts::{FencedRuntimeCommand, RuntimeStartRequest};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot, watch};

type Reply<T> = oneshot::Sender<Result<T, ServerFailure>>;
enum OperationReplySender {
    Ordinary(Reply<OperationResponse>),
    Pairing {
        sender: pairing_actor::PairingReply,
        authorization: Option<spaceterm_protocol::pairing_adapter::EnrollmentAuthorization>,
    },
}
struct OperationReply {
    sender: OperationReplySender,
    permit: Option<Box<dyn ReliableOutcomePermit>>,
}
impl OperationReply {
    fn service(sender: Reply<OperationResponse>) -> Self {
        Self {
            sender: OperationReplySender::Ordinary(sender),
            permit: None,
        }
    }
    fn send(mut self, mut value: Result<OperationResponse, ServerFailure>) {
        if let Ok(response) = &value
            && let Some(permit) = &mut self.permit
            && let Err(error) = permit.publish(response)
        {
            value = Err(error);
        }
        match self.sender {
            OperationReplySender::Ordinary(sender) => {
                let _ = sender.send(value);
            }
            OperationReplySender::Pairing {
                sender,
                authorization,
            } => {
                let result = match value {
                    Ok(OperationResponse::Completed {
                        outcome: Outcome::Pairing(outcome),
                        ..
                    }) => Ok(crate::pairing::service::PairingExecution {
                        outcome: *outcome,
                        authorization,
                    }),
                    Ok(OperationResponse::Pending(_)) => Err(PairingFailure::StorageUnavailable),
                    Ok(OperationResponse::Retired) => Err(PairingFailure::AuthorizationConsumed),
                    Ok(_) => Err(PairingFailure::InvalidDurableState),
                    Err(error) => Err(pairing_actor::pairing_failure(error)),
                };
                let _ = sender.send(result);
            }
        }
    }
}
pub(crate) enum ActorRequest {
    OrganizationSnapshot {
        principal: VerifiedPrincipal,
        major: u32,
        request: u64,
        generation: u64,
        maximum_payload_bytes: usize,
        reply: Reply<crate::publication::SnapshotTransfer>,
    },
    Git(Box<GitRequest>),
    Register {
        principal: VerifiedPrincipal,
        reply: Reply<ConnectionAccessSnapshot>,
    },
    Reconcile {
        principal: VerifiedPrincipal,
        revision: OrganizationRevision,
        reply: Reply<ConnectionAccessSnapshot>,
    },
    Submit {
        principal: VerifiedPrincipal,
        request: CanonicalOperationRequest,
        reply: Reply<OperationResponse>,
    },
    Lookup {
        principal: VerifiedPrincipal,
        binding: OperationBinding,
        reply: Reply<OperationResponse>,
    },
    LookupState {
        principal: VerifiedPrincipal,
        binding: OperationBinding,
        reply: Reply<OperationLookup>,
    },
    Acknowledge {
        principal: VerifiedPrincipal,
        stream: OperationStreamId,
        through: OperationSequence,
        reply: Reply<CommitReceipt>,
    },
    ReserveGitAllocation {
        binding: OperationBinding,
        needs: GitAllocationNeeds,
        reply: Reply<ReservedGitAllocation>,
    },
    ExternalProgress {
        binding: OperationBinding,
        progress: PendingExternalProgress,
        writes: Vec<OperationWrite>,
        reply: Reply<CommitReceipt>,
    },
    GitCleanup {
        binding: OperationBinding,
        reply: Reply<Vec<SessionCleanupObservation>>,
    },
    SettleGit {
        intent: Box<GitWriteIntent>,
        result: RetainedExternalResult,
        reply: Reply<OperationResponse>,
    },
    CompletePairing {
        request: CanonicalOperationRequest,
        completion: PairingCompletionRequest,
        reply: Reply<OperationResponse>,
    },
    Pairing(pairing_actor::PairingActorMessage),
    Observe {
        reply: Reply<()>,
    },
    Focus {
        principal: VerifiedPrincipal,
        update: ClientFocusUpdate,
        reply: Reply<ContextGeneration>,
    },
    RuntimeAccess {
        principal: VerifiedPrincipal,
        session: SessionKey,
        reply: Reply<RuntimeAccess>,
    },
    SubscribeRuntime {
        principal: VerifiedPrincipal,
        fence: EffectFence,
        reply: Reply<Box<dyn RuntimeStream<RuntimePresentationEvent>>>,
    },
    RuntimeCommand {
        principal: VerifiedPrincipal,
        command: Box<FencedRuntimeCommand>,
        reply: Reply<EffectDisposition>,
    },
}
pub(crate) enum LifecycleRequest {
    NativeAssess(Reply<spaceterm_host::account_service::RuntimeAssessment>),
    NativeReserve(
        spaceterm_host::account_service::RuntimeAssessment,
        Reply<()>,
    ),
    NativeAbort(
        spaceterm_host::account_service::RuntimeAssessment,
        Reply<()>,
    ),
    Disconnect {
        principal: VerifiedPrincipal,
        reply: Reply<()>,
    },
    RevokeClients {
        clients: Vec<ClientId>,
        reply: Reply<CommitReceipt>,
    },
    Shutdown {
        request: TrustedShutdownRequest,
        reply: Reply<ShutdownReport>,
    },
    Listeners(Arc<dyn ListenerLifecyclePort>, Reply<()>),
    CloseOwnerEnrollment(Reply<ShutdownReport>),
}
enum RetirementReply {
    Operation(OperationReply),
    Progress(Reply<CommitReceipt>, CommitReceipt),
    Native,
}
enum Event {
    GitBarrier {
        intent: GitWriteIntent,
        reply: Reply<()>,
        result: Result<Vec<AuthorityApplied>, ServerFailure>,
    },
    DirectoriesPrepared {
        request: CanonicalOperationRequest,
        reply: OperationReply,
        result: Result<Box<spaceterm_core::reducer::DirectoryValidation>, ServerFailure>,
    },
    Commit(CommitDisposition),
    RetirementFenced {
        request: OwnedRetirement,
        reply: RetirementReply,
        applied: Result<Vec<AuthorityApplied>, ServerFailure>,
    },
    PairingPaused {
        parts: Box<TransactionParts>,
        action: Box<CommitAction>,
        result: Result<Vec<AuthorityApplied>, ServerFailure>,
    },
    PairingRestored {
        clients: Vec<ClientId>,
        reply: OperationReply,
        error: ServerFailure,
        result: Result<Vec<AuthorityApplied>, ServerFailure>,
    },
    PairingFenced {
        response: OperationResponse,
        reply: OperationReply,
        result: Result<Vec<AuthorityApplied>, ServerFailure>,
    },
    FocusApplied {
        principal: VerifiedPrincipal,
        context: ContextGeneration,
        reply: Reply<ContextGeneration>,
        result: Result<Vec<AuthorityApplied>, ServerFailure>,
    },
    ControlRestored {
        reply: OperationReply,
        error: ServerFailure,
        result: Result<AuthorityApplied, ServerFailure>,
    },
    RemoteApplied {
        reply: OperationReply,
        response: OperationResponse,
        result: Result<(), ServerFailure>,
    },
    Reconciled {
        principal: VerifiedPrincipal,
        revision: OrganizationRevision,
        reply: Reply<ConnectionAccessSnapshot>,
        result: Result<Vec<AuthorityApplied>, ServerFailure>,
    },
    RuntimeJoined(SessionKey, Result<(), ServerFailure>),
    ControlPaused {
        principal: VerifiedPrincipal,
        request: CanonicalOperationRequest,
        reply: OperationReply,
        result: Result<(), ServerFailure>,
    },
    Authority {
        applied: Result<AuthorityApplied, ServerFailure>,
        reply: Option<OperationReply>,
        response: Option<OperationResponse>,
    },
    LaunchPrepared {
        principal: VerifiedPrincipal,
        request: CanonicalOperationRequest,
        reservation: Box<MeasuredStartupReservation>,
        start: Box<RuntimeStartRequest>,
        reply: OperationReply,
        result: Result<Box<dyn PreparedLaunchPort>, OperationFailure>,
    },
    Started {
        session: SessionKey,
        authority: RuntimeAuthority,
        result: Result<Box<dyn RuntimeOwnerPort>, ServerFailure>,
    },
    Cleanup(Result<SessionCleanupObservation, ServerFailure>),
    Observed(Reply<()>, Result<(), ServerFailure>),
    WritesFinished(Result<(), ServerFailure>),
    ListenersClosed(Result<(), ServerFailure>),
}
struct ConnectionState {
    principal: VerifiedPrincipal,
    access: ConnectionAccessSnapshot,
    context: ContextGeneration,
    focus: Option<ClientFocusUpdate>,
    connected: bool,
}
struct PendingCommit {
    transaction: Arc<DurableTransaction>,
    action: CommitAction,
}
enum CommitAction {
    PairingAdmission {
        principal: Option<VerifiedPrincipal>,
        request: CanonicalOperationRequest,
        mutation: Box<dyn crate::pairing::service::PairingMutation>,
        reply: OperationReply,
    },
    PairingReserved {
        execution: crate::pairing::service::PairingExecution,
        reply: pairing_actor::PairingReply,
    },
    PairingInvalidated {
        continuation: Option<pairing_actor::PairingActorMessage>,
        reply: Option<Reply<CommitReceipt>>,
    },
    Git(git_actor::GitCommit),
    Admission {
        principal: VerifiedPrincipal,
        request: CanonicalOperationRequest,
        reply: OperationReply,
    },
    Completed {
        binding: OperationBinding,
        reduction: PreparedReduction,
        reply: OperationReply,
        control: Option<PaneId>,
    },
    Startup {
        binding: OperationBinding,
        reduction: PreparedReduction,
        start: Box<RuntimeStartRequest>,
        launch: Box<dyn PreparedLaunchPort>,
        reply: OperationReply,
    },
    AcceptedRetirement {
        descriptor: Arc<RetirementDescriptor>,
        reply: OperationReply,
    },
    RetirementObserved {
        descriptor: Arc<RetirementDescriptor>,
    },
    RetirementCompleted {
        descriptor: Arc<RetirementDescriptor>,
    },
    Observed {
        directory: Option<ConfirmedDirectoryObservation>,
        terminal: Option<SessionTerminalResult>,
        cleanup: Option<SessionCleanupObservation>,
    },
    Auxiliary {
        writes: Vec<OperationWrite>,
        retirement: Option<Arc<RetirementDescriptor>>,
        reply: Reply<CommitReceipt>,
    },
    Allocated {
        reservation: Box<GitAllocationReservation>,
        reply: Reply<ReservedGitAllocation>,
    },
    PairingCompleted {
        binding: OperationBinding,
        reduction: PreparedReduction,
        writes: PairingWrites,
        revoked_clients: Vec<ClientId>,
        reply: OperationReply,
    },
    Acknowledged(Reply<CommitReceipt>),
    Revoked {
        clients: Vec<ClientId>,
        reply: Reply<CommitReceipt>,
    },
    NativeAccepted(Arc<NativeRetirementDescriptor>),
    NativeObserved(Arc<NativeRetirementDescriptor>),
    NativeCompleted,
}
impl CommitAction {
    fn retirement(&self) -> Option<RetirementRequest<'_>> {
        match self {
            Self::AcceptedRetirement { descriptor, .. }
            | Self::Auxiliary {
                retirement: Some(descriptor),
                ..
            } => Some(RetirementRequest::Operation(descriptor)),
            Self::NativeAccepted(descriptor) => Some(RetirementRequest::Native(descriptor)),
            _ => None,
        }
    }
}

pub struct RecoveredServer {
    handle: ServerHandle,
    join: Option<tokio::task::JoinHandle<Result<ShutdownReport, ServerFailure>>>,
    owner_drop: Option<oneshot::Sender<()>>,
}
impl RecoveredServer {
    pub async fn recover(
        mut config: ServerConfig,
        dependencies: ServerDependencies,
    ) -> Result<Self, ServerFailure> {
        let ServerDependencies {
            store,
            credentials,
            pairing,
            runtime,
            external_work,
            host_management,
            clock,
        } = dependencies;
        let worker = StoreWorker::new(store)?;
        let mut recovered = crate::recovery::recover(
            &mut config,
            credentials.as_ref(),
            external_work.as_ref(),
            host_management.as_ref(),
            worker,
        )
        .await?;
        let publication = crate::publication::PublicationHub::new(config.core.reducer.snapshot())?;
        pairing_actor::invalidate_recovered_tickets(&mut config, &mut recovered, clock.as_ref())
            .await?;
        let retained_pairing = recovered
            .recovered
            .pairing
            .clone()
            .map(|pairing| {
                spaceterm_core::pairing::policy::PairingSnapshot::new(
                    recovered.cursor.server,
                    recovered.cursor.epoch,
                    recovered.cursor.store,
                    recovered.cursor.high_water,
                    pairing,
                )
                .map_err(crate::pairing::service::server_failure)
            })
            .transpose()?;
        let (pairing_state, pairing_snapshot) = watch::channel(retained_pairing);
        let registry = RuntimeRegistry::new(config.limits.runtime_owners, config.cleanup.clone())?;
        let (requests, incoming) = mpsc::channel(config.limits.actor_requests);
        let (lifecycle, lifecycle_incoming) = mpsc::channel(64);
        let (events, event_incoming) = mpsc::channel(128);
        let (phase, phase_received) = watch::channel(ServicePhase::Ready);
        let (runtime_status, runtime_status_received) =
            watch::channel(spaceterm_core::management::ServerRuntimeStatus {
                storage: spaceterm_core::management::StorageStatus::Ready,
                intent: spaceterm_core::management::ServerRunIntent::Running,
            });
        let (snapshot, snapshot_received) = watch::channel(config.core.reducer.snapshot());
        let (owner_drop, owner_dropped) = oneshot::channel();
        let handle = ServerHandle {
            requests,
            lifecycle,
            phase: phase_received,
            runtime_status: runtime_status_received,
            snapshot: snapshot_received,
            pairing_snapshot,
        };
        let actor = Actor {
            publication,
            pending_publication: None,
            config,
            store: Some(recovered.store),
            cursor: recovered.cursor,
            pairing,
            pairing_state,
            pairing_connections: Vec::new(),
            pairing_assessments: BTreeMap::new(),
            runtime,
            external_work,
            clock,
            registry: Some(registry),
            intents: None,
            connections: BTreeMap::new(),
            revoked: BTreeSet::new(),
            pairing_barrier: BTreeSet::new(),
            pairing_waiting: None,
            pending: None,
            delivery_pending: None,
            busy: false,
            terminal_barrier: false,
            native_assessments: BTreeMap::new(),
            native_reservation: None,
            retirements: BTreeMap::new(),
            git_cleanup_waiters: BTreeMap::new(),
            git: git_actor::GitActorState::default(),
            git_repositories: recovered.recovered.git_repositories.clone(),
            git_allocations: recovered
                .recovered
                .git_allocations
                .iter()
                .map(|v| (v.binding().id, v.clone()))
                .collect(),
            git_references: recovered
                .recovered
                .ledger
                .as_ref()
                .into_iter()
                .flat_map(|v| v.operations())
                .filter_map(|entry| {
                    if let RecoveredOperationRecord::Pending(record) = entry
                        && let Some(progress) = &record.external_progress
                        && let ExternalProgressState::RecordedIntent { reference, .. } =
                            progress.state()
                    {
                        Some((record.binding.id, *reference))
                    } else {
                        None
                    }
                })
                .collect(),
            native_retirement: None,
            git_intents: recovered
                .recovered
                .git_intents
                .into_iter()
                .map(|v| (v.binding().id, v))
                .collect(),
            confirmed_sequences: BTreeMap::new(),
            terminal_results: BTreeMap::new(),
            starting: BTreeMap::new(),
            uninstalled: BTreeSet::new(),
            startup_failures: BTreeMap::new(),
            events,
            phase,
            runtime_status,
            snapshot,
            handle: handle.clone(),
            inspection_inflight: false,
            shutdown: None,
            listeners: None,
            listeners_closed: false,
            writes_finished: false,
            shutdown_error: None,
            native_completed: false,
        };
        let join =
            tokio::spawn(actor.run(incoming, lifecycle_incoming, event_incoming, owner_dropped));
        Ok(Self {
            handle,
            join: Some(join),
            owner_drop: Some(owner_drop),
        })
    }
    pub fn handle(&self) -> ServerHandle {
        self.handle.clone()
    }
    pub async fn run(self, listeners: ListenerFactories) -> Result<ShutdownReport, ServerFailure> {
        self.run_until(listeners, std::future::pending()).await
    }

    /// Native process termination closes this owner without issuing durable Stop.
    /// The same actor still drains started writes, retires workers and closes listeners.
    pub async fn run_until(
        mut self,
        listeners: ListenerFactories,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> Result<ShutdownReport, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.handle
            .lifecycle
            .send(LifecycleRequest::Listeners(
                listeners.lifecycle.clone(),
                reply,
            ))
            .await
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        crate::api::receive(result).await?;
        let open_result = listeners
            .lifecycle
            .open(self.handle.clone(), self.handle.snapshot())
            .await;
        if open_result.is_err()
            && let Some(sender) = self.owner_drop.take()
        {
            let _ = sender.send(());
        }
        let mut join = self
            .join
            .take()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
        let result = tokio::select! {
            result = &mut join => result,
            () = shutdown => {
                if let Some(sender) = self.owner_drop.take() {
                    let _ = sender.send(());
                }
                join.await
            }
        }
        .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?;
        self.owner_drop.take();
        // An open in progress can finish after close. The owning listener port
        // must close that same generation before this composition returns.
        listeners.lifecycle.close().await?;
        open_result?;
        result
    }
    /// Only the exclusive stopped-owner wrapper calls this listener-free close.
    /// It drains and joins without fabricating a native Stop request.
    pub(crate) async fn close_owner_enrollment(mut self) -> Result<ShutdownReport, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.handle
            .lifecycle
            .send(LifecycleRequest::CloseOwnerEnrollment(reply))
            .await
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        let closed = crate::api::receive(result).await;
        if closed.is_err()
            && let Some(sender) = self.owner_drop.take()
        {
            let _ = sender.send(());
        }
        let joined = self.wait().await;
        closed?;
        joined
    }
    /// Join an explicitly stopped injected composition without opening listeners.
    pub async fn wait(mut self) -> Result<ShutdownReport, ServerFailure> {
        let join = self
            .join
            .take()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
        let result = join
            .await
            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?;
        self.owner_drop.take();
        result
    }
}
impl Drop for RecoveredServer {
    fn drop(&mut self) {
        if let Some(sender) = self.owner_drop.take() {
            let _ = sender.send(());
        }
    }
}
struct ShutdownState {
    request: Option<(NativeManagementRequestRef, NativeRetirementAction)>,
    replies: Vec<Reply<ShutdownReport>>,
}
struct Actor {
    publication: crate::publication::PublicationHub,
    pending_publication: Option<(CommitReceipt, Option<Arc<OrganizationDelta>>)>,
    config: ServerConfig,
    store: Option<StoreWorker>,
    cursor: CommitCursor,
    pairing: Arc<dyn PairingAuthorizationPort>,
    pairing_state: watch::Sender<Option<spaceterm_core::pairing::policy::PairingSnapshot>>,
    pairing_connections: Vec<crate::connection::Authenticator>,
    pairing_assessments: BTreeMap<OperationId, (UnpairAssessment, u64)>,
    runtime: Arc<dyn RuntimeFactoryPort>,
    external_work: Arc<dyn ExternalWorkPort>,
    clock: Arc<dyn ServerClock>,
    registry: Option<RuntimeRegistry>,
    intents: Option<StartupIntents>,
    connections: BTreeMap<ClientId, ConnectionState>,
    revoked: BTreeSet<ClientId>,
    pairing_barrier: BTreeSet<ClientId>,
    pairing_waiting: Option<(TransactionParts, CommitAction)>,
    pending: Option<PendingCommit>,
    delivery_pending: Option<OperationBinding>,
    busy: bool,
    terminal_barrier: bool,
    native_assessments: BTreeMap<[u8; 32], spaceterm_host::account_service::RuntimeAssessment>,
    native_reservation: Option<spaceterm_host::account_service::RuntimeAssessment>,
    retirements: BTreeMap<OperationId, Arc<RetirementDescriptor>>,
    git_cleanup_waiters: BTreeMap<OperationId, Vec<Reply<Vec<SessionCleanupObservation>>>>,
    native_retirement: Option<Arc<NativeRetirementDescriptor>>,
    git_intents: BTreeMap<OperationId, GitWriteIntent>,
    git: git_actor::GitActorState,
    git_repositories: Vec<RepositoryLocator>,
    git_allocations: BTreeMap<OperationId, GitAllocationReservation>,
    git_references: BTreeMap<OperationId, ExternalIntentReference>,
    confirmed_sequences: BTreeMap<SessionKey, RuntimeObservationSequence>,
    terminal_results: BTreeMap<SessionKey, Option<CleanupResult>>,
    starting: BTreeMap<SessionKey, (ConnectionBinding, Arc<LaunchGate>)>,
    uninstalled: BTreeSet<SessionKey>,
    startup_failures: BTreeMap<SessionKey, (SessionTerminalResult, SessionCleanupObservation)>,
    events: mpsc::Sender<Event>,
    phase: watch::Sender<ServicePhase>,
    runtime_status: watch::Sender<spaceterm_core::management::ServerRuntimeStatus>,
    snapshot: watch::Sender<Arc<OrganizationSnapshot>>,
    handle: ServerHandle,
    inspection_inflight: bool,
    shutdown: Option<ShutdownState>,
    listeners: Option<Arc<dyn ListenerLifecyclePort>>,
    listeners_closed: bool,
    writes_finished: bool,
    shutdown_error: Option<ServerFailure>,
    native_completed: bool,
}
impl Actor {
    async fn run(
        mut self,
        mut requests: mpsc::Receiver<ActorRequest>,
        mut lifecycle: mpsc::Receiver<LifecycleRequest>,
        mut events: mpsc::Receiver<Event>,
        mut owner_drop: oneshot::Receiver<()>,
    ) -> Result<ShutdownReport, ServerFailure> {
        self.intents = Some(StartupIntents::new(self.config.limits));
        let mut owner_present = true;
        let mut poll = tokio::time::interval(std::time::Duration::from_millis(50));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if let Some((_, action)) = &self.pairing_waiting
                && (*self.phase.borrow() == ServicePhase::RecoveryRequired
                    || !self.pairing_has_starting(action))
                && let Some((parts, action)) = self.pairing_waiting.take()
            {
                if *self.phase.borrow() == ServicePhase::RecoveryRequired {
                    self.abort_preparation();
                    reject_action(action, failure(ServerFailureClass::RecoveryRequired));
                } else {
                    self.begin_pairing_commit(parts, action);
                }
            }
            if self.pending.is_none()
                && !self.busy
                && let Some(request) = self.git.waiting.pop_front()
            {
                self.git_request(request);
            }
            if self.pending.is_none() && !self.busy && self.advance().is_err() {
                self.enter_recovery();
            }
            if self.can_finish() {
                break;
            }
            tokio::select! { biased;
                _=&mut owner_drop,if owner_present=> { owner_present=false; self.enter_recovery(); }
                Some(request)=lifecycle.recv()=>self.lifecycle(request),
                Some(event)=events.recv()=>self.event(event),
                Some(request)=requests.recv()=>self.request(request),
                _ = poll.tick() => {
                    self.capture_observations();
                    if !self.inspection_inflight && let Some(registry) = &self.registry {
                        self.inspection_inflight = true;
                        let task = registry.inspect();
                        let events = self.events.clone();
                        tokio::spawn(async move {
                            let (reply, _) = oneshot::channel();
                            let _ = events.send(Event::Observed(reply, task.await)).await;
                        });
                    }
                },
            }
        }
        for (_, waiters) in std::mem::take(&mut self.git_cleanup_waiters) {
            for reply in waiters {
                let _ = reply.send(Err(failure(ServerFailureClass::CleanupIncomplete)));
            }
        }
        let cleanup_result = if let Some(registry) = self.registry.take() {
            registry.close().await
        } else {
            Ok(())
        };
        if let Err(error) = cleanup_result {
            self.shutdown_error = Some(error);
        }
        if let Some(store) = self.store.take()
            && let Err(error) = store.close().await
        {
            self.shutdown_error = Some(error);
        }
        let report = ShutdownReport {
            final_store_revision: self.cursor.store,
            final_organization_revision: self.cursor.organization,
            cleanup_confirmed: self.shutdown_error.is_none(),
            listeners_closed: self.listeners_closed,
            started_writes_finished: self.writes_finished,
            recovery_required: *self.phase.borrow() == ServicePhase::RecoveryRequired,
        };
        let result = if let Some(error) = self.shutdown_error {
            Err(error)
        } else {
            Ok(report)
        };
        self.phase.send_replace(if report.recovery_required {
            ServicePhase::RecoveryRequired
        } else {
            ServicePhase::Stopped
        });
        if let Some(shutdown) = self.shutdown.take() {
            for reply in shutdown.replies {
                let _ = reply.send(result);
            }
        }
        result
    }
    fn ordinary_open(&self) -> bool {
        self.native_reservation.is_none()
            && matches!(
                *self.phase.borrow(),
                ServicePhase::Ready | ServicePhase::Serving
            )
    }
    fn available(&self) -> Result<(), ServerFailure> {
        if *self.phase.borrow() == ServicePhase::RecoveryRequired {
            Err(failure(ServerFailureClass::RecoveryRequired))
        } else if self.pending.is_some() || self.busy {
            Err(failure(ServerFailureClass::Capacity))
        } else {
            Ok(())
        }
    }
    fn registry(&mut self) -> Result<&mut RuntimeRegistry, ServerFailure> {
        self.registry
            .as_mut()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))
    }
    fn intents(&mut self) -> Result<&mut StartupIntents, ServerFailure> {
        self.intents
            .as_mut()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))
    }
    fn check_principal(
        &self,
        principal: VerifiedPrincipal,
        effects: bool,
    ) -> Result<(), ServerFailure> {
        if principal.server() != self.cursor.server
            || principal.epoch() != self.cursor.epoch
            || self.revoked.contains(&principal.client())
        {
            return Err(failure(ServerFailureClass::Authentication));
        }
        if self.pairing_barrier.contains(&principal.client()) {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let connection = self
            .connections
            .get(&principal.client())
            .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
        if connection.principal != principal
            || !connection.connected
            || (effects && !connection.access.effects_enabled)
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let record = self.pairing_record(principal)?;
        if !record.active || record.principal.device != principal.device().bytes() {
            return Err(failure(ServerFailureClass::Authentication));
        }
        Ok(())
    }
    fn is_enrollment_stream(&self, stream: OperationStreamId) -> bool {
        self.pairing_state.borrow().as_ref().is_some_and(|state| {
            state
                .recovery()
                .parts()
                .enrollments
                .iter()
                .any(|reservation| reservation.parts().enrollment_stream == stream)
        })
    }
    fn check_binding(
        &self,
        principal: VerifiedPrincipal,
        binding: OperationBinding,
    ) -> Result<(), ServerFailure> {
        let stream = binding.id.stream();
        if stream.server() != principal.server()
            || stream.pairing() != principal.pairing()
            || stream.client() != principal.client()
        {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        if self.is_enrollment_stream(stream) {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        Ok(())
    }
    fn request(&mut self, request: ActorRequest) {
        match request {
            ActorRequest::OrganizationSnapshot {
                principal,
                major,
                request,
                generation,
                maximum_payload_bytes,
                reply,
            } => {
                let result = self.check_principal(principal, false).and_then(|_| {
                    self.publication.begin_snapshot(
                        major,
                        request,
                        generation,
                        maximum_payload_bytes,
                    )
                });
                let _ = reply.send(result);
            }
            ActorRequest::Git(request) => self.git_request(*request),
            ActorRequest::Register { principal, reply } => {
                let result = self.register(principal);
                let _ = reply.send(result);
            }
            ActorRequest::Reconcile {
                principal,
                revision,
                reply,
            } => self.reconcile(principal, revision, reply),
            ActorRequest::Submit {
                principal,
                request,
                reply,
            } => self.submit(principal, request, reply),
            ActorRequest::Lookup {
                principal,
                binding,
                reply,
            } => {
                let result = (|| {
                    self.check_principal(principal, false)?;
                    self.check_binding(principal, binding)?;
                    self.lookup(binding)
                })();
                let _ = reply.send(result);
            }
            ActorRequest::LookupState {
                principal,
                binding,
                reply,
            } => {
                let result = (|| {
                    self.check_principal(principal, false)?;
                    self.check_binding(principal, binding)?;
                    if self.delivery_pending == Some(binding) {
                        Ok(OperationLookup::Pending)
                    } else {
                        self.config.core.ledger.lookup(binding)
                    }
                })();
                let _ = reply.send(result);
            }
            ActorRequest::Acknowledge {
                principal,
                stream,
                through,
                reply,
            } => {
                let result = (|| {
                    self.check_principal(principal, false)?;
                    self.available()?;
                    if stream.client() != principal.client()
                        || stream.pairing() != principal.pairing()
                    {
                        return Err(failure(ServerFailureClass::ScopeMismatch));
                    }
                    if self.is_enrollment_stream(stream) {
                        return Err(failure(ServerFailureClass::ScopeMismatch));
                    }
                    let projection = self.config.core.ledger.prepare_acknowledgement(
                        StreamBinding::from_stream(stream),
                        through,
                        self.cursor.context().map_err(ledger_failure)?,
                    )?;
                    let mut parts = self.cursor.parts().map_err(store_failure)?;
                    projection.append_to(&mut parts).map_err(ledger_failure)?;
                    Ok(parts)
                })();
                match result {
                    Ok(parts) => self.commit(parts, CommitAction::Acknowledged(reply)),
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            ActorRequest::ReserveGitAllocation {
                binding,
                needs,
                reply,
            } => self.reserve_git_allocation(binding, needs, reply),
            ActorRequest::ExternalProgress {
                binding,
                progress,
                writes,
                reply,
            } => self.external_progress(binding, progress, writes, reply),
            ActorRequest::GitCleanup { binding, reply } => {
                let result = (|| {
                    let descriptor = self
                        .retirements
                        .get(&binding.id)
                        .filter(|descriptor| {
                            descriptor.binding() == binding
                                && descriptor.parts().completion
                                    == RetirementCompletion::ContinueGitSettlement
                        })
                        .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                    if descriptor
                        .sessions()
                        .iter()
                        .any(|v| matches!(v.cleanup(), RetirementCleanupState::Failed(_)))
                    {
                        return Err(failure(ServerFailureClass::CleanupIncomplete));
                    }
                    if !descriptor.cleanup_confirmed() {
                        return Ok(None);
                    }
                    self.registry
                        .as_ref()
                        .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
                        .cleanup_for(RetirementRequest::Operation(descriptor))
                })();
                match result {
                    Ok(Some(cleanup)) => {
                        let _ = reply.send(Ok(cleanup));
                    }
                    Ok(None)
                        if self
                            .git_cleanup_waiters
                            .values()
                            .map(Vec::len)
                            .sum::<usize>()
                            < 64
                            && self
                                .git_cleanup_waiters
                                .get(&binding.id)
                                .is_none_or(|waiters| waiters.len() < 4) =>
                    {
                        self.git_cleanup_waiters
                            .entry(binding.id)
                            .or_default()
                            .push(reply);
                    }
                    Ok(None) => {
                        let _ = reply.send(Err(failure(ServerFailureClass::Capacity)));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            ActorRequest::SettleGit {
                intent,
                result,
                reply,
            } => self.settle_git(*intent, result, OperationReply::service(reply)),
            ActorRequest::CompletePairing {
                request,
                completion,
                reply,
            } => {
                self.complete_pairing(request, completion, OperationReply::service(reply));
            }
            ActorRequest::Pairing(message) => self.pairing_request(message),
            ActorRequest::Observe { reply } => {
                if self.inspection_inflight {
                    let _ = reply.send(Err(failure(ServerFailureClass::Capacity)));
                    return;
                }
                if let Some(registry) = &self.registry {
                    self.inspection_inflight = true;
                    let task = registry.inspect();
                    let events = self.events.clone();
                    tokio::spawn(async move {
                        let _ = events.send(Event::Observed(reply, task.await)).await;
                    });
                } else {
                    let _ = reply.send(Err(failure(ServerFailureClass::RecoveryRequired)));
                }
            }
            ActorRequest::Focus {
                principal,
                update,
                reply,
            } => self.update_focus(principal, update, reply),
            ActorRequest::RuntimeAccess {
                principal,
                session,
                reply,
            } => {
                let result = (|| {
                    self.check_principal(principal, true)?;
                    if !self.ordinary_open() || self.terminal_barrier {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    self.registry()?.access(principal.binding(), session)
                })();
                let _ = reply.send(result);
            }
            ActorRequest::SubscribeRuntime {
                principal,
                fence,
                reply,
            } => {
                let result = (|| {
                    self.check_principal(principal, true)?;
                    if !self.ordinary_open() || self.terminal_barrier {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    self.registry()?.subscribe(principal.binding(), fence)
                })();
                match result {
                    Ok(task) => {
                        tokio::spawn(async move {
                            let _ = reply.send(task.await);
                        });
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            ActorRequest::RuntimeCommand {
                principal,
                command,
                reply,
            } => {
                let result = (|| {
                    self.check_principal(principal, true)?;
                    if !self.ordinary_open() || self.terminal_barrier {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    self.registry()?.submit(principal.binding(), *command)
                })();
                match result {
                    Ok(task) => {
                        tokio::spawn(async move {
                            let _ = reply.send(task.await);
                        });
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
        }
    }
    fn register(
        &mut self,
        principal: VerifiedPrincipal,
    ) -> Result<ConnectionAccessSnapshot, ServerFailure> {
        if !matches!(
            *self.phase.borrow(),
            ServicePhase::Ready | ServicePhase::Serving
        ) || principal.server() != self.cursor.server
            || principal.epoch() != self.cursor.epoch
            || self.revoked.contains(&principal.client())
            || self.pairing_barrier.contains(&principal.client())
        {
            return Err(failure(ServerFailureClass::Authentication));
        }
        let record = self.pairing_record(principal)?;
        if !record.active || record.principal.device != principal.device().bytes() {
            return Err(failure(ServerFailureClass::Authentication));
        }
        if let Some(old) = self.connections.get(&principal.client()) {
            if old.principal.binding().generation >= principal.binding().generation {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let previous = old.principal;
            self.disconnect(previous)?;
        } else if self.connections.len() >= 4096 {
            return Err(failure(ServerFailureClass::Capacity));
        }
        if self.connections.values().filter(|v| v.connected).count()
            >= self.config.limits.connections
        {
            return Err(failure(ServerFailureClass::Capacity));
        }
        let access = ConnectionAccessSnapshot {
            server: principal.server(),
            device: principal.device(),
            pairing: principal.pairing(),
            client: principal.client(),
            binding: principal.binding(),
            epoch: self.cursor.epoch,
            reconciled_revision: self.cursor.organization,
            effects_enabled: false,
        };
        self.connections.insert(
            principal.client(),
            ConnectionState {
                principal,
                access,
                context: ContextGeneration::new(1),
                focus: None,
                connected: true,
            },
        );
        Ok(access)
    }
    fn reconcile(
        &mut self,
        principal: VerifiedPrincipal,
        revision: OrganizationRevision,
        reply: Reply<ConnectionAccessSnapshot>,
    ) {
        let prepared = (|| {
            self.available()?;
            self.check_principal(principal, false)?;
            if !self.ordinary_open()
                || revision != self.cursor.organization
                || self.snapshot.borrow().state().revision != revision
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let context = self
                .connections
                .get(&principal.client())
                .ok_or_else(|| failure(ServerFailureClass::Authentication))?
                .context;
            let snapshot = self.config.core.reducer.snapshot();
            let mut tasks = Vec::new();
            for record in snapshot.runtime() {
                if let ControlRecord::Held { client, generation } = record.control
                    && client == principal.client()
                {
                    let session = SessionKey {
                        epoch: self.cursor.epoch,
                        pane: record.pane,
                        generation: record.session_generation,
                    };
                    if self.registry()?.authority(session).is_some() {
                        let grant = ControlGrant::new(record.pane, client, generation)
                            .map_err(|_| failure(ServerFailureClass::StaleAuthority))?;
                        let next = RuntimeAuthority::new(RuntimeAuthorityParts {
                            session,
                            grant: Some(grant),
                            connection: Some(principal.binding()),
                            context,
                            focused_pane: false,
                            terminal_input_focus: false,
                            native_effects_eligible: false,
                        })
                        .map_err(|_| failure(ServerFailureClass::StaleAuthority))?;
                        tasks.push(self.registry()?.transition(session, next)?);
                    }
                }
            }
            Ok(tasks)
        })();
        match prepared {
            Ok(tasks) => {
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result =
                        async {
                            let mut applied = Vec::new();
                            for task in tasks {
                                applied.push(task.await.map_err(|_| {
                                    failure(ServerFailureClass::RuntimeUnavailable)
                                })??);
                            }
                            Ok(applied)
                        }
                        .await;
                    let _ = events
                        .send(Event::Reconciled {
                            principal,
                            revision,
                            reply,
                            result,
                        })
                        .await;
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }
    fn update_focus(
        &mut self,
        principal: VerifiedPrincipal,
        update: ClientFocusUpdate,
        reply: Reply<ContextGeneration>,
    ) {
        let result = (|| {
            self.available()?;
            self.check_principal(principal, true)?;
            let snapshot = self.config.core.reducer.snapshot();
            if !self.ordinary_open()
                || self.terminal_barrier
                || update.focused_pane.is_some_and(|pane| {
                    !snapshot.state().panes.iter().any(|value| value.id == pane)
                })
                || (update.terminal_input_focus && update.focused_pane.is_none())
                || (update.native_effects_eligible && !update.terminal_input_focus)
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let connection = self
                .connections
                .get_mut(&principal.client())
                .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
            if update.context <= connection.context {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            connection.context = update.context;
            connection.focus = Some(update);
            self.intents()?.cancel_connection(principal.binding());
            let sessions = self.registry()?.sessions();
            let mut tasks = Vec::new();
            for session in sessions {
                if self.registry()?.is_retiring(session)
                    || self
                        .registry()?
                        .lifecycle(session)
                        .is_some_and(|lifecycle| lifecycle.terminal_result().is_some())
                    || !snapshot.runtime().iter().any(|record| {
                        record.pane == session.pane
                            && record.session_generation == session.generation
                    })
                {
                    continue;
                }
                if self
                    .registry()?
                    .authority(session)
                    .is_some_and(|authority| {
                        authority
                            .parts()
                            .grant
                            .is_some_and(|grant| grant.client() == principal.client())
                    })
                {
                    let next = self.current_authority(session)?;
                    tasks.push(self.registry()?.transition(session, next)?);
                }
            }
            Ok(tasks)
        })();
        match result {
            Ok(tasks) => {
                self.busy = true;
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result =
                        async {
                            let mut applied = Vec::new();
                            for task in tasks {
                                applied.push(task.await.map_err(|_| {
                                    failure(ServerFailureClass::RuntimeUnavailable)
                                })??);
                            }
                            Ok(applied)
                        }
                        .await;
                    let _ = events
                        .send(Event::FocusApplied {
                            principal,
                            context: update.context,
                            reply,
                            result,
                        })
                        .await;
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }
    fn lookup(&self, binding: OperationBinding) -> Result<OperationResponse, ServerFailure> {
        if self.delivery_pending == Some(binding) {
            return Ok(OperationResponse::Pending(binding));
        }
        match self.config.core.ledger.lookup(binding)? {
            OperationLookup::Pending => Ok(OperationResponse::Pending(binding)),
            OperationLookup::Completed {
                result,
                publication_revision,
            } => Ok(OperationResponse::Completed {
                binding,
                revision: publication_revision,
                outcome: result,
            }),
            OperationLookup::AcknowledgedRetired => Ok(OperationResponse::Retired),
            _ => Err(failure(ServerFailureClass::StaleAuthority)),
        }
    }
    fn submit(
        &mut self,
        principal: VerifiedPrincipal,
        request: CanonicalOperationRequest,
        sender: Reply<OperationResponse>,
    ) {
        let permit = match self.config.outcomes.reserve(principal, request.binding()) {
            Ok(permit) => permit,
            Err(error) => {
                let _ = sender.send(Err(error));
                return;
            }
        };
        let reply = OperationReply {
            sender: OperationReplySender::Ordinary(sender),
            permit: Some(permit),
        };
        let preflight = (|| {
            self.check_principal(principal, false)?;
            self.check_binding(principal, request.binding())?;
            let checked = CanonicalOperationRequest::new(
                request.binding().id,
                request.command().clone(),
                self.config.core.codec.as_ref(),
            )
            .map_err(ledger_failure)?;
            checked
                .verify_binding(request.binding())
                .map_err(ledger_failure)?;
            if request
                .encode(self.config.core.codec.as_ref())
                .map_err(ledger_failure)?
                .len()
                > MAX_CANONICAL_REQUEST_BYTES
            {
                return Err(failure(ServerFailureClass::Capacity));
            }
            self.config.core.ledger.lookup(request.binding())
        })();
        match preflight {
            Ok(OperationLookup::Completed {
                result,
                publication_revision,
            }) => {
                if self.delivery_pending == Some(request.binding()) {
                    reply.send(Ok(OperationResponse::Pending(request.binding())));
                } else {
                    reply.send(Ok(OperationResponse::Completed {
                        binding: request.binding(),
                        revision: publication_revision,
                        outcome: result,
                    }));
                }
            }
            Ok(OperationLookup::AcknowledgedRetired) => {
                reply.send(Ok(OperationResponse::Retired));
            }
            Ok(OperationLookup::Pending) => {
                if self.available().is_ok()
                    && self.ordinary_open()
                    && self
                        .config
                        .core
                        .ledger
                        .resume_atomic(&request)
                        .unwrap_or(false)
                {
                    self.busy = true;
                    self.reduce_admitted(principal, request, reply);
                } else {
                    reply.send(Ok(OperationResponse::Pending(request.binding())));
                }
            }
            Ok(OperationLookup::NeverAdmitted) => {
                let result = (|| {
                    self.available()?;
                    self.check_principal(principal, true)?;
                    if !self.ordinary_open() || self.terminal_barrier {
                        return Err(failure(ServerFailureClass::Capacity));
                    }
                    let projection = self.config.core.ledger.prepare_admission(
                        &request,
                        classify(request.command()),
                        self.cursor.context().map_err(ledger_failure)?,
                    )?;
                    let mut parts = self.cursor.parts().map_err(store_failure)?;
                    projection.append_to(&mut parts).map_err(ledger_failure)?;
                    Ok(parts)
                })();
                match result {
                    Ok(parts) => {
                        self.busy = true;
                        self.commit(
                            parts,
                            CommitAction::Admission {
                                principal,
                                request,
                                reply,
                            },
                        );
                    }
                    Err(error) => {
                        reply.send(Err(error));
                    }
                }
            }
            Ok(_) => {
                reply.send(Err(failure(ServerFailureClass::StaleAuthority)));
            }
            Err(error) => {
                reply.send(Err(error));
            }
        }
    }
    fn reduce_admitted(
        &mut self,
        principal: VerifiedPrincipal,
        request: CanonicalOperationRequest,
        reply: OperationReply,
    ) {
        if !self.ordinary_open() || self.revoked.contains(&principal.client()) {
            self.reject_admitted(request.binding(), OperationFailure::StaleAuthority, reply);
            return;
        }
        if self.git_conflicts(request.command()) {
            self.reject_admitted(request.binding(), OperationFailure::BusyRetiring, reply);
            return;
        }
        match request.command() {
            Command::CompareAcquire(_) | Command::TakeOver(_) | Command::ReleaseControl(_) => {
                let transition = ControlRegistry::prepare(
                    principal,
                    &self.config.core.reducer.snapshot(),
                    request.command(),
                );
                match transition {
                    Ok(transition) => {
                        let pane = transition.expected().pane;
                        self.cancel_pane(pane);
                        let session = self
                            .registry
                            .as_ref()
                            .and_then(|v| v.sessions().into_iter().find(|v| v.pane == pane));
                        if let Some(session) = session {
                            let result = (|| {
                                let current = self
                                    .registry()?
                                    .authority(session)
                                    .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                                let next = neutral(current)?;
                                self.registry()?.transition(session, next)
                            })();
                            match result {
                                Ok(task) => {
                                    let events = self.events.clone();
                                    tokio::spawn(async move {
                                        let result = task
                                            .await
                                            .map_err(|_| {
                                                failure(ServerFailureClass::RuntimeUnavailable)
                                            })
                                            .and_then(|v| v)
                                            .map(|_| ());
                                        let _ = events
                                            .send(Event::ControlPaused {
                                                principal,
                                                request,
                                                reply,
                                                result,
                                            })
                                            .await;
                                    });
                                }
                                Err(_) => self.reject_admitted(
                                    request.binding(),
                                    OperationFailure::StaleAuthority,
                                    reply,
                                ),
                            }
                        } else {
                            self.prepare_control(principal, request, reply);
                        }
                    }
                    Err(_) => self.reject_admitted(
                        request.binding(),
                        OperationFailure::StaleAuthority,
                        reply,
                    ),
                }
            }
            Command::BeginStartup(intent) => {
                let result = (|| {
                    self.check_principal(principal, true)?;
                    let current = self.startup_context(principal, *intent)?;
                    self.intents()?.begin(principal, *intent, current)?;
                    self.config
                        .core
                        .reducer
                        .prepare(ReductionRequest::Command(&request), self.cursor)
                })();
                match result {
                    Ok(reduction) => {
                        self.complete_reduction(request.binding(), reduction, reply, None)
                    }
                    Err(_) => self.reject_admitted(
                        request.binding(),
                        OperationFailure::StaleAuthority,
                        reply,
                    ),
                }
            }
            Command::CompleteStartup(measurement) => {
                self.prepare_launch(principal, request.clone(), (*measurement).into(), reply)
            }
            Command::CompleteMeasuredStartup(measurement) => {
                self.prepare_launch(principal, request.clone(), *measurement, reply)
            }
            Command::CancelStartup { intent } => {
                let _ = self.intents().map(|v| v.cancel(*intent));
                self.prepare_command(request, reply);
            }
            Command::Worktree(_)
            | Command::Pairing(_)
            | Command::CreateWorktree { .. }
            | Command::RemoveWorktreeConfirmed { .. }
            | Command::RevokePairing { .. } => {
                self.busy = false;
                let result = self.config.extensions.admitted(
                    request.clone(),
                    CommitReceipt {
                        store_revision: self.cursor.store,
                        organization_revision: self.cursor.organization,
                    },
                    self.handle.clone(),
                );
                reply.send(result.map(|_| OperationResponse::Pending(request.binding())));
            }
            _ => self.prepare_command(request, reply),
        }
    }
    fn prepare_control(
        &mut self,
        principal: VerifiedPrincipal,
        request: CanonicalOperationRequest,
        reply: OperationReply,
    ) {
        let result: Result<_, ServerFailure> = (|| {
            let transition = ControlRegistry::prepare(
                principal,
                &self.config.core.reducer.snapshot(),
                request.command(),
            )?;
            let pane = transition.next().pane;
            let reduction = self.config.core.reducer.prepare(
                ReductionRequest::Control {
                    binding: request.binding(),
                    transition: &transition,
                },
                self.cursor,
            )?;
            Ok((pane, reduction))
        })();
        match result {
            Ok((pane, reduction)) => {
                self.complete_reduction(request.binding(), reduction, reply, Some(pane))
            }
            Err(_) => {
                self.reject_admitted(request.binding(), OperationFailure::StaleAuthority, reply)
            }
        }
    }
    fn prepare_command(&mut self, request: CanonicalOperationRequest, reply: OperationReply) {
        match self.config.core.reducer.prepare_directories(&request) {
            Ok(Some(task)) => {
                let task = self.git_directory_task(task, request.command());
                let events = self.events.clone();
                tokio::spawn(async move {
                    let _ = events
                        .send(Event::DirectoriesPrepared {
                            request,
                            reply,
                            result: task.await.map(Box::new),
                        })
                        .await;
                });
                return;
            }
            Err(_) => {
                self.reject_admitted(
                    request.binding(),
                    OperationFailure::DirectoryUnavailable,
                    reply,
                );
                return;
            }
            Ok(None) => {}
        }
        self.prepare_validated_command(request, reply, None);
    }
    fn prepare_validated_command(
        &mut self,
        request: CanonicalOperationRequest,
        reply: OperationReply,
        directories: Option<Box<spaceterm_core::reducer::DirectoryValidation>>,
    ) {
        if !self.ordinary_open()
            || self
                .revoked
                .contains(&request.binding().id.stream().client())
        {
            self.reject_admitted(request.binding(), OperationFailure::StaleAuthority, reply);
            return;
        }
        let reduction_request = match directories.as_deref() {
            Some(directories) => ReductionRequest::ValidatedCommand {
                request: &request,
                directories,
            },
            None => ReductionRequest::Command(&request),
        };
        match self
            .config
            .core
            .reducer
            .prepare(reduction_request, self.cursor)
        {
            Ok(reduction) => {
                if let Some(descriptor) = reduction.retirement.clone() {
                    let result = (|| {
                        if descriptor.binding() != request.binding()
                            || descriptor.acceptance_revision()
                                != reduction.snapshot.state().revision
                        {
                            return Err(failure(ServerFailureClass::RecoveryRequired));
                        }
                        let mut parts = self.cursor.parts().map_err(store_failure)?;
                        reduction.append_to(&mut parts)?;
                        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
                            binding: request.binding(),
                            base: self.cursor.organization,
                            resulting: parts.resulting_organization_revision,
                        })
                        .map_err(ledger_failure)?;
                        parts
                            .operation_writes
                            .push(OperationWrite::ReductionBasis(basis));
                        let context = TransactionContext::new(
                            self.cursor.store,
                            self.cursor.organization,
                            parts.resulting_organization_revision,
                            Some(basis),
                        )
                        .map_err(ledger_failure)?;
                        let progress = PendingExternalProgress::new(PendingExternalProgressParts {
                            binding: request.binding(),
                            effect: ExternalEffectKind::SessionLifecycle,
                            state: ExternalProgressState::AcceptedRetirement(Box::new(
                                descriptor.clone(),
                            )),
                        })
                        .map_err(ledger_failure)?;
                        self.config
                            .core
                            .ledger
                            .prepare_external_progress(request.binding(), progress, context)?
                            .append_to(&mut parts)
                            .map_err(ledger_failure)?;
                        Ok(parts)
                    })();
                    match result {
                        Ok(parts) => self.commit(
                            parts,
                            CommitAction::AcceptedRetirement {
                                descriptor: Arc::new(descriptor),
                                reply,
                            },
                        ),
                        Err(error) => {
                            self.abort_preparation();
                            reply.send(Err(error));
                        }
                    }
                } else {
                    self.complete_reduction(request.binding(), reduction, reply, None);
                }
            }
            Err(_) => {
                self.reject_admitted(request.binding(), OperationFailure::StaleAuthority, reply)
            }
        }
    }
    fn reject_admitted(
        &mut self,
        binding: OperationBinding,
        value: OperationFailure,
        reply: OperationReply,
    ) {
        match self.config.core.reducer.prepare(
            ReductionRequest::Rejected {
                binding,
                failure: value,
            },
            self.cursor,
        ) {
            Ok(reduction) => self.complete_reduction(binding, reduction, reply, None),
            Err(error) => {
                self.abort_preparation();
                reply.send(Err(error));
            }
        }
    }
    fn complete_reduction(
        &mut self,
        binding: OperationBinding,
        reduction: PreparedReduction,
        reply: OperationReply,
        control: Option<PaneId>,
    ) {
        let result =
            self.completion_parts(binding, &reduction, reduction.snapshot.state().revision);
        match result {
            Ok(parts) => self.commit(
                parts,
                CommitAction::Completed {
                    binding,
                    reduction,
                    reply,
                    control,
                },
            ),
            Err(error) => {
                self.abort_preparation();
                reply.send(Err(error));
            }
        }
    }
    fn completion_parts(
        &mut self,
        binding: OperationBinding,
        reduction: &PreparedReduction,
        outcome_revision: OrganizationRevision,
    ) -> Result<TransactionParts, ServerFailure> {
        let mut parts = self.cursor.parts().map_err(store_failure)?;
        reduction.append_to(&mut parts)?;
        let basis = OperationReductionBasis::new(OperationReductionBasisParts {
            binding,
            base: self.cursor.organization,
            resulting: parts.resulting_organization_revision,
        })
        .map_err(ledger_failure)?;
        parts
            .operation_writes
            .push(OperationWrite::ReductionBasis(basis));
        let context = TransactionContext::new(
            self.cursor.store,
            self.cursor.organization,
            parts.resulting_organization_revision,
            Some(basis),
        )
        .map_err(ledger_failure)?;
        self.config
            .core
            .ledger
            .prepare_completion(binding, &reduction.outcome, outcome_revision, context)?
            .append_to(&mut parts)
            .map_err(ledger_failure)?;
        Ok(parts)
    }
    fn startup_context(
        &self,
        principal: VerifiedPrincipal,
        intent: StartupIntent,
    ) -> Result<StartupContext, ServerFailure> {
        let snapshot = self.config.core.reducer.snapshot();
        let pane = intent.fence().session().pane;
        let runtime = *snapshot
            .runtime()
            .iter()
            .find(|v| v.pane == pane)
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        let tab = snapshot
            .state()
            .panes
            .iter()
            .find(|v| v.id == pane)
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?
            .tab;
        let connection = self
            .connections
            .get(&principal.client())
            .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
        Ok(StartupContext {
            epoch: self.cursor.epoch,
            runtime,
            tab,
            connection: principal.binding(),
            context: connection.context,
            admissions_open: self.ordinary_open()
                && !self.terminal_barrier
                && !self.starting.keys().any(|v| v.pane == pane)
                && !self
                    .registry
                    .as_ref()
                    .is_some_and(|v| v.contains_pane(pane)),
        })
    }
    fn prepare_launch(
        &mut self,
        principal: VerifiedPrincipal,
        request: CanonicalOperationRequest,
        measurement: MeasuredStartupMeasurement,
        reply: OperationReply,
    ) {
        let result = (|| {
            self.check_principal(principal, true)?;
            if self.starting.len() >= self.config.limits.launch_preparations
                || !self
                    .registry
                    .as_ref()
                    .is_some_and(|v| v.has_capacity(self.starting.len()))
            {
                return Err(failure(ServerFailureClass::Capacity));
            }
            let intent = StartupIntent::new(
                measurement.intent,
                measurement.fence,
                StartupSource::StartSession,
            )
            .map_err(|_| failure(ServerFailureClass::StaleAuthority))?;
            let current = self.startup_context(principal, intent)?;
            let limits = self.config.limits.grid;
            let reservation = self
                .intents()?
                .measure(principal, measurement, current, limits)?
                .reserve()?;
            let snapshot = self.config.core.reducer.snapshot();
            let directory = snapshot
                .state()
                .panes
                .iter()
                .find(|v| v.id == reservation.session().pane)
                .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?
                .restoration_directory()
                .clone();
            let authority = RuntimeAuthority::new(RuntimeAuthorityParts {
                session: reservation.session(),
                grant: Some(reservation.intent().fence().grant()),
                connection: Some(principal.binding()),
                context: reservation.intent().fence().context(),
                focused_pane: false,
                terminal_input_focus: false,
                native_effects_eligible: false,
            })
            .map_err(|_| failure(ServerFailureClass::StaleAuthority))?;
            let start = RuntimeStartRequest::new(
                reservation,
                directory,
                authority,
                self.config.initial_appearance.clone(),
            )
            .map_err(|_| failure(ServerFailureClass::StaleAuthority))?;
            Ok((reservation, Box::new(start)))
        })();
        match result {
            Ok((reservation, start)) => {
                let launch = self.config.launch.clone();
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = launch.prepare(&start).await;
                    let _ = events
                        .send(Event::LaunchPrepared {
                            principal,
                            request,
                            reservation: Box::new(reservation),
                            start,
                            reply,
                            result,
                        })
                        .await;
                });
            }
            Err(_) => {
                self.reject_admitted(request.binding(), OperationFailure::StaleAuthority, reply)
            }
        }
    }
    fn reserve_git_allocation(
        &mut self,
        binding: OperationBinding,
        needs: GitAllocationNeeds,
        reply: Reply<ReservedGitAllocation>,
    ) {
        let admission = (|| {
            self.available()?;
            if !self.ordinary_open()
                || self.terminal_barrier
                || self.revoked.contains(&binding.id.stream().client())
                || needs.fallbacks.len() > spaceterm_core::worktrees::types::MAX_GIT_TABS
                || self.config.core.ledger.lookup(binding)? != OperationLookup::Pending
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            Ok(())
        })();
        if let Err(error) = admission {
            let _ = reply.send(Err(error));
            return;
        }
        // Only this preparation owns the candidate. Admission rejection must not
        // discard the reducer state staged for another in-flight transaction.
        let result = (|| {
            let reduction = self.config.core.reducer.prepare(
                ReductionRequest::ReserveGitAllocation {
                    binding,
                    repository: needs.repository,
                    worktree: needs.worktree,
                    fallbacks: &needs.fallbacks,
                },
                self.cursor,
            )?;
            let reservations: Vec<_> = reduction
                .auxiliary_writes
                .iter()
                .filter_map(|write| {
                    if let OperationWrite::GitAllocation(GitAllocationMutation::Reserve(value)) =
                        write
                    {
                        Some(value.as_ref())
                    } else {
                        None
                    }
                })
                .collect();
            if reservations.len() != 1
                || !reduction.organization_writes.is_empty()
                || reduction.snapshot.state().revision != self.cursor.organization
            {
                return Err(failure(ServerFailureClass::RecoveryRequired));
            }
            let reservation = reservations[0].clone();
            if reservation.binding() != binding
                || reservation.parts().expected_high_water != self.cursor.high_water
                || reservation.parts().resulting_high_water != reduction.high_water
            {
                return Err(failure(ServerFailureClass::RecoveryRequired));
            }
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            reduction.append_to(&mut parts)?;
            Ok((Box::new(reservation), parts))
        })();
        match result {
            Ok((reservation, parts)) => {
                self.commit(parts, CommitAction::Allocated { reservation, reply })
            }
            Err(error) => {
                self.config.core.reducer.not_applied();
                let _ = reply.send(Err(error));
            }
        }
    }
    fn external_progress(
        &mut self,
        binding: OperationBinding,
        progress: PendingExternalProgress,
        writes: Vec<OperationWrite>,
        reply: Reply<CommitReceipt>,
    ) {
        if let Err(error) = self.available() {
            let _ = reply.send(Err(error));
            return;
        }
        let mut reducer_prepared = false;
        let mut ledger_prepared = false;
        let result = (|| {
            let settlement = self.git_intents.get(&binding.id).is_some_and(|intent| {
                intent.binding() == binding && intent.phase() == GitIntentPhase::Started
                    && matches!(progress.state(), ExternalProgressState::RecordedIntent { reference, retirement }
                        if reference.scope() == ExternalIntentScope::Git(intent.parts().repository.scope())
                            && ((reference.phase() == ExternalIntentPhase::Started && retirement.is_some() && writes.is_empty())
                                || (reference.phase() == ExternalIntentPhase::ExternalFinished
                                    && matches!(writes.as_slice(), [OperationWrite::GitIntent(GitIntentMutation::Advance {
                                        binding: compared, expected_phase: GitIntentPhase::Started,
                                        next: GitIntentPhase::ExternalFinished, result: Some(_),
                                    })] if *compared == binding))))
            });
            if ((!self.ordinary_open()
                || self.terminal_barrier
                || self.revoked.contains(&binding.id.stream().client()))
                && !settlement)
                || binding.id.stream().server() != self.cursor.server
                || progress.binding() != binding
                || writes.len() > MAX_LEDGER_PROJECTION_WRITES
                || writes.iter().any(|v| {
                    !matches!(
                        v,
                        OperationWrite::GitIntent(_)
                            | OperationWrite::GitRepository(_)
                            | OperationWrite::Pairing(_)
                    )
                })
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            let retirement = if let ExternalProgressState::RecordedIntent {
                retirement: Some(descriptor),
                ..
            } = progress.state()
            {
                let intent = self
                    .git_intents
                    .get(&binding.id)
                    .filter(|intent| {
                        intent.binding() == binding && intent.phase() == GitIntentPhase::Started
                    })
                    .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                if let Some(existing) = self.retirements.get(&binding.id) {
                    if existing.as_ref() != descriptor.as_ref() {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    None
                } else {
                    reducer_prepared = true;
                    let reduction = self.config.core.reducer.prepare(
                        ReductionRequest::StartedGitRetirement { intent, descriptor },
                        self.cursor,
                    )?;
                    let current = self.config.core.reducer.snapshot();
                    let next = reduction.snapshot.state();
                    if next.workspaces != current.state().workspaces
                        || next.tabs != current.state().tabs
                        || next.panes != current.state().panes
                        || next.repositories != current.state().repositories
                        || next.worktrees != current.state().worktrees
                        || next.settings != current.state().settings
                        || reduction.high_water != self.cursor.high_water
                        || reduction.organization_writes.iter().any(deletes_membership)
                        || reduction.retirement.as_ref() != Some(descriptor.as_ref())
                        || descriptor.parts().completion
                            != RetirementCompletion::ContinueGitSettlement
                        || descriptor.acceptance_revision() != reduction.snapshot.state().revision
                    {
                        return Err(failure(ServerFailureClass::RecoveryRequired));
                    }
                    reduction.append_to(&mut parts)?;
                    Some(Arc::new(*descriptor.clone()))
                }
            } else {
                None
            };
            parts.operation_writes.extend(writes.iter().cloned());
            let basis = retirement
                .as_ref()
                .map(|_| {
                    OperationReductionBasis::new(OperationReductionBasisParts {
                        binding,
                        base: self.cursor.organization,
                        resulting: parts.resulting_organization_revision,
                    })
                })
                .transpose()
                .map_err(ledger_failure)?;
            if let Some(basis) = basis {
                parts
                    .operation_writes
                    .push(OperationWrite::ReductionBasis(basis));
            }
            let context = TransactionContext::new(
                self.cursor.store,
                self.cursor.organization,
                parts.resulting_organization_revision,
                basis,
            )
            .map_err(ledger_failure)?;
            ledger_prepared = true;
            self.config
                .core
                .ledger
                .prepare_external_progress(binding, progress, context)?
                .append_to(&mut parts)
                .map_err(ledger_failure)?;
            Ok((parts, retirement))
        })();
        match result {
            Ok((parts, retirement)) => self.commit(
                parts,
                CommitAction::Auxiliary {
                    writes,
                    retirement,
                    reply,
                },
            ),
            Err(error) => {
                if reducer_prepared {
                    self.config.core.reducer.not_applied();
                }
                if ledger_prepared {
                    self.config.core.ledger.not_applied();
                }
                let _ = reply.send(Err(error));
            }
        }
    }
    fn settle_git(
        &mut self,
        intent: GitWriteIntent,
        result: RetainedExternalResult,
        reply: OperationReply,
    ) {
        if let Ok(response @ (OperationResponse::Completed { .. } | OperationResponse::Retired)) =
            self.lookup(intent.binding())
        {
            reply.send(Ok(response));
            return;
        }
        let admission = (|| {
            self.available()?;
            if !matches!(
                intent.phase(),
                GitIntentPhase::Started | GitIntentPhase::ExternalFinished
            ) || self.git_intents.get(&intent.binding().id) != Some(&intent)
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            Ok(())
        })();
        if let Err(error) = admission {
            reply.send(Err(error));
            return;
        }
        let prepared = (|| {
            let revoked: Vec<_> = self.revoked.iter().copied().collect();
            let reduction = self.config.core.reducer.prepare(
                ReductionRequest::SettleStartedGit {
                    intent: &intent,
                    result: &result,
                    revoked_clients: &revoked,
                },
                self.cursor,
            )?;
            if reduction.snapshot.runtime().iter().any(|v|matches!(v.control,ControlRecord::Held{client,..} if self.revoked.contains(&client))) || reduction.snapshot.runtime().iter().any(|v|matches!(v.phase,SessionPhase::Starting{..}|SessionPhase::Running{..}) && !self.config.core.reducer.snapshot().runtime().iter().any(|old|old.pane==v.pane && old.phase==v.phase)) {return Err(failure(ServerFailureClass::RecoveryRequired));}
            Ok(reduction)
        })();
        match prepared {
            Ok(reduction) => self.complete_reduction(intent.binding(), reduction, reply, None),
            Err(error) => {
                self.config.core.reducer.not_applied();
                reply.send(Err(error));
            }
        }
    }
    fn complete_pairing(
        &mut self,
        request: CanonicalOperationRequest,
        mut completion: PairingCompletionRequest,
        reply: OperationReply,
    ) {
        let binding = request.binding();
        if let Ok(response @ (OperationResponse::Completed { .. } | OperationResponse::Retired)) =
            self.lookup(binding)
        {
            reply.send(Ok(response));
            return;
        }
        let admission = (|| {
            self.available()?;
            completion.validate_bounds()?;
            if !self.ordinary_open()
                || self.revoked.contains(&binding.id.stream().client())
                || !matches!(
                    request.command(),
                    Command::Pairing(_) | Command::RevokePairing { .. }
                )
                || self.config.core.ledger.lookup(binding)? != OperationLookup::Pending
                || self
                    .config
                    .core
                    .codec
                    .canonical_request_digest(
                        &StreamBinding::from_stream(binding.id.stream()),
                        binding.id,
                        request.command(),
                    )
                    .map_err(ledger_failure)?
                    != binding.digest
                || completion.revoked_clients.iter().any(|client| {
                    client.server() != self.cursor.server
                        || client.local() > self.cursor.high_water.client
                })
            {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            if self
                .revoked
                .union(&completion.revoked_clients.iter().copied().collect())
                .count()
                > 4096
            {
                return Err(failure(ServerFailureClass::Capacity));
            }
            if let PairingAllocationRequest::AdditionalClient(pairing) = completion.allocation
                && !matches!(request.command(), Command::Pairing(PairingCommand::BindAdditionalClient { target, .. }) if target.parts().pairing == pairing)
            {
                return Err(failure(ServerFailureClass::ScopeMismatch));
            }
            Ok(())
        })();
        if let Err(error) = admission {
            reply.send(Err(error));
            return;
        }
        let prepared = (|| {
            let mut reduction = self.config.core.reducer.prepare(
                ReductionRequest::PairingCompletion {
                    binding,
                    allocation: completion.allocation,
                    revoked_clients: &completion.revoked_clients,
                },
                self.cursor,
            )?;
            let allocated_client =
                pairing_allocation(self.cursor, completion.allocation, reduction.high_water)?;
            let current = self.config.core.reducer.snapshot();
            if reduction.retirement.is_some()
                || reduction.native_retirement.is_some()
                || !reduction.auxiliary_writes.is_empty()
                || reduction.snapshot.state().workspaces != current.state().workspaces
                || reduction.snapshot.state().tabs != current.state().tabs
                || reduction.snapshot.state().panes != current.state().panes
                || reduction.snapshot.state().repositories != current.state().repositories
                || reduction.snapshot.state().worktrees != current.state().worktrees
                || reduction.snapshot.state().settings != current.state().settings
                || reduction.organization_writes.iter().any(deletes_membership)
                || reduction.snapshot.runtime().len() != current.runtime().len()
                || reduction.snapshot.runtime().iter().any(|next| {
                    current.runtime().iter().find(|old| old.pane == next.pane).is_none_or(|old| {
                        let mut expected = *old;
                        if matches!(old.control, ControlRecord::Held { client, .. } if completion.revoked_clients.contains(&client)) {
                            expected.control = ControlRecord::Unclaimed;
                            if let Some(generation) = old.control_generation.checked_successor() {
                                expected.control_generation = generation;
                            } else {
                                return true;
                            }
                        }
                        *next != expected
                    })
                })
            {
                return Err(failure(ServerFailureClass::RecoveryRequired));
            }
            let proposal = completion.preparation.prepare(PairingCompletionContext {
                request: &request,
                cursor: self.cursor,
                allocated_client: allocated_client.clone(),
            })?;
            let retired_streams = validate_pairing_completion(
                &proposal,
                binding,
                self.cursor,
                reduction.high_water,
                allocated_client.as_ref(),
                &completion.revoked_clients,
            )?;
            reduction.outcome = Outcome::Pairing(Box::new(proposal.outcome));
            let mut parts = self.cursor.parts().map_err(store_failure)?;
            reduction.append_to(&mut parts)?;
            parts.operation_writes.extend(
                proposal
                    .writes
                    .parts()
                    .writes
                    .iter()
                    .cloned()
                    .map(|write| OperationWrite::Pairing(Box::new(write))),
            );
            let basis = OperationReductionBasis::new(OperationReductionBasisParts {
                binding,
                base: self.cursor.organization,
                resulting: parts.resulting_organization_revision,
            })
            .map_err(ledger_failure)?;
            parts
                .operation_writes
                .push(OperationWrite::ReductionBasis(basis));
            let context = TransactionContext::new(
                self.cursor.store,
                self.cursor.organization,
                parts.resulting_organization_revision,
                Some(basis),
            )
            .map_err(ledger_failure)?;
            self.config
                .core
                .ledger
                .prepare_pairing_completion(
                    binding,
                    &proposal.streams,
                    &retired_streams,
                    &reduction.outcome,
                    reduction.snapshot.state().revision,
                    context,
                )?
                .append_to(&mut parts)
                .map_err(ledger_failure)?;
            Ok((parts, reduction, proposal.writes))
        })();
        match prepared {
            Ok((parts, reduction, writes)) => {
                let action = CommitAction::PairingCompleted {
                    binding,
                    reduction,
                    writes,
                    revoked_clients: completion.revoked_clients.clone(),
                    reply,
                };
                self.begin_pairing_commit(parts, action);
            }
            Err(error) => {
                self.abort_preparation();
                reply.send(Err(error));
            }
        }
    }
    fn commit(&mut self, parts: TransactionParts, action: CommitAction) {
        if *self.phase.borrow() == ServicePhase::RecoveryRequired {
            self.busy = false;
            reject_action(action, failure(ServerFailureClass::RecoveryRequired));
            return;
        }
        let result =
            DurableTransaction::new_with_validation(parts, self.config.core.validator.as_ref())
                .map_err(store_failure)
                .and_then(|transaction| {
                    let transaction = Arc::new(transaction);
                    let task = self
                        .store
                        .as_ref()
                        .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
                        .commit(transaction.clone())?;
                    Ok((transaction, task))
                });
        match result {
            Ok((transaction, task)) => {
                self.pending = Some(PendingCommit {
                    transaction,
                    action,
                });
                let events = self.events.clone();
                tokio::spawn(async move {
                    let disposition = task.await.unwrap_or(CommitDisposition::Indeterminate);
                    let _ = events.send(Event::Commit(disposition)).await;
                });
            }
            Err(error) => {
                self.abort_preparation();
                match action {
                    CommitAction::PairingCompleted {
                        revoked_clients,
                        reply,
                        ..
                    } if !revoked_clients.is_empty()
                        && error.class != ServerFailureClass::RecoveryRequired =>
                    {
                        self.restore_pairing(revoked_clients, reply, error);
                    }
                    action => {
                        reject_action(action, error);
                        if error.class == ServerFailureClass::RecoveryRequired {
                            self.enter_recovery();
                        }
                    }
                }
            }
        }
    }
    fn abort_preparation(&mut self) {
        self.config.core.reducer.not_applied();
        self.config.core.ledger.not_applied();
        self.busy = false;
    }
    fn event(&mut self, event: Event) {
        match event {
            Event::GitBarrier {
                intent,
                reply,
                result,
            } => self.git_barrier_finished(intent, reply, result),
            Event::DirectoriesPrepared {
                request,
                reply,
                result,
            } => match result {
                Ok(directories) => {
                    self.prepare_validated_command(request, reply, Some(directories))
                }
                Err(error) => self.reject_admitted(
                    request.binding(),
                    if error.class == ServerFailureClass::Capacity {
                        OperationFailure::BusyRetiring
                    } else {
                        OperationFailure::DirectoryUnavailable
                    },
                    reply,
                ),
            },
            Event::Commit(disposition) => self.committed(disposition),
            Event::RetirementFenced {
                request,
                reply,
                applied,
            } => {
                self.busy = false;
                let result = (|| {
                    if *self.phase.borrow() == ServicePhase::RecoveryRequired {
                        return Err(failure(ServerFailureClass::RecoveryRequired));
                    }
                    for applied in applied? {
                        self.registry()?.authority_applied(applied)?;
                    }
                    Ok(())
                })();
                let result = result.and_then(|_| self.publish_installed());
                let binding = match &request {
                    OwnedRetirement::Operation(descriptor) => Some(descriptor.binding()),
                    OwnedRetirement::Native(_) => None,
                };
                self.retire(request);
                if result.is_err() {
                    self.enter_recovery();
                }
                match reply {
                    RetirementReply::Operation(reply) => reply.send(result.and_then(|_| {
                        binding
                            .map(OperationResponse::Pending)
                            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))
                    })),
                    RetirementReply::Progress(reply, receipt) => {
                        let _ = reply.send(result.map(|_| receipt));
                    }
                    RetirementReply::Native => {}
                }
            }
            Event::PairingPaused {
                parts,
                action,
                result,
            } => {
                let result = result.and_then(|applied| {
                    if *self.phase.borrow() == ServicePhase::RecoveryRequired {
                        return Err(failure(ServerFailureClass::RecoveryRequired));
                    }
                    for applied in applied {
                        self.registry()?.authority_applied(applied)?;
                    }
                    Ok(())
                });
                match result {
                    Ok(()) if self.pairing_has_starting(&action) => {
                        // An admitted launch may already have crossed process
                        // creation. Retain the candidate until its owner arrives,
                        // then fence that owner before submitting the transaction.
                        self.pairing_waiting = Some((*parts, *action));
                    }
                    Ok(()) => self.commit(*parts, *action),
                    Err(error) => {
                        self.abort_preparation();
                        reject_action(*action, error);
                        self.enter_recovery();
                    }
                }
            }
            Event::PairingRestored {
                clients,
                reply,
                error,
                result,
            } => {
                let result = result.and_then(|applied| {
                    for applied in applied {
                        self.registry()?.authority_applied(applied)?;
                    }
                    Ok(())
                });
                self.busy = false;
                if result.is_ok() {
                    for client in clients {
                        self.pairing_barrier.remove(&client);
                    }
                } else {
                    self.enter_recovery();
                }
                reply.send(Err(error));
            }
            Event::PairingFenced {
                response,
                reply,
                result,
            } => {
                let result = result.and_then(|applied| {
                    for applied in applied {
                        self.registry()?.authority_applied(applied)?;
                    }
                    Ok(())
                });
                self.busy = false;
                self.delivery_pending = None;
                let result = result.and_then(|_| self.publish_installed());
                if result.is_err() {
                    self.enter_recovery();
                }
                reply.send(result.map(|_| response));
            }
            Event::FocusApplied {
                principal,
                context,
                reply,
                result,
            } => {
                self.busy = false;
                let result = (|| {
                    for applied in result? {
                        self.registry()?.authority_applied(applied)?;
                    }
                    self.check_principal(principal, true)?;
                    if !self.ordinary_open()
                        || self.terminal_barrier
                        || self
                            .connections
                            .get(&principal.client())
                            .is_none_or(|state| state.context != context)
                    {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    Ok(context)
                })();
                let _ = reply.send(result);
            }
            Event::ControlRestored {
                reply,
                error,
                result,
            } => {
                self.busy = false;
                if result
                    .and_then(|applied| self.registry()?.authority_applied(applied))
                    .is_err()
                {
                    self.enter_recovery();
                }
                reply.send(Err(error));
            }
            Event::RemoteApplied {
                reply,
                response,
                result,
            } => {
                self.busy = false;
                reply.send(result.map(|_| response));
            }
            Event::Reconciled {
                principal,
                revision,
                reply,
                result,
            } => {
                let result = (|| {
                    for applied in result? {
                        self.registry()?.authority_applied(applied)?;
                    }
                    self.check_principal(principal, false)?;
                    if !self.ordinary_open() || revision != self.cursor.organization {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    let state = self
                        .connections
                        .get_mut(&principal.client())
                        .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
                    state.access.reconciled_revision = revision;
                    state.access.effects_enabled = true;
                    Ok(state.access)
                })();
                let _ = reply.send(result);
            }
            Event::RuntimeJoined(session, result) => {
                if let Some(registry) = &mut self.registry {
                    registry.joined(session);
                }
                self.confirmed_sequences.remove(&session);
                self.terminal_results.remove(&session);
                self.uninstalled.remove(&session);
                if let Err(error) = result {
                    self.shutdown_error = Some(error);
                }
            }
            Event::ControlPaused {
                principal,
                request,
                reply,
                result,
            } => match result {
                Ok(()) => self.prepare_control(principal, request, reply),
                Err(_) => {
                    self.reject_admitted(request.binding(), OperationFailure::StaleAuthority, reply)
                }
            },
            Event::Authority {
                applied,
                reply,
                response,
            } => {
                let result =
                    applied.and_then(|applied| self.registry()?.authority_applied(applied));
                if let Some(reply) = reply {
                    self.busy = false;
                    self.delivery_pending = None;
                    let result = result.and_then(|_| self.publish_installed());
                    if result.is_err() {
                        self.enter_recovery();
                    }
                    reply.send(result.and_then(|_| {
                        response.ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))
                    }));
                } else if result.is_err() {
                    self.enter_recovery();
                }
            }
            Event::LaunchPrepared {
                principal,
                request,
                reservation,
                start,
                reply,
                result,
            } => {
                let result = match result {
                    Ok(launch) => launch,
                    Err(failure) => {
                        if self.check_principal(principal, true).is_err()
                            || !self.ordinary_open()
                            || self.terminal_barrier
                        {
                            self.reject_admitted(
                                request.binding(),
                                OperationFailure::StaleAuthority,
                                reply,
                            );
                            return;
                        }
                        let intent = reservation.intent();
                        match self.config.core.reducer.prepare(
                            ReductionRequest::StartupPreparationFailed {
                                binding: request.binding(),
                                intent: &intent,
                                failure,
                            },
                            self.cursor,
                        ) {
                            Ok(reduction) => {
                                self.complete_reduction(request.binding(), reduction, reply, None)
                            }
                            Err(error) => {
                                self.abort_preparation();
                                reply.send(Err(error));
                            }
                        }
                        return;
                    }
                };
                let check = (|| {
                    self.check_principal(principal, true)?;
                    if !self.ordinary_open() || self.terminal_barrier {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    let current = self.startup_context(principal, reservation.intent())?;
                    spaceterm_core::startup_policy::validate_explicit_use(
                        reservation.intent(),
                        current,
                    )
                    .map_err(crate::failure::startup_failure)?;
                    let launch = result;
                    let reduction = self.config.core.reducer.prepare(
                        ReductionRequest::StartupReserved {
                            binding: request.binding(),
                            reservation: &reservation,
                        },
                        self.cursor,
                    )?;
                    let parts = self.completion_parts(
                        request.binding(),
                        &reduction,
                        reduction.snapshot.state().revision,
                    )?;
                    Ok((launch, reduction, parts))
                })();
                match check {
                    Ok((launch, reduction, parts)) => {
                        let session = reservation.session();
                        if matches!(reduction.outcome, Outcome::Failed(_)) {
                            self.commit(
                                parts,
                                CommitAction::Completed {
                                    binding: request.binding(),
                                    reduction,
                                    reply,
                                    control: None,
                                },
                            );
                            return;
                        }
                        let accepted = reduction.outcome == Outcome::SessionStarted(session)
                            && reduction.snapshot.epoch() == session.epoch
                            && reduction.snapshot.runtime().iter().any(|record| {
                                record.pane == session.pane
                                    && record.session_generation == session.generation
                                    && record.phase
                                        == (SessionPhase::Starting {
                                            generation: session.generation,
                                        })
                                    && record.control
                                        == (ControlRecord::Held {
                                            client: reservation.intent().fence().grant().client(),
                                            generation: reservation
                                                .intent()
                                                .fence()
                                                .grant()
                                                .generation(),
                                        })
                                    && record.control_generation
                                        == reservation.intent().fence().grant().generation()
                            });
                        if !accepted {
                            self.abort_preparation();
                            reply.send(Err(failure(ServerFailureClass::RecoveryRequired)));
                            self.enter_recovery();
                            return;
                        }
                        self.commit(
                            parts,
                            CommitAction::Startup {
                                binding: request.binding(),
                                reduction,
                                start,
                                launch,
                                reply,
                            },
                        );
                    }
                    Err(_) => self.reject_admitted(
                        request.binding(),
                        OperationFailure::StaleAuthority,
                        reply,
                    ),
                }
            }
            Event::Started {
                session,
                authority,
                result,
            } => {
                let start = self.starting.remove(&session);
                let retirement = self
                    .retirements
                    .values()
                    .filter(|descriptor| {
                        descriptor
                            .sessions()
                            .iter()
                            .any(|retained| retained.session() == session)
                    })
                    .min_by_key(|descriptor| descriptor.acceptance_revision())
                    .cloned();
                match result {
                    Ok(owner) => {
                        if self
                            .registry()
                            .and_then(|registry| registry.insert(owner, authority))
                            .is_err()
                        {
                            self.enter_recovery();
                        } else if let Some(descriptor) = retirement {
                            self.retire(OwnedRetirement::Operation(descriptor));
                        } else if self
                            .registry
                            .as_ref()
                            .and_then(|registry| registry.lifecycle(session))
                            .is_some_and(|lifecycle| lifecycle.terminal_result().is_some())
                        {
                            self.capture_observations();
                        } else {
                            // A late owner cannot install the old controller
                            // while another admitted transition is in flight.
                            self.uninstalled.insert(session);
                        }
                    }
                    Err(error) => {
                        if let Some(descriptor) = retirement {
                            if start
                                .as_ref()
                                .is_some_and(|(_, gate)| !gate.process_admitted())
                            {
                                let reason = descriptor
                                    .sessions()
                                    .iter()
                                    .find(|retained| retained.session() == session)
                                    .map(|retained| retained.first_reason());
                                let result = reason.ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete))
                                    .and_then(|reason| SessionCleanupObservation::new(session, reason, CleanupResult::Confirmed(ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership)).map_err(|_| failure(ServerFailureClass::CleanupIncomplete)))
                                    .and_then(|value| self.registry()?.record_unstarted_cleanup(value));
                                if result.is_err() {
                                    self.enter_recovery();
                                }
                            } else {
                                self.enter_recovery();
                            }
                            return;
                        }
                        if start
                            .as_ref()
                            .is_some_and(|(_, gate)| gate.process_admitted())
                        {
                            // The frozen factory error carries no cleanup proof.
                            // A failed-but-cleaned launch must return a retained
                            // terminal owner, not an optimistic absence of handles.
                            self.enter_recovery();
                            return;
                        }
                        let class = if error.class == ServerFailureClass::StaleAuthority {
                            FailureClass::StaleAuthority
                        } else {
                            FailureClass::LaunchUnavailable
                        };
                        if let Ok(terminal) = SessionTerminalResult::new(
                            session,
                            RuntimeTerminalStatus::StartupFailed { class },
                        ) {
                            // The retained gate, not missing handles, proves no
                            // process was admitted for this exact reservation.
                            if start.is_none() {
                                self.enter_recovery();
                                return;
                            }
                            match SessionCleanupObservation::new(
                                session,
                                RetirementReason::RuntimeFailure,
                                CleanupResult::Confirmed(
                                    ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
                                ),
                            ) {
                                Ok(cleanup) => {
                                    self.startup_failures.insert(session, (terminal, cleanup));
                                }
                                Err(_) => self.enter_recovery(),
                            }
                        }
                    }
                }
            }
            Event::Cleanup(result) => match result {
                Ok(value) => {
                    if self
                        .registry()
                        .and_then(|registry| registry.record_cleanup(value))
                        .is_err()
                    {
                        self.shutdown_error = Some(failure(ServerFailureClass::CleanupIncomplete));
                    }
                    if !matches!(value.result(), CleanupResult::Confirmed(_)) {
                        self.shutdown_error = Some(failure(ServerFailureClass::CleanupIncomplete));
                    }
                }
                Err(error) => self.shutdown_error = Some(error),
            },
            Event::Observed(reply, result) => {
                self.inspection_inflight = false;
                if result.is_ok() {
                    self.capture_observations();
                }
                let _ = reply.send(result);
            }
            Event::WritesFinished(result) => {
                self.writes_finished = true;
                if let Err(error) = result {
                    self.shutdown_error = Some(error);
                }
            }
            Event::ListenersClosed(result) => {
                self.listeners_closed = true;
                if let Err(error) = result {
                    self.shutdown_error = Some(error);
                }
                self.drain_external_work();
            }
        }
    }
    fn publish_installed(&mut self) -> Result<(), ServerFailure> {
        let snapshot = self.config.core.reducer.snapshot();
        if let Some((receipt, delta)) = self.pending_publication.take() {
            self.publication
                .committed(&receipt, snapshot.clone(), delta)?;
        }
        if self.snapshot.borrow().state().revision != snapshot.state().revision {
            self.snapshot.send_replace(snapshot);
        }
        Ok(())
    }
    fn committed(&mut self, disposition: CommitDisposition) {
        let Some(pending) = self.pending.take() else {
            self.enter_recovery();
            return;
        };
        match disposition {
            CommitDisposition::Durable(receipt) => {
                self.runtime_status.send_if_modified(|status| {
                    let changed =
                        status.storage != spaceterm_core::management::StorageStatus::Ready;
                    status.storage = spaceterm_core::management::StorageStatus::Ready;
                    changed
                });
                if self
                    .cursor
                    .install(pending.transaction.parts(), receipt)
                    .and_then(|_| self.config.core.reducer.committed(receipt))
                    .and_then(|_| self.config.core.ledger.committed(receipt))
                    .is_err()
                {
                    reject_action(
                        pending.action,
                        failure(ServerFailureClass::RecoveryRequired),
                    );
                    self.busy = false;
                    self.enter_recovery();
                    return;
                }
                if self
                    .install_pairing_writes(pending.transaction.parts())
                    .is_err()
                {
                    reject_action(
                        pending.action,
                        failure(ServerFailureClass::RecoveryRequired),
                    );
                    self.enter_recovery();
                    return;
                }
                self.git_observe_writes(&pending.transaction.parts().operation_writes);
                if let CommitAction::Git(git_actor::GitCommit::Inventory(Some(lease), _)) =
                    &pending.action
                    && lease.retain_committed().is_err()
                {
                    reject_action(
                        pending.action,
                        failure(ServerFailureClass::RecoveryRequired),
                    );
                    self.enter_recovery();
                    return;
                }
                let delta = match &pending.action {
                    CommitAction::Completed { reduction, .. }
                    | CommitAction::Startup { reduction, .. }
                    | CommitAction::PairingCompleted { reduction, .. } => reduction.delta.clone(),
                    _ => None,
                };
                self.pending_publication = Some((receipt, delta));
                let publication_barrier = matches!(&pending.action, CommitAction::PairingCompleted { revoked_clients, .. } if !revoked_clients.is_empty())
                    || matches!(&pending.action, CommitAction::Completed { control: Some(pane), .. } if self.registry.as_ref().is_some_and(|v| v.contains_pane(*pane)))
                    || matches!(
                        &pending.action,
                        CommitAction::AcceptedRetirement { .. }
                            | CommitAction::NativeAccepted(_)
                            | CommitAction::Auxiliary {
                                retirement: Some(_),
                                ..
                            }
                    );
                if !publication_barrier && let Err(error) = self.publish_installed() {
                    reject_action(pending.action, error);
                    self.busy = false;
                    self.enter_recovery();
                    return;
                }
                self.busy = publication_barrier;
                match pending.action {
                    CommitAction::PairingAdmission {
                        principal,
                        request,
                        mutation,
                        reply,
                    } => {
                        self.busy = false;
                        self.prepare_pairing_mutation(principal, request, mutation, reply);
                    }
                    CommitAction::PairingReserved { execution, reply } => {
                        let _ = reply.send(Ok(execution));
                    }
                    CommitAction::PairingInvalidated {
                        continuation,
                        reply,
                    } => {
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(receipt));
                        }
                        if let Some(message) = continuation {
                            self.pairing_request(message);
                        }
                    }
                    CommitAction::Git(action) => self.git_committed(
                        action,
                        &pending.transaction.parts().operation_writes,
                        receipt,
                    ),
                    CommitAction::Admission {
                        principal,
                        request,
                        reply,
                    } => {
                        self.busy = true;
                        self.reduce_admitted(principal, request, reply);
                    }
                    CommitAction::Completed {
                        binding,
                        reduction,
                        reply,
                        control,
                    } => {
                        let response = OperationResponse::Completed {
                            binding,
                            revision: reduction.snapshot.state().revision,
                            outcome: reduction.outcome,
                        };
                        if let Some(pane) = control
                            && let Some(session) = self
                                .registry
                                .as_ref()
                                .and_then(|v| v.sessions().into_iter().find(|v| v.pane == pane))
                        {
                            self.delivery_pending = Some(binding);
                            match self.current_authority(session) {
                                Ok(next) => {
                                    self.authority_task(session, next, Some(reply), Some(response))
                                }
                                Err(error) => {
                                    reply.send(Err(error));
                                    self.busy = false;
                                    self.enter_recovery();
                                }
                            }
                        } else if let Some((listeners, policy)) = self.listeners.clone().and_then(|listeners| {
                            pending.transaction.parts().organization_writes.iter().find_map(|write| {
                                if let OrganizationWrite::Retained(spaceterm_core::organization_retained::RetainedOrganizationWrite::SetRemoteAccess(policy)) = write {
                                    Some((listeners.clone(), policy.clone()))
                                } else { None }
                            })
                        }) {
                            self.busy = true;
                            let events = self.events.clone();
                            tokio::spawn(async move {
                                let result = listeners.apply_remote_policy(policy, receipt).await;
                                let _ = events.send(Event::RemoteApplied { reply, response, result }).await;
                            });
                        } else {
                            reply.send(Ok(response));
                        }
                        self.apply_git_writes(
                            pending.transaction.parts().operation_writes.as_slice(),
                        );
                    }
                    CommitAction::Startup {
                        binding,
                        reduction,
                        start,
                        launch,
                        reply,
                    } => {
                        let session = start.reservation().session();
                        let authority = start.authority();
                        let valid = self.ordinary_open()
                            && !self.terminal_barrier
                            && self
                                .connections
                                .get(&authority.parts().connection.map(|v| v.client).unwrap_or(
                                    start.reservation().intent().fence().grant().client(),
                                ))
                                .is_some_and(|v| {
                                    v.connected
                                        && v.principal.binding()
                                            == start.reservation().intent().fence().connection()
                                });
                        let gate = Arc::new(LaunchGate::default());
                        if !valid {
                            gate.cancel();
                        }
                        self.starting.insert(
                            session,
                            (
                                start.reservation().intent().fence().connection(),
                                gate.clone(),
                            ),
                        );
                        let factory = self.runtime.clone();
                        let events = self.events.clone();
                        let cleanup = self.config.cleanup.clone();
                        tokio::spawn(async move {
                            let result = launch.start(factory, *start, gate).await;
                            if let Err(error) = events
                                .send(Event::Started {
                                    session,
                                    authority,
                                    result,
                                })
                                .await
                                && let Event::Started {
                                    result: Ok(owner), ..
                                } = error.0
                            {
                                let _ = cleanup.abandon(owner).await;
                            }
                        });
                        reply.send(Ok(OperationResponse::Completed {
                            binding,
                            revision: reduction.snapshot.state().revision,
                            outcome: reduction.outcome,
                        }));
                    }
                    CommitAction::AcceptedRetirement { descriptor, reply } => {
                        self.terminal_barrier |= descriptor.server_wide_barrier();
                        if descriptor.server_wide_barrier() {
                            let _ = self.intents().map(|v| v.cancel_all());
                        }
                        for pane in descriptor.panes() {
                            self.cancel_pane(pane.pane());
                        }
                        self.retirements
                            .insert(descriptor.binding().id, descriptor.clone());
                        self.fence_retirement(
                            OwnedRetirement::Operation(descriptor),
                            RetirementReply::Operation(reply),
                        );
                    }
                    CommitAction::RetirementObserved { descriptor } => {
                        self.retirements.insert(descriptor.binding().id, descriptor);
                    }
                    CommitAction::RetirementCompleted { descriptor } => {
                        self.retirements.remove(&descriptor.binding().id);
                        for session in descriptor.sessions() {
                            self.terminal_results.remove(&session.session());
                            self.release_unreferenced(session.session());
                        }
                        self.terminal_barrier = self.shutdown.is_some()
                            || self.native_reservation.is_some()
                            || self.retirements.values().any(|v| v.server_wide_barrier());
                    }
                    CommitAction::Observed {
                        directory,
                        terminal,
                        cleanup,
                    } => {
                        if let Some(observation) = directory {
                            self.confirmed_sequences
                                .insert(observation.session(), observation.sequence());
                            self.config
                                .extensions
                                .confirmed_directory_committed(&observation, receipt);
                        }
                        if let Some(terminal) = terminal
                            && self.registry.as_ref().is_some_and(|registry| {
                                registry.authority(terminal.session()).is_some()
                            })
                        {
                            self.terminal_results.insert(terminal.session(), None);
                        }
                        if let Some(cleanup) = cleanup {
                            if terminal.is_some_and(|terminal| {
                                self.startup_failures.get(&cleanup.session())
                                    == Some(&(terminal, cleanup))
                            }) {
                                self.startup_failures.remove(&cleanup.session());
                            }
                            if self.registry.as_ref().is_some_and(|registry| {
                                registry.authority(cleanup.session()).is_some()
                            }) {
                                self.terminal_results
                                    .insert(cleanup.session(), Some(cleanup.result()));
                            } else {
                                self.terminal_results.remove(&cleanup.session());
                            }
                            self.release_unreferenced(cleanup.session());
                        }
                    }
                    CommitAction::Auxiliary {
                        writes,
                        retirement,
                        reply,
                    } => {
                        self.apply_git_writes(&writes);
                        if let Some(descriptor) = retirement {
                            for pane in descriptor.panes() {
                                self.cancel_pane(pane.pane());
                            }
                            self.retirements
                                .insert(descriptor.binding().id, descriptor.clone());
                            self.fence_retirement(
                                OwnedRetirement::Operation(descriptor),
                                RetirementReply::Progress(reply, receipt),
                            );
                        } else {
                            let _ = reply.send(Ok(receipt));
                        }
                    }
                    CommitAction::PairingCompleted {
                        binding,
                        reduction,
                        writes,
                        revoked_clients,
                        reply,
                    } => {
                        if let Err(error) = self.retain_pairing_assessment(&reduction.outcome) {
                            reply.send(Err(error));
                            self.enter_recovery();
                            return;
                        }
                        let response = OperationResponse::Completed {
                            binding,
                            revision: reduction.snapshot.state().revision,
                            outcome: reduction.outcome,
                        };
                        if let Err(error) =
                            self.config.extensions.pairing_committed(&writes, receipt)
                        {
                            self.busy = false;
                            reply.send(Err(error));
                            self.enter_recovery();
                        } else if revoked_clients.is_empty() {
                            reply.send(Ok(response));
                        } else {
                            self.fence_pairing(revoked_clients, response, reply);
                        }
                    }
                    CommitAction::Allocated { reservation, reply } => {
                        let _ = reply.send(Ok(ReservedGitAllocation {
                            reservation: *reservation,
                            receipt,
                        }));
                    }
                    CommitAction::Acknowledged(reply) => {
                        let _ = reply.send(Ok(receipt));
                    }
                    CommitAction::Revoked { clients, reply } => {
                        for client in clients {
                            self.revoked.insert(client);
                        }
                        let _ = reply.send(Ok(receipt));
                    }
                    CommitAction::NativeAccepted(descriptor) => {
                        self.native_retirement = Some(descriptor.clone());
                        self.fence_retirement(
                            OwnedRetirement::Native(descriptor),
                            RetirementReply::Native,
                        );
                    }
                    CommitAction::NativeObserved(descriptor) => {
                        self.native_retirement = Some(descriptor);
                    }
                    CommitAction::NativeCompleted => {
                        self.native_completed = true;
                        if let Some(descriptor) = self.native_retirement.clone() {
                            for session in descriptor.sessions() {
                                self.terminal_results.remove(&session.session());
                                self.release_unreferenced(session.session());
                            }
                        }
                    }
                }
            }
            CommitDisposition::NotApplied(error) => {
                self.runtime_status.send_modify(|status| {
                    use spaceterm_core::{management::StorageStatus, storage::StoreFailure};
                    status.storage = match error {
                        StoreFailure::Locked => StorageStatus::Locked,
                        StoreFailure::Unavailable | StoreFailure::Io => StorageStatus::Unavailable,
                        _ => StorageStatus::RecoveryRequired,
                    };
                });
                let released: Vec<_> = pending
                    .action
                    .retirement()
                    .into_iter()
                    .flat_map(|request| {
                        request.sessions().iter().map(|retained| retained.session())
                    })
                    .filter(|session| self.terminal_results.contains_key(session))
                    .collect();
                self.abort_preparation();
                let error = store_failure(error);
                if error.class == ServerFailureClass::RecoveryRequired {
                    reject_action(pending.action, error);
                    self.enter_recovery();
                } else {
                    match pending.action {
                        CommitAction::Completed {
                            control: Some(pane),
                            reply,
                            ..
                        } => self.restore_control(pane, reply, error),
                        CommitAction::PairingCompleted {
                            revoked_clients,
                            reply,
                            ..
                        } if !revoked_clients.is_empty() => {
                            self.restore_pairing(revoked_clients, reply, error);
                        }
                        action => reject_action(action, error),
                    }
                }
                for session in released {
                    self.release_unreferenced(session);
                }
            }
            CommitDisposition::Indeterminate => {
                self.runtime_status.send_modify(|status| {
                    status.storage = spaceterm_core::management::StorageStatus::RecoveryRequired;
                });
                self.busy = false;
                reject_action(
                    pending.action,
                    failure(ServerFailureClass::RecoveryRequired),
                );
                self.enter_recovery();
            }
        }
    }
    fn native_scope(
        &self,
    ) -> Result<spaceterm_host::account_service::RuntimeAssessment, ServerFailure> {
        if !matches!(
            *self.phase.borrow(),
            ServicePhase::Ready | ServicePhase::Serving
        ) || self.pending.is_some()
            || self.busy
            || !self.starting.is_empty()
            || !self.retirements.is_empty()
            || self.shutdown.is_some()
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let mut sessions = self
            .registry
            .as_ref()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
            .sessions();
        sessions.sort_unstable();
        if sessions.len() > 64 {
            return Err(failure(ServerFailureClass::Capacity));
        }
        Ok(spaceterm_host::account_service::RuntimeAssessment {
            server: self.cursor.server,
            epoch: Some(self.cursor.epoch),
            generation: self.cursor.store.get().max(1),
            sessions,
            token: [0; 32],
        })
    }
    fn native_matches(
        &self,
        assessment: &spaceterm_host::account_service::RuntimeAssessment,
    ) -> Result<(), ServerFailure> {
        let mut current = self.native_scope()?;
        current.token = assessment.token;
        if current != *assessment
            || self.native_assessments.get(&assessment.token) != Some(assessment)
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        Ok(())
    }
    fn lifecycle(&mut self, request: LifecycleRequest) {
        match request {
            LifecycleRequest::NativeAssess(reply) => {
                let result = (|| {
                    if self.native_reservation.is_some() {
                        return Err(failure(ServerFailureClass::StaleAuthority));
                    }
                    let mut scope = self.native_scope()?;
                    self.native_assessments.retain(|_, old| {
                        old.generation == scope.generation && old.sessions == scope.sessions
                    });
                    if self.native_assessments.len() >= 64 {
                        return Err(failure(ServerFailureClass::Capacity));
                    }
                    getrandom::fill(&mut scope.token)
                        .map_err(|_| failure(ServerFailureClass::NativeUnavailable))?;
                    if scope.token == [0; 32] || self.native_assessments.contains_key(&scope.token)
                    {
                        return Err(failure(ServerFailureClass::Exhausted));
                    }
                    self.native_assessments.insert(scope.token, scope.clone());
                    Ok(scope)
                })();
                let _ = reply.send(result);
            }
            LifecycleRequest::NativeReserve(assessment, reply) => {
                let result = if self.native_reservation.as_ref() == Some(&assessment) {
                    Ok(())
                } else if self.native_reservation.is_some() {
                    Err(failure(ServerFailureClass::StaleAuthority))
                } else {
                    self.native_matches(&assessment).map(|()| {
                        self.native_reservation = Some(assessment);
                        self.terminal_barrier = true;
                    })
                };
                let _ = reply.send(result);
            }
            LifecycleRequest::NativeAbort(assessment, reply) => {
                let result = if self.shutdown.is_some()
                    || self
                        .native_reservation
                        .as_ref()
                        .is_some_and(|old| old != &assessment)
                    || self
                        .native_assessments
                        .get(&assessment.token)
                        .is_some_and(|old| old != &assessment)
                {
                    Err(failure(ServerFailureClass::StaleAuthority))
                } else {
                    self.native_assessments.remove(&assessment.token);
                    self.native_reservation = None;
                    self.terminal_barrier = self
                        .retirements
                        .values()
                        .any(|old| old.server_wide_barrier());
                    Ok(())
                };
                let _ = reply.send(result);
            }
            LifecycleRequest::Disconnect { principal, reply } => {
                let result = self.disconnect(principal);
                let _ = reply.send(result);
            }
            LifecycleRequest::RevokeClients { clients, reply } => {
                let result = (|| {
                    if clients.len() > 4096
                        || clients.iter().any(|v| {
                            v.server() != self.cursor.server
                                || v.local() > self.cursor.high_water.client
                        })
                        || self
                            .revoked
                            .union(&clients.iter().copied().collect())
                            .count()
                            > 4096
                    {
                        return Err(failure(ServerFailureClass::ScopeMismatch));
                    }
                    for client in &clients {
                        if let Some(connection) = self.connections.get(client) {
                            let principal = connection.principal;
                            self.disconnect(principal)?;
                        }
                        self.revoked.insert(*client);
                    }
                    self.available()?;
                    let reduction = self
                        .config
                        .core
                        .reducer
                        .prepare(ReductionRequest::ReleaseClients(&clients), self.cursor)?;
                    let mut parts = self.cursor.parts().map_err(store_failure)?;
                    reduction.append_to(&mut parts)?;
                    Ok(parts)
                })();
                match result {
                    Ok(parts) => self.commit(parts, CommitAction::Revoked { clients, reply }),
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            LifecycleRequest::Shutdown { request, reply } => {
                let result = request
                    .authority
                    .verify_stopped(self.cursor.server, self.cursor.epoch);
                match result {
                    Ok((native, action)) if native.server() == self.cursor.server => {
                        if let Some(shutdown) = &mut self.shutdown {
                            if shutdown.request == Some((native, action))
                                && shutdown.replies.len() < 8
                            {
                                shutdown.replies.push(reply);
                            } else {
                                let _ = reply.send(Err(failure(ServerFailureClass::Capacity)));
                            }
                        } else {
                            // Only provider-verified deliberate Stop changes intent.
                            // Restart/update and process termination remain Running.
                            if action == NativeRetirementAction::Stop {
                                self.runtime_status.send_modify(|status| {
                                    status.intent =
                                        spaceterm_core::management::ServerRunIntent::Stopped;
                                });
                            }
                            self.shutdown = Some(ShutdownState {
                                request: Some((native, action)),
                                replies: vec![reply],
                            });
                            self.begin_quiescence();
                        }
                    }
                    _ => {
                        let _ = reply.send(Err(failure(ServerFailureClass::NativeUnavailable)));
                    }
                }
            }
            LifecycleRequest::CloseOwnerEnrollment(reply) => {
                if self.listeners.is_some()
                    || !self.connections.is_empty()
                    || !self.starting.is_empty()
                    || self.shutdown.is_some()
                    || self.native_retirement.is_some()
                    || self
                        .registry
                        .as_ref()
                        .is_none_or(|r| !r.sessions().is_empty())
                {
                    let _ = reply.send(Err(failure(ServerFailureClass::NativeUnavailable)));
                } else {
                    self.shutdown = Some(ShutdownState {
                        request: None,
                        replies: vec![reply],
                    });
                    self.native_completed = true;
                    self.begin_quiescence();
                }
            }
            LifecycleRequest::Listeners(port, reply) => {
                if self.listeners.is_some() || !self.ordinary_open() {
                    let _ = reply.send(Err(failure(ServerFailureClass::StaleAuthority)));
                } else {
                    self.listeners = Some(port);
                    self.phase.send_replace(ServicePhase::Serving);
                    let _ = reply.send(Ok(()));
                }
            }
        }
    }
    fn disconnect(&mut self, principal: VerifiedPrincipal) -> Result<(), ServerFailure> {
        for task in self.disconnect_tasks(principal)? {
            let events = self.events.clone();
            tokio::spawn(async move {
                let applied = task
                    .await
                    .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))
                    .and_then(|value| value);
                let _ = events
                    .send(Event::Authority {
                        applied,
                        reply: None,
                        response: None,
                    })
                    .await;
            });
        }
        Ok(())
    }
    fn pairing_has_starting(&self, action: &CommitAction) -> bool {
        matches!(action, CommitAction::PairingCompleted { revoked_clients, .. }
            if self.starting.values().any(|(binding, _)| revoked_clients.contains(&binding.client)))
    }
    fn begin_pairing_commit(&mut self, parts: TransactionParts, action: CommitAction) {
        let CommitAction::PairingCompleted {
            revoked_clients, ..
        } = &action
        else {
            self.abort_preparation();
            reject_action(action, failure(ServerFailureClass::RecoveryRequired));
            self.enter_recovery();
            return;
        };
        if revoked_clients.is_empty() {
            self.commit(parts, action);
            return;
        }
        self.busy = true;
        let prepared = self.pause_pairing(revoked_clients);
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = await_authorities(prepared).await;
            let _ = events
                .send(Event::PairingPaused {
                    parts: Box::new(parts),
                    action: Box::new(action),
                    result,
                })
                .await;
        });
    }
    fn pairing_sessions(&self, clients: &[ClientId]) -> Vec<SessionKey> {
        self.registry.as_ref().map_or_else(Vec::new, |registry| {
            registry
                .sessions()
                .into_iter()
                .filter(|session| {
                    !registry.is_retiring(*session)
                        && registry
                            .lifecycle(*session)
                            .is_none_or(|value| value.terminal_result().is_none())
                        && registry.authority(*session).is_some_and(|authority| {
                            authority
                                .parts()
                                .grant
                                .is_some_and(|grant| clients.contains(&grant.client()))
                        })
                })
                .collect()
        })
    }
    fn pause_pairing(
        &mut self,
        clients: &[ClientId],
    ) -> Result<Vec<oneshot::Receiver<Result<AuthorityApplied, ServerFailure>>>, ServerFailure>
    {
        // Registry admission alone cannot fence retained handles. The worker's
        // neutral-authority acknowledgement precedes the revocation commit.
        self.pairing_barrier.extend(clients.iter().copied());
        for client in clients {
            if let Some(binding) = self
                .connections
                .get(client)
                .map(|state| state.principal.binding())
            {
                self.intents()?.cancel_connection(binding);
            }
        }
        for (binding, gate) in self.starting.values() {
            if clients.contains(&binding.client) {
                gate.cancel();
            }
        }
        let mut tasks = Vec::new();
        for session in self.pairing_sessions(clients) {
            self.intents()?.cancel_pane(session.pane);
            let authority = self
                .registry()?
                .authority(session)
                .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
            tasks.push(self.registry()?.transition(session, neutral(authority)?)?);
        }
        Ok(tasks)
    }
    fn restore_pairing(
        &mut self,
        clients: Vec<ClientId>,
        reply: OperationReply,
        error: ServerFailure,
    ) {
        self.busy = true;
        let prepared = (|| {
            for client in &clients {
                self.pairing_barrier.remove(client);
                if let Some(state) = self.connections.get_mut(client) {
                    state.context = state
                        .context
                        .checked_successor()
                        .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
                    state.focus = None;
                }
            }
            let mut tasks = Vec::new();
            for session in self.pairing_sessions(&clients) {
                let next = self.current_authority(session)?;
                tasks.push(self.registry()?.transition(session, next)?);
            }
            Ok(tasks)
        })();
        // No actor turn runs between removing the barrier for validation and
        // reinstalling it. Discarded commands keep their original fences.
        self.pairing_barrier.extend(clients.iter().copied());
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = await_authorities(prepared).await;
            let _ = events
                .send(Event::PairingRestored {
                    clients,
                    reply,
                    error,
                    result,
                })
                .await;
        });
    }
    fn fence_pairing(
        &mut self,
        clients: Vec<ClientId>,
        response: OperationResponse,
        reply: OperationReply,
    ) {
        self.busy = true;
        if let OperationResponse::Completed { binding, .. } = &response {
            self.delivery_pending = Some(*binding);
        }
        let authenticators = self.pairing_connections.clone();
        let disconnected_clients = clients.clone();
        let prepared = (|| {
            let sessions = self.pairing_sessions(&clients);
            let mut tasks = Vec::new();
            for client in clients {
                self.revoked.insert(client);
                self.pairing_barrier.remove(&client);
                if let Some(principal) = self.connections.get(&client).map(|state| state.principal)
                {
                    tasks.extend(self.disconnect_tasks(principal)?);
                }
            }
            for session in sessions {
                let next = self.current_authority(session)?;
                tasks.push(self.registry()?.transition(session, next)?);
            }
            Ok(tasks)
        })();
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = async {
                let mut applied = Vec::new();
                for authenticator in authenticators {
                    authenticator.close_clients(&disconnected_clients).await?;
                }
                for task in prepared? {
                    applied.push(
                        task.await
                            .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))??,
                    );
                }
                Ok(applied)
            }
            .await;
            let _ = events
                .send(Event::PairingFenced {
                    response,
                    reply,
                    result,
                })
                .await;
        });
    }
    fn disconnect_tasks(
        &mut self,
        principal: VerifiedPrincipal,
    ) -> Result<Vec<oneshot::Receiver<Result<AuthorityApplied, ServerFailure>>>, ServerFailure>
    {
        let state = self
            .connections
            .get_mut(&principal.client())
            .ok_or_else(|| failure(ServerFailureClass::Authentication))?;
        if state.principal != principal {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        state.connected = false;
        state.access.effects_enabled = false;
        self.intents()?.cancel_connection(principal.binding());
        for (binding, gate) in self.starting.values() {
            if *binding == principal.binding() {
                gate.cancel();
            }
        }
        self.registry()?.pause_connection(principal.binding());
        let sessions = self.registry()?.sessions();
        let mut tasks = Vec::new();
        for session in sessions {
            if !self.registry()?.is_retiring(session)
                && self
                    .registry()?
                    .lifecycle(session)
                    .is_none_or(|lifecycle| lifecycle.terminal_result().is_none())
                && let Some(authority) = self.registry()?.authority(session)
                && authority.parts().connection == Some(principal.binding())
            {
                let next = neutral(authority)?;
                tasks.push(self.registry()?.transition(session, next)?);
            }
        }
        Ok(tasks)
    }
    fn cancel_pane(&mut self, pane: PaneId) {
        for (session, (_, gate)) in &self.starting {
            if session.pane == pane {
                gate.cancel();
            }
        }
        let _ = self.intents().map(|v| v.cancel_pane(pane));
        if let Some(registry) = &mut self.registry {
            registry.pause_pane(pane);
        }
    }
    fn authority_task(
        &mut self,
        session: SessionKey,
        next: RuntimeAuthority,
        reply: Option<OperationReply>,
        response: Option<OperationResponse>,
    ) {
        match self.registry().and_then(|v| v.transition(session, next)) {
            Ok(task) => {
                let events = self.events.clone();
                tokio::spawn(async move {
                    let applied = task
                        .await
                        .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))
                        .and_then(|v| v);
                    let _ = events
                        .send(Event::Authority {
                            applied,
                            reply,
                            response,
                        })
                        .await;
                });
            }
            Err(error) => {
                if let Some(reply) = reply {
                    reply.send(Err(error));
                    self.busy = false;
                }
                self.enter_recovery();
            }
        }
    }
    fn restore_control(&mut self, pane: PaneId, reply: OperationReply, error: ServerFailure) {
        let prepared = (|| {
            let session = self
                .registry()?
                .sessions()
                .into_iter()
                .find(|session| session.pane == pane)
                .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
            let snapshot = self.config.core.reducer.snapshot();
            if let Some(record) = snapshot.runtime().iter().find(|record| record.pane == pane)
                && let ControlRecord::Held { client, .. } = record.control
                && let Some(connection) = self.connections.get_mut(&client)
            {
                connection.context = connection
                    .context
                    .checked_successor()
                    .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
                connection.focus = None;
            }
            let next = self.current_authority(session)?;
            self.registry()?.transition(session, next)
        })();
        match prepared {
            Ok(task) => {
                self.busy = true;
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = task
                        .await
                        .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))
                        .and_then(|value| value);
                    let _ = events
                        .send(Event::ControlRestored {
                            reply,
                            error,
                            result,
                        })
                        .await;
                });
            }
            Err(_) => {
                reply.send(Err(error));
                self.enter_recovery();
            }
        }
    }
    fn current_authority(&self, session: SessionKey) -> Result<RuntimeAuthority, ServerFailure> {
        let snapshot = self.config.core.reducer.snapshot();
        let record = snapshot
            .runtime()
            .iter()
            .find(|v| v.pane == session.pane)
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        if record.session_generation != session.generation {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let (grant, connection, context) = match record.control {
            ControlRecord::Held { client, generation } => {
                let grant = ControlGrant::new(session.pane, client, generation)
                    .map_err(|_| failure(ServerFailureClass::StaleAuthority))?;
                let connection = self.connections.get(&client).filter(|v| {
                    v.connected
                        && v.access.effects_enabled
                        && !self.revoked.contains(&client)
                        && self.check_principal(v.principal, true).is_ok()
                });
                (
                    Some(grant),
                    connection.map(|v| v.principal.binding()),
                    connection.map_or(ContextGeneration::new(1), |v| v.context),
                )
            }
            ControlRecord::Unclaimed => (None, None, ContextGeneration::new(1)),
        };
        let focus = connection
            .and_then(|binding| {
                self.connections
                    .get(&binding.client)
                    .and_then(|state| state.focus)
            })
            .filter(|focus| focus.context == context && focus.focused_pane == Some(session.pane));
        RuntimeAuthority::new(RuntimeAuthorityParts {
            session,
            grant,
            connection,
            context,
            focused_pane: focus.is_some(),
            terminal_input_focus: focus.is_some_and(|focus| focus.terminal_input_focus),
            native_effects_eligible: focus.is_some_and(|focus| focus.native_effects_eligible),
        })
        .map_err(|_| failure(ServerFailureClass::StaleAuthority))
    }
    fn fence_retirement(&mut self, request: OwnedRetirement, reply: RetirementReply) {
        self.busy = true;
        let prepared = (|| {
            let mut tasks = Vec::new();
            for retained in request.request().sessions() {
                let session = retained.session();
                if !self.registry()?.is_retiring(session)
                    && let Some(authority) = self.registry()?.authority(session)
                {
                    tasks.push(self.registry()?.transition(session, neutral(authority)?)?);
                }
            }
            Ok(tasks)
        })();
        let events = self.events.clone();
        tokio::spawn(async move {
            let applied = async {
                let mut results = Vec::new();
                for task in prepared? {
                    results.push(
                        task.await
                            .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))??,
                    );
                }
                Ok(results)
            }
            .await;
            let _ = events
                .send(Event::RetirementFenced {
                    request,
                    reply,
                    applied,
                })
                .await;
        });
    }
    fn retire(&mut self, request: OwnedRetirement) {
        let starting: Vec<_> = self.starting.keys().copied().collect();
        match self.registry().and_then(|v| v.retire(request, &starting)) {
            Ok(tasks) => {
                for (_, task) in tasks {
                    let events = self.events.clone();
                    tokio::spawn(async move {
                        let result = task
                            .await
                            .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))
                            .and_then(|v| v);
                        let _ = events.send(Event::Cleanup(result)).await;
                    });
                }
            }
            Err(error) => {
                self.shutdown_error = Some(error);
            }
        }
    }
    fn release_unreferenced(&mut self, session: SessionKey) {
        // A staged acceptance owns the same evidence as an installed batch.
        // Joining its owner here would erase evidence before Durable installs it.
        if *self.phase.borrow() == ServicePhase::RecoveryRequired
            || self.terminal_results.get(&session) == Some(&None)
            || self
                .pending
                .as_ref()
                .and_then(|pending| pending.action.retirement())
                .is_some_and(|request| {
                    request
                        .sessions()
                        .iter()
                        .any(|retained| retained.session() == session)
                })
            || self
                .retirements
                .values()
                .any(|v| v.sessions().iter().any(|v| v.session() == session))
            || self.git_intents.values().any(|intent| {
                matches!(
                    intent.phase(),
                    GitIntentPhase::Started | GitIntentPhase::ExternalFinished
                ) && intent
                    .parts()
                    .affected
                    .parts()
                    .panes
                    .iter()
                    .any(|pane| pane.session == Some(session))
            })
            || (!self.native_completed
                && self
                    .native_retirement
                    .as_ref()
                    .is_some_and(|v| v.sessions().iter().any(|v| v.session() == session)))
        {
            return;
        }
        match self.registry().and_then(|v| v.release(session)) {
            Ok(Some(task)) => {
                let events = self.events.clone();
                tokio::spawn(async move {
                    let _ = events.send(Event::RuntimeJoined(session, task.await)).await;
                });
            }
            Ok(None) => {
                if let Some(registry) = &mut self.registry {
                    registry.forget_unstarted(session);
                }
            }
            Err(error) => self.shutdown_error = Some(error),
        }
    }
    fn apply_git_writes(&mut self, writes: &[OperationWrite]) {
        for write in writes {
            if let OperationWrite::GitIntent(mutation) = write {
                match mutation {
                    GitIntentMutation::Insert(value) => {
                        self.git_intents.insert(value.binding().id, *value.clone());
                    }
                    GitIntentMutation::Advance {
                        binding,
                        next,
                        result,
                        ..
                    } => {
                        if let Some(previous) = self.git_intents.get(&binding.id) {
                            let mut parts = previous.parts().clone();
                            parts.phase = *next;
                            parts.external_result = result.clone();
                            if let Ok(next) = GitWriteIntent::new(parts) {
                                self.git_intents.insert(binding.id, next);
                            } else {
                                self.enter_recovery();
                            }
                        }
                    }
                    GitIntentMutation::Complete { binding } => {
                        self.git_intents.remove(&binding.id);
                        if let Some(descriptor) = self.retirements.remove(&binding.id) {
                            for session in descriptor.sessions() {
                                self.release_unreferenced(session.session());
                            }
                        }
                    }
                }
            }
        }
    }
    fn capture_observations(&mut self) {
        if let Some(registry) = &self.registry {
            for (session, observations) in registry.observations() {
                self.observe(session, observations);
            }
        }
    }
    fn observe(&mut self, session: SessionKey, observations: RetainedRuntimeObservations) {
        if session.epoch != self.cursor.epoch {
            return;
        }
        // Cleanup is retained independently of terminal reduction. A durable
        // ordinary exit may already have deleted this Pane, and a failed Pane
        // no longer has a Starting/Running phase when its cleanup finishes.
        if let Some(cleanup) = observations.cleanup {
            if cleanup.session() != session
                || self
                    .registry()
                    .and_then(|registry| registry.record_cleanup(cleanup))
                    .is_err()
            {
                self.enter_recovery();
                return;
            }
            if matches!(cleanup.result(), CleanupResult::Failed(_)) {
                self.shutdown_error = Some(failure(ServerFailureClass::CleanupIncomplete));
            }
            if self.terminal_results.get(&session) == Some(&None)
                && self.pending.is_none()
                && !self.busy
                && !self
                    .retirements
                    .values()
                    .any(|v| v.sessions().iter().any(|v| v.session() == session))
                && !self
                    .native_retirement
                    .as_ref()
                    .is_some_and(|v| v.sessions().iter().any(|v| v.session() == session))
            {
                self.observe_reduction(ReductionRequest::RuntimeCleanup(cleanup), None, None);
                return;
            }
        }
        if self
            .retirements
            .values()
            .any(|v| v.sessions().iter().any(|v| v.session() == session))
            || self
                .native_retirement
                .as_ref()
                .is_some_and(|v| v.sessions().iter().any(|v| v.session() == session))
        {
            return;
        }
        let snapshot = self.config.core.reducer.snapshot();
        let current = snapshot
            .runtime()
            .iter()
            .find(|v| v.pane == session.pane && v.session_generation == session.generation);
        if current.is_none() {
            return;
        }
        if matches!(observations.lifecycle,RuntimeLifecycle::Running) && current.is_some_and(|v|matches!(v.phase,SessionPhase::Starting{generation} if generation==session.generation)) && self.pending.is_none() && !self.busy{self.observe_reduction(ReductionRequest::RuntimeStarted(session),None,None);return;}
        if let Some(directory) = observations.confirmed_directory
            && directory.session() == session
            && self
                .confirmed_sequences
                .get(&session)
                .is_none_or(|previous| *previous < directory.sequence())
            && self.pending.is_none()
            && !self.busy
        {
            self.observe_reduction(
                ReductionRequest::RestorationDirectoryConfirmed(&directory),
                Some(directory.clone()),
                None,
            );
            return;
        }
        if let Some(terminal) = observations.lifecycle.terminal_result() {
            self.observe_terminal(terminal, None);
            return;
        }
        if let Some(observation) = observations.close_work
            && observation.session() == session
            && self.pending.is_none()
            && !self.busy
        {
            match self.clock.now().and_then(|now| {
                self.config
                    .core
                    .reducer
                    .observe_close_work(observation, now)
            }) {
                Ok(()) => {}
                Err(_) => self.enter_recovery(),
            }
        }
    }
    fn observe_terminal(
        &mut self,
        terminal: SessionTerminalResult,
        cleanup: Option<SessionCleanupObservation>,
    ) {
        let snapshot = self.config.core.reducer.snapshot();
        if !snapshot.runtime().iter().any(|v| {
            v.pane == terminal.session().pane
                && v.session_generation == terminal.session().generation
                && matches!(
                    v.phase,
                    SessionPhase::Starting { .. } | SessionPhase::Running { .. }
                )
        }) {
            return;
        }
        if self.terminal_results.contains_key(&terminal.session())
            || self.pending.is_some()
            || self.busy
            || matches!(terminal.status(), RuntimeTerminalStatus::Retired { .. })
        {
            return;
        }
        self.observe_reduction(
            cleanup.map_or(ReductionRequest::RuntimeFinished(terminal), |cleanup| {
                ReductionRequest::UnstartedLaunchFailed { terminal, cleanup }
            }),
            None,
            Some(terminal),
        );
    }
    fn observe_reduction(
        &mut self,
        request: ReductionRequest<'_>,
        directory: Option<ConfirmedDirectoryObservation>,
        terminal: Option<SessionTerminalResult>,
    ) {
        if *self.phase.borrow() == ServicePhase::RecoveryRequired {
            return;
        }
        let cleanup = match request {
            ReductionRequest::RuntimeCleanup(observation) => Some(observation),
            ReductionRequest::UnstartedLaunchFailed { cleanup, .. } => Some(cleanup),
            _ => None,
        };
        match self.config.core.reducer.prepare(request, self.cursor) {
            Ok(reduction) => {
                let result = self
                    .cursor
                    .parts()
                    .map_err(store_failure)
                    .and_then(|mut parts| {
                        reduction.append_to(&mut parts)?;
                        Ok(parts)
                    });
                match result {
                    Ok(parts) => self.commit(
                        parts,
                        CommitAction::Observed {
                            directory,
                            terminal,
                            cleanup,
                        },
                    ),
                    Err(_) => self.enter_recovery(),
                }
            }
            Err(_) => self.config.core.reducer.not_applied(),
        }
    }
    fn advance(&mut self) -> Result<(), ServerFailure> {
        if *self.phase.borrow() != ServicePhase::RecoveryRequired
            && let Some(session) = self.uninstalled.pop_first()
            && !self.registry()?.is_retiring(session)
        {
            let next = self.current_authority(session)?;
            self.authority_task(session, next, None, None);
        }
        if self.pending.is_some() || self.busy {
            return Ok(());
        }
        if let Some(&(terminal, cleanup)) = self.startup_failures.values().next() {
            // Keep the gate's no-process proof through preparation and definite
            // rollback. Only the matching durable observation releases it.
            self.observe_terminal(terminal, Some(cleanup));
            if self.pending.is_some() {
                return Ok(());
            }
        }
        if self.pending.is_some() || self.busy {
            return Ok(());
        }
        if *self.phase.borrow() != ServicePhase::RecoveryRequired {
            for descriptor in self.retirements.values().cloned().collect::<Vec<_>>() {
                if descriptor.parts().completion == RetirementCompletion::ContinueGitSettlement {
                    if self.advance_git_cleanup(descriptor)? {
                        return Ok(());
                    }
                    continue;
                }
                match self
                    .registry
                    .as_ref()
                    .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
                    .cleanup_for(RetirementRequest::Operation(&descriptor))
                {
                    Ok(Some(cleanup)) => {
                        let observed = observed_retirement(&descriptor, &cleanup)?;
                        if observed != *descriptor {
                            let progress =
                                PendingExternalProgress::new(PendingExternalProgressParts {
                                    binding: descriptor.binding(),
                                    effect: ExternalEffectKind::SessionLifecycle,
                                    state: ExternalProgressState::AcceptedRetirement(Box::new(
                                        observed.clone(),
                                    )),
                                })
                                .map_err(ledger_failure)?;
                            let mut parts = self.cursor.parts().map_err(store_failure)?;
                            self.config
                                .core
                                .ledger
                                .prepare_external_progress(
                                    descriptor.binding(),
                                    progress,
                                    self.cursor.context().map_err(ledger_failure)?,
                                )?
                                .append_to(&mut parts)
                                .map_err(ledger_failure)?;
                            self.commit(
                                parts,
                                CommitAction::RetirementObserved {
                                    descriptor: Arc::new(observed),
                                },
                            );
                            return Ok(());
                        }
                        let reduction = self.config.core.reducer.prepare(
                            ReductionRequest::RetirementCompleted {
                                descriptor: &descriptor,
                                cleanup: &cleanup,
                            },
                            self.cursor,
                        )?;
                        let current = self.config.core.reducer.snapshot();
                        if reduction.snapshot.state() != current.state()
                            || reduction.snapshot.runtime() != current.runtime()
                            || reduction.high_water != self.cursor.high_water
                            || !reduction.organization_writes.is_empty()
                        {
                            return Err(failure(ServerFailureClass::RecoveryRequired));
                        }
                        let mut parts = self.cursor.parts().map_err(store_failure)?;
                        reduction.append_to(&mut parts)?;
                        self.config
                            .core
                            .ledger
                            .prepare_completion(
                                descriptor.binding(),
                                &reduction.outcome,
                                descriptor.acceptance_revision(),
                                self.cursor.context().map_err(ledger_failure)?,
                            )?
                            .append_to(&mut parts)
                            .map_err(ledger_failure)?;
                        self.commit(parts, CommitAction::RetirementCompleted { descriptor });
                        return Ok(());
                    }
                    Err(error) => self.shutdown_error = Some(error),
                    _ => {}
                }
            }
        }
        if self.shutdown.is_some()
            && self.writes_finished
            && self.starting.is_empty()
            && *self.phase.borrow() != ServicePhase::RecoveryRequired
            && self.shutdown_error.is_none()
        {
            if let Some(descriptor) = self.native_retirement.clone() {
                if !self.native_completed
                    && let Some(cleanup) = self
                        .registry
                        .as_ref()
                        .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
                        .cleanup_for(RetirementRequest::Native(&descriptor))?
                {
                    let observed = observed_native_retirement(&descriptor, &cleanup)?;
                    if observed != *descriptor {
                        let reduction = self.config.core.reducer.prepare(
                            ReductionRequest::NativeRetirementObserved {
                                descriptor: &descriptor,
                                cleanup: &cleanup,
                            },
                            self.cursor,
                        )?;
                        let mut parts = self.cursor.parts().map_err(store_failure)?;
                        reduction.append_to(&mut parts)?;
                        self.commit(parts, CommitAction::NativeObserved(Arc::new(observed)));
                        return Ok(());
                    }
                    let reduction = self.config.core.reducer.prepare(
                        ReductionRequest::NativeRetirementCompleted {
                            descriptor: &descriptor,
                            cleanup: &cleanup,
                        },
                        self.cursor,
                    )?;
                    let current = self.config.core.reducer.snapshot();
                    if reduction.snapshot.state() != current.state()
                        || reduction.snapshot.runtime() != current.runtime()
                        || reduction.high_water != self.cursor.high_water
                        || !reduction.organization_writes.is_empty()
                    {
                        return Err(failure(ServerFailureClass::RecoveryRequired));
                    }
                    let mut parts = self.cursor.parts().map_err(store_failure)?;
                    reduction.append_to(&mut parts)?;
                    self.commit(parts, CommitAction::NativeCompleted);
                }
            } else if let Some((request, action)) = self.shutdown.as_ref().and_then(|v| v.request) {
                let sessions = self
                    .registry
                    .as_ref()
                    .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
                    .retirement_sessions(action.reason())?;
                let reduction = self.config.core.reducer.prepare(
                    ReductionRequest::NativeRetirementAccepted {
                        request,
                        action,
                        sessions: &sessions,
                    },
                    self.cursor,
                )?;
                let descriptor = reduction
                    .native_retirement
                    .clone()
                    .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?;
                let mut parts = self.cursor.parts().map_err(store_failure)?;
                reduction.append_to(&mut parts)?;
                self.phase.send_replace(ServicePhase::Draining);
                self.commit(parts, CommitAction::NativeAccepted(Arc::new(descriptor)));
            }
        }
        Ok(())
    }
    fn begin_quiescence(&mut self) {
        if *self.phase.borrow() != ServicePhase::RecoveryRequired {
            self.phase.send_replace(ServicePhase::Quiescing);
        }
        self.terminal_barrier = true;
        for (_, gate) in self.starting.values() {
            gate.cancel();
        }
        let _ = self.intents().map(|v| v.cancel_all());
        if let Some(registry) = &mut self.registry {
            registry.pause_all();
        }
        for connection in self.connections.values_mut() {
            connection.connected = false;
            connection.access.effects_enabled = false;
        }
        if let Some(registry) = &self.registry {
            let transitions: Vec<_> = registry
                .sessions()
                .into_iter()
                .filter(|session| !registry.is_retiring(*session))
                .filter_map(|session| {
                    registry
                        .authority(session)
                        .map(|authority| (session, neutral(authority)))
                })
                .collect();
            for (session, next) in transitions {
                match next {
                    Ok(next) => self.authority_task(session, next, None, None),
                    Err(error) => self.shutdown_error = Some(error),
                }
            }
        }
        if let Some(listeners) = self.listeners.clone() {
            let events = self.events.clone();
            tokio::spawn(async move {
                let result = listeners.close().await;
                let _ = events.send(Event::ListenersClosed(result)).await;
            });
        } else {
            self.listeners_closed = true;
            self.drain_external_work();
        }
    }
    fn drain_external_work(&mut self) {
        // Listener close joins connection admission and its transports. Started
        // writes retain their original durable authority after those joins.
        self.external_work.cancel_reads();
        let work = self.external_work.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = work.finish_started_writes().await;
            let _ = events.send(Event::WritesFinished(result)).await;
        });
    }
    fn enter_recovery(&mut self) {
        self.phase.send_replace(ServicePhase::RecoveryRequired);
        self.shutdown_error = Some(failure(ServerFailureClass::RecoveryRequired));
        if self.shutdown.is_none() {
            self.shutdown = Some(ShutdownState {
                request: None,
                replies: Vec::new(),
            });
            self.begin_quiescence();
        }
    }
    fn can_finish(&self) -> bool {
        self.shutdown.is_some()
            && self.writes_finished
            && self.listeners_closed
            && self.pending.is_none()
            && !self.busy
            && self.starting.is_empty()
            && !self.registry.as_ref().is_some_and(|v| v.joins_pending())
            && (*self.phase.borrow() == ServicePhase::RecoveryRequired
                || self.shutdown_error.is_some()
                || (self.native_completed && self.retirements.is_empty()))
    }
}
async fn await_authorities(
    prepared: Result<
        Vec<oneshot::Receiver<Result<AuthorityApplied, ServerFailure>>>,
        ServerFailure,
    >,
) -> Result<Vec<AuthorityApplied>, ServerFailure> {
    let mut applied = Vec::new();
    for task in prepared? {
        applied.push(
            task.await
                .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))??,
        );
    }
    Ok(applied)
}
fn encode_local(n: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&n.to_be_bytes());
    bytes
}
fn neutral(authority: RuntimeAuthority) -> Result<RuntimeAuthority, ServerFailure> {
    let mut parts = authority.parts();
    parts.connection = None;
    parts.context = parts
        .context
        .checked_successor()
        .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
    parts.focused_pane = false;
    parts.terminal_input_focus = false;
    parts.native_effects_eligible = false;
    RuntimeAuthority::new(parts).map_err(|_| failure(ServerFailureClass::StaleAuthority))
}
fn pairing_allocation(
    cursor: CommitCursor,
    allocation: PairingAllocationRequest,
    actual: IdentityHighWater,
) -> Result<Option<PairingClientStreams>, ServerFailure> {
    let mut expected = cursor.high_water;
    let allocated = match allocation {
        PairingAllocationRequest::None => None,
        PairingAllocationRequest::AdditionalClient(pairing) => {
            expected.client = expected
                .client
                .checked_add(1)
                .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
            expected.operation_stream = expected
                .operation_stream
                .checked_add(2)
                .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
            let client = ClientId::new(cursor.server, expected.client)
                .map_err(|_| failure(ServerFailureClass::Exhausted))?;
            let stream = |local| {
                OperationStreamId::new(
                    OperationStreamKey::new(cursor.server, local)
                        .map_err(|_| failure(ServerFailureClass::Exhausted))?,
                    pairing,
                    client,
                )
                .map_err(|_| failure(ServerFailureClass::ScopeMismatch))
            };
            Some(
                PairingClientStreams::new(PairingClientStreamsParts {
                    client: ClientRecord {
                        id: client,
                        pairing,
                    },
                    ordinary: stream(expected.operation_stream - 1)?,
                    self_revocation: stream(expected.operation_stream)?,
                })
                .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?,
            )
        }
    };
    if actual != expected {
        return Err(failure(ServerFailureClass::RecoveryRequired));
    }
    Ok(allocated)
}
fn validate_pairing_completion(
    proposal: &PreparedPairingCompletion,
    binding: OperationBinding,
    cursor: CommitCursor,
    marks: IdentityHighWater,
    allocated_client: Option<&PairingClientStreams>,
    revoked_clients: &[ClientId],
) -> Result<Vec<StreamBinding>, ServerFailure> {
    if proposal.writes.parts().writes.len() > 2 || proposal.streams.len() > 2 {
        return Err(failure(ServerFailureClass::Capacity));
    }
    let revision = cursor
        .store
        .checked_successor()
        .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
    proposal
        .writes
        .validate_for(cursor.server, revision, marks)
        .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?;
    proposal
        .outcome
        .validate_scope(binding.id.stream())
        .map_err(|_| failure(ServerFailureClass::ScopeMismatch))?;
    if !proposal.outcome.fits_marks(marks) {
        return Err(failure(ServerFailureClass::ScopeMismatch));
    }
    if let PairingOutcome::UnpairAssessed(assessment) = &proposal.outcome
        && assessment.parts().reference
            != (AssessmentRef {
                operation: binding.id,
                digest: binding.digest,
            })
    {
        return Err(failure(ServerFailureClass::ScopeMismatch));
    }
    match allocated_client {
        Some(allocated) => {
            let PairingOutcome::ClientBound(receipt) = &proposal.outcome else {
                return Err(failure(ServerFailureClass::ScopeMismatch));
            };
            let PairingReceiptBody::ClientBinding {
                client,
                operation,
                revision: receipt_revision,
                target,
                keys,
            } = &receipt.parts().claims
            else {
                return Err(failure(ServerFailureClass::ScopeMismatch));
            };
            if client != allocated
                || *operation != binding
                || *receipt_revision != revision
                || !proposal.writes.parts().writes.iter().any(|write| {
                    matches!(write,
                    PairingWrite::PutAuthorization { value, .. }
                        if value.parts().record.state == PairingState::Authorized
                            && value.parts().record.id == target.parts().pairing
                            && value.parts().record.device == target.parts().device
                            && value.parts().keys == *keys
                            && value.parts().clients.contains(allocated))
                })
            {
                return Err(failure(ServerFailureClass::ScopeMismatch));
            }
            let expected = [
                (allocated.parts().ordinary, StreamPurpose::Ordinary, None),
                (
                    allocated.parts().self_revocation,
                    StreamPurpose::SelfRevocation,
                    Some(OutcomeReservation::full(
                        ReservationClass::DedicatedSelfRevocation,
                    )),
                ),
            ];
            if proposal.streams.len() != expected.len() {
                return Err(failure(ServerFailureClass::ScopeMismatch));
            }
            for (stream, purpose, reservation) in expected {
                if !proposal.streams.iter().any(|value| {
                    value.binding.stream() == stream
                        && value.purpose == purpose
                        && value.status == StreamStatus::Active
                        && value.admitted_through == 0
                        && value.acknowledged_through == 0
                        && value.dormant_reservation == reservation
                }) {
                    return Err(failure(ServerFailureClass::ScopeMismatch));
                }
            }
        }
        None if !proposal.streams.is_empty()
            || matches!(proposal.outcome, PairingOutcome::ClientBound(_)) =>
        {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        None => {}
    }
    for stream in &proposal.streams {
        stream.validate_shape().map_err(ledger_failure)?;
    }
    let mut released = BTreeSet::new();
    let mut retired_streams = Vec::new();
    for write in &proposal.writes.parts().writes {
        if let PairingWrite::PutAuthorization { value, .. } = write
            && matches!(value.parts().record.state, PairingState::Revoked { .. })
        {
            for client in &value.parts().clients {
                released.insert(client.parts().client.id);
                retired_streams.push(StreamBinding::from_stream(client.parts().ordinary));
                retired_streams.push(StreamBinding::from_stream(client.parts().self_revocation));
            }
        }
    }
    if released.into_iter().collect::<Vec<_>>() != revoked_clients {
        return Err(failure(ServerFailureClass::ScopeMismatch));
    }
    Ok(retired_streams)
}
fn deletes_membership(write: &OrganizationWrite) -> bool {
    use spaceterm_core::organization_retained::RetainedOrganizationWrite;
    matches!(
        write,
        OrganizationWrite::DeleteWorkspace(_)
            | OrganizationWrite::DeleteTab(_)
            | OrganizationWrite::DeletePane(_)
            | OrganizationWrite::DeleteRepository(_)
            | OrganizationWrite::DeleteWorktree(_)
            | OrganizationWrite::Retained(
                RetainedOrganizationWrite::DeleteWorkspaceNameBasis(_)
                    | RetainedOrganizationWrite::DeleteCreationDefault(_)
                    | RetainedOrganizationWrite::DeletePaneGenerationHighWater(_)
                    | RetainedOrganizationWrite::DeleteTabMembershipVersion(_)
                    | RetainedOrganizationWrite::DeleteRepositoryInventoryVersion(_)
                    | RetainedOrganizationWrite::DeleteWorktreeInventoryFacts(_)
            )
    )
}
fn classify(command: &Command) -> PendingKind {
    spaceterm_core::operation_ledger::admission::command_pending_kind(command)
}

fn reject_action(action: CommitAction, error: ServerFailure) {
    match action {
        CommitAction::Git(action) => git_actor::reject_commit(action, error),
        CommitAction::Admission { reply, .. }
        | CommitAction::Completed { reply, .. }
        | CommitAction::Startup { reply, .. }
        | CommitAction::AcceptedRetirement { reply, .. }
        | CommitAction::PairingCompleted { reply, .. }
        | CommitAction::PairingAdmission { reply, .. } => {
            reply.send(Err(error));
        }
        CommitAction::PairingReserved { reply, .. } => {
            let _ = reply.send(Err(pairing_actor::pairing_failure(error)));
        }
        CommitAction::PairingInvalidated {
            continuation,
            reply,
        } => {
            if let Some(reply) = reply {
                let _ = reply.send(Err(error));
            }
            if let Some(message) = continuation {
                message.reject(error);
            }
        }
        CommitAction::Allocated { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        CommitAction::Auxiliary { reply, .. }
        | CommitAction::Acknowledged(reply)
        | CommitAction::Revoked { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        _ => {}
    }
}
