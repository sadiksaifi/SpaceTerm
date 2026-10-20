//! Requested SSH server connections, retained intent and bounded continuation.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use spaceterm_server::LocalServer;

use super::cancellation::SshCancellationToken;
use crate::domain::SshDestination;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectPhase {
    Authenticating,
    Discovering,
    Installing,
    Starting,
    Connecting,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InteractionReason {
    Authentication,
    HostVerification,
    CredentialProvider,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[expect(
    clippy::enum_variant_names,
    reason = "the shared UI contract names each observed continuity change"
)]
pub(crate) enum ContinuityChange {
    HostChanged,
    AccountChanged,
    ServerChanged,
    BuildChanged,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IncompatibilityKind {
    Protocol,
    Capability,
    Platform,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefusalKind {
    AuthorizationRefused,
    InstallRefused,
    InvalidArtifact,
    IntentPersistence,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnavailableKind {
    LocalIpcUnavailable,
    Network,
    HostUnreachable,
    SshFailed,
    ServiceUnavailable,
    CandidateUnavailable,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnconfirmedOperation {
    Install,
    Connect,
}

#[derive(Clone)]
pub(crate) enum RemoteConnectionStatus {
    Inactive,
    Working {
        phase: ConnectPhase,
        cancellable: bool,
    },
    Reconnecting {
        phase: ConnectPhase,
        attempt: u32,
        next_retry: Option<Instant>,
    },
    Cancelling,
    NeedsPerson {
        reason: InteractionReason,
    },
    InstallationOffered {
        offer_id: u64,
        version: String,
        platform: (String, String),
    },
    Connected {
        protocol_major: u16,
    },
    Paused {
        reason: ContinuityChange,
    },
    Stopped,
    Incompatible {
        kind: IncompatibilityKind,
    },
    Refused {
        kind: RefusalKind,
    },
    Unavailable {
        kind: UnavailableKind,
    },
    Unconfirmed {
        operation: UnconfirmedOperation,
    },
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SshServerContinuity {
    pub route: [u8; 32],
    pub account: u32,
    pub build: String,
    pub server: [u8; 32],
}
impl SshServerContinuity {
    pub(crate) fn verify(&self, observed: &Self) -> Result<(), SshConnectFailure> {
        let reason = if self.route != observed.route {
            Some(ContinuityChange::HostChanged)
        } else if self.account != observed.account {
            Some(ContinuityChange::AccountChanged)
        } else if self.build != observed.build {
            Some(ContinuityChange::BuildChanged)
        } else if self.server != observed.server {
            Some(ContinuityChange::ServerChanged)
        } else {
            None
        };
        reason.map_or(Ok(()), |reason| Err(SshConnectFailure::Changed(reason)))
    }
}
#[derive(Clone)]
pub(crate) struct RetainedSshConnection {
    pub destination: SshDestination,
    pub continuity: Option<SshServerContinuity>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SshIntentStoreError {
    Unavailable,
    Malformed,
}
pub(crate) trait SshConnectionIntentStore: Send + Sync {
    fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError>;
    fn retain(&self, connection: RetainedSshConnection) -> Result<(), SshIntentStoreError>;
    fn remove(&self, destination: &SshDestination) -> Result<(), SshIntentStoreError>;
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectMode {
    Requested,
    QuietContinuation,
}
pub(crate) type ConnectProgress<'a> = dyn Fn(ConnectPhase) + Send + Sync + 'a;

/// A live route is retained with the connected handle. Its drop releases only owned SSH processes.
pub(crate) trait SshServerLease: Send + Sync {
    fn is_alive(&self) -> bool;
}
pub(crate) struct ConnectedSshServer {
    pub server: LocalServer,
    pub continuity: SshServerContinuity,
    pub lease: Arc<dyn SshServerLease>,
    pub protocol_major: u16,
}
pub(crate) trait SshInstallation: Send + Sync {
    fn install(
        &self,
        cancellation: &SshCancellationToken,
        progress: &ConnectProgress<'_>,
    ) -> Result<ConnectedSshServer, SshConnectFailure>;
}
pub(crate) struct SshInstallationOffer {
    pub version: String,
    pub platform: (String, String),
    pub installation: Arc<dyn SshInstallation>,
}
pub(crate) enum SshConnectFailure {
    Cancelled,
    NeedsPerson(InteractionReason),
    Changed(ContinuityChange),
    Stopped,
    Incompatible(IncompatibilityKind),
    Refused(RefusalKind),
    Unavailable(UnavailableKind),
    Unconfirmed(UnconfirmedOperation),
    InstallationOffered(SshInstallationOffer),
}
/// All remote effects run on the registry's dedicated worker, never on a render thread.
pub(crate) trait SshConnectBackend: Send + Sync {
    fn disconnect(&self, _destination: &SshDestination) {}
    fn forget(&self, destination: &SshDestination) {
        self.disconnect(destination);
    }
    fn connect(
        &self,
        destination: &SshDestination,
        mode: ConnectMode,
        expected: Option<&SshServerContinuity>,
        cancellation: &SshCancellationToken,
        progress: &ConnectProgress<'_>,
    ) -> Result<ConnectedSshServer, SshConnectFailure>;
}

struct Entry {
    status: RemoteConnectionStatus,
    continuity: Option<SshServerContinuity>,
    presented: Option<LocalServer>,
    cancellation: SshCancellationToken,
    generation: u64,
    running: bool,
    offer: Option<(u64, Arc<dyn SshInstallation>)>,
}
impl Default for Entry {
    fn default() -> Self {
        Self {
            status: RemoteConnectionStatus::Inactive,
            continuity: None,
            presented: None,
            cancellation: SshCancellationToken::default(),
            generation: 0,
            running: false,
            offer: None,
        }
    }
}
struct State {
    entries: HashMap<String, Entry>,
    subscribers: Vec<async_channel::Sender<()>>,
    next_offer: u64,
    resumed: bool,
    persistence_error: Option<SshIntentStoreError>,
}
struct Inner {
    state: Mutex<State>,
    backend: Arc<dyn SshConnectBackend>,
    intents: Arc<dyn SshConnectionIntentStore>,
    persistence: async_channel::Sender<PersistenceCommand>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(state) = self.state.lock() {
            for entry in state.entries.values() {
                entry.cancellation.cancel();
            }
        }
    }
}
/// One app-wide owner. Constructing or reading it performs no discovery or storage access.
#[derive(Clone)]
pub(crate) struct SshServerConnections {
    inner: Arc<Inner>,
}
impl SshServerConnections {
    pub(crate) fn new(
        backend: Arc<dyn SshConnectBackend>,
        intents: Arc<dyn SshConnectionIntentStore>,
    ) -> Self {
        let (persistence, receiver) = async_channel::unbounded();
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                entries: HashMap::new(),
                subscribers: Vec::new(),
                next_offer: 0,
                resumed: false,
                persistence_error: None,
            }),
            backend,
            intents,
            persistence,
        });
        let weak = Arc::downgrade(&inner);
        if std::thread::Builder::new()
            .name("spaceterm-ssh-intent".into())
            .spawn(move || persistence_worker(weak, receiver))
            .is_err()
            && let Ok(mut state) = inner.state.lock()
        {
            state.persistence_error = Some(SshIntentStoreError::Unavailable);
        }
        Self { inner }
    }
    pub(crate) fn status(&self, destination: &SshDestination) -> RemoteConnectionStatus {
        self.inner
            .state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .entries
                    .get(destination.as_str())
                    .map(|entry| entry.status.clone())
            })
            .unwrap_or(RemoteConnectionStatus::Inactive)
    }
    pub(crate) fn connections(&self) -> Vec<(SshDestination, RemoteConnectionStatus)> {
        let mut connections: Vec<_> = self
            .inner
            .state
            .lock()
            .ok()
            .map(|state| {
                state
                    .entries
                    .iter()
                    .filter_map(|(destination, entry)| {
                        SshDestination::new(destination.clone())
                            .ok()
                            .map(|destination| (destination, entry.status.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        connections.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        connections
    }
    pub(crate) fn server(&self, destination: &SshDestination) -> Option<LocalServer> {
        self.inner.state.lock().ok().and_then(|state| {
            state
                .entries
                .get(destination.as_str())
                .and_then(|entry| entry.presented.clone())
        })
    }
    pub(crate) fn persistence_error(&self) -> Option<SshIntentStoreError> {
        self.inner
            .state
            .lock()
            .ok()
            .and_then(|state| state.persistence_error)
    }
    pub(crate) fn subscribe(&self) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        if let Ok(mut state) = self.inner.state.lock() {
            state.subscribers.push(sender);
        }
        receiver
    }
    pub(crate) fn connect(&self, destination: SshDestination) {
        self.begin(destination, ConnectMode::Requested, None);
    }
    pub(crate) fn install(&self, destination: &SshDestination, offer_id: u64) {
        let installation = self.inner.state.lock().ok().and_then(|mut state| {
            let entry = state.entries.get_mut(destination.as_str())?;
            if entry.running || entry.offer.as_ref()?.0 != offer_id {
                return None;
            }
            entry.offer.take().map(|(_, offer)| offer)
        });
        if let Some(installation) = installation {
            self.begin(
                destination.clone(),
                ConnectMode::Requested,
                Some(installation),
            );
        }
    }
    pub(crate) fn cancel(&self, destination: &SshDestination) {
        self.end(destination, true);
    }
    pub(crate) fn disconnect(&self, destination: &SshDestination) {
        self.end(destination, false);
    }
    fn end(&self, destination: &SshDestination, cancelling: bool) {
        if let Ok(mut state) = self.inner.state.lock() {
            let entry = state
                .entries
                .entry(destination.as_str().to_owned())
                .or_default();
            entry.cancellation.cancel();
            entry.offer = None;
            entry.presented = None;
            entry.status = if cancelling && entry.running {
                RemoteConnectionStatus::Cancelling
            } else {
                RemoteConnectionStatus::Inactive
            };
            if !cancelling {
                entry.generation = entry.generation.saturating_add(1);
                entry.running = false;
            }
            if cancelling {
                self.inner.backend.disconnect(destination);
            } else {
                entry.continuity = None;
                self.inner.backend.forget(destination);
            }
            let _ = self
                .inner
                .persistence
                .try_send(PersistenceCommand::Remove(destination.clone()));
            notify(&mut state);
        }
    }
    /// Called once after a Workspace Window exists. Only retained active intent is considered.
    pub(crate) fn resume_retained(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            if state.resumed {
                return;
            }
            state.resumed = true;
        }
        let _ = self.inner.persistence.try_send(PersistenceCommand::Load);
    }
    fn begin(
        &self,
        destination: SshDestination,
        mode: ConnectMode,
        installation: Option<Arc<dyn SshInstallation>>,
    ) {
        let (generation, cancellation, expected) = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            let entry = state
                .entries
                .entry(destination.as_str().to_owned())
                .or_default();
            if entry.running {
                return;
            }
            entry.generation = entry.generation.saturating_add(1);
            entry.cancellation = SshCancellationToken::default();
            entry.running = true;
            entry.offer = None;
            entry.status = RemoteConnectionStatus::Working {
                phase: if installation.is_some() {
                    ConnectPhase::Installing
                } else {
                    ConnectPhase::Authenticating
                },
                cancellable: true,
            };
            let result = (
                entry.generation,
                entry.cancellation.clone(),
                entry.continuity.clone(),
            );
            let _ = self
                .inner
                .persistence
                .try_send(PersistenceCommand::Retain(destination.clone(), result.0));
            notify(&mut state);
            result
        };

        let weak = Arc::downgrade(&self.inner);
        let backend = self.inner.backend.clone();
        let key = destination.clone();
        if std::thread::Builder::new()
            .name("spaceterm-ssh-server".into())
            .spawn(move || {
                run_worker(
                    weak,
                    backend,
                    destination,
                    mode,
                    expected,
                    installation,
                    generation,
                    cancellation,
                )
            })
            .is_err()
        {
            update(&Arc::downgrade(&self.inner), &key, generation, |entry| {
                entry.running = false;
                entry.status = RemoteConnectionStatus::Unavailable {
                    kind: UnavailableKind::ServiceUnavailable,
                };
            });
        }
    }
}
fn notify(state: &mut State) {
    state
        .subscribers
        .retain(|sender| match sender.try_send(()) {
            Ok(()) | Err(async_channel::TrySendError::Full(_)) => true,
            Err(async_channel::TrySendError::Closed(_)) => false,
        });
}
fn update(
    inner: &Weak<Inner>,
    destination: &SshDestination,
    generation: u64,
    change: impl FnOnce(&mut Entry),
) -> bool {
    let Some(inner) = inner.upgrade() else {
        return false;
    };
    let Ok(mut state) = inner.state.lock() else {
        return false;
    };
    let Some(entry) = state
        .entries
        .get_mut(destination.as_str())
        .filter(|entry| entry.generation == generation)
    else {
        return false;
    };
    change(entry);
    notify(&mut state);
    true
}
fn wait(cancellation: &SshCancellationToken, duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while !cancellation.is_cancelled() && Instant::now() < deadline {
        std::thread::sleep(
            Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    !cancellation.is_cancelled()
}
fn retry_delay(attempt: u32) -> Duration {
    Duration::from_secs(1_u64 << attempt.saturating_sub(1).min(5))
}
#[expect(
    clippy::too_many_arguments,
    reason = "one worker owns one admitted generation and its immutable inputs"
)]
fn run_worker(
    inner: Weak<Inner>,
    backend: Arc<dyn SshConnectBackend>,
    destination: SshDestination,
    mut mode: ConnectMode,
    mut expected: Option<SshServerContinuity>,
    mut installation: Option<Arc<dyn SshInstallation>>,
    generation: u64,
    cancellation: SshCancellationToken,
) {
    let mut attempt = 0_u32;
    loop {
        if cancellation.is_cancelled() {
            break;
        }
        let progress = |phase| {
            if !cancellation.is_cancelled() {
                update(&inner, &destination, generation, |entry| {
                    entry.status = if attempt == 0 {
                        RemoteConnectionStatus::Working {
                            phase,
                            cancellable: true,
                        }
                    } else {
                        RemoteConnectionStatus::Reconnecting {
                            phase,
                            attempt,
                            next_retry: None,
                        }
                    }
                });
            }
        };
        let result = if let Some(installation) = installation.take() {
            installation.install(&cancellation, &progress)
        } else {
            backend.connect(
                &destination,
                mode,
                expected.as_ref(),
                &cancellation,
                &progress,
            )
        };
        match result {
            Ok(connected) => {
                if cancellation.is_cancelled() {
                    drop(connected);
                    break;
                }
                if let Some(previous) = &expected
                    && let Err(error) = previous.verify(&connected.continuity)
                {
                    finish(&inner, &destination, generation, error);
                    return;
                }
                expected = Some(connected.continuity.clone());
                let Some(owner) = inner.upgrade() else {
                    return;
                };
                let Ok(mut state) = owner.state.lock() else {
                    return;
                };
                let Some(entry) = state.entries.get_mut(destination.as_str()).filter(|entry| {
                    entry.generation == generation && !entry.cancellation.is_cancelled()
                }) else {
                    return;
                };
                entry.continuity = expected.clone();
                entry.presented = Some(connected.server.clone());
                entry.status = RemoteConnectionStatus::Connected {
                    protocol_major: connected.protocol_major,
                };
                let _ = owner
                    .persistence
                    .try_send(PersistenceCommand::Retain(destination.clone(), generation));
                notify(&mut state);
                drop(state);
                drop(owner);
                attempt = 0;
                while connected.lease.is_alive() {
                    if !wait(&cancellation, Duration::from_secs(1)) {
                        break;
                    }
                }
                drop(connected);
                if cancellation.is_cancelled() {
                    break;
                }
                mode = ConnectMode::QuietContinuation;
            }
            Err(SshConnectFailure::Unavailable(kind))
                if matches!(
                    kind,
                    UnavailableKind::Network
                        | UnavailableKind::HostUnreachable
                        | UnavailableKind::SshFailed
                ) && mode == ConnectMode::QuietContinuation => {}
            Err(error) => {
                finish(&inner, &destination, generation, error);
                return;
            }
        }
        attempt = attempt.saturating_add(1);
        let delay = retry_delay(attempt);
        if !update(&inner, &destination, generation, |entry| {
            entry.status = RemoteConnectionStatus::Reconnecting {
                phase: ConnectPhase::Connecting,
                attempt,
                next_retry: Some(Instant::now() + delay),
            }
        }) {
            return;
        }
        if !wait(&cancellation, delay) {
            break;
        }
    }
    update(&inner, &destination, generation, |entry| {
        entry.running = false;
        entry.status = RemoteConnectionStatus::Inactive;
    });
}
fn finish(
    inner: &Weak<Inner>,
    destination: &SshDestination,
    generation: u64,
    error: SshConnectFailure,
) {
    let Some(owner) = inner.upgrade() else {
        return;
    };
    let Ok(mut state) = owner.state.lock() else {
        return;
    };
    let offer_id = state.next_offer.saturating_add(1);
    state.next_offer = offer_id;
    let Some(entry) = state
        .entries
        .get_mut(destination.as_str())
        .filter(|entry| entry.generation == generation)
    else {
        return;
    };
    if !matches!(error, SshConnectFailure::InstallationOffered(_)) {
        owner.backend.disconnect(destination);
    }
    entry.running = false;
    let error = if entry.cancellation.is_cancelled()
        && !matches!(error, SshConnectFailure::Unconfirmed(_))
    {
        SshConnectFailure::Cancelled
    } else {
        error
    };
    entry.status = match error {
        SshConnectFailure::Cancelled => RemoteConnectionStatus::Inactive,
        SshConnectFailure::NeedsPerson(reason) => RemoteConnectionStatus::NeedsPerson { reason },
        SshConnectFailure::Changed(reason) => RemoteConnectionStatus::Paused { reason },
        SshConnectFailure::Stopped => RemoteConnectionStatus::Stopped,
        SshConnectFailure::Incompatible(kind) => RemoteConnectionStatus::Incompatible { kind },
        SshConnectFailure::Refused(kind) => RemoteConnectionStatus::Refused { kind },
        SshConnectFailure::Unavailable(kind) => RemoteConnectionStatus::Unavailable { kind },
        SshConnectFailure::Unconfirmed(operation) => {
            RemoteConnectionStatus::Unconfirmed { operation }
        }
        SshConnectFailure::InstallationOffered(offer) => {
            entry.offer = Some((offer_id, offer.installation));
            RemoteConnectionStatus::InstallationOffered {
                offer_id,
                version: offer.version,
                platform: offer.platform,
            }
        }
    };
    let stopped = matches!(entry.status, RemoteConnectionStatus::Stopped);
    notify(&mut state);
    drop(state);
    if stopped {
        let _ = owner
            .persistence
            .try_send(PersistenceCommand::Remove(destination.clone()));
    }
}

enum PersistenceCommand {
    Load,
    Retain(SshDestination, u64),
    Remove(SshDestination),
}
fn persistence_worker(inner: Weak<Inner>, receiver: async_channel::Receiver<PersistenceCommand>) {
    while let Ok(command) = receiver.recv_blocking() {
        let Some(owner) = inner.upgrade() else {
            return;
        };
        let result = match command {
            PersistenceCommand::Load => match owner.intents.load() {
                Ok(connections) => {
                    for retained in connections {
                        if let Ok(mut state) = owner.state.lock() {
                            let entry = state
                                .entries
                                .entry(retained.destination.as_str().to_owned())
                                .or_default();
                            if entry.generation != 0 {
                                continue;
                            }
                            entry.continuity = retained.continuity;
                        }
                        SshServerConnections {
                            inner: owner.clone(),
                        }
                        .begin(
                            retained.destination,
                            ConnectMode::QuietContinuation,
                            None,
                        );
                    }
                    Ok(())
                }
                Err(error) => Err(error),
            },
            PersistenceCommand::Retain(destination, generation) => {
                let record = owner.state.lock().ok().and_then(|state| {
                    state
                        .entries
                        .get(destination.as_str())
                        .filter(|entry| {
                            entry.generation == generation && !entry.cancellation.is_cancelled()
                        })
                        .map(|entry| RetainedSshConnection {
                            destination: destination.clone(),
                            continuity: entry.continuity.clone(),
                        })
                });
                match record {
                    Some(record) => owner.intents.retain(record),
                    None => Ok(()),
                }
            }
            PersistenceCommand::Remove(destination) => {
                let result = owner.intents.remove(&destination);
                if result.is_err()
                    && let Ok(mut state) = owner.state.lock()
                    && let Some(entry) = state.entries.get_mut(destination.as_str())
                    && matches!(entry.status, RemoteConnectionStatus::Inactive)
                {
                    entry.status = RemoteConnectionStatus::Refused {
                        kind: RefusalKind::IntentPersistence,
                    };
                }
                result
            }
        };
        if let Err(error) = result
            && let Ok(mut state) = owner.state.lock()
        {
            state.persistence_error = Some(error);
            notify(&mut state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Intents {
        records: Mutex<Vec<RetainedSshConnection>>,
        loads: AtomicUsize,
        writes: AtomicUsize,
    }
    impl SshConnectionIntentStore for Intents {
        fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            Ok(self.records.lock().unwrap().clone())
        }
        fn retain(&self, record: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
            let mut records = self.records.lock().unwrap();
            records.retain(|other| other.destination != record.destination);
            records.push(record);
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn remove(&self, destination: &SshDestination) -> Result<(), SshIntentStoreError> {
            self.records
                .lock()
                .unwrap()
                .retain(|record| record.destination != *destination);
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    struct Lease;
    impl SshServerLease for Lease {
        fn is_alive(&self) -> bool {
            true
        }
    }
    fn connected() -> ConnectedSshServer {
        ConnectedSshServer {
            server: LocalServer::for_identity("spaceterm-development").unwrap(),
            continuity: pin(),
            lease: Arc::new(Lease),
            protocol_major: 1,
        }
    }
    fn pin() -> SshServerContinuity {
        SshServerContinuity {
            route: [1; 32],
            account: 501,
            build: "spaceterm-development".into(),
            server: [2; 32],
        }
    }
    struct Backend {
        calls: AtomicUsize,
        disconnects: AtomicUsize,
        entered: async_channel::Sender<ConnectMode>,
        release: async_channel::Receiver<()>,
        stopped: bool,
    }
    impl SshConnectBackend for Backend {
        fn disconnect(&self, _: &SshDestination) {
            self.disconnects.fetch_add(1, Ordering::SeqCst);
        }
        fn connect(
            &self,
            _: &SshDestination,
            mode: ConnectMode,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<ConnectedSshServer, SshConnectFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.send_blocking(mode).unwrap();
            self.release.recv_blocking().unwrap();
            if self.stopped {
                Err(SshConnectFailure::Stopped)
            } else {
                Ok(connected())
            }
        }
    }
    fn setup(
        stopped: bool,
    ) -> (
        SshServerConnections,
        Arc<Backend>,
        Arc<Intents>,
        async_channel::Receiver<ConnectMode>,
        async_channel::Sender<()>,
    ) {
        let (entered, seen) = async_channel::unbounded();
        let (release, released) = async_channel::unbounded();
        let backend = Arc::new(Backend {
            calls: AtomicUsize::new(0),
            disconnects: AtomicUsize::new(0),
            entered,
            release: released,
            stopped,
        });
        let intents = Arc::new(Intents::default());
        (
            SshServerConnections::new(backend.clone(), intents.clone()),
            backend,
            intents,
            seen,
            release,
        )
    }
    fn destination() -> SshDestination {
        SshDestination::new("test-host".into()).unwrap()
    }
    fn eventually(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "worker did not reach expected state"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn constructing_and_rendering_inactive_hosts_performs_no_discovery_or_storage() {
        let (registry, backend, intents, _, _) = setup(false);
        assert!(matches!(
            registry.status(&destination()),
            RemoteConnectionStatus::Inactive
        ));
        assert!(registry.connections().is_empty());
        assert_eq!(
            (
                backend.calls.load(Ordering::SeqCst),
                intents.loads.load(Ordering::SeqCst),
                intents.writes.load(Ordering::SeqCst)
            ),
            (0, 0, 0)
        );
    }
    #[test]
    fn disconnect_fences_late_success_and_does_not_recreate_retained_intent() {
        let (registry, backend, intents, entered, release) = setup(false);
        registry.connect(destination());
        entered.recv_blocking().unwrap();
        registry.connect(destination());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        registry.disconnect(&destination());
        release.send_blocking(()).unwrap();
        eventually(|| {
            intents.writes.load(Ordering::SeqCst) > 0 && intents.records.lock().unwrap().is_empty()
        });
        assert!(matches!(
            registry.status(&destination()),
            RemoteConnectionStatus::Inactive
        ));
        assert!(registry.server(&destination()).is_none());
        assert_eq!(backend.disconnects.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn restored_active_intent_is_quiet_and_stop_ends_retained_retry_intent() {
        let (registry, backend, intents, entered, release) = setup(true);
        intents.records.lock().unwrap().push(RetainedSshConnection {
            destination: destination(),
            continuity: Some(pin()),
        });
        registry.resume_retained();
        registry.resume_retained();
        assert_eq!(
            entered.recv_blocking().unwrap(),
            ConnectMode::QuietContinuation
        );
        release.send_blocking(()).unwrap();
        eventually(|| {
            matches!(
                registry.status(&destination()),
                RemoteConnectionStatus::Stopped
            ) && intents.records.lock().unwrap().is_empty()
        });
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn continuity_refuses_each_changed_boundary() {
        let original = pin();
        for (changed, expected) in [
            (
                SshServerContinuity {
                    route: [3; 32],
                    ..pin()
                },
                ContinuityChange::HostChanged,
            ),
            (
                SshServerContinuity {
                    account: 502,
                    ..pin()
                },
                ContinuityChange::AccountChanged,
            ),
            (
                SshServerContinuity {
                    build: "spaceterm".into(),
                    ..pin()
                },
                ContinuityChange::BuildChanged,
            ),
            (
                SshServerContinuity {
                    server: [3; 32],
                    ..pin()
                },
                ContinuityChange::ServerChanged,
            ),
        ] {
            assert!(
                matches!(original.verify(&changed), Err(SshConnectFailure::Changed(reason)) if reason == expected)
            );
        }
    }
}
