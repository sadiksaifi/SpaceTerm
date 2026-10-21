//! Saved SSH server connections, their retained intent and bounded continuation.
//!
//! Every connection the person connects to stays saved until they remove it. A saved connection is
//! either active, so the registry keeps it connected and continues it after relaunch, or inactive,
//! so nothing reaches its host until the person asks. Check Status observes an inactive host once
//! without changing either.
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

/// What one observation found about the account's server, short of connecting to it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServerAvailability {
    /// The server runs and accepts this SpaceTerm.
    Running,
    /// The server is installed but not running. Connect starts it unless it was stopped.
    NotRunning,
    /// The account has no server. Connect offers a verified installation when one is available.
    NotInstalled,
}

/// The facts the account's discovery reported, which Settings presents without asking again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SshServerFacts {
    /// The other computer's operating system and processor, as the backend names them.
    pub platform: (String, String),
    /// The installed server's version, or `None` when the account has no server.
    pub version: Option<String>,
    /// Whether the account's service manager starts the server on its own.
    pub starts_automatically: bool,
}

/// One Check Status result.
pub(crate) struct SshServerObservation {
    pub availability: ServerAvailability,
    pub facts: SshServerFacts,
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
        facts: SshServerFacts,
    },
    /// An inactive connection's latest Check Status found the server reachable. Nothing
    /// connected, started, or installed.
    Checked {
        availability: ServerAvailability,
        facts: SshServerFacts,
        /// When the observation finished, which the result describes rather than the present.
        at: std::time::SystemTime,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
/// One saved connection as it survives relaunch.
#[derive(Clone)]
pub(crate) struct RetainedSshConnection {
    pub destination: SshDestination,
    pub continuity: Option<SshServerContinuity>,
    /// Whether the person last wanted it connected, so relaunch continues it quietly. An inactive
    /// saved connection is listed without reaching its host.
    pub active: bool,
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
    pub facts: SshServerFacts,
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
    /// Signs in as Connect does and reads the account's server state, without starting,
    /// installing, or connecting to a server. The caller ends the route afterwards.
    fn observe(
        &self,
        destination: &SshDestination,
        expected: Option<&SshServerContinuity>,
        cancellation: &SshCancellationToken,
        progress: &ConnectProgress<'_>,
    ) -> Result<SshServerObservation, SshConnectFailure>;
}

struct Entry {
    status: RemoteConnectionStatus,
    continuity: Option<SshServerContinuity>,
    presented: Option<LocalServer>,
    cancellation: SshCancellationToken,
    generation: u64,
    running: bool,
    /// Whether the running worker only observes. Its end leaves the saved intent unchanged.
    checking: bool,
    /// Whether the person wants this connection kept up, which relaunch continues.
    active: bool,
    offer: Option<(u64, Arc<dyn SshInstallation>)>,
    /// What the latest connection or Check Status reported, kept for this process only.
    last_facts: Option<SshServerFacts>,
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
            checking: false,
            active: false,
            offer: None,
            last_facts: None,
        }
    }
}
struct State {
    entries: HashMap<String, Entry>,
    subscribers: Vec<async_channel::Sender<()>>,
    next_offer: u64,
    /// Generations come from one counter, so a removed connection's late work never matches a
    /// connection saved again under the same destination.
    next_generation: u64,
    resumed: bool,
    /// Whether the saved connections have been read. A read that fails at launch is tried again
    /// once storage works.
    loaded: bool,
    persistence_error: Option<SshIntentStoreError>,
    /// Removed connections whose removal SpaceTerm couldn't save, so they come back when it
    /// reopens until a later removal saves.
    unsaved_removals: Vec<SshDestination>,
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
                next_generation: 0,
                resumed: false,
                loaded: false,
                persistence_error: None,
                unsaved_removals: Vec::new(),
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
    /// Every saved connection with its status, by destination.
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
    /// Whether the person wants a saved connection kept up, whether or not work for it runs now.
    pub(crate) fn intends(&self, destination: &SshDestination) -> bool {
        self.inner.state.lock().ok().is_some_and(|state| {
            state
                .entries
                .get(destination.as_str())
                .is_some_and(|entry| entry.active)
        })
    }
    /// Whether SpaceTerm tries a saved connection again on its own, as it does after relaunch,
    /// although no work for it runs now. A requested Connect that failed leaves this intent.
    pub(crate) fn reconnects(&self, destination: &SshDestination) -> bool {
        self.inner.state.lock().ok().is_some_and(|state| {
            state
                .entries
                .get(destination.as_str())
                .is_some_and(|entry| entry.active && !entry.running)
        })
    }
    /// The facts the latest connection or Check Status reported for a saved connection. They
    /// outlive Disconnect and later failures until the connection is removed or SpaceTerm quits.
    pub(crate) fn last_facts(&self, destination: &SshDestination) -> Option<SshServerFacts> {
        self.inner.state.lock().ok().and_then(|state| {
            state
                .entries
                .get(destination.as_str())
                .and_then(|entry| entry.last_facts.clone())
        })
    }
    /// The identity the latest admitted connection validated, which one maintenance operation
    /// binds before dispatch. `None` until a connection has been admitted for this destination.
    pub(crate) fn continuity(&self, destination: &SshDestination) -> Option<SshServerContinuity> {
        self.inner.state.lock().ok().and_then(|state| {
            state
                .entries
                .get(destination.as_str())
                .and_then(|entry| entry.continuity.clone())
        })
    }
    pub(crate) fn server(&self, destination: &SshDestination) -> Option<LocalServer> {
        self.inner.state.lock().ok().and_then(|state| {
            state
                .entries
                .get(destination.as_str())
                .and_then(|entry| entry.presented.clone())
        })
    }
    /// Connections the person removed whose removal couldn't be saved. Each one reappears and may
    /// reconnect when SpaceTerm reopens unless a later removal saves.
    pub(crate) fn unsaved_removals(&self) -> Vec<SshDestination> {
        self.inner
            .state
            .lock()
            .ok()
            .map(|state| state.unsaved_removals.clone())
            .unwrap_or_default()
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
    /// Ends the connection and its reconnect intent and keeps it saved. Its server and Terminal
    /// Sessions keep running.
    pub(crate) fn disconnect(&self, destination: &SshDestination) {
        self.end(destination, false);
    }
    /// Forgets a saved connection at once: its presentation, its reconnect intent and anything
    /// it observed. Late results from its work find nothing to update, so they never recreate it.
    /// SSH account access, the server and its Terminal Sessions are untouched.
    pub(crate) fn remove(&self, destination: &SshDestination) {
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        match state.entries.remove(destination.as_str()) {
            Some(entry) => {
                entry.cancellation.cancel();
                self.inner.backend.forget(destination);
            }
            // Removing again retries a removal whose saving failed.
            None if state.unsaved_removals.contains(destination) => {}
            None => return,
        }
        let _ = self
            .inner
            .persistence
            .try_send(PersistenceCommand::Remove(destination.clone()));
        notify(&mut state);
    }
    /// Observes a saved connection that has no work running, once, and reports what it found.
    /// It may ask the person to sign in, because they asked for it. It connects, starts and
    /// installs nothing, and the connection's intent stays as it was.
    pub(crate) fn check(&self, destination: &SshDestination) {
        let (generation, cancellation, expected) = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            if state
                .entries
                .get(destination.as_str())
                .is_none_or(|entry| entry.running || entry.presented.is_some())
            {
                return;
            }
            let generation = next_generation(&mut state);
            let Some(entry) = state.entries.get_mut(destination.as_str()) else {
                return;
            };
            entry.generation = generation;
            entry.cancellation = SshCancellationToken::default();
            entry.running = true;
            entry.checking = true;
            entry.offer = None;
            entry.status = RemoteConnectionStatus::Working {
                phase: ConnectPhase::Authenticating,
                cancellable: true,
            };
            let admitted = (
                entry.generation,
                entry.cancellation.clone(),
                entry.continuity.clone(),
            );
            notify(&mut state);
            admitted
        };
        let weak = Arc::downgrade(&self.inner);
        let backend = self.inner.backend.clone();
        let key = destination.clone();
        let destination = destination.clone();
        if std::thread::Builder::new()
            .name("spaceterm-ssh-check".into())
            .spawn(move || {
                let progress = |phase| {
                    if !cancellation.is_cancelled() {
                        update(&weak, &destination, generation, |entry| {
                            entry.status = RemoteConnectionStatus::Working {
                                phase,
                                cancellable: true,
                            };
                        });
                    }
                };
                match backend.observe(&destination, expected.as_ref(), &cancellation, &progress) {
                    Ok(observation) => {
                        // Only the current observation ends the route, never a newer one's.
                        update(&weak, &destination, generation, |entry| {
                            backend.disconnect(&destination);
                            entry.running = false;
                            entry.checking = false;
                            entry.status = if entry.cancellation.is_cancelled() {
                                RemoteConnectionStatus::Inactive
                            } else {
                                entry.last_facts = Some(observation.facts.clone());
                                RemoteConnectionStatus::Checked {
                                    availability: observation.availability,
                                    facts: observation.facts,
                                    at: std::time::SystemTime::now(),
                                }
                            };
                        });
                    }
                    Err(error) => finish(&weak, &destination, generation, error),
                }
            })
            .is_err()
        {
            update(&Arc::downgrade(&self.inner), &key, generation, |entry| {
                entry.running = false;
                entry.checking = false;
                entry.status = RemoteConnectionStatus::Unavailable {
                    kind: UnavailableKind::ServiceUnavailable,
                };
            });
        }
    }
    fn end(&self, destination: &SshDestination, cancelling: bool) {
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        let generation = next_generation(&mut state);
        let Some(entry) = state.entries.get_mut(destination.as_str()) else {
            return;
        };
        entry.cancellation.cancel();
        entry.offer = None;
        entry.presented = None;
        entry.status = if cancelling && entry.running {
            RemoteConnectionStatus::Cancelling
        } else {
            RemoteConnectionStatus::Inactive
        };
        // Cancelling an observation leaves the intent the person had before it.
        let saves = !(cancelling && entry.checking);
        if saves {
            entry.active = false;
        }
        if !cancelling {
            entry.generation = generation;
            entry.running = false;
            entry.checking = false;
        }
        if cancelling {
            self.inner.backend.disconnect(destination);
        } else {
            entry.continuity = None;
            self.inner.backend.forget(destination);
        }
        if saves {
            let _ = self
                .inner
                .persistence
                .try_send(PersistenceCommand::Save(destination.clone()));
        }
        notify(&mut state);
    }
    /// Called once after a Workspace Window exists. Only retained active intent reaches a host;
    /// an inactive saved connection is listed and left alone.
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
            if state
                .entries
                .get(destination.as_str())
                .is_some_and(|entry| entry.running)
            {
                return;
            }
            let generation = next_generation(&mut state);
            let entry = state
                .entries
                .entry(destination.as_str().to_owned())
                .or_default();
            entry.generation = generation;
            entry.cancellation = SshCancellationToken::default();
            entry.running = true;
            entry.checking = false;
            entry.active = true;
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
                .try_send(PersistenceCommand::Save(destination.clone()));
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
fn next_generation(state: &mut State) -> u64 {
    state.next_generation = state.next_generation.saturating_add(1);
    state.next_generation
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
/// Waits for `duration` unless cancellation ends it first, and reports whether it ran out.
pub(super) fn wait(cancellation: &SshCancellationToken, duration: Duration) -> bool {
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
                entry.last_facts = Some(connected.facts.clone());
                entry.status = RemoteConnectionStatus::Connected {
                    protocol_major: connected.protocol_major,
                    facts: connected.facts.clone(),
                };
                let _ = owner
                    .persistence
                    .try_send(PersistenceCommand::Save(destination.clone()));
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
    entry.checking = false;
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
    // A deliberately stopped server stays stopped, so nothing reconnects to it on its own.
    let stopped = matches!(entry.status, RemoteConnectionStatus::Stopped) && entry.active;
    if stopped {
        entry.active = false;
    }
    notify(&mut state);
    drop(state);
    if stopped {
        let _ = owner
            .persistence
            .try_send(PersistenceCommand::Save(destination.clone()));
    }
}

enum PersistenceCommand {
    Load,
    /// Saves the connection as it stands when the command runs; a removed one is skipped.
    Save(SshDestination),
    Remove(SshDestination),
}
fn persistence_worker(inner: Weak<Inner>, receiver: async_channel::Receiver<PersistenceCommand>) {
    while let Ok(command) = receiver.recv_blocking() {
        let Some(owner) = inner.upgrade() else {
            return;
        };
        let result = match command {
            PersistenceCommand::Load => load(&owner),
            PersistenceCommand::Save(destination) => {
                let record = owner.state.lock().ok().and_then(|state| {
                    state
                        .entries
                        .get(destination.as_str())
                        .map(|entry| RetainedSshConnection {
                            destination: destination.clone(),
                            continuity: entry.continuity.clone(),
                            active: entry.active,
                        })
                });
                match record {
                    Some(record) => {
                        let result = owner.intents.retain(record);
                        // Saving the connection again supersedes a removal that wasn't saved.
                        if result.is_ok()
                            && let Ok(mut state) = owner.state.lock()
                            && state.unsaved_removals.contains(&destination)
                        {
                            state
                                .unsaved_removals
                                .retain(|unsaved| *unsaved != destination);
                            notify(&mut state);
                        }
                        result
                    }
                    None => Ok(()),
                }
            }
            PersistenceCommand::Remove(destination) => {
                let result = owner.intents.remove(&destination);
                if let Ok(mut state) = owner.state.lock() {
                    state
                        .unsaved_removals
                        .retain(|unsaved| *unsaved != destination);
                    // Only unavailable storage keeps the record that brings it back. A damaged
                    // record loads nothing, so its damage is the notice that matters.
                    if result == Err(SshIntentStoreError::Unavailable) {
                        state.unsaved_removals.push(destination);
                    }
                    notify(&mut state);
                }
                result
            }
        };
        let result = match result {
            // Storage works again, so what failed before is read or saved now.
            Ok(())
                if owner
                    .state
                    .lock()
                    .is_ok_and(|state| state.persistence_error.is_some()) =>
            {
                load(&owner).and_then(|()| resync(&owner))
            }
            result => result,
        };
        if let Ok(mut state) = owner.state.lock() {
            let error = result.err();
            if state.persistence_error != error {
                state.persistence_error = error;
                notify(&mut state);
            }
        }
    }
}

/// Reads the saved connections once a Workspace Window exists, then lists each one and continues
/// the active ones quietly. A connection the person already changed keeps their change.
fn load(owner: &Arc<Inner>) -> Result<(), SshIntentStoreError> {
    if owner
        .state
        .lock()
        .map_or(true, |state| !state.resumed || state.loaded)
    {
        return Ok(());
    }
    let connections = owner.intents.load()?;
    if let Ok(mut state) = owner.state.lock() {
        state.loaded = true;
    }
    for retained in connections {
        let Ok(mut state) = owner.state.lock() else {
            continue;
        };
        let entry = state
            .entries
            .entry(retained.destination.as_str().to_owned())
            .or_default();
        if entry.generation != 0 {
            continue;
        }
        entry.continuity = retained.continuity;
        entry.active = retained.active;
        notify(&mut state);
        drop(state);
        if retained.active {
            SshServerConnections {
                inner: owner.clone(),
            }
            .begin(retained.destination, ConnectMode::QuietContinuation, None);
        }
    }
    Ok(())
}

/// Saves every saved connection as it stands and every removal that wasn't saved, after an
/// earlier write failed.
fn resync(owner: &Inner) -> Result<(), SshIntentStoreError> {
    let (records, removals) = {
        let Ok(state) = owner.state.lock() else {
            return Err(SshIntentStoreError::Unavailable);
        };
        let records: Vec<_> = state
            .entries
            .iter()
            .filter_map(|(destination, entry)| {
                SshDestination::new(destination.clone())
                    .ok()
                    .map(|destination| RetainedSshConnection {
                        destination,
                        continuity: entry.continuity.clone(),
                        active: entry.active,
                    })
            })
            .collect();
        (records, state.unsaved_removals.clone())
    };
    for record in records {
        owner.intents.retain(record)?;
    }
    for destination in removals {
        owner.intents.remove(&destination)?;
        if let Ok(mut state) = owner.state.lock() {
            state
                .unsaved_removals
                .retain(|unsaved| *unsaved != destination);
            notify(&mut state);
        }
    }
    Ok(())
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
        /// The failure every write returns while set, as unavailable or damaged storage does.
        fails: Mutex<Option<SshIntentStoreError>>,
    }
    impl SshConnectionIntentStore for Intents {
        fn load(&self) -> Result<Vec<RetainedSshConnection>, SshIntentStoreError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = *self.fails.lock().unwrap() {
                return Err(error);
            }
            Ok(self.records.lock().unwrap().clone())
        }
        fn retain(&self, record: RetainedSshConnection) -> Result<(), SshIntentStoreError> {
            if let Some(error) = *self.fails.lock().unwrap() {
                return Err(error);
            }
            let mut records = self.records.lock().unwrap();
            records.retain(|other| other.destination != record.destination);
            records.push(record);
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn remove(&self, destination: &SshDestination) -> Result<(), SshIntentStoreError> {
            if let Some(error) = *self.fails.lock().unwrap() {
                return Err(error);
            }
            self.records
                .lock()
                .unwrap()
                .retain(|record| record.destination != *destination);
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    impl Intents {
        /// The saved record's activity, or `None` when nothing is saved for the destination.
        fn active(&self, destination: &SshDestination) -> Option<bool> {
            self.records
                .lock()
                .unwrap()
                .iter()
                .find(|record| record.destination == *destination)
                .map(|record| record.active)
        }
    }
    struct Lease;
    impl SshServerLease for Lease {
        fn is_alive(&self) -> bool {
            true
        }
    }
    fn facts() -> SshServerFacts {
        SshServerFacts {
            platform: ("linux".into(), "x86_64".into()),
            version: Some("1.2.3".into()),
            starts_automatically: true,
        }
    }
    fn connected() -> ConnectedSshServer {
        ConnectedSshServer {
            server: LocalServer::for_identity("spaceterm-development").unwrap(),
            continuity: pin(),
            lease: Arc::new(Lease),
            protocol_major: 1,
            facts: facts(),
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
        observations: AtomicUsize,
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
        fn observe(
            &self,
            _: &SshDestination,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<SshServerObservation, SshConnectFailure> {
            self.observations.fetch_add(1, Ordering::SeqCst);
            if self.stopped {
                Err(SshConnectFailure::Stopped)
            } else {
                Ok(SshServerObservation {
                    availability: ServerAvailability::NotRunning,
                    facts: facts(),
                })
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
            observations: AtomicUsize::new(0),
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
    fn saved(active: bool) -> RetainedSshConnection {
        RetainedSshConnection {
            destination: destination(),
            continuity: Some(pin()),
            active,
        }
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
    fn disconnect_fences_late_success_and_keeps_the_connection_saved_without_intent() {
        let (registry, backend, intents, entered, release) = setup(false);
        registry.connect(destination());
        entered.recv_blocking().unwrap();
        registry.connect(destination());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        registry.disconnect(&destination());
        release.send_blocking(()).unwrap();
        eventually(|| intents.active(&destination()) == Some(false));
        assert!(matches!(
            registry.status(&destination()),
            RemoteConnectionStatus::Inactive
        ));
        assert!(registry.server(&destination()).is_none());
        assert_eq!(registry.connections().len(), 1, "it stays saved");
        assert_eq!(backend.disconnects.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn the_latest_observed_facts_outlive_disconnect_until_removal() {
        let (registry, _, intents, entered, release) = setup(false);
        assert_eq!(registry.last_facts(&destination()), None);
        registry.connect(destination());
        entered.recv_blocking().unwrap();
        release.send_blocking(()).unwrap();
        eventually(|| registry.server(&destination()).is_some());

        registry.disconnect(&destination());
        eventually(|| intents.active(&destination()) == Some(false));
        assert!(matches!(
            registry.status(&destination()),
            RemoteConnectionStatus::Inactive
        ));
        assert_eq!(registry.last_facts(&destination()), Some(facts()));

        registry.remove(&destination());
        assert_eq!(registry.last_facts(&destination()), None);
    }
    #[test]
    fn restored_active_intent_is_quiet_and_stop_ends_retained_retry_intent() {
        let (registry, backend, intents, entered, release) = setup(true);
        intents.records.lock().unwrap().push(saved(true));
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
            ) && intents.active(&destination()) == Some(false)
        });
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn an_inactive_saved_connection_is_listed_after_relaunch_without_reaching_its_host() {
        let (registry, backend, intents, _, _) = setup(false);
        intents.records.lock().unwrap().push(saved(false));
        registry.resume_retained();
        eventually(|| registry.connections().len() == 1);
        std::thread::sleep(Duration::from_millis(50));
        assert!(matches!(
            registry.status(&destination()),
            RemoteConnectionStatus::Inactive
        ));
        assert_eq!(
            (
                backend.calls.load(Ordering::SeqCst),
                backend.observations.load(Ordering::SeqCst)
            ),
            (0, 0),
            "listing a saved host never reaches it"
        );
    }
    #[test]
    fn removal_ends_presentation_and_intent_and_late_success_never_recreates_it() {
        let (registry, backend, intents, entered, release) = setup(false);
        registry.connect(destination());
        entered.recv_blocking().unwrap();
        eventually(|| intents.active(&destination()) == Some(true));
        registry.remove(&destination());
        assert!(registry.connections().is_empty(), "removal is immediate");
        release.send_blocking(()).unwrap();
        eventually(|| intents.active(&destination()).is_none());
        std::thread::sleep(Duration::from_millis(50));
        assert!(registry.connections().is_empty());
        assert!(registry.server(&destination()).is_none());
        assert!(intents.active(&destination()).is_none());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }
    /// Holds each connect attempt until the test releases that attempt by its order.
    struct Gated {
        attempts: AtomicUsize,
        released: [std::sync::atomic::AtomicBool; 2],
    }
    impl SshConnectBackend for Gated {
        fn connect(
            &self,
            _: &SshDestination,
            _: ConnectMode,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<ConnectedSshServer, SshConnectFailure> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            while !self.released[attempt].load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(connected())
        }
        fn observe(
            &self,
            _: &SshDestination,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<SshServerObservation, SshConnectFailure> {
            Err(SshConnectFailure::Stopped)
        }
    }
    #[test]
    fn a_removed_connections_late_result_never_reaches_its_re_added_successor() {
        let backend = Arc::new(Gated {
            attempts: AtomicUsize::new(0),
            released: Default::default(),
        });
        let registry = SshServerConnections::new(backend.clone(), Arc::new(Intents::default()));
        registry.connect(destination());
        eventually(|| backend.attempts.load(Ordering::SeqCst) == 1);
        registry.remove(&destination());
        registry.connect(destination());
        eventually(|| backend.attempts.load(Ordering::SeqCst) == 2);
        // Only the removed attempt succeeds now.
        backend.released[0].store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            matches!(
                registry.status(&destination()),
                RemoteConnectionStatus::Working { .. }
            ),
            "the removed attempt's success must not present the re-added connection"
        );
        assert!(registry.server(&destination()).is_none());
        backend.released[1].store(true, Ordering::SeqCst);
        eventually(|| {
            matches!(
                registry.status(&destination()),
                RemoteConnectionStatus::Connected { .. }
            )
        });
    }
    #[test]
    fn a_removal_that_could_not_be_saved_is_reported_until_a_later_removal_saves() {
        let (registry, _, intents, _, _) = setup(false);
        intents.records.lock().unwrap().push(saved(false));
        registry.resume_retained();
        eventually(|| registry.connections().len() == 1);
        *intents.fails.lock().unwrap() = Some(SshIntentStoreError::Unavailable);
        registry.remove(&destination());
        eventually(|| registry.unsaved_removals() == vec![destination()]);
        assert!(
            registry.connections().is_empty(),
            "the removal still applies now"
        );

        *intents.fails.lock().unwrap() = None;
        registry.remove(&destination());
        eventually(|| registry.unsaved_removals().is_empty());
        assert!(intents.active(&destination()).is_none());
    }
    #[test]
    fn a_later_successful_write_saves_what_failed_and_clears_the_warning() {
        let (registry, _, intents, _, _) = setup(false);
        let other = SshDestination::new("other-host".into()).unwrap();
        intents.records.lock().unwrap().extend([
            saved(false),
            RetainedSshConnection {
                destination: other.clone(),
                continuity: None,
                active: false,
            },
        ]);
        registry.resume_retained();
        eventually(|| registry.connections().len() == 2);
        intents.records.lock().unwrap()[0].active = true;
        *intents.fails.lock().unwrap() = Some(SshIntentStoreError::Unavailable);
        registry.disconnect(&destination());
        eventually(|| registry.persistence_error().is_some());

        *intents.fails.lock().unwrap() = None;
        registry.disconnect(&other);
        eventually(|| registry.persistence_error().is_none());
        assert_eq!(
            intents.active(&destination()),
            Some(false),
            "the write that failed is saved with the next one"
        );
    }
    #[test]
    fn saved_connections_that_failed_to_load_return_once_storage_works() {
        let (registry, _, intents, entered, release) = setup(false);
        intents.records.lock().unwrap().push(saved(true));
        *intents.fails.lock().unwrap() = Some(SshIntentStoreError::Unavailable);
        registry.resume_retained();
        eventually(|| registry.persistence_error() == Some(SshIntentStoreError::Unavailable));
        assert!(registry.connections().is_empty());

        *intents.fails.lock().unwrap() = None;
        let other = SshDestination::new("other-host".into()).unwrap();
        registry.connect(other.clone());
        eventually(|| entered.len() == 2);
        let mut modes = [entered.try_recv().unwrap(), entered.try_recv().unwrap()];
        modes.sort_by_key(|mode| *mode == ConnectMode::QuietContinuation);
        assert_eq!(
            modes,
            [ConnectMode::Requested, ConnectMode::QuietContinuation],
            "the saved active connection resumes quietly once it loads"
        );
        eventually(|| registry.persistence_error().is_none());
        assert_eq!(
            registry
                .connections()
                .into_iter()
                .map(|(destination, _)| destination)
                .collect::<Vec<_>>(),
            vec![other, destination()],
            "the warning clears only once the saved connections are listed"
        );
        release.send_blocking(()).unwrap();
        release.send_blocking(()).unwrap();
    }
    /// A route that stays up until the test ends it.
    struct Route(Arc<std::sync::atomic::AtomicBool>);
    impl SshServerLease for Route {
        fn is_alive(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
    /// Connects once over a route the test can end, then answers every later attempt with
    /// `later`.
    struct ConnectsThen {
        route: Arc<std::sync::atomic::AtomicBool>,
        attempts: AtomicUsize,
        later: fn() -> SshConnectFailure,
    }
    impl SshConnectBackend for ConnectsThen {
        fn connect(
            &self,
            _: &SshDestination,
            _: ConnectMode,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<ConnectedSshServer, SshConnectFailure> {
            if self.attempts.fetch_add(1, Ordering::SeqCst) > 0 {
                return Err((self.later)());
            }
            Ok(ConnectedSshServer {
                lease: Arc::new(Route(self.route.clone())),
                ..connected()
            })
        }
        fn observe(
            &self,
            _: &SshDestination,
            _: Option<&SshServerContinuity>,
            _: &SshCancellationToken,
            _: &ConnectProgress<'_>,
        ) -> Result<SshServerObservation, SshConnectFailure> {
            Err((self.later)())
        }
    }
    #[test]
    fn a_server_stopped_while_presented_stays_presented_until_disconnect() {
        let route = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let intents = Arc::new(Intents::default());
        let registry = SshServerConnections::new(
            Arc::new(ConnectsThen {
                route: route.clone(),
                attempts: AtomicUsize::new(0),
                later: || SshConnectFailure::Stopped,
            }),
            intents.clone(),
        );
        registry.connect(destination());
        eventually(|| registry.server(&destination()).is_some());
        route.store(false, Ordering::SeqCst);
        eventually(|| {
            matches!(
                registry.status(&destination()),
                RemoteConnectionStatus::Stopped
            )
        });
        assert!(
            registry.server(&destination()).is_some(),
            "its frozen Workspaces stay in place to explain the stop"
        );
        eventually(|| intents.active(&destination()) == Some(false));
        assert!(!registry.reconnects(&destination()));

        registry.disconnect(&destination());
        assert!(registry.server(&destination()).is_none());
        assert_eq!(registry.connections().len(), 1, "it stays saved");
    }
    #[test]
    fn a_damaged_record_reports_damage_rather_than_an_unsaved_removal() {
        let (registry, _, intents, _, _) = setup(false);
        intents.records.lock().unwrap().push(saved(false));
        registry.resume_retained();
        eventually(|| registry.connections().len() == 1);
        *intents.fails.lock().unwrap() = Some(SshIntentStoreError::Malformed);
        registry.remove(&destination());
        eventually(|| registry.persistence_error() == Some(SshIntentStoreError::Malformed));
        assert!(
            registry.unsaved_removals().is_empty(),
            "a damaged record loads nothing, so the removed connection can't return"
        );
    }
    #[test]
    fn saving_a_connection_again_supersedes_its_unsaved_removal() {
        let (registry, _, intents, entered, release) = setup(false);
        intents.records.lock().unwrap().push(saved(false));
        registry.resume_retained();
        eventually(|| registry.connections().len() == 1);
        *intents.fails.lock().unwrap() = Some(SshIntentStoreError::Unavailable);
        registry.remove(&destination());
        eventually(|| !registry.unsaved_removals().is_empty());

        *intents.fails.lock().unwrap() = None;
        registry.connect(destination());
        entered.recv_blocking().unwrap();
        eventually(|| registry.unsaved_removals().is_empty());
        release.send_blocking(()).unwrap();
    }
    #[test]
    fn check_status_observes_once_and_leaves_intent_and_presentation_unchanged() {
        let (registry, backend, intents, _, _) = setup(false);
        intents.records.lock().unwrap().push(saved(false));
        registry.resume_retained();
        eventually(|| registry.connections().len() == 1);
        let writes = intents.writes.load(Ordering::SeqCst);
        registry.check(&destination());
        eventually(|| {
            matches!(
                registry.status(&destination()),
                RemoteConnectionStatus::Checked {
                    availability: ServerAvailability::NotRunning,
                    ..
                }
            )
        });
        let RemoteConnectionStatus::Checked {
            facts: observed, ..
        } = registry.status(&destination())
        else {
            unreachable!()
        };
        assert_eq!(observed, facts());
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0, "nothing connects");
        assert_eq!(backend.observations.load(Ordering::SeqCst), 1);
        assert_eq!(
            backend.disconnects.load(Ordering::SeqCst),
            1,
            "the route ends with the check"
        );
        assert!(registry.server(&destination()).is_none());
        assert_eq!(intents.writes.load(Ordering::SeqCst), writes);
        assert_eq!(intents.active(&destination()), Some(false));
    }
    #[test]
    fn check_status_is_refused_while_work_runs_and_for_unsaved_hosts() {
        let (registry, backend, _, entered, release) = setup(false);
        registry.check(&destination());
        assert!(registry.connections().is_empty(), "checking saves nothing");
        registry.connect(destination());
        entered.recv_blocking().unwrap();
        registry.check(&destination());
        release.send_blocking(()).unwrap();
        eventually(|| {
            matches!(
                registry.status(&destination()),
                RemoteConnectionStatus::Connected { .. }
            )
        });
        registry.check(&destination());
        assert_eq!(backend.observations.load(Ordering::SeqCst), 0);
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
