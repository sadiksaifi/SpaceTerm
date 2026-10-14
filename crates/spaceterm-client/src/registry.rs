//! Independent server handles. The runtime, not disposable handles, owns workers.

use crate::{
    api::{ConnectedInfo, ConnectionState, PairedServerBinding, RetryState, ServerSummary},
    connection::{self, ConnectRequest, DispatchRequest},
    failure::{ClientFailure, ClientFailureClass, resource_limit},
    ports::{ClientServices, ConnectionInvalidation, OutboundClass},
    replica::{OrganizationResidency, Replica, ReplicaState},
};
use spaceterm_core::{identity::ServerId, pairing::LocalPairingKey};
use spaceterm_protocol::{
    codec::Payload,
    limits::{
        MAX_EGRESS_BYTES, MAX_EGRESS_ENVELOPES, RESERVED_LIFECYCLE_BYTES,
        RESERVED_LIFECYCLE_ENVELOPES,
    },
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::{Semaphore, mpsc, oneshot, watch},
    task::JoinHandle,
};

pub(crate) struct Shared {
    pub services: ClientServices,
    pub slots: Arc<Semaphore>,
    pub stopped: AtomicBool,
    entries: Mutex<BTreeMap<ServerId, ServerHandle>>,
    tasks: Mutex<WorkerTasks>,
    residency: Arc<OrganizationResidency>,
}

impl Shared {
    pub(crate) fn server(&self, server: ServerId) -> Option<ServerHandle> {
        self.entries.lock().ok()?.get(&server).cloned()
    }
}

type WorkerCompletion = watch::Receiver<Option<Result<(), ClientFailure>>>;

#[derive(Default)]
struct WorkerTasks {
    active: BTreeMap<ServerId, WorkerCompletion>,
    draining: Vec<WorkerCompletion>,
    failure: Option<ClientFailure>,
}
impl WorkerTasks {
    fn reap_drained(&mut self) {
        self.draining.retain(|task| {
            if let Some(result) = *task.borrow() {
                if let Err(error) = result {
                    self.failure.get_or_insert(error);
                }
                false
            } else {
                true
            }
        });
    }
}

fn track_worker(task: JoinHandle<Result<(), ClientFailure>>) -> WorkerCompletion {
    let (finished, completion) = watch::channel(None);
    // The join owner is independent of removal/shutdown callers. Cancelling a
    // waiter cannot detach the worker or consume its sole cleanup completion.
    tokio::spawn(async move {
        let result = task
            .await
            .map_err(|_| cancelled())
            .and_then(|result| result);
        finished.send_replace(Some(result));
    });
    completion
}
async fn wait_worker(mut completion: WorkerCompletion) -> Result<(), ClientFailure> {
    loop {
        if let Some(result) = *completion.borrow_and_update() {
            return result;
        }
        completion.changed().await.map_err(|_| cancelled())?;
    }
}

#[derive(Clone)]
pub struct ClientRegistry {
    shared: Arc<Shared>,
}
pub struct ClientRuntime {
    shared: Arc<Shared>,
}

impl ClientRegistry {
    pub async fn recover(services: ClientServices) -> Result<(Self, ClientRuntime), ClientFailure> {
        Self::recover_with_retained(services, vec![]).await
    }

    /// Seed bounded metadata before actors or presentation consume a recovered personal view.
    /// These snapshots are stale presentation only, never connection or control authority.
    pub async fn recover_with_retained(
        services: ClientServices,
        snapshots: Vec<spaceterm_core::organization::OrganizationSnapshot>,
    ) -> Result<(Self, ClientRuntime), ClientFailure> {
        let limits = services.limits.validate()?;
        if snapshots.len() > limits.saved_servers {
            return Err(resource_limit());
        }
        let mut retained = BTreeMap::new();
        for snapshot in snapshots {
            if retained.insert(snapshot.state().server, snapshot).is_some() {
                return Err(ClientFailure::new(ClientFailureClass::InvalidStorage));
            }
        }
        if services.additional_capabilities.len() > 7
            || services.additional_capabilities.iter().any(|capability| {
                !matches!(
                    *capability,
                    spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT
                        | spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES
                        | spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS
                        | spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS
                        | spaceterm_protocol::negotiation::CAPABILITY_NATIVE_NOTIFICATIONS
                        | spaceterm_protocol::negotiation::CAPABILITY_APPLICATION_MOUSE
                        | spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA
                )
            })
        {
            return Err(resource_limit());
        }
        let bindings = services.journal.recover_bindings().await?;
        if bindings.len() > limits.saved_servers {
            return Err(resource_limit());
        }
        let mut seen = std::collections::BTreeSet::new();
        for binding in &bindings {
            if !seen.insert(binding.target().server()) {
                return Err(ClientFailure::new(ClientFailureClass::InvalidStorage));
            }
        }
        let shared = Arc::new(Shared {
            services,
            slots: Arc::new(Semaphore::new(limits.connection_actors)),
            stopped: AtomicBool::new(false),
            entries: Mutex::new(BTreeMap::new()),
            tasks: Mutex::new(WorkerTasks::default()),
            residency: Arc::new(OrganizationResidency::new(limits)?),
        });
        let registry = Self {
            shared: Arc::clone(&shared),
        };
        let runtime = ClientRuntime { shared };
        for binding in bindings {
            let snapshot = retained.remove(&binding.target().server());
            registry.register_retained(binding, snapshot).await?;
        }
        Ok((registry, runtime))
    }

    pub async fn register(
        &self,
        binding: PairedServerBinding,
    ) -> Result<ServerHandle, ClientFailure> {
        self.register_retained(binding, None).await
    }
    async fn register_retained(
        &self,
        binding: PairedServerBinding,
        snapshot: Option<spaceterm_core::organization::OrganizationSnapshot>,
    ) -> Result<ServerHandle, ClientFailure> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        if !self
            .shared
            .services
            .journal
            .ordinary_use_allowed(&binding)
            .await?
        {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        let mut entries = self.shared.entries.lock().map_err(|_| cancelled())?;
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let server = binding.target().server();
        if let Some(existing) = entries.get(&server) {
            if existing.binding == binding {
                return Ok(existing.clone());
            }
            // Replacement is explicit local removal/re-enrollment, never reconnect.
            return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
        }
        if entries.len() >= self.shared.services.limits.saved_servers {
            return Err(resource_limit());
        }
        let (state, receive_state) = watch::channel(ConnectionState::Offline {
            failure: ClientFailure::new(ClientFailureClass::Unavailable),
            retry: RetryState {
                failures: 0,
                delay: None,
            },
        });
        let mut replica = Replica::new(server, Arc::clone(&self.shared.residency));
        if let Some(snapshot) = snapshot {
            let token = replica.begin_resync(snapshot.epoch())?;
            replica.install_snapshot(token, snapshot)?;
            replica.invalidate(crate::replica::ResyncReason::ConnectionLost);
        }
        let receive_replica = replica.subscribe();
        let (connect, requests) = mpsc::channel(8);
        let (shutdown, receive_shutdown) = watch::channel(false);
        let (dispatch, outgoing) = mpsc::channel(MAX_EGRESS_ENVELOPES);
        let egress = Arc::new(EgressBudget::default());
        let queries = Arc::new(connection::queries::QueryHub::default());
        let handle = ServerHandle {
            queries: Arc::clone(&queries),
            binding: binding.clone(),
            state: receive_state,
            replica: receive_replica,
            connect,
            dispatch,
            egress: Arc::clone(&egress),
            shared: Arc::downgrade(&self.shared),
            shutdown,
        };
        let shared = Arc::clone(&self.shared);
        let task = tokio::spawn(connection::run(
            shared,
            binding,
            state,
            replica,
            connection::WorkerChannels {
                requests,
                outgoing,
                shutdown: receive_shutdown,
                egress,
                queries,
            },
        ));
        let mut tasks = self.shared.tasks.lock().map_err(|_| cancelled())?;
        tasks.reap_drained();
        tasks.active.insert(server, track_worker(task));
        entries.insert(server, handle.clone());
        Ok(handle)
    }

    /// Applies the one journal's already-durable ordinary-use suppression to
    /// the exact saved pairing. An old completion cannot remove a new binding.
    /// This operation never submits a server revocation or releases control.
    pub async fn remove_suppressed(&self, key: &LocalPairingKey) -> Result<bool, ClientFailure> {
        let server = key.parts().server;
        let Some(handle) = self.server(server) else {
            return Ok(false);
        };
        if &handle.binding.target().local_pairing()? != key {
            return Ok(false);
        }
        if self
            .shared
            .services
            .journal
            .ordinary_use_allowed(&handle.binding)
            .await?
        {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        self.shared
            .services
            .delegate
            .invalidate(&handle.binding, ConnectionInvalidation::BindingRemoved);
        handle.shutdown.send_replace(true);
        let task = {
            let mut entries = self.shared.entries.lock().map_err(|_| cancelled())?;
            if entries
                .get(&server)
                .is_none_or(|current| current.binding != handle.binding)
            {
                return Ok(false);
            }
            let mut tasks = self.shared.tasks.lock().map_err(|_| cancelled())?;
            tasks.reap_drained();
            entries.remove(&server);
            let task = tasks.active.remove(&server);
            if let Some(task) = &task {
                tasks.draining.push(task.clone());
            }
            task
        };
        if let Some(task) = task {
            let result = wait_worker(task).await;
            self.shared
                .tasks
                .lock()
                .map_err(|_| cancelled())?
                .reap_drained();
            result?;
        }
        Ok(true)
    }

    /// Restores personal state with journal markers, then seeds every retained
    /// replica before the frontend consumes a Ready publication.
    pub fn restore_view<S: crate::view::ViewStore>(
        &self,
        persistence: &mut crate::view::ViewPersistence<S>,
        journal: &crate::journal::JournalController,
    ) -> Result<
        (crate::view::ClientViewState, Vec<crate::view::ViewUpdate>),
        crate::view::launch::LaunchFailure,
    > {
        use crate::view::{ReplicaFacts, ReplicaObservation, ReplicaReadiness, ServerViewStatus};
        let entries: Vec<_> = self
            .shared
            .entries
            .lock()
            .map_err(|_| cancelled())?
            .values()
            .cloned()
            .collect();
        let retained: Vec<_> = entries
            .iter()
            .filter_map(|handle| {
                handle
                    .replica
                    .borrow()
                    .retained()
                    .cloned()
                    .map(|snapshot| (handle, snapshot))
            })
            .collect();
        let observations = retained
            .iter()
            .map(|(handle, snapshot)| {
                let status = match *handle.state.borrow() {
                    ConnectionState::Connected { .. } => ServerViewStatus::Connected,
                    ConnectionState::Connecting { .. } => ServerViewStatus::Connecting,
                    ConnectionState::Offline { .. } => ServerViewStatus::Offline,
                    ConnectionState::Incompatible { .. } => ServerViewStatus::Incompatible,
                    ConnectionState::Stopped => ServerViewStatus::Stopped,
                };
                ReplicaObservation::new(
                    &snapshot.organization,
                    &[],
                    ReplicaFacts {
                        pairing: handle.binding.target().pairing(),
                        client: handle.binding.target().client(),
                        connection: None,
                        readiness: ReplicaReadiness::Retained,
                        status,
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        crate::view::launch::restore(persistence, journal, &observations)
    }

    pub fn server(&self, server: ServerId) -> Option<ServerHandle> {
        self.shared.entries.lock().ok()?.get(&server).cloned()
    }
    pub fn servers(&self) -> Vec<ServerSummary> {
        self.shared
            .entries
            .lock()
            .map(|entries| {
                entries
                    .values()
                    .map(|h| ServerSummary {
                        server: h.identity(),
                        state: h.state.borrow().clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl ClientRuntime {
    /// Stops client routes only. No release, retirement, shutdown or Start message
    /// is sent to any server, even while a connection is reconciling.
    pub async fn shutdown(self) -> Result<(), ClientFailure> {
        self.invalidate();
        let (tasks, mut result) = {
            let mut tasks = self.shared.tasks.lock().map_err(|_| cancelled())?;
            tasks.reap_drained();
            (
                tasks
                    .active
                    .values()
                    .chain(&tasks.draining)
                    .cloned()
                    .collect::<Vec<_>>(),
                tasks.failure.map_or(Ok(()), Err),
            )
        };
        for task in tasks {
            let joined = wait_worker(task).await;
            if result.is_ok() {
                result = joined;
            }
        }
        result
    }
    fn invalidate(&self) {
        self.shared.stopped.store(true, Ordering::Release);
        let handles = self
            .shared
            .entries
            .lock()
            .map(|entries| entries.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for handle in handles {
            self.shared
                .services
                .delegate
                .invalidate(&handle.binding, ConnectionInvalidation::Shutdown);
            handle.shutdown.send_replace(true);
        }
    }
}
impl Drop for ClientRuntime {
    fn drop(&mut self) {
        self.invalidate();
    }
}

#[derive(Clone)]
pub struct ServerHandle {
    pub(crate) binding: PairedServerBinding,
    state: watch::Receiver<ConnectionState>,
    replica: watch::Receiver<ReplicaState>,
    connect: mpsc::Sender<ConnectRequest>,
    dispatch: mpsc::Sender<DispatchRequest>,
    egress: Arc<EgressBudget>,
    pub(crate) shared: std::sync::Weak<Shared>,
    pub(crate) queries: Arc<connection::queries::QueryHub>,
    shutdown: watch::Sender<bool>,
}
impl ServerHandle {
    pub fn identity(&self) -> ServerId {
        self.binding.target().server()
    }
    pub fn state(&self) -> watch::Receiver<ConnectionState> {
        self.state.clone()
    }
    pub fn replica(&self) -> watch::Receiver<ReplicaState> {
        self.replica.clone()
    }
    pub async fn connect(&self) -> Result<(), ClientFailure> {
        self.request(false).await
    }
    pub async fn retry(&self) -> Result<(), ClientFailure> {
        self.request(true).await
    }
    async fn request(&self, retry: bool) -> Result<(), ClientFailure> {
        let (completion, result) = oneshot::channel();
        self.connect
            .try_send(ConnectRequest { retry, completion })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => resource_limit(),
                mpsc::error::TrySendError::Closed(_) => cancelled(),
            })?;
        result.await.map_err(|_| cancelled())?
    }

    /// Operation/Pane providers supply messages admitted by the injected delegate.
    /// The original connection token is checked again at final transmission. New
    /// offline work and old queued input never survive a reconnect.
    pub async fn dispatch(
        &self,
        captured: ConnectedInfo,
        message: Payload,
    ) -> Result<(), ClientFailure> {
        self.dispatch_inner(captured, message, false).await
    }
    pub fn queries(
        &self,
        journal: Arc<crate::journal::JournalController>,
    ) -> Result<crate::api::ServerQueries, ClientFailure> {
        let shared = self.shared.upgrade().ok_or_else(cancelled)?;
        let supplied: Arc<dyn crate::ports::ConnectionJournal> = journal.clone();
        if !Arc::ptr_eq(&supplied, &shared.services.journal) {
            return Err(ClientFailure::new(ClientFailureClass::InvalidStorage));
        }
        connection::queries::ServerQueries::new(self.clone(), journal)
    }
    pub(crate) async fn dispatch_query(
        &self,
        captured: ConnectedInfo,
        message: Payload,
    ) -> Result<(), ClientFailure> {
        self.dispatch_inner(captured, message, true).await
    }
    async fn dispatch_inner(
        &self,
        captured: ConnectedInfo,
        message: Payload,
        query: bool,
    ) -> Result<(), ClientFailure> {
        let shared = self.shared.upgrade().ok_or_else(cancelled)?;
        if shared.stopped.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let current = match &*self.state.borrow() {
            ConnectionState::Connected { info } if same_connection(*info, captured) => *info,
            _ => return Err(ClientFailure::new(ClientFailureClass::StaleAuthority)),
        };
        if message.major() != current.major {
            return Err(ClientFailure::new(ClientFailureClass::InvalidPeer));
        }
        let class = if query {
            self.queries.admit(current, &message, &self.binding)?;
            OutboundClass::Ordinary
        } else {
            shared.services.delegate.admit(current, &message)?
        };
        let bytes = message
            .encode_with_limits(current.limits)?
            .len()
            .checked_add(512)
            .ok_or_else(resource_limit)?;
        let reservation = self.egress.reserve(bytes, class)?;
        let (completion, result) = oneshot::channel();
        self.dispatch
            .try_send(DispatchRequest {
                captured,
                query,
                message,
                completion,
                _reservation: reservation,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => resource_limit(),
                mpsc::error::TrySendError::Closed(_) => cancelled(),
            })?;
        result.await.map_err(|_| cancelled())?
    }
}

pub(crate) fn same_connection(left: ConnectedInfo, right: ConnectedInfo) -> bool {
    left.server == right.server
        && left.pairing == right.pairing
        && left.client == right.client
        && left.epoch == right.epoch
        && left.binding == right.binding
        && left.major == right.major
        && left.attempt == right.attempt
}
pub(crate) fn cancelled() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::Cancelled)
}

#[derive(Default)]
pub(crate) struct EgressBudget(Mutex<EgressHeld>);
#[derive(Default)]
struct EgressHeld {
    bytes: usize,
    envelopes: usize,
    ordinary_bytes: usize,
    ordinary_envelopes: usize,
}
impl EgressBudget {
    pub(crate) fn reserve(
        self: &Arc<Self>,
        bytes: usize,
        class: OutboundClass,
    ) -> Result<EgressReservation, ClientFailure> {
        let mut held = self.0.lock().map_err(|_| resource_limit())?;
        let total = held.bytes.checked_add(bytes).ok_or_else(resource_limit)?;
        if total > MAX_EGRESS_BYTES || held.envelopes >= MAX_EGRESS_ENVELOPES {
            return Err(resource_limit());
        }
        if class == OutboundClass::Ordinary
            && (held
                .ordinary_bytes
                .checked_add(bytes)
                .is_none_or(|n| n > MAX_EGRESS_BYTES - RESERVED_LIFECYCLE_BYTES)
                || held.ordinary_envelopes >= MAX_EGRESS_ENVELOPES - RESERVED_LIFECYCLE_ENVELOPES)
        {
            return Err(resource_limit());
        }
        held.bytes = total;
        held.envelopes += 1;
        if class == OutboundClass::Ordinary {
            held.ordinary_bytes += bytes;
            held.ordinary_envelopes += 1;
        }
        Ok(EgressReservation {
            owner: Arc::clone(self),
            bytes,
            class,
        })
    }
}
pub(crate) struct EgressReservation {
    owner: Arc<EgressBudget>,
    bytes: usize,
    class: OutboundClass,
}
impl Drop for EgressReservation {
    fn drop(&mut self) {
        if let Ok(mut held) = self.owner.0.lock() {
            held.bytes -= self.bytes;
            held.envelopes -= 1;
            if self.class == OutboundClass::Ordinary {
                held.ordinary_bytes -= self.bytes;
                held.ordinary_envelopes -= 1;
            }
        }
    }
}
