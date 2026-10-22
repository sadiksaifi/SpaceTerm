//! Saved direct routes and restricted revocation recovery, outside Client Settings.
pub(crate) mod account;
pub(crate) mod credentials;
pub(crate) mod records;
#[cfg(test)]
pub(crate) mod tests;
mod worker;
#[cfg(test)]
mod worker_tests;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use spaceterm_server::LocalServer;
use spaceterm_server::direct::{self, DeviceCredential, DirectError, PairedDevice, PairingId};
use spaceterm_server::pane::control::ClientName;
use zeroize::Zeroizing;

use credentials::{CredentialStore, Interaction};
use records::{Document, PendingRecord, PrivateRecords, Records, SavedRoute};

pub(crate) type DirectRouteId = [u8; 32];
pub(crate) type JoinId = [u8; 32];
pub(crate) type UnpairKey = [u8; 32];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefusalReason {
    Revoked,
    PinMismatch,
    InvalidProof,
    Incompatible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnavailableReason {
    CredentialStore,
    Network,
    Storage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectRouteStatus {
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
pub(crate) struct DirectRoute {
    pub(crate) id: DirectRouteId,
    pub(crate) label: String,
    pub(crate) endpoint: String,
    pub(crate) paired: PairedDevice,
}

#[derive(Clone, Debug)]
pub(crate) enum JoinStatus {
    Validating,
    AwaitingApproval {
        server_name: String,
        server_fingerprint: String,
        device_fingerprint: String,
        expires_at: u64,
    },
    Approved(DirectRouteId),
    Rejected,
    Expired,
    Failed(DirectError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnpairState {
    Retrying,
    NeedsInteraction,
    Unconfirmed,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingUnpair {
    pub(crate) key: UnpairKey,
    pub(crate) server_label: String,
    pub(crate) state: UnpairState,
    pub(crate) dismissed: bool,
    pub(crate) enrollment_cleanup: bool,
    pub(crate) self_unpair: bool,
    pub(crate) server_confirmed: bool,
}

/// Resolves only the retained account route. A changed alias or later connection cannot
/// supply authority. Quiet resolution never starts discovery or authentication.
pub(crate) trait AccountRevocationResolver: Send + Sync {
    fn route_for(&self, server: &LocalServer) -> Result<records::AccountRoute, DirectError>;
    fn resolve(
        &self,
        route: &records::AccountRoute,
        interaction: Interaction,
    ) -> Result<LocalServer, DirectError>;
}

#[derive(Clone)]
pub(crate) struct DirectConnections(Arc<Inner>);

struct Inner {
    identity: String,
    records: Arc<dyn Records>,
    credentials: Arc<dyn CredentialStore>,
    network: Arc<dyn worker::DirectNetwork>,
    state: Mutex<State>,
    changes: Mutex<Vec<async_channel::Sender<()>>>,
    resolver: Mutex<Option<Arc<dyn AccountRevocationResolver>>>,
}

#[derive(Default)]
struct State {
    loaded: bool,
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
    server: Option<LocalServer>,
    gate: Arc<std::sync::atomic::AtomicBool>,
    retry: Retry,
}

struct JoinState {
    status: JoinStatus,
    cancelled: bool,
}

struct Retry {
    due: Instant,
    attempt: u32,
    interaction: Interaction,
    state: UnpairState,
}

impl Retry {
    fn new(interaction: Interaction) -> Self {
        Self {
            due: Instant::now(),
            attempt: 0,
            interaction,
            state: UnpairState::Retrying,
        }
    }
}

impl DirectConnections {
    pub(crate) fn native(identity: &str, paths: Arc<crate::platform::app_paths::AppPaths>) -> Self {
        let registry = Self::new(
            identity,
            Arc::new(PrivateRecords::new(paths)),
            credentials::native(identity),
            Arc::new(worker::NativeNetwork),
        );
        if let Ok(local) = LocalServer::for_identity(identity) {
            registry.set_account_resolver(Arc::new(account::LocalRevocationResolver(local)));
        }
        registry
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
        }))
    }

    pub(crate) fn set_account_resolver(&self, resolver: Arc<dyn AccountRevocationResolver>) {
        *self.0.resolver.lock().unwrap_or_else(|e| e.into_inner()) = Some(resolver);
    }

    /// The startup owner calls this once. Construction and snapshot reads are inert.
    pub(crate) fn resume_retained(&self) {
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
                            retry: Retry::new(Interaction::Quiet),
                        },
                    );
                }
                for pending in &document.pending {
                    state
                        .pending
                        .insert(pending.operation, Retry::new(Interaction::Quiet));
                }
                state.document = document;
            }
            Err(()) => state.persistence_error = true,
        }
        drop(state);
        self.start_worker();
        self.0.notify();
    }

    pub(crate) fn subscribe(&self) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        self.0
            .changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(sender);
        receiver
    }

    pub(crate) fn routes(&self) -> Vec<(DirectRouteId, DirectRouteStatus)> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .routes
            .iter()
            .map(|(id, route)| (*id, route.status))
            .collect()
    }

    pub(crate) fn route(&self, id: &DirectRouteId) -> Option<DirectRoute> {
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

    pub(crate) fn status(&self, id: &DirectRouteId) -> Option<DirectRouteStatus> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .routes
            .get(id)
            .map(|r| r.status)
    }

    /// A retained handle keeps interrupted screens frozen until reconciliation succeeds.
    pub(crate) fn server(&self, id: &DirectRouteId) -> Option<LocalServer> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .routes
            .get(id)?
            .server
            .clone()
    }

    pub(crate) fn persistence_error(&self) -> bool {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .persistence_error
    }

    pub(crate) fn connect(&self, id: &DirectRouteId) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(saved) = state.document.routes.iter_mut().find(|r| &r.route.id == id) else {
            return;
        };
        saved.active = true;
        if let Some(route) = state.routes.get_mut(id) {
            route.gate.store(true, std::sync::atomic::Ordering::Release);
            route.status = DirectRouteStatus::Connecting;
            route.retry = Retry::new(Interaction::Requested);
        }
        self.0.save(&mut state);
        drop(state);
        self.start_worker();
        self.0.notify();
    }

    pub(crate) fn resume(&self, id: &DirectRouteId) {
        self.connect(id);
    }

    pub(crate) fn disconnect(&self, id: &DirectRouteId) {
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
            route.server = None;
            route.status = DirectRouteStatus::Disconnected;
        }
        self.0.save(&mut state);
        drop(state);
        self.0.notify();
    }

    pub(crate) fn cancel(&self, id: &DirectRouteId) {
        self.disconnect(id);
    }

    pub(crate) fn join_status(&self, id: JoinId) -> Option<JoinStatus> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .joins
            .get(&id)
            .map(|j| j.status.clone())
    }

    pub(crate) fn cancel_join(&self, id: JoinId) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(join) = state.joins.get_mut(&id) {
            join.cancelled = true;
            join.status = JoinStatus::Rejected;
        }
        if let Some(pending) = state
            .document
            .pending
            .iter_mut()
            .find(|p| p.operation == id)
        {
            pending.cancel_requested = true;
            self.0.save(&mut state);
        }
        drop(state);
        self.start_worker();
        self.0.notify();
    }

    pub(crate) fn pending_unpairs(&self) -> Vec<PendingUnpair> {
        let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.document.pending.iter().filter(|p| !matches!(p.authority, records::Authority::Enrollment { .. }) || p.cancel_requested).map(|pending| PendingUnpair {
            key: pending.operation,
            server_label: pending.label.clone(),
            state: state.pending.get(&pending.operation).map_or(UnpairState::Unconfirmed, |r| r.state),
            dismissed: pending.dismissed,
            enrollment_cleanup: matches!(pending.authority, records::Authority::Enrollment { .. }),
            self_unpair: matches!(&pending.authority, records::Authority::Direct(actor) if actor.route.paired.pairing == pending.target),
            server_confirmed: pending.confirmed,
        }).collect()
    }

    pub(crate) fn resume_unpair(&self, key: UnpairKey) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(retry) = state.pending.get_mut(&key) {
            *retry = Retry::new(Interaction::Requested);
        }
        drop(state);
        self.start_worker();
        self.0.notify();
    }

    pub(crate) fn dismiss_unpair_notice(&self, key: UnpairKey) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
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

    pub(crate) fn unpair_self(&self, id: &DirectRouteId) -> Result<(), DirectError> {
        let target = self.route(id).ok_or(DirectError::Revoked)?.paired.pairing;
        self.unpair(id, target)
    }

    pub(crate) fn unpair(&self, id: &DirectRouteId, target: PairingId) -> Result<(), DirectError> {
        let operation = random_id()?;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
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

    pub(crate) fn revoke_other(
        &self,
        server: LocalServer,
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
        self.changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|s| match s.try_send(()) {
                Ok(()) | Err(async_channel::TrySendError::Full(())) => true,
                Err(async_channel::TrySendError::Closed(())) => false,
            });
    }

    fn save(&self, state: &mut State) -> bool {
        let saved = self.records.save(&state.document).is_ok();
        state.persistence_error = !saved;
        saved
    }
}

fn random_id() -> Result<[u8; 32], DirectError> {
    let mut id = [0; 32];
    getrandom::fill(&mut id).map_err(|_| DirectError::StorageUnavailable)?;
    Ok(id)
}
