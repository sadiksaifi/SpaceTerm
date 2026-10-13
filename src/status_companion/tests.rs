//! Companion behavior through its public ports, with in-crate fakes of the frozen
//! status-only facade, native observation, desktop activation and status area.

use std::collections::BTreeSet;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_client::management_cli::status::{StatusResync, StatusUpdate};
use spaceterm_client::management_cli::{
    AuthenticatedManagementContext, ManagementClientKind, ManagementFailure, ManagementFuture,
    ServerStatus, StatusBackend, StatusManagement, StatusSubscription,
};
use spaceterm_client_native::cli::bindings::{
    NativeObservation, ProcessObservation, RegistrationAvailability,
};
use spaceterm_core::authority::{
    ConnectionBinding, ConnectionGeneration, FieldVersion, ServerEpoch,
};
use spaceterm_core::identity::{
    ClientId, OperationStreamId, OperationStreamKey, PairingId, ServerId,
};
use spaceterm_core::management::{
    NativeVerification, ServerStatusParts, ServerVersion, StatusPublication, StatusQuery,
};
use spaceterm_core::organization::Versioned;
use spaceterm_core::remote_access::RemoteAccessPolicy;
use spaceterm_host::startup::DesiredServerState;
use spaceterm_protocol::ProtocolError;
use spaceterm_protocol::authentication::{
    ClientHandshake, Established, HandshakeConfig, PairingLookup, PairingRecord, Principal,
    Purpose, ServerHandshake, ServerPin, SigningKey,
};
use spaceterm_protocol::connection::AuthenticatedConnection;
use tokio::sync::watch;

use super::controller::{
    ActionIngress, CompanionAction, CompanionController, CompanionDependencies, CompanionFailure,
    CompanionTimer, DesktopActivation, NativeStatus, StatusArea,
};
use super::model::{CompanionModel, MenuEntry, Notice, Reason, ServerState, mnemonic_label};
use super::native::{ModelSlot, enclosing_desktop_bundle, sibling_desktop_executable};
use super::{CompanionIdentity, report, status_client};
use crate::application_identity::testing::all as desktop_identities;

const SERVER: ServerId = ServerId::new([1; 16]);
const EPOCH: u64 = 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Call {
    Status,
    Subscribe,
    Inspect,
    OpenDesktop,
}

#[derive(Default)]
struct Log(Mutex<Vec<Call>>);
impl Log {
    fn push(&self, call: Call) {
        self.0.lock().unwrap().push(call);
    }
    fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
    fn count(&self, call: Call) -> usize {
        self.calls().into_iter().filter(|c| *c == call).count()
    }
}

#[derive(Default)]
struct Resync(Mutex<usize>);
impl StatusResync for Resync {
    fn request_resync(&self) {
        *self.0.lock().unwrap() += 1;
    }
}

fn version(text: &str) -> ServerVersion {
    ServerVersion::new(text.to_owned()).unwrap()
}

fn status(
    revision: u64,
    running: Option<&str>,
    approved: Option<&str>,
    staged: Option<&str>,
) -> ServerStatus {
    ServerStatus::new(ServerStatusParts {
        runtime: Default::default(),
        server: SERVER,
        epoch: ServerEpoch::new(EPOCH),
        revision: NonZeroU64::new(revision).unwrap(),
        running: running.map(version),
        approved: approved.map(version),
        staged: staged.map(version),
        native_verification: NativeVerification::Unverified,
        capabilities: vec![],
        remote_access: Versioned {
            version: FieldVersion::new(1),
            value: RemoteAccessPolicy::disabled(SERVER),
        },
        listeners: vec![],
        progress: vec![],
    })
    .unwrap()
}

fn running(revision: u64, text: &str) -> ServerStatus {
    status(revision, Some(text), Some(text), None)
}

fn query() -> StatusQuery {
    StatusQuery {
        connection: ConnectionBinding {
            client: ClientId::new(SERVER, 9).unwrap(),
            generation: ConnectionGeneration::new(1),
        },
        request: NonZeroU64::new(1).unwrap(),
        generation: 1,
        known_epoch: None,
        known_revision: None,
        subscribe: true,
    }
}

fn snapshot(status: ServerStatus) -> StatusPublication {
    StatusPublication::Snapshot {
        request: NonZeroU64::new(1).unwrap(),
        generation: 1,
        status,
    }
}

fn changed(base: u64, status: ServerStatus) -> StatusPublication {
    StatusPublication::Changed {
        request: NonZeroU64::new(1).unwrap(),
        generation: 1,
        base: NonZeroU64::new(base).unwrap(),
        status,
    }
}

enum Mode {
    Serving(Box<ServerStatus>),
    Failing(ManagementFailure),
}

/// A server reachable only through status reads. `StatusManagement` has no attach,
/// control, startup or mutation method, so the call log is the whole of what the
/// companion asked of the server.
struct FakeServer {
    log: Arc<Log>,
    mode: Mutex<Mode>,
    worker: Mutex<Vec<StatusSubscription>>,
    resync: Mutex<Vec<Weak<Resync>>>,
}
impl FakeServer {
    fn new(log: Arc<Log>, mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            log,
            mode: Mutex::new(mode),
            worker: Mutex::new(Vec::new()),
            resync: Mutex::new(Vec::new()),
        })
    }
    fn set(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }
    /// The connection worker's handle to the newest subscription.
    fn publish(&self, publication: StatusPublication) -> StatusUpdate {
        let worker = self.worker.lock().unwrap();
        worker
            .last()
            .expect("a subscription exists")
            .apply(publication)
            .unwrap()
    }
    fn resync_requests(&self) -> usize {
        self.resync
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .map(|resync| *resync.0.lock().unwrap())
            .sum()
    }
    /// Drops the worker's handles. Afterwards a live subscription exists only while
    /// the companion still holds its client.
    fn companion_holds_a_subscription(&self) -> bool {
        self.worker.lock().unwrap().clear();
        self.resync
            .lock()
            .unwrap()
            .iter()
            .any(|resync| resync.upgrade().is_some())
    }
}
impl StatusManagement for FakeServer {
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        self.log.push(Call::Status);
        let result = match &*self.mode.lock().unwrap() {
            Mode::Serving(status) => Ok(status.as_ref().clone()),
            Mode::Failing(failure) => Err(*failure),
        };
        Box::pin(async move { result })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        self.log.push(Call::Subscribe);
        let result = match &*self.mode.lock().unwrap() {
            Mode::Serving(status) => {
                let resync = Arc::new(Resync::default());
                self.resync.lock().unwrap().push(Arc::downgrade(&resync));
                let subscription = StatusSubscription::new(SERVER, query(), resync).unwrap();
                subscription
                    .apply(snapshot(status.as_ref().clone()))
                    .unwrap();
                self.worker.lock().unwrap().push(subscription.clone());
                Ok(subscription)
            }
            Mode::Failing(failure) => Err(*failure),
        };
        Box::pin(async move { result })
    }
}

struct FakeNative {
    log: Arc<Log>,
    observation: Mutex<Result<NativeObservation, ManagementFailure>>,
}
impl FakeNative {
    fn new(log: Arc<Log>, desired: Option<DesiredServerState>) -> Arc<Self> {
        Arc::new(Self {
            log,
            observation: Mutex::new(Ok(NativeObservation {
                availability: RegistrationAvailability::Registered,
                desired,
                process: ProcessObservation::Absent,
                verified: true,
                approved: None,
                staged: None,
            })),
        })
    }
}
impl NativeStatus for FakeNative {
    fn inspect(&self) -> ManagementFuture<'_, NativeObservation> {
        self.log.push(Call::Inspect);
        let result = self.observation.lock().unwrap().clone();
        Box::pin(async move { result })
    }
}

struct FakeDesktop {
    log: Arc<Log>,
    available: bool,
}
impl DesktopActivation for FakeDesktop {
    fn open(&self) -> Result<(), CompanionFailure> {
        self.log.push(Call::OpenDesktop);
        if self.available {
            Ok(())
        } else {
            Err(CompanionFailure::DesktopUnavailable)
        }
    }
}

#[derive(Clone)]
struct Presented(Arc<Mutex<Vec<CompanionModel>>>);
impl Presented {
    fn all(&self) -> Vec<CompanionModel> {
        self.0.lock().unwrap().clone()
    }
    fn last(&self) -> Option<CompanionModel> {
        self.0.lock().unwrap().last().cloned()
    }
    async fn until(&self, predicate: impl Fn(&CompanionModel) -> bool) -> CompanionModel {
        for _ in 0..5_000 {
            if let Some(model) = self.last().filter(|model| predicate(model)) {
                return model;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!(
            "the expected presentation did not arrive: {:?}",
            self.last()
        );
    }
}

struct FakeArea {
    presented: Presented,
    ingress: Arc<ActionIngress>,
    available: bool,
}
impl StatusArea for FakeArea {
    fn present(&mut self, model: &CompanionModel) -> Result<(), CompanionFailure> {
        if !self.available {
            return Err(CompanionFailure::StatusAreaUnavailable);
        }
        self.presented.0.lock().unwrap().push(model.clone());
        Ok(())
    }
    fn next_action(&mut self) -> Result<Option<CompanionAction>, CompanionFailure> {
        Ok(self.ingress.pop())
    }
}

/// Waits complete only when a test releases them, so no test depends on a wall clock.
struct FakeTimer {
    release: watch::Sender<u64>,
    requests: Mutex<Vec<Duration>>,
}
impl FakeTimer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            release: watch::channel(0).0,
            requests: Mutex::new(Vec::new()),
        })
    }
    fn requests(&self) -> Vec<Duration> {
        self.requests.lock().unwrap().clone()
    }
    /// Releases every pending wait once the controller has requested `count` waits.
    async fn release_after(&self, count: usize) {
        for _ in 0..5_000 {
            if self.requests.lock().unwrap().len() >= count {
                self.release.send_modify(|value| *value += 1);
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("the controller did not wait");
    }
    /// Resolves once the controller has requested `count` waits.
    async fn requested(&self, count: usize) {
        for _ in 0..5_000 {
            if self.requests.lock().unwrap().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("the controller did not wait again");
    }
}
impl CompanionTimer for FakeTimer {
    fn sleep(&self, duration: Duration) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
        self.requests.lock().unwrap().push(duration);
        let mut released = self.release.subscribe();
        Box::pin(async move {
            let _ = released.changed().await;
        })
    }
}

struct Harness {
    log: Arc<Log>,
    server: Arc<FakeServer>,
    native: Arc<FakeNative>,
    presented: Presented,
    ingress: Arc<ActionIngress>,
    timer: Arc<FakeTimer>,
    task: tokio::task::JoinHandle<Result<(), CompanionFailure>>,
}
impl Harness {
    fn start(mode: Mode, desired: Option<DesiredServerState>) -> Self {
        Self::start_with(mode, desired, true, true)
    }
    fn start_with(
        mode: Mode,
        desired: Option<DesiredServerState>,
        area_available: bool,
        desktop_available: bool,
    ) -> Self {
        Self::compose(mode, desired, area_available, desktop_available)
    }
    fn compose(
        mode: Mode,
        desired: Option<DesiredServerState>,
        area_available: bool,
        desktop_available: bool,
    ) -> Self {
        let log = Arc::new(Log::default());
        let server = FakeServer::new(log.clone(), mode);
        let native = FakeNative::new(log.clone(), desired);
        let presented = Presented(Arc::new(Mutex::new(Vec::new())));
        let ingress = Arc::new(ActionIngress::new());
        let timer = FakeTimer::new();
        let dependencies = CompanionDependencies {
            status: server.clone(),
            native: native.clone(),
            desktop: Arc::new(FakeDesktop {
                log: log.clone(),
                available: desktop_available,
            }),
            area: Box::new(FakeArea {
                presented: presented.clone(),
                ingress: ingress.clone(),
                available: area_available,
            }),
            actions: ingress.clone(),
            timer: timer.clone(),
        };
        let task = tokio::spawn(CompanionController::new(dependencies).run());
        Self {
            log,
            server,
            native,
            presented,
            ingress,
            timer,
            task,
        }
    }
    fn act(&self, action: CompanionAction) {
        assert!(self.ingress.push(action));
    }
    async fn quit(self) -> Harness {
        self.act(CompanionAction::Quit);
        let Self {
            log,
            server,
            native,
            presented,
            ingress,
            timer,
            task,
        } = self;
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the companion quits promptly")
            .unwrap();
        assert_eq!(result, Ok(()));
        Self {
            log,
            server,
            native,
            presented,
            ingress,
            timer,
            task: tokio::spawn(async { Ok(()) }),
        }
    }
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
}

fn identity() -> CompanionIdentity {
    CompanionIdentity::for_desktop(desktop_identities()[2])
}

fn actions(entries: &[MenuEntry]) -> Vec<CompanionAction> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            MenuEntry::Action { action, .. } => Some(*action),
            _ => None,
        })
        .collect()
}

fn texts(model: &CompanionModel) -> Vec<String> {
    let identity = identity();
    let mut texts: Vec<String> = model
        .menu(identity)
        .into_iter()
        .filter_map(|entry| match entry {
            MenuEntry::Header { text, .. } | MenuEntry::Information { text, .. } => Some(text),
            MenuEntry::Action { title, .. } => Some(title),
            MenuEntry::Separator { .. } => None,
        })
        .collect();
    texts.push(model.accessibility_label(identity));
    texts.push(model.tooltip(identity));
    texts
}

#[tokio::test]
async fn launch_and_quit_leave_sessions_and_grants_unchanged() {
    let harness = Harness::start(Mode::Serving(Box::new(running(3, "1.2.3"))), None);
    let shown = harness
        .presented
        .until(|model| model.state() == ServerState::Running)
        .await;
    assert_eq!(shown.running_version(), Some("1.2.3"));
    assert_eq!(
        harness.presented.all().first().map(CompanionModel::state),
        Some(ServerState::Connecting)
    );

    let harness = harness.quit().await;

    // Sessions and grants are unchanged because only status reads were made, and the
    // status-only port offers no other request.
    assert!(
        harness
            .log
            .calls()
            .iter()
            .all(|call| matches!(call, Call::Subscribe | Call::Status))
    );
    // Quitting released the companion's only client handle; nothing else was closed.
    assert!(!harness.server.companion_holds_a_subscription());
}

#[tokio::test]
async fn passive_status_never_attaches_acquires_or_starts() {
    let harness = Harness::start(
        Mode::Failing(ManagementFailure::Offline),
        Some(DesiredServerState::Running),
    );
    let offline = harness
        .presented
        .until(|model| model.state() == ServerState::Offline)
        .await;
    assert!(offline.offers_retry());

    // Automatic retries and an explicit retry read status again and nothing else.
    harness.timer.release_after(1).await;
    harness.timer.release_after(2).await;
    harness.act(CompanionAction::OpenDesktop);
    harness.act(CompanionAction::RetryConnection);
    settle().await;
    harness
        .server
        .set(Mode::Serving(Box::new(running(1, "2.0.0"))));
    harness.act(CompanionAction::RetryConnection);
    harness
        .presented
        .until(|model| model.state() == ServerState::Running)
        .await;

    let harness = harness.quit().await;
    let calls = harness.log.calls();
    assert!(
        calls.iter().all(|call| matches!(
            call,
            Call::Status | Call::Subscribe | Call::Inspect | Call::OpenDesktop
        )),
        "{calls:?}"
    );
    assert_eq!(harness.log.count(Call::OpenDesktop), 1);
    assert!(harness.log.count(Call::Subscribe) >= 4);
    for model in harness.presented.all() {
        for action in actions(&model.menu(identity())) {
            assert!(matches!(
                action,
                CompanionAction::OpenDesktop
                    | CompanionAction::RetryConnection
                    | CompanionAction::Quit
            ));
        }
    }
}

struct BoundClient {
    context: AuthenticatedManagementContext,
}
impl StatusBackend for BoundClient {
    fn context(&self) -> Result<AuthenticatedManagementContext, ManagementFailure> {
        Ok(self.context)
    }
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        Box::pin(async { Ok(running(1, "1.0.0")) })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(async { Err(ManagementFailure::Offline) })
    }
}

struct Key(Ed25519KeyPair);
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
struct Pairings(PairingRecord);
impl PairingLookup for Pairings {
    fn lookup(
        &self,
        s: [u8; 16],
        p: [u8; 16],
        c: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        let principal = self.0.principal;
        if (s, p, c) != (principal.server, principal.pairing, principal.client) {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(self.0)
    }
}
fn local(value: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&value.to_be_bytes());
    bytes
}
/// An ordinary authenticated connection for one Client of one pairing.
fn authenticated(client: u64) -> (AuthenticatedConnection, OperationStreamId) {
    authenticated_on_server(
        client,
        SERVER,
        Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[41; 32]).unwrap())),
        1,
    )
}

fn authenticated_on_server(
    client: u64,
    server_id: ServerId,
    server: Arc<dyn SigningKey>,
    major: u32,
) -> (AuthenticatedConnection, OperationStreamId) {
    let device: Arc<dyn SigningKey> =
        Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap()));
    let principal = Principal {
        server: server_id.bytes(),
        device: [2; 16],
        pairing: local(3),
        client: local(client),
    };
    let pairings = Pairings(PairingRecord {
        principal,
        device_public_key: device.public_key(),
        active: true,
    });
    let (handshake, hello) = ClientHandshake::begin(
        ServerPin {
            identity: principal.server,
            public_key: server.public_key(),
        },
        principal,
        Purpose::Ordinary,
        HandshakeConfig {
            majors: vec![major],
            ..Default::default()
        },
        device,
    )
    .unwrap();
    let (accepted, reply) = ServerHandshake::accept(
        principal.server,
        EPOCH,
        HandshakeConfig::default(),
        server,
        &pairings,
        &hello,
    )
    .unwrap();
    let (handshake, proof) = handshake.receive_server(&reply).unwrap();
    let (_, ready) = accepted.receive_device(&pairings, &proof).unwrap();
    let Established::Ordinary(connection) = handshake.receive_ready(&ready).unwrap() else {
        panic!("an ordinary connection");
    };
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server_id, 5).unwrap(),
        PairingId::new(server_id, 3).unwrap(),
        ClientId::new(server_id, client).unwrap(),
    )
    .unwrap();
    (connection, stream)
}

/// The companion's application, bundle and bus identifiers never equal a desktop's. Its
/// distinct ClientId comes from pairing's additional-Client binding (C23), which the
/// composed backend supplies; this crate cannot observe it.
#[test]
fn companion_identifiers_differ_from_every_desktop_identity() {
    let mut names = BTreeSet::new();
    for desktop in desktop_identities() {
        let companion = CompanionIdentity::for_desktop(desktop);
        assert_eq!(
            companion.application_id(),
            format!("{}.status", desktop.application_id())
        );
        assert_eq!(
            companion.display_name(),
            format!("{} Status", desktop.display_name())
        );
        assert_eq!(companion.desktop_id(), desktop.application_id());
        assert!(names.insert(desktop.application_id().to_owned()));
        assert!(names.insert(companion.application_id()));
    }
    assert_eq!(names.len(), 6);
}

/// The status-only facade admits only a context the backend tagged as the companion's.
/// It checks the tag, not the ClientId behind it.
#[tokio::test]
async fn status_facade_admits_only_companion_bound_contexts() {
    let (connection, stream) = authenticated(4);
    let cli = AuthenticatedManagementContext::from_connection(
        &connection,
        stream,
        ManagementClientKind::Cli,
    )
    .unwrap();
    let borrowed = status_client(BoundClient { context: cli });
    assert_eq!(
        borrowed.status().await.unwrap_err(),
        ManagementFailure::AuthorizationDenied
    );

    let (connection, stream) = authenticated(9);
    let own = AuthenticatedManagementContext::from_connection(
        &connection,
        stream,
        ManagementClientKind::StatusCompanion,
    )
    .unwrap();
    let companion = status_client(BoundClient { context: own });
    assert_eq!(companion.status().await.unwrap().parts().server, SERVER);
}

#[tokio::test]
async fn revoked_companion_does_not_reenroll() {
    let harness = Harness::start(Mode::Failing(ManagementFailure::Revoked), None);
    let revoked = harness
        .presented
        .until(|model| model.state() == ServerState::NotPaired)
        .await;
    assert_eq!(revoked.reason(), Some(Reason::Revoked));
    assert!(revoked.offers_retry());
    settle().await;
    // No automatic attempt is scheduled for a revoked binding.
    assert!(harness.timer.requests().is_empty());
    assert_eq!(harness.log.count(Call::Subscribe), 1);

    harness.act(CompanionAction::RetryConnection);
    settle().await;
    harness
        .presented
        .until(|model| model.reason() == Some(Reason::Revoked))
        .await;
    assert_eq!(harness.log.count(Call::Subscribe), 2);

    let harness = harness.quit().await;
    // The status-only port has no enrollment path; only reads were made.
    assert!(
        harness
            .log
            .calls()
            .iter()
            .all(|call| *call == Call::Subscribe)
    );
}

#[tokio::test]
async fn status_gap_requires_resync() {
    let harness = Harness::start(Mode::Serving(Box::new(running(1, "1.0.0"))), None);
    harness
        .presented
        .until(|model| model.running_version() == Some("1.0.0"))
        .await;

    let gap = harness.server.publish(changed(5, running(7, "9.9.9")));
    assert_eq!(gap, StatusUpdate::ResyncRequired);
    assert_eq!(harness.server.resync_requests(), 1);
    let refreshing = harness.presented.until(CompanionModel::is_refreshing).await;
    assert_eq!(refreshing.state(), ServerState::Running);
    assert_eq!(refreshing.running_version(), Some("1.0.0"));
    assert!(
        harness
            .presented
            .all()
            .iter()
            .all(|model| model.running_version() != Some("9.9.9"))
    );

    harness.server.publish(snapshot(running(8, "2.0.0")));
    let replaced = harness
        .presented
        .until(|model| model.running_version() == Some("2.0.0"))
        .await;
    assert!(!replaced.is_refreshing());
    assert_eq!(harness.server.resync_requests(), 1);
    harness.quit().await;
}

#[tokio::test]
async fn stale_subscription_after_reconnect_cannot_replace_current_status() {
    let harness = Harness::start(Mode::Serving(Box::new(running(1, "1.0.0"))), None);
    harness
        .presented
        .until(|model| model.running_version() == Some("1.0.0"))
        .await;
    let stale = harness
        .server
        .worker
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    harness
        .server
        .set(Mode::Failing(ManagementFailure::Offline));
    harness.timer.release_after(1).await;
    harness
        .presented
        .until(|model| model.state() == ServerState::Offline)
        .await;
    harness
        .server
        .set(Mode::Serving(Box::new(running(2, "2.0.0"))));
    harness.act(CompanionAction::RetryConnection);
    harness
        .presented
        .until(|model| model.running_version() == Some("2.0.0"))
        .await;
    assert_eq!(harness.log.count(Call::Subscribe), 2);
    stale.apply(snapshot(running(100, "9.9.9"))).unwrap();
    settle().await;
    assert_eq!(
        harness.presented.last().unwrap().running_version(),
        Some("2.0.0")
    );
    harness.quit().await;
}

#[tokio::test]
async fn delayed_resync_must_reach_the_highest_observed_epoch_and_revision() {
    let harness = Harness::start(Mode::Serving(Box::new(running(1, "1.0.0"))), None);
    harness
        .presented
        .until(|model| model.running_version() == Some("1.0.0"))
        .await;
    let newer = |epoch, revision, text| {
        let mut parts = running(revision, text).parts().clone();
        parts.epoch = ServerEpoch::new(epoch);
        ServerStatus::new(parts).unwrap()
    };
    harness
        .server
        .publish(changed(2, newer(EPOCH + 1, 4, "4.0.0")));
    harness.presented.until(CompanionModel::is_refreshing).await;
    harness
        .server
        .publish(changed(4, newer(EPOCH + 1, 8, "8.0.0")));
    for delayed in [running(100, "9.9.9"), newer(EPOCH + 1, 7, "7.0.0")] {
        assert_eq!(
            harness.server.publish(snapshot(delayed)),
            StatusUpdate::ResyncRequired
        );
    }
    settle().await;
    let shown = harness.presented.last().unwrap();
    assert_eq!(shown.running_version(), Some("1.0.0"));
    assert!(shown.is_refreshing());
    harness
        .server
        .publish(snapshot(newer(EPOCH + 1, 8, "8.0.0")));
    let shown = harness
        .presented
        .until(|model| model.running_version() == Some("8.0.0"))
        .await;
    assert!(!shown.is_refreshing());
    harness.quit().await;
}

#[tokio::test]
async fn liveness_reads_during_a_resync_keep_the_shown_status_fenced() {
    let harness = Harness::start(Mode::Serving(Box::new(running(1, "1.0.0"))), None);
    harness
        .presented
        .until(|model| model.running_version() == Some("1.0.0"))
        .await;
    let gap = harness.server.publish(changed(5, running(7, "9.9.9")));
    assert_eq!(gap, StatusUpdate::ResyncRequired);
    harness.presented.until(CompanionModel::is_refreshing).await;

    // Direct reads at or below the observed revision 7 prove only that the server answers.
    for (liveness, read) in [(1, running(1, "1.0.0")), (2, running(6, "6.0.0"))] {
        harness.server.set(Mode::Serving(Box::new(read)));
        harness.timer.release_after(liveness).await;
        harness.timer.requested(liveness + 1).await;
        assert_eq!(harness.log.count(Call::Status), liveness);
        let shown = harness.presented.last().unwrap();
        assert!(shown.is_refreshing(), "{shown:?}");
        assert_eq!(shown.running_version(), Some("1.0.0"));
    }
    assert!(
        harness
            .presented
            .all()
            .iter()
            .all(|model| model.running_version() != Some("6.0.0"))
    );

    // Only the qualifying resync snapshot ends the refresh.
    harness.server.publish(snapshot(running(8, "2.0.0")));
    let replaced = harness
        .presented
        .until(|model| model.running_version() == Some("2.0.0"))
        .await;
    assert!(!replaced.is_refreshing());

    // Without a pending resync, a newer direct read is shown and an older one is not.
    harness
        .server
        .set(Mode::Serving(Box::new(running(9, "3.0.0"))));
    harness.timer.release_after(3).await;
    harness
        .presented
        .until(|model| model.running_version() == Some("3.0.0"))
        .await;
    harness
        .server
        .set(Mode::Serving(Box::new(running(8, "2.0.0"))));
    harness.timer.release_after(4).await;
    harness.timer.requested(5).await;
    assert_eq!(
        harness.presented.last().unwrap().running_version(),
        Some("3.0.0")
    );
    harness.quit().await;
}

/// A host the server cannot run on stays unsupported, so nothing polls it.
#[tokio::test]
async fn an_unsupported_host_offers_no_retry_and_does_not_poll() {
    let harness = Harness::start(Mode::Failing(ManagementFailure::UnsupportedHost), None);
    let unsupported = harness
        .presented
        .until(|model| model.state() == ServerState::Unavailable)
        .await;
    assert_eq!(unsupported.reason(), Some(Reason::Unsupported));
    assert!(!unsupported.offers_retry());
    assert_eq!(
        actions(&unsupported.menu(identity())),
        vec![CompanionAction::OpenDesktop, CompanionAction::Quit]
    );

    harness.act(CompanionAction::RetryConnection);
    settle().await;
    assert!(harness.timer.requests().is_empty());
    assert_eq!(harness.log.count(Call::Subscribe), 1);
    harness.quit().await;
}

/// The native discovery classification covers a missing registration or installation,
/// an unavailable supervisor and suppressed background items. The user can fix each of
/// them, so a bound companion keeps reading on its own and offers Retry Connection.
#[tokio::test]
async fn native_unavailability_from_a_bound_backend_retries_automatically() {
    let harness = Harness::start(Mode::Failing(ManagementFailure::NativeUnavailable), None);
    let not_set_up = harness
        .presented
        .until(|model| model.state() == ServerState::Unavailable)
        .await;
    assert_eq!(not_set_up.reason(), Some(Reason::NotSetUp));
    assert!(not_set_up.offers_retry());
    assert!(not_set_up.retries_automatically());
    harness.timer.release_after(1).await;
    harness.timer.requested(2).await;
    assert_eq!(harness.log.count(Call::Subscribe), 2);

    // Once the server is set up, the next attempt shows it.
    harness
        .server
        .set(Mode::Serving(Box::new(running(1, "1.0.0"))));
    harness.act(CompanionAction::RetryConnection);
    harness
        .presented
        .until(|model| model.state() == ServerState::Running)
        .await;
    harness.quit().await;

    // Other temporary failures also retry on their own and offer Retry Connection.
    for failure in [
        ManagementFailure::CredentialUnavailable,
        ManagementFailure::StorageLocked,
    ] {
        let harness = Harness::start(Mode::Failing(failure), None);
        let temporary = harness
            .presented
            .until(|model| model.state() == ServerState::Unavailable)
            .await;
        assert!(temporary.offers_retry(), "{failure:?}");
        harness.timer.release_after(1).await;
        harness.timer.requested(2).await;
        assert_eq!(harness.log.count(Call::Subscribe), 2, "{failure:?}");
        harness.quit().await;
    }
}

#[tokio::test]
async fn absent_native_setup_is_not_reported_as_an_offline_or_stopped_server() {
    let harness = Harness::start(
        Mode::Failing(ManagementFailure::Offline),
        Some(DesiredServerState::Stopped),
    );
    harness
        .native
        .observation
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .availability = RegistrationAvailability::Missing;
    let shown = harness
        .presented
        .until(|model| model.state() != ServerState::Connecting)
        .await;
    assert_eq!(shown.reason(), Some(Reason::NotSetUp));
    assert!(shown.offers_retry());
    harness.quit().await;
}

#[tokio::test]
async fn native_setup_failures_override_offline_without_hiding_authentication_failures() {
    for (failure, reason) in [
        (ManagementFailure::NativeUnavailable, Reason::NotSetUp),
        (ManagementFailure::StorageLocked, Reason::StorageLocked),
        (ManagementFailure::UnsafeNativeState, Reason::Unavailable),
        (ManagementFailure::UnsupportedHost, Reason::Unsupported),
    ] {
        let harness = Harness::start(Mode::Failing(ManagementFailure::Offline), None);
        *harness.native.observation.lock().unwrap() = Err(failure);
        let shown = harness
            .presented
            .until(|model| model.state() != ServerState::Connecting)
            .await;
        assert_eq!(shown.reason(), Some(reason));
        harness.quit().await;
    }
    let harness = Harness::start(Mode::Failing(ManagementFailure::Revoked), None);
    *harness.native.observation.lock().unwrap() = Err(ManagementFailure::NativeUnavailable);
    let shown = harness
        .presented
        .until(|model| model.state() != ServerState::Connecting)
        .await;
    assert_eq!(shown.reason(), Some(Reason::Revoked));
    assert_eq!(harness.log.count(Call::Inspect), 0);
    harness.quit().await;
}

#[tokio::test]
async fn unavailable_native_observations_never_claim_stopped_or_offline() {
    for (availability, reason) in [
        (RegistrationAvailability::Missing, Reason::NotSetUp),
        (RegistrationAvailability::Unavailable, Reason::NotSetUp),
        (RegistrationAvailability::Locked, Reason::StorageLocked),
        (RegistrationAvailability::Unsafe, Reason::Unavailable),
        (RegistrationAvailability::Unsupported, Reason::Unsupported),
    ] {
        for desired in [None, Some(DesiredServerState::Stopped)] {
            let harness = Harness::start(Mode::Failing(ManagementFailure::Offline), desired);
            harness
                .native
                .observation
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .availability = availability;
            let shown = harness
                .presented
                .until(|model| model.state() != ServerState::Connecting)
                .await;
            assert_eq!(shown.state(), ServerState::Unavailable);
            assert_eq!(shown.reason(), Some(reason));
            harness.quit().await;
        }
    }
}

#[tokio::test]
async fn stopped_state_does_not_reconnect_by_starting() {
    let harness = Harness::start(
        Mode::Failing(ManagementFailure::Offline),
        Some(DesiredServerState::Stopped),
    );
    let stopped = harness
        .presented
        .until(|model| model.state() == ServerState::Stopped)
        .await;
    assert_eq!(stopped.reason(), Some(Reason::Stopped));
    assert!(!stopped.offers_retry());
    assert_eq!(
        actions(&stopped.menu(identity())),
        vec![CompanionAction::OpenDesktop, CompanionAction::Quit]
    );

    // Passive refreshes only read status. A Retry request is not offered and is ignored.
    for count in 1..=3 {
        harness.timer.release_after(count).await;
    }
    harness.act(CompanionAction::RetryConnection);
    harness.timer.release_after(4).await;
    settle().await;
    let attempts = harness.log.count(Call::Subscribe);
    assert_eq!(attempts, 5);
    assert_eq!(harness.log.count(Call::Inspect), attempts);
    assert_eq!(
        harness.presented.last().map(|model| model.state()),
        Some(ServerState::Stopped)
    );
    let harness = harness.quit().await;
    assert!(
        harness
            .log
            .calls()
            .iter()
            .all(|call| matches!(call, Call::Subscribe | Call::Inspect))
    );
    assert!(harness.native.observation.lock().unwrap().is_ok());
}

#[tokio::test]
async fn open_desktop_uses_local_installation_not_server_path() {
    assert_eq!(
        enclosing_desktop_bundle(Path::new(
            "/Applications/SpaceTerm.app/Contents/Helpers/SpaceTerm Status.app/Contents/MacOS/SpaceTerm Status"
        )),
        Some(PathBuf::from("/Applications/SpaceTerm.app"))
    );
    for unrelated in [
        "/usr/local/bin/spaceterm-status",
        "/Applications/SpaceTerm Status.app/Contents/MacOS/SpaceTerm Status",
        "/Applications/SpaceTerm.app/Contents/Helpers/SpaceTerm Status.app/Contents/Resources/SpaceTerm Status",
        "/Applications/SpaceTerm/Contents/Helpers/SpaceTerm Status.app/Contents/MacOS/SpaceTerm Status",
    ] {
        assert_eq!(
            enclosing_desktop_bundle(Path::new(unrelated)),
            None,
            "{unrelated}"
        );
    }
    assert_eq!(
        sibling_desktop_executable(Path::new("/opt/spaceterm-preflight/bin/spaceterm-status")),
        Some(PathBuf::from("/opt/spaceterm-preflight/bin/spaceterm"))
    );
    assert_eq!(
        sibling_desktop_executable(Path::new("spaceterm-status")),
        None
    );

    // A server value shaped like a path is never presented or used to open anything.
    let harness = Harness::start_with(
        Mode::Serving(Box::new(status(
            1,
            Some("/tmp/evil"),
            Some("/tmp/evil"),
            Some("../x"),
        ))),
        None,
        true,
        false,
    );
    harness
        .presented
        .until(|model| model.state() == ServerState::Running)
        .await;
    harness.act(CompanionAction::OpenDesktop);
    let failed = harness
        .presented
        .until(|model| model.notice().is_some())
        .await;
    assert_eq!(failed.state(), ServerState::Running);
    let harness = harness.quit().await;
    assert_eq!(harness.log.count(Call::OpenDesktop), 1);
    for model in harness.presented.all() {
        assert_eq!(model.running_version(), None);
        for text in texts(&model) {
            assert!(
                !text.contains("evil") && !text.contains('/') && !text.contains(".."),
                "{text}"
            );
        }
    }
}

#[tokio::test]
async fn missing_status_area_does_not_affect_server() {
    let harness = Harness::start_with(
        Mode::Serving(Box::new(running(1, "1.0.0"))),
        None,
        false,
        true,
    );
    let result = tokio::time::timeout(Duration::from_secs(5), harness.task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result, Err(CompanionFailure::StatusAreaUnavailable));
    // The companion failed before it made any request of the server.
    assert!(harness.log.calls().is_empty());

    // No package installs spaceterm-cli, so the next step names the desktop instead.
    let (code, message) = report(identity(), Err(CompanionFailure::StatusAreaUnavailable));
    assert_eq!(code, 69);
    assert_eq!(
        message.as_deref(),
        Some(
            "SpaceTerm Development Status: this session has no status area. \
             Open SpaceTerm Development to see server status."
        )
    );
    assert_eq!(report(identity(), Ok(())), (0, None));
}

#[tokio::test]
async fn status_updates_coalesce_without_unbounded_queue_growth() {
    let ingress = ActionIngress::new();
    for round in 0..1_000 {
        let action = [
            CompanionAction::RetryConnection,
            CompanionAction::OpenDesktop,
            CompanionAction::Quit,
        ][round % 3];
        assert!(ingress.push(action));
        assert!(ingress.len() <= ActionIngress::CAPACITY);
    }
    assert_eq!(ingress.len(), 3);
    assert_eq!(ingress.pop(), Some(CompanionAction::RetryConnection));
    assert_eq!(ingress.pop(), Some(CompanionAction::OpenDesktop));
    assert_eq!(ingress.pop(), Some(CompanionAction::Quit));
    assert_eq!(ingress.pop(), None);

    let slot = ModelSlot::new();
    let mut scheduled = 0;
    for revision in 1..=1_000 {
        let model = CompanionModel::from_status(&running(revision, &format!("1.0.{revision}")));
        scheduled += usize::from(slot.replace(model));
    }
    assert_eq!(scheduled, 1);
    assert_eq!(slot.take().unwrap().running_version(), Some("1.0.1000"));
    assert_eq!(slot.take(), None);
    assert!(slot.replace(CompanionModel::connecting()));

    let harness = Harness::start(Mode::Serving(Box::new(running(1, "1.0.1"))), None);
    harness
        .presented
        .until(|model| model.running_version() == Some("1.0.1"))
        .await;
    let before = harness.presented.all().len();
    for revision in 2..=500 {
        let update = harness.server.publish(changed(
            revision - 1,
            running(revision, &format!("1.0.{revision}")),
        ));
        assert_eq!(update, StatusUpdate::Replaced);
    }
    harness
        .presented
        .until(|model| model.running_version() == Some("1.0.500"))
        .await;
    assert!(harness.presented.all().len() - before <= 2);
    harness.quit().await;
}

#[test]
fn menu_is_keyboard_operable_and_status_is_named_accessibly() {
    let offline = |failure| CompanionModel::disconnected(failure, None);
    let stopped = NativeObservation {
        availability: RegistrationAvailability::Registered,
        desired: Some(DesiredServerState::Stopped),
        process: ProcessObservation::Absent,
        verified: false,
        approved: None,
        staged: None,
    };
    let models = [
        CompanionModel::connecting(),
        CompanionModel::from_status(&running(1, "1.2.3")),
        CompanionModel::from_status(&status(1, Some("1.2.3"), Some("1.3.0"), None)),
        CompanionModel::disconnected(ManagementFailure::Offline, Some(&stopped)),
        offline(ManagementFailure::Offline),
        offline(ManagementFailure::Incompatible),
        offline(ManagementFailure::PairingRequired),
        offline(ManagementFailure::NativeUnavailable),
    ];
    let states: Vec<ServerState> = models.iter().map(CompanionModel::state).collect();
    assert_eq!(states, ServerState::ALL.to_vec());

    let mut state_texts = BTreeSet::new();
    let mut macos_symbols = BTreeSet::new();
    let mut linux_icons = BTreeSet::new();
    for desktop in desktop_identities() {
        let identity = CompanionIdentity::for_desktop(desktop);
        for model in &models {
            let menu = model.menu(identity);
            let MenuEntry::Header { text, .. } = &menu[0] else {
                panic!("the menu starts with the state");
            };
            assert_eq!(text, model.state_text());
            state_texts.insert(model.state_text());
            assert_eq!(model.state_description(), model.state_text());
            let label = model.accessibility_label(identity);
            assert_eq!(
                label,
                format!("{}, {}", identity.display_name(), model.state_text())
            );
            assert!(model.tooltip(identity).contains(model.state_text()));
            macos_symbols.insert(model.symbol().macos());
            linux_icons.insert(model.symbol().freedesktop());
            assert!(model.symbol().freedesktop().ends_with("-symbolic"));

            let mut keys = BTreeSet::new();
            let mut mnemonics = BTreeSet::new();
            let mut ids = BTreeSet::new();
            let mut separated = false;
            for entry in &menu {
                assert!(ids.insert(entry.id()), "menu identifiers are unique");
                match entry {
                    MenuEntry::Separator { .. } => separated = true,
                    MenuEntry::Header { .. } | MenuEntry::Information { .. } => {
                        assert!(!separated, "status lines precede the commands")
                    }
                    MenuEntry::Action {
                        title,
                        key,
                        mnemonic,
                        action,
                        ..
                    } => {
                        assert!(separated);
                        assert!(key.is_ascii_lowercase());
                        assert!(keys.insert(*key));
                        assert!(mnemonics.insert(mnemonic.to_ascii_lowercase()));
                        assert!(title.contains(*mnemonic));
                        let label = mnemonic_label(title, *mnemonic);
                        assert_eq!(label.matches('_').count(), 1, "{label}");
                        assert_eq!(label.replace('_', ""), *title);
                        for word in title.split(' ') {
                            assert!(word.starts_with(|c: char| c.is_uppercase()), "{title}");
                        }
                        for destructive in [
                            "Stop", "Start", "End", "Unpair", "Activate", "Delete", "Remove",
                            "Install",
                        ] {
                            assert!(!title.contains(destructive), "{title}");
                        }
                        let expected = match action {
                            CompanionAction::OpenDesktop => {
                                format!("Open {}", desktop.display_name())
                            }
                            CompanionAction::RetryConnection => "Retry Connection".to_owned(),
                            CompanionAction::Quit => format!("Quit {}", identity.display_name()),
                        };
                        assert_eq!(*title, expected);
                    }
                }
            }
            let offered = actions(&menu);
            assert_eq!(offered.first(), Some(&CompanionAction::OpenDesktop));
            assert_eq!(offered.last(), Some(&CompanionAction::Quit));
            assert_eq!(
                offered.contains(&CompanionAction::RetryConnection),
                model.offers_retry()
            );
            assert_eq!(
                model.offers_retry(),
                matches!(
                    model.state(),
                    ServerState::Offline
                        | ServerState::Incompatible
                        | ServerState::NotPaired
                        | ServerState::Unavailable
                ) && model.reason() != Some(Reason::Unsupported)
            );
            // The tool tip description repeats every status line, starting with the state.
            let description = model.description(identity);
            assert!(description.starts_with(model.state_text()), "{description}");
            for entry in &menu {
                if let MenuEntry::Information { text, .. } = entry {
                    assert!(description.contains(text.as_str()), "{description}");
                }
            }
        }
    }
    // State is distinguished by text and by symbol shape, never by color alone.
    assert_eq!(state_texts.len(), ServerState::ALL.len());
    assert_eq!(macos_symbols.len(), ServerState::ALL.len());
    assert_eq!(linux_icons.len(), ServerState::ALL.len());

    let updating = &models[2];
    let lines = texts(updating);
    assert!(
        lines.iter().any(|line| line == "Version 1.2.3"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line == "Updating to 1.3.0"),
        "{lines:?}"
    );
    let staged =
        CompanionModel::from_status(&status(1, Some("1.2.3"), Some("1.2.3"), Some("1.4.0")));
    assert_eq!(staged.state(), ServerState::Running);
    assert!(
        texts(&staged)
            .iter()
            .any(|line| line == "Update 1.4.0 Staged")
    );
    let unverified = &models[3];
    assert!(
        texts(unverified)
            .iter()
            .any(|line| line == "This state is from an unverified local check.")
    );
    assert_eq!(
        mnemonic_label("Quit My_App Status", 'Q'),
        "_Quit My__App Status"
    );

    // A resync in progress is part of what the status item says, on every platform.
    let refreshing = CompanionModel::from_status(&running(1, "1.2.3")).with_refreshing(true);
    assert_eq!(refreshing.state(), ServerState::Running);
    assert_eq!(
        refreshing.state_description(),
        "Server Running, Refreshing Status"
    );
    assert_eq!(
        refreshing.accessibility_label(identity()),
        "SpaceTerm Development Status, Server Running, Refreshing Status"
    );
    assert_eq!(
        refreshing.tooltip(identity()),
        "SpaceTerm Development Status: Server Running, Refreshing Status"
    );
    assert!(
        texts(&refreshing)
            .iter()
            .any(|line| line == "Refreshing Status")
    );
    assert_eq!(
        refreshing.description(identity()),
        "Server Running, Refreshing Status\nVersion 1.2.3"
    );

    // Keyboard focus skips disabled status lines in GTK and Qt menus, so the description
    // carries the reason and any notice to screen readers.
    let unreachable = CompanionModel::disconnected(ManagementFailure::PairingRequired, None)
        .with_notice(Some(Notice::DesktopUnavailable));
    assert_eq!(
        unreachable.description(identity()),
        "Not Paired\n\
         Pair this device in SpaceTerm Development Settings.\n\
         Could not open SpaceTerm Development."
    );
}

#[test]
fn every_reason_names_the_next_step() {
    let reason = |failure| {
        let model = CompanionModel::disconnected(failure, None);
        // A failure without a version, notice or local observation shows one status line.
        let lines: Vec<String> = model
            .menu(identity())
            .into_iter()
            .filter_map(|entry| match entry {
                MenuEntry::Information { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        let [text] = lines.as_slice() else {
            panic!("one reason line: {lines:?}");
        };
        (model.state(), model.reason().unwrap(), text.clone())
    };
    let expected = [
        (
            ManagementFailure::PairingRequired,
            ServerState::NotPaired,
            Reason::PairingRequired,
            "Pair this device in SpaceTerm Development Settings.",
        ),
        (
            ManagementFailure::AuthorizationDenied,
            ServerState::NotPaired,
            Reason::NotAllowed,
            "Allow SpaceTerm Development Status to read status in SpaceTerm Development Settings.",
        ),
        (
            ManagementFailure::Revoked,
            ServerState::NotPaired,
            Reason::Revoked,
            "Pair this device again in SpaceTerm Development Settings.",
        ),
        (
            ManagementFailure::Incompatible,
            ServerState::Incompatible,
            Reason::Incompatible,
            "Update SpaceTerm Development or the server to reconnect.",
        ),
        (
            ManagementFailure::Offline,
            ServerState::Offline,
            Reason::Offline,
            "Retrying automatically.",
        ),
        (
            ManagementFailure::StorageLocked,
            ServerState::Unavailable,
            Reason::StorageLocked,
            "The server is waiting for its storage to unlock. Status updates automatically.",
        ),
        (
            ManagementFailure::CredentialUnavailable,
            ServerState::Unavailable,
            Reason::Unavailable,
            "Server status could not be read. Retrying automatically.",
        ),
        (
            ManagementFailure::NativeUnavailable,
            ServerState::Unavailable,
            Reason::NotSetUp,
            "Set up the server in SpaceTerm Development Settings.",
        ),
        (
            ManagementFailure::UnsupportedHost,
            ServerState::Unavailable,
            Reason::Unsupported,
            "This installation cannot report server status. Open SpaceTerm Development to see it.",
        ),
    ];
    for (failure, state, kind, text) in expected {
        assert_eq!(
            reason(failure),
            (state, kind, text.to_owned()),
            "{failure:?}"
        );
    }

    let stopped = NativeObservation {
        availability: RegistrationAvailability::Registered,
        desired: Some(DesiredServerState::Stopped),
        process: ProcessObservation::Absent,
        verified: true,
        approved: None,
        staged: None,
    };
    let model = CompanionModel::disconnected(ManagementFailure::Offline, Some(&stopped));
    assert_eq!(model.reason(), Some(Reason::Stopped));
    assert!(
        texts(&model)
            .iter()
            .any(|line| line == "Start it from SpaceTerm Development Settings."),
        "{:?}",
        texts(&model)
    );
    for model in [
        model,
        CompanionModel::disconnected(ManagementFailure::Offline, None),
    ] {
        for line in texts(&model) {
            assert!(!line.contains("spaceterm-"), "{line}");
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_session {
    //! The Linux status area against a private session bus and a controlled watcher.
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use spaceterm_client::management_cli::ManagementFailure;
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Structure, Value};

    use super::super::controller::{ActionIngress, CompanionAction, CompanionFailure, StatusArea};
    use super::super::linux::LinuxStatusArea;
    use super::super::model::{CompanionModel, Notice};
    use super::super::native::ActivationToken;
    use super::{identity, running};

    /// Short enough for tests, long enough that a replacement watcher can return in time.
    const HOST_GRACE: Duration = Duration::from_millis(400);

    struct PrivateBus {
        daemon: Child,
        address: String,
    }
    impl PrivateBus {
        fn start() -> Self {
            let mut daemon = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("dbus-daemon starts a private bus");
            let mut address = String::new();
            BufReader::new(daemon.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();
            Self {
                daemon,
                address: address.trim().to_owned(),
            }
        }
    }
    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    struct Watcher {
        items: Arc<Mutex<Vec<String>>>,
        host: Arc<AtomicBool>,
        reject_once: AtomicBool,
    }
    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl Watcher {
        fn register_status_notifier_item(&self, service: String) -> zbus::fdo::Result<()> {
            if self.reject_once.swap(false, Ordering::AcqRel) {
                return Err(zbus::fdo::Error::Failed("registration rejected".to_owned()));
            }
            self.items.lock().unwrap().push(service);
            Ok(())
        }
        #[zbus(signal)]
        async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn status_notifier_host_unregistered(emitter: &SignalEmitter<'_>)
        -> zbus::Result<()>;
        #[zbus(property)]
        fn is_status_notifier_host_registered(&self) -> bool {
            self.host.load(Ordering::Acquire)
        }
        #[zbus(property)]
        fn registered_status_notifier_items(&self) -> Vec<String> {
            self.items.lock().unwrap().clone()
        }
        #[zbus(property)]
        fn protocol_version(&self) -> i32 {
            0
        }
    }

    async fn watcher(bus: &PrivateBus, host: bool) -> (zbus::Connection, Arc<Mutex<Vec<String>>>) {
        let (connection, items, _) = hosted_watcher(bus, host).await;
        (connection, items)
    }

    /// A watcher whose host can leave while the watcher stays.
    async fn hosted_watcher(
        bus: &PrivateBus,
        host: bool,
    ) -> (zbus::Connection, Arc<Mutex<Vec<String>>>, Arc<AtomicBool>) {
        watcher_with_rejection(bus, host, false).await
    }

    async fn watcher_with_rejection(
        bus: &PrivateBus,
        host: bool,
        reject_once: bool,
    ) -> (zbus::Connection, Arc<Mutex<Vec<String>>>, Arc<AtomicBool>) {
        let items = Arc::new(Mutex::new(Vec::new()));
        let host = Arc::new(AtomicBool::new(host));
        let connection = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.kde.StatusNotifierWatcher")
            .unwrap()
            .serve_at(
                "/StatusNotifierWatcher",
                Watcher {
                    items: items.clone(),
                    host: host.clone(),
                    reject_once: AtomicBool::new(reject_once),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        (connection, items, host)
    }

    async fn host(bus: &PrivateBus) -> zbus::Connection {
        zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    /// Connects with the private bus standing in for both the session bus and the system
    /// bus that logind is on. A test without a [`Login`] has no logind, so the session
    /// never counts as away.
    async fn connect(
        bus: &PrivateBus,
    ) -> Result<(LinuxStatusArea, Arc<ActionIngress>), CompanionFailure> {
        let ingress = Arc::new(ActionIngress::new());
        LinuxStatusArea::connect(
            identity(),
            ingress.clone(),
            ActivationToken::default(),
            Some(bus.address.clone()),
            Some(bus.address.clone()),
            HOST_GRACE,
        )
        .await
        .map(|area| (area, ingress))
    }

    type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);
    type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);

    async fn layout(connection: &zbus::Connection, name: &str) -> (u32, Layout) {
        let reply = connection
            .call_method(
                Some(name),
                "/MenuBar",
                Some("com.canonical.dbusmenu"),
                "GetLayout",
                &(0i32, -1i32, Vec::<String>::new()),
            )
            .await
            .unwrap();
        reply.body().deserialize().unwrap()
    }

    fn labels(layout: &Layout) -> Vec<(i32, String, bool)> {
        layout
            .2
            .iter()
            .map(|child| {
                let Value::Structure(structure) = &**child else {
                    panic!("a menu item structure");
                };
                let fields = Structure::fields(structure);
                let id = i32::try_from(&fields[0]).unwrap();
                let Value::Dict(properties) = &fields[1] else {
                    panic!("menu item properties");
                };
                let label: Option<String> = properties.get(&"label").unwrap();
                let enabled: Option<bool> = properties.get(&"enabled").unwrap();
                (id, label.unwrap_or_default(), enabled.unwrap_or(true))
            })
            .collect()
    }

    async fn property<T: TryFrom<OwnedValue>>(
        connection: &zbus::Connection,
        name: &str,
        property: &str,
    ) -> T {
        let reply = connection
            .call_method(
                Some(name),
                "/StatusNotifierItem",
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("org.kde.StatusNotifierItem", property),
            )
            .await
            .unwrap();
        let value: OwnedValue = reply.body().deserialize().unwrap();
        T::try_from(value).ok().unwrap()
    }

    #[tokio::test]
    async fn status_area_registers_and_serves_an_accessible_keyboard_menu() {
        let bus = PrivateBus::start();
        let (_watcher, items) = watcher(&bus, true).await;
        let (mut area, ingress) = connect(&bus).await.unwrap();
        let name = identity().application_id();
        assert_eq!(items.lock().unwrap().clone(), vec![name.clone()]);

        area.present(&CompanionModel::from_status(&running(1, "1.2.3")))
            .unwrap();
        let host = host(&bus).await;
        let mut found = Vec::new();
        for _ in 0..500 {
            let (_, layout) = layout(&host, &name).await;
            found = labels(&layout);
            if found.iter().any(|(_, label, _)| label == "Server Running") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let open = found
            .iter()
            .find(|(_, label, _)| label == "_Open SpaceTerm Development")
            .expect("an Open item with a mnemonic");
        assert!(open.2);
        assert!(
            found
                .iter()
                .any(|(_, label, enabled)| label == "Server Running" && !enabled)
        );
        assert!(
            found
                .iter()
                .any(|(_, label, enabled)| label == "Version 1.2.3" && !enabled)
        );
        assert!(
            found.iter().any(
                |(_, label, enabled)| label == "_Quit SpaceTerm Development Status" && *enabled
            )
        );
        assert!(!found.iter().any(|(_, label, _)| label.contains("Retry")));

        let icon: String = property(&host, &name, "IconName").await;
        assert_eq!(icon, "utilities-terminal-symbolic");
        let menu: bool = property(&host, &name, "ItemIsMenu").await;
        assert!(menu);
        let tool_tip: ToolTip = property(&host, &name, "ToolTip").await;
        assert_eq!(tool_tip.2, "SpaceTerm Development Status");
        assert_eq!(tool_tip.3, "Server Running\nVersion 1.2.3");

        host.call_method(
            Some(name.as_str()),
            "/MenuBar",
            Some("com.canonical.dbusmenu"),
            "Event",
            &(open.0, "clicked", Value::from(""), 0u32),
        )
        .await
        .unwrap();
        assert_eq!(
            area.next_action().unwrap(),
            Some(CompanionAction::OpenDesktop)
        );
        assert_eq!(ingress.pop(), None);

        // Hosts read the tool tip text as the item's accessible description. It carries
        // every status line, because keyboard focus skips the disabled ones in the menu.
        area.present(&CompanionModel::from_status(&running(1, "1.2.3")).with_refreshing(true))
            .unwrap();
        assert_eq!(
            description(&host, &name, "Server Running\nVersion 1.2.3").await,
            "Server Running, Refreshing Status\nVersion 1.2.3"
        );
        area.present(
            &CompanionModel::disconnected(ManagementFailure::PairingRequired, None)
                .with_notice(Some(Notice::DesktopUnavailable)),
        )
        .unwrap();
        let unreachable = description(
            &host,
            &name,
            "Server Running, Refreshing Status\nVersion 1.2.3",
        )
        .await;
        assert!(
            unreachable.contains("Pair this device in SpaceTerm Development Settings."),
            "{unreachable}"
        );
        assert!(
            unreachable.contains("Could not open SpaceTerm Development."),
            "{unreachable}"
        );
    }

    /// The tool tip description once it differs from `previous`.
    async fn description(host: &zbus::Connection, name: &str, previous: &str) -> String {
        for _ in 0..500 {
            let tool_tip: ToolTip = property(host, name, "ToolTip").await;
            if tool_tip.3 != previous {
                return tool_tip.3;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the tool tip did not change");
    }

    /// Waits until the area reports itself lost, as the controller would observe it.
    async fn lost(area: &mut LinuxStatusArea, ingress: &ActionIngress) {
        // A stalled logind needs a recheck interval and a bus deadline before the grace.
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                ingress.wait().await;
                match area.next_action() {
                    Err(failure) => return failure,
                    Ok(_) => tokio::time::sleep(Duration::from_millis(5)).await,
                }
            }
        })
        .await
        .map(|failure| assert_eq!(failure, CompanionFailure::StatusAreaUnavailable))
        .expect("the area reports its host lost");
        assert_eq!(
            area.present(&CompanionModel::connecting()),
            Err(CompanionFailure::StatusAreaUnavailable)
        );
    }

    #[tokio::test]
    async fn a_watcher_that_leaves_and_does_not_return_loses_the_status_area() {
        let bus = PrivateBus::start();
        let (original, _) = watcher(&bus, true).await;
        let (mut area, ingress) = connect(&bus).await.unwrap();
        original.close().await.unwrap();
        lost(&mut area, &ingress).await;
    }

    #[tokio::test]
    async fn a_replacement_watcher_without_a_host_loses_the_status_area() {
        let bus = PrivateBus::start();
        let (original, _) = watcher(&bus, true).await;
        let (mut area, ingress) = connect(&bus).await.unwrap();
        original.close().await.unwrap();
        let (_replacement, items) = watcher(&bus, false).await;
        lost(&mut area, &ingress).await;
        // The companion registered with the replacement before it gave up on a host.
        assert_eq!(
            items.lock().unwrap().clone(),
            vec![identity().application_id()]
        );
    }

    #[tokio::test]
    async fn a_host_that_leaves_a_running_watcher_loses_the_status_area() {
        let bus = PrivateBus::start();
        let (original, _, host) = hosted_watcher(&bus, true).await;
        let (mut area, ingress) = connect(&bus).await.unwrap();
        host.store(false, Ordering::Release);
        let interface = original
            .object_server()
            .interface::<_, Watcher>("/StatusNotifierWatcher")
            .await
            .unwrap();
        Watcher::status_notifier_host_unregistered(interface.signal_emitter())
            .await
            .unwrap();
        lost(&mut area, &ingress).await;
    }

    #[tokio::test]
    async fn a_replacement_watcher_with_a_host_keeps_the_status_area() {
        let bus = PrivateBus::start();
        let (original, _) = watcher(&bus, true).await;
        let (mut area, ingress) = connect(&bus).await.unwrap();
        original.close().await.unwrap();
        let (_replacement, items) = watcher(&bus, true).await;
        let name = identity().application_id();
        for _ in 0..500 {
            if items.lock().unwrap().contains(&name) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(items.lock().unwrap().clone(), vec![name]);
        tokio::time::sleep(HOST_GRACE * 3).await;
        assert_eq!(area.next_action(), Ok(None));
        area.present(&CompanionModel::from_status(&running(1, "1.2.3")))
            .unwrap();
        assert_eq!(ingress.pop(), None);
    }

    const SESSION_PATH: &str = "/org/freedesktop/login1/session/_32";

    struct LoginManager {
        session_path: &'static str,
    }
    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl LoginManager {
        /// logind resolves "auto" to the caller's session, or to its user's display session
        /// when the caller runs outside one, as an application scope does.
        fn get_session(&self, session_id: String) -> zbus::fdo::Result<OwnedObjectPath> {
            if session_id != "auto" {
                return Err(zbus::fdo::Error::InvalidArgs("unknown session".to_owned()));
            }
            Ok(OwnedObjectPath::try_from(self.session_path).unwrap())
        }
    }

    #[derive(Clone, Default)]
    struct LoginSession {
        locked: Arc<AtomicBool>,
        idle: Arc<AtomicBool>,
        reads: Arc<AtomicUsize>,
        stalled: Arc<AtomicBool>,
    }
    #[zbus::interface(name = "org.freedesktop.login1.Session")]
    impl LoginSession {
        #[zbus(property)]
        async fn locked_hint(&self) -> bool {
            self.reads.fetch_add(1, Ordering::AcqRel);
            if self.stalled.load(Ordering::Acquire) {
                std::future::pending().await
            } else {
                self.locked.load(Ordering::Acquire)
            }
        }
        #[zbus(property)]
        async fn idle_hint(&self) -> bool {
            self.reads.fetch_add(1, Ordering::AcqRel);
            if self.stalled.load(Ordering::Acquire) {
                std::future::pending().await
            } else {
                self.idle.load(Ordering::Acquire)
            }
        }
    }

    /// A logind on the private bus whose one session can be locked or go idle, as GNOME
    /// Shell and gnome-session report them.
    struct Login {
        connection: zbus::Connection,
        session: LoginSession,
        session_path: &'static str,
    }
    impl Login {
        async fn start(bus: &PrivateBus) -> Self {
            Self::start_at(bus, SESSION_PATH).await
        }

        async fn start_at(bus: &PrivateBus, session_path: &'static str) -> Self {
            let session = LoginSession::default();
            let connection = zbus::connection::Builder::address(bus.address.as_str())
                .unwrap()
                .name("org.freedesktop.login1")
                .unwrap()
                .serve_at("/org/freedesktop/login1", LoginManager { session_path })
                .unwrap()
                .serve_at(session_path, session.clone())
                .unwrap()
                .build()
                .await
                .unwrap();
            Self {
                connection,
                session,
                session_path,
            }
        }

        /// Waits until the companion has read the session at least `count` times.
        async fn read(&self, count: usize) {
            for _ in 0..1_000 {
                if self.session.reads.load(Ordering::Acquire) >= count {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("the companion did not read the login session");
        }

        /// Changes the hints and emits logind's property signals. Generating those signals
        /// also reads the hints, so this does not prove the companion observed them.
        async fn set(&self, locked: bool, idle: bool) {
            let before = self.session.reads.load(Ordering::Acquire);
            self.session.locked.store(locked, Ordering::Release);
            self.session.idle.store(idle, Ordering::Release);
            let interface = self
                .connection
                .object_server()
                .interface::<_, LoginSession>(self.session_path)
                .await
                .unwrap();
            let emitter = interface.signal_emitter();
            let session = interface.get().await;
            session.locked_hint_changed(emitter).await.unwrap();
            session.idle_hint_changed(emitter).await.unwrap();
            drop(session);
            self.read(before + 2).await;
        }
    }

    /// Waits until the watcher has the companion's item registered.
    async fn registered(items: &Mutex<Vec<String>>) {
        let name = identity().application_id();
        for _ in 0..1_000 {
            if items.lock().unwrap().contains(&name) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the companion did not register its item");
    }

    /// GNOME Shell disables its StatusNotifierWatcher extension on the lock screen, and on
    /// the idle shield when locking is off, so the watcher leaves for as long as the user is
    /// away. The companion waits for the session to return instead of quitting.
    #[tokio::test]
    async fn a_watcher_that_leaves_while_the_session_is_away_keeps_the_status_area() {
        for (locked, idle) in [(true, false), (false, true)] {
            let bus = PrivateBus::start();
            let login = Login::start(&bus).await;
            let (original, _) = watcher(&bus, true).await;
            let (mut area, ingress) = connect(&bus).await.unwrap();
            login.read(2).await;

            // GNOME Shell disables the extension before it sets the hint.
            original.close().await.unwrap();
            login.set(locked, idle).await;
            tokio::time::sleep(HOST_GRACE * 3).await;
            assert_eq!(area.next_action(), Ok(None), "locked {locked}, idle {idle}");

            // On return the extension starts a new watcher before the hint clears.
            let (_replacement, items) = watcher(&bus, true).await;
            registered(&items).await;
            login.set(false, false).await;
            tokio::time::sleep(HOST_GRACE * 3).await;
            assert_eq!(area.next_action(), Ok(None));
            area.present(&CompanionModel::from_status(&running(1, "1.2.3")))
                .unwrap();
            assert_eq!(ingress.pop(), None);
        }
    }

    #[tokio::test]
    async fn a_replacement_watcher_retries_rejected_registration_while_the_session_is_away() {
        let bus = PrivateBus::start();
        let login = Login::start(&bus).await;
        let (original, _) = watcher(&bus, true).await;
        let (mut area, _) = connect(&bus).await.unwrap();
        login.read(2).await;
        login.set(true, false).await;
        original.close().await.unwrap();
        // Surviving without a watcher proves the companion observed the away hint.
        tokio::time::sleep(HOST_GRACE * 3).await;
        assert_eq!(area.next_action(), Ok(None));

        let (replacement, items, _) = watcher_with_rejection(&bus, true, true).await;
        let interface = replacement
            .object_server()
            .interface::<_, Watcher>("/StatusNotifierWatcher")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while interface.get().await.reject_once.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the replacement rejects the first registration");
        assert!(items.lock().unwrap().is_empty());
        Watcher::status_notifier_host_registered(interface.signal_emitter())
            .await
            .unwrap();
        tokio::time::sleep(HOST_GRACE * 3).await;
        assert_eq!(area.next_action(), Ok(None));
        registered(&items).await;

        login.set(false, false).await;
        tokio::time::sleep(HOST_GRACE * 3).await;
        assert_eq!(area.next_action(), Ok(None));
    }

    /// Returning to the session starts a fresh grace period, so a watcher that never comes
    /// back still loses the status area.
    #[tokio::test]
    async fn a_watcher_that_does_not_return_after_the_session_does_loses_the_status_area() {
        let bus = PrivateBus::start();
        let login = Login::start(&bus).await;
        let (original, _) = watcher(&bus, true).await;
        let (mut area, ingress) = connect(&bus).await.unwrap();
        login.read(2).await;
        original.close().await.unwrap();
        login.set(true, false).await;
        tokio::time::sleep(HOST_GRACE * 3).await;
        assert_eq!(area.next_action(), Ok(None));

        login.set(false, false).await;
        lost(&mut area, &ingress).await;
    }

    #[tokio::test]
    async fn a_logind_that_leaves_while_the_session_is_away_resumes_the_host_grace() {
        for close in [false, true] {
            let bus = PrivateBus::start();
            let login = Login::start(&bus).await;
            let (original, _) = watcher(&bus, true).await;
            let (mut area, ingress) = connect(&bus).await.unwrap();
            login.read(2).await;
            login.set(true, false).await;
            original.close().await.unwrap();
            tokio::time::sleep(HOST_GRACE * 3).await;
            assert_eq!(area.next_action(), Ok(None));

            if close {
                login.connection.close().await.unwrap();
            } else {
                assert!(
                    login
                        .connection
                        .release_name("org.freedesktop.login1")
                        .await
                        .unwrap()
                );
            }
            tokio::time::timeout(Duration::from_secs(3), lost(&mut area, &ingress))
                .await
                .expect("logind disappearing promptly resumes the host grace");
        }
    }

    #[tokio::test]
    async fn a_restarted_logind_tracks_away_hints_and_keeps_the_status_area() {
        for (locked, idle) in [(true, false), (false, true)] {
            let bus = PrivateBus::start();
            let login = Login::start(&bus).await;
            let (original, _) = watcher(&bus, true).await;
            let (mut area, ingress) = connect(&bus).await.unwrap();
            login.read(2).await;

            assert!(
                login
                    .connection
                    .release_name("org.freedesktop.login1")
                    .await
                    .unwrap()
            );
            // The old owner stays connected, but the new owner resolves another session.
            let replacement = Login::start_at(&bus, "/org/freedesktop/login1/session/_33").await;
            replacement.read(2).await;
            replacement.set(locked, idle).await;
            original.close().await.unwrap();
            tokio::time::sleep(HOST_GRACE * 3).await;
            assert_eq!(area.next_action(), Ok(None), "locked {locked}, idle {idle}");
            area.present(&CompanionModel::connecting()).unwrap();

            replacement.set(false, false).await;
            let (_watcher, items) = watcher(&bus, true).await;
            registered(&items).await;
            tokio::time::sleep(HOST_GRACE * 3).await;
            assert_eq!(area.next_action(), Ok(None));
            area.present(&CompanionModel::from_status(&running(1, "1.2.3")))
                .unwrap();
            assert_eq!(ingress.pop(), None);
        }
    }

    #[tokio::test]
    async fn a_logind_that_stops_answering_while_the_session_is_away_resumes_the_host_grace() {
        let bus = PrivateBus::start();
        let login = Login::start(&bus).await;
        let (original, _) = watcher(&bus, true).await;
        let (mut area, ingress) = connect(&bus).await.unwrap();
        login.read(2).await;
        login.set(false, true).await;
        original.close().await.unwrap();
        tokio::time::sleep(HOST_GRACE * 3).await;
        assert_eq!(area.next_action(), Ok(None));

        // Keep the name and connection alive, but never answer another hint read or signal.
        login.session.stalled.store(true, Ordering::Release);
        lost(&mut area, &ingress).await;
    }

    #[tokio::test]
    async fn missing_watcher_or_host_reports_an_unavailable_status_area() {
        let bus = PrivateBus::start();
        assert_eq!(
            connect(&bus).await.err(),
            Some(CompanionFailure::StatusAreaUnavailable)
        );
        let (_watcher, items) = watcher(&bus, false).await;
        assert_eq!(
            connect(&bus).await.err(),
            Some(CompanionFailure::StatusAreaUnavailable)
        );
        assert!(items.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_second_companion_in_the_session_is_refused() {
        let bus = PrivateBus::start();
        let (_watcher, items) = watcher(&bus, true).await;
        let (_first, _) = connect(&bus).await.unwrap();
        assert_eq!(
            connect(&bus).await.err(),
            Some(CompanionFailure::AlreadyRunning)
        );
        assert_eq!(items.lock().unwrap().len(), 1);
    }
}

#[path = "production_tests.rs"]
mod production;
