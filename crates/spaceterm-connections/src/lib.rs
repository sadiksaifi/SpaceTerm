//! Saved direct routes and restricted revocation recovery, outside Client Settings.
pub mod account;
pub mod credentials;
pub mod records;
pub mod setup;
#[cfg(any(test, feature = "test-support"))]
#[allow(dead_code)]
pub mod tests;
mod worker;
#[cfg(test)]
mod worker_tests;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use spaceterm_server::ServerConnection;
use spaceterm_server::direct::{self, DeviceCredential, DirectError, PairedDevice, PairingId};
use spaceterm_server::pane::control::ClientName;
use zeroize::Zeroizing;

pub use credentials::{CredentialStore, Interaction};
use records::{Document, Durability, PendingRecord, Records, SaveError, SavedRoute};

/// An exact enrollment retained before setup can change the server.
/// Dropping an unfinished setup requests restricted QUIC cancellation.
pub struct SetupEnrollment {
    owner: DirectConnections,
    operation: JoinId,
    public_key: [u8; 32],
    endpoint: String,
    pin: direct::ServerPin,
    settled: std::sync::atomic::AtomicBool,
}

pub type DirectRouteId = [u8; 32];
pub type JoinId = [u8; 32];
pub type UnpairKey = [u8; 32];
pub type RouteCleanup = dyn Fn(&DirectRouteId) + Send + Sync;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefusalReason {
    Revoked,
    PinMismatch,
    InvalidProof,
    Incompatible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnavailableReason {
    CredentialStore,
    Network,
    Storage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectRouteStatus {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
    NeedsInteraction,
    Refused(RefusalReason),
    Unavailable(UnavailableReason),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectRoute {
    pub id: DirectRouteId,
    pub label: String,
    pub endpoint: String,
    pub paired: PairedDevice,
}

#[derive(Clone, Debug)]
pub enum JoinStatus {
    Validating,
    /// The live original is retained until explicit retry, cancellation or expiry.
    Retryable {
        stage: EnrollmentStage,
    },
    AwaitingApproval {
        comparison_code: direct::ComparisonCode,
    },
    Approved(DirectRouteId),
    /// The route was published and is usable; synchronization remains unconfirmed.
    ApprovedUncertain(DirectRouteId),
    /// The save may have published. Neither ordinary use nor withdrawal is presumed.
    PublicationUnconfirmed,
    Rejected,
    Expired,
    Failed(DirectError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnrollmentStage {
    Claim,
    Approval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnpairProgress {
    Retrying,
    NeedsInteraction,
    Unconfirmed,
}

#[derive(Clone, Debug)]
pub struct PendingUnpair {
    pub key: UnpairKey,
    pub server_label: String,
    pub state: UnpairProgress,
    pub dismissed: bool,
    pub enrollment_cleanup: bool,
    pub self_unpair: bool,
    pub server_confirmed: bool,
    /// The request was still pending over WSS when QUIC replaced it, and was dropped. The person
    /// must unpair the device again from the server's machine.
    pub abandoned: bool,
}

/// Refuses every connection, so a handle built on it reaches only client storage.
struct NoConnection;

impl direct::DirectConnector for NoConnection {
    fn connect(&self) -> Result<direct::DirectConnection, DirectError> {
        Err(DirectError::Transport)
    }
}

/// Resolves account-local revocation authority without network or credential access.
pub trait AccountRevocationResolver: Send + Sync {
    fn route_for(&self, server: &ServerConnection) -> Result<records::AccountRoute, DirectError>;
    fn resolve(
        &self,
        route: &records::AccountRoute,
        interaction: Interaction,
    ) -> Result<ServerConnection, DirectError>;
}

#[derive(Clone)]
pub struct DirectConnections(Arc<Inner>);

struct Inner {
    identity: String,
    records: Arc<dyn Records>,
    credentials: Arc<dyn CredentialStore>,
    network: Arc<dyn worker::DirectNetwork>,
    state: Mutex<State>,
    changes: Mutex<Vec<async_channel::Sender<()>>>,
    resolver: Mutex<Option<Arc<dyn AccountRevocationResolver>>>,
    /// Removes what the client retained for a route once the person removes the route.
    route_removed: Mutex<Option<Arc<RouteCleanup>>>,
    /// Wakes the worker thread. The worker holds only this and a weak owner while it waits.
    work: Arc<worker::WorkerWake>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The waiting worker finds its owner gone and ends.
        self.work.wake();
    }
}

#[derive(Default)]
struct State {
    loaded: bool,
    inventory_loaded: bool,
    worker: bool,
    document: Document,
    routes: BTreeMap<DirectRouteId, RouteState>,
    joins: BTreeMap<JoinId, JoinState>,
    pending: BTreeMap<UnpairKey, Retry>,
    route_jobs: std::collections::BTreeSet<DirectRouteId>,
    recovery_jobs: std::collections::BTreeSet<UnpairKey>,
    persistence_error: bool,
}

struct RouteState {
    status: DirectRouteStatus,
    server: Option<ServerConnection>,
    gate: Arc<std::sync::atomic::AtomicBool>,
    retry: Retry,
    /// Follows the connected route's link, so its end, not a timer, makes the route due.
    health: Option<Arc<dyn spaceterm_server::connection::link::LinkSink>>,
}

struct JoinState {
    status: JoinStatus,
    cancelled: bool,
    retry_requested: bool,
    deadline: Instant,
}

struct Retry {
    due: Instant,
    attempt: u32,
    interaction: Interaction,
    state: UnpairProgress,
}

impl Retry {
    fn new(interaction: Interaction) -> Self {
        Self {
            due: Instant::now(),
            attempt: 0,
            interaction,
            state: UnpairProgress::Retrying,
        }
    }
}

impl DirectConnections {
    pub fn with_credentials(
        identity: &str,
        records: Arc<dyn Records>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        let registry = Self::new(
            identity,
            records,
            credentials,
            Arc::new(worker::NativeNetwork),
        );
        if let Ok(local) = ServerConnection::for_identity(identity) {
            registry.set_account_resolver(Arc::new(account::LocalRevocationResolver(local)));
        }
        let owner = identity.to_owned();
        registry.on_route_removed(Arc::new(move |route: &DirectRouteId| {
            // A copy that stays behind names a route nothing presents again.
            let _ = ServerConnection::discard_retained_inventory(&owner, *route);
        }));
        registry
    }

    /// Runs `cleanup` for each route the person removes, after its removal is saved.
    pub fn on_route_removed(&self, cleanup: Arc<RouteCleanup>) {
        *self
            .0
            .route_removed
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(cleanup);
    }

    fn new(
        identity: &str,
        records: Arc<dyn Records>,
        credentials: Arc<dyn CredentialStore>,
        network: Arc<dyn worker::DirectNetwork>,
    ) -> Self {
        Self(Arc::new(Inner {
            identity: identity.to_owned(),
            records,
            credentials,
            network,
            state: Mutex::new(State::default()),
            changes: Mutex::default(),
            resolver: Mutex::default(),
            route_removed: Mutex::default(),
            work: Arc::default(),
        }))
    }

    pub fn set_account_resolver(&self, resolver: Arc<dyn AccountRevocationResolver>) {
        *self.0.resolver.lock().unwrap_or_else(|e| e.into_inner()) = Some(resolver);
    }

    /// The startup owner calls this once. Construction and snapshot reads are inert.
    pub fn resume_retained(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.loaded {
            return;
        }
        state.loaded = true;
        match self.0.records.load() {
            Ok(mut document) => {
                for pending in &mut document.pending {
                    if matches!(pending.authority, records::Authority::Enrollment { .. }) {
                        pending.cancel_requested = true;
                    }
                }
                for saved in &document.routes {
                    state.routes.insert(
                        saved.route.id,
                        RouteState {
                            status: if saved.active {
                                DirectRouteStatus::Connecting
                            } else {
                                DirectRouteStatus::Disconnected
                            },
                            server: None,
                            gate: Arc::new(std::sync::atomic::AtomicBool::new(saved.active)),
                            // The person left this route connected. Its first attempt after
                            // launch may let the credential store ask them, so a store that no
                            // longer trusts this build asks now instead of waiting for Resume.
                            // Later retries stay quiet.
                            retry: Retry::new(if saved.active {
                                Interaction::Requested
                            } else {
                                Interaction::Quiet
                            }),
                            health: None,
                        },
                    );
                }
                for pending in &document.pending {
                    state
                        .pending
                        .insert(pending.operation, Retry::new(Interaction::Quiet));
                }
                state.document = document;
                state.inventory_loaded = true;
            }
            Err(_) => state.persistence_error = true,
        }
        drop(state);
        self.start_worker();
        self.0.notify();
    }

    pub fn subscribe(&self) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        self.0
            .changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(sender);
        receiver
    }

    /// Closes a view's subscription before its local task or executor is dropped.
    /// When this returns, no notification for this receiver remains in flight.
    pub fn close_subscription(&self, receiver: &async_channel::Receiver<()>) {
        // Wait for earlier notify calls to finish before closing this receiver.
        let mut changes = self.0.changes.lock().unwrap_or_else(|e| e.into_inner());
        receiver.close();
        changes.retain(|sender| !sender.is_closed());
    }

    /// Whether startup successfully read the saved inventory, including an empty one.
    /// Until then, an absent route is not evidence that the person removed it.
    pub fn retained_inventory_loaded(&self) -> bool {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .inventory_loaded
    }

    pub fn routes(&self) -> Vec<(DirectRouteId, DirectRouteStatus)> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .routes
            .iter()
            .map(|(id, route)| (*id, route.status))
            .collect()
    }

    pub fn route(&self, id: &DirectRouteId) -> Option<DirectRoute> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .document
            .routes
            .iter()
            .find(|route| &route.route.id == id)
            .map(|route| route.route.clone())
    }

    pub fn status(&self, id: &DirectRouteId) -> Option<DirectRouteStatus> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .routes
            .get(id)
            .map(|r| r.status)
    }

    /// A handle to this route's client storage, which holds its client views and the
    /// Workspaces it retained. It never connects, so reading it needs no network or credential.
    pub fn storage(&self, id: &DirectRouteId) -> Option<ServerConnection> {
        let route = self.route(id)?;
        ServerConnection::direct(
            &self.0.identity,
            route.id,
            route.paired,
            Arc::new(NoConnection),
        )
        .ok()
    }

    /// A retained handle keeps interrupted screens frozen until reconciliation succeeds.
    pub fn server(&self, id: &DirectRouteId) -> Option<ServerConnection> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .routes
            .get(id)?
            .server
            .clone()
    }

    pub fn persistence_error(&self) -> bool {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .persistence_error
    }

    /// Connects a saved route. On a route that is already connected this is Reconnect: the
    /// live link is dropped and a new connection, with a new socket and admission, replaces it.
    pub fn connect(&self, id: &DirectRouteId) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(saved) = state.document.routes.iter_mut().find(|r| &r.route.id == id) else {
            return;
        };
        saved.active = true;
        if let Some(route) = state.routes.get_mut(id) {
            if route.server.is_some() {
                // The old link's connector stays disabled; the worker follows the new link.
                route
                    .gate
                    .store(false, std::sync::atomic::Ordering::Release);
                route.gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
                route.server = None;
                route.health = None;
            }
            route.gate.store(true, std::sync::atomic::Ordering::Release);
            route.status = DirectRouteStatus::Connecting;
            route.retry = Retry::new(Interaction::Requested);
        }
        self.0.save(&mut state);
        drop(state);
        self.start_worker();
        self.0.notify();
    }

    pub fn resume(&self, id: &DirectRouteId) {
        self.connect(id);
    }

    pub fn disconnect(&self, id: &DirectRouteId) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(saved) = state.document.routes.iter_mut().find(|r| &r.route.id == id) else {
            return;
        };
        saved.active = false;
        if let Some(route) = state.routes.get_mut(id) {
            route
                .gate
                .store(false, std::sync::atomic::Ordering::Release);
            // Old connectors stay disabled even if the person reconnects before an
            // in-flight attempt returns. Its result belongs to the previous gate.
            route.gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
            // The next connection is a new link, which the worker follows afresh.
            route.server = None;
            route.health = None;
            route.status = DirectRouteStatus::Disconnected;
        }
        self.0.save(&mut state);
        drop(state);
        self.0.notify();
    }

    pub fn cancel(&self, id: &DirectRouteId) {
        self.disconnect(id);
    }

    pub fn join_status(&self, id: JoinId) -> Option<JoinStatus> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .joins
            .get(&id)
            .map(|j| j.status.clone())
    }

    /// Resumes only this paused live original. It never prepares another invitation or claim.
    pub fn retry_join(&self, id: JoinId) -> Result<(), DirectError> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let join = state
            .joins
            .get_mut(&id)
            .ok_or(DirectError::InvalidRequest)?;
        if join.cancelled
            || Instant::now() >= join.deadline
            || !matches!(join.status, JoinStatus::Retryable { .. })
        {
            return Err(DirectError::InvalidRequest);
        }
        join.retry_requested = true;
        join.status = JoinStatus::Validating;
        drop(state);
        self.0.notify();
        Ok(())
    }

    /// Requests cancellation, reporting a failure to synchronize retained cleanup.
    /// Failed retention leaves the request in memory and authorizes no network effect.
    pub fn cancel_join(&self, id: JoinId) -> Result<(), DirectError> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(join) = state.joins.get_mut(&id) {
            join.cancelled = true;
            join.status = JoinStatus::Rejected;
        }
        let result = if let Some(pending) = state.document.pending.iter_mut().find(|p| {
            p.operation == id && matches!(p.authority, records::Authority::Enrollment { .. })
        }) {
            pending.cancel_requested = true;
            if self.0.save(&mut state) {
                Ok(())
            } else {
                Err(DirectError::StorageUnavailable)
            }
        } else if let Some(target) = state
            .document
            .routes
            .iter()
            .find(|saved| saved.route.id == id)
            .map(|saved| saved.route.paired.pairing)
        {
            // Completion uses this same lock. A published join transfers its exact
            // route into the ordinary self-unpair path before cancellation returns.
            self.unpair_locked(&mut state, &id, target)
        } else {
            // Another self-unpair may already own the exact cleanup. Reuse it,
            // including its retention failure, instead of adding another operation.
            let retained = state.document.pending.iter().any(|pending| {
                matches!(&pending.authority, records::Authority::Direct(actor)
                    if actor.route.id == id && pending.target == actor.route.paired.pairing)
            });
            if retained && state.persistence_error && !self.0.save(&mut state) {
                Err(DirectError::StorageUnavailable)
            } else {
                Ok(())
            }
        };
        drop(state);
        self.start_worker();
        self.0.notify();
        result
    }

    pub fn pending_unpairs(&self) -> Vec<PendingUnpair> {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.document.pending.iter().filter(|p| !matches!(p.authority, records::Authority::Enrollment { .. }) || p.cancel_requested).map(|pending| PendingUnpair {
            key: pending.operation,
            server_label: pending.label.clone(),
            state: state.pending.get(&pending.operation).map_or(UnpairProgress::Unconfirmed, |r| r.state),
            dismissed: pending.dismissed,
            enrollment_cleanup: matches!(pending.authority, records::Authority::Enrollment { .. }),
            self_unpair: matches!(&pending.authority, records::Authority::Direct(actor) if actor.route.paired.pairing == pending.target),
            server_confirmed: pending.confirmed,
            abandoned: false,
        }).chain(state.document.abandoned.iter().map(|notice| PendingUnpair {
            key: notice.operation,
            server_label: notice.label.clone(),
            state: UnpairProgress::Unconfirmed,
            dismissed: false,
            enrollment_cleanup: false,
            self_unpair: false,
            server_confirmed: false,
            abandoned: true,
        })).collect()
    }

    pub fn resume_unpair(&self, key: UnpairKey) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(retry) = state.pending.get_mut(&key) {
            *retry = Retry::new(Interaction::Requested);
        }
        drop(state);
        self.start_worker();
        self.0.notify();
    }

    pub fn dismiss_unpair_notice(&self, key: UnpairKey) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let abandoned = state.document.abandoned.len();
        state
            .document
            .abandoned
            .retain(|notice| notice.operation != key);
        if state.document.abandoned.len() != abandoned {
            self.0.save(&mut state);
        }
        if let Some(pending) = state
            .document
            .pending
            .iter_mut()
            .find(|p| p.operation == key)
        {
            pending.dismissed = true;
            self.0.save(&mut state);
        }
        drop(state);
        self.0.notify();
    }

    pub fn unpair_self(&self, id: &DirectRouteId) -> Result<(), DirectError> {
        let target = self.route(id).ok_or(DirectError::Revoked)?.paired.pairing;
        self.unpair(id, target)
    }

    pub fn unpair(&self, id: &DirectRouteId, target: PairingId) -> Result<(), DirectError> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let result = self.unpair_locked(&mut state, id, target);
        let removed = !state.document.routes.iter().any(|r| &r.route.id == id);
        drop(state);
        if removed
            && let Some(cleanup) = self
                .0
                .route_removed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        {
            cleanup(id);
        }
        self.start_worker();
        self.0.notify();
        result
    }

    fn unpair_locked(
        &self,
        state: &mut State,
        id: &DirectRouteId,
        target: PairingId,
    ) -> Result<(), DirectError> {
        let operation = random_id()?;
        let actor = state
            .document
            .routes
            .iter()
            .find(|r| &r.route.id == id)
            .cloned()
            .ok_or(DirectError::Revoked)?;
        let own = actor.route.paired.pairing == target;
        // Removing this route transfers its existing slot to restricted recovery.
        if !own {
            state.document.ensure_capacity()?;
        }
        let pending = PendingRecord::direct(operation, actor, target);
        state.document.pending.push(pending);
        state
            .pending
            .insert(operation, Retry::new(Interaction::Quiet));
        if own {
            state.document.routes.retain(|r| &r.route.id != id);
            if let Some(route) = state.routes.remove(id) {
                route
                    .gate
                    .store(false, std::sync::atomic::Ordering::Release);
            }
        }
        if self.0.save(state) {
            Ok(())
        } else {
            Err(DirectError::StorageUnavailable)
        }
    }

    pub fn revoke_other(
        &self,
        server: ServerConnection,
        target: PairingId,
        label: String,
    ) -> Result<(), DirectError> {
        let pin = server.direct_status()?.pin;
        let route = self
            .0
            .resolver
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .ok_or(DirectError::Transport)?
            .route_for(&server)?;
        let operation = random_id()?;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.document.ensure_capacity()?;
        state
            .document
            .pending
            .push(PendingRecord::account(operation, label, pin, route, target));
        state
            .pending
            .insert(operation, Retry::new(Interaction::Quiet));
        let saved = self.0.save(&mut state);
        drop(state);
        self.start_worker();
        self.0.notify();
        if saved {
            Ok(())
        } else {
            Err(DirectError::StorageUnavailable)
        }
    }
}

impl Inner {
    fn notify(&self) {
        // Every change the views hear of may also make worker jobs due.
        self.work.wake();
        self.changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|s| match s.try_send(()) {
                Ok(()) | Err(async_channel::TrySendError::Full(())) => true,
                Err(async_channel::TrySendError::Closed(())) => false,
            });
    }

    fn save(&self, state: &mut State) -> bool {
        // Claim submission and credential retirement require synchronized retention,
        // even when an unsynchronized publication is a truthful successful join.
        self.publish(state) == Ok(Durability::Synchronized)
    }

    fn publish(&self, state: &mut State) -> Result<Durability, SaveError> {
        let result = self.records.save(&state.document);
        state.persistence_error = result != Ok(Durability::Synchronized);
        if result == Ok(Durability::Synchronized) {
            for join in state.joins.values_mut() {
                if let JoinStatus::ApprovedUncertain(id) = join.status {
                    join.status = JoinStatus::Approved(id);
                }
            }
        }
        result
    }
}

fn random_id() -> Result<[u8; 32], DirectError> {
    let mut id = [0; 32];
    getrandom::fill(&mut id).map_err(|_| DirectError::StorageUnavailable)?;
    Ok(id)
}

pub use credentials::{CredentialError, StoreKind, native};

mod store;
pub use store::{
    ConnectionStore, PendingEnrollment, PendingRevocation, RemovalConfirmation, SavedConnection,
    StoreError, UnpairState, valid_name,
};

pub mod enrollment;
