use std::sync::Condvar;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use super::credentials::CredentialError;
use super::worker::DirectNetwork;
use super::*;

#[derive(Default)]
struct MemoryRecords(Mutex<Document>);
impl Records for MemoryRecords {
    fn load(&self) -> Result<Document, spaceterm_core::direct::DirectError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, document: &Document) -> Result<Durability, SaveError> {
        *self.0.lock().unwrap() = document.clone();
        Ok(Durability::Synchronized)
    }
}

#[derive(Default)]
struct Credentials(Mutex<BTreeMap<[u8; 32], Zeroizing<Vec<u8>>>>);
impl CredentialStore for Credentials {
    fn kind(&self) -> crate::StoreKind {
        crate::StoreKind::Keychain
    }
    fn read(
        &self,
        key: &[u8; 32],
        _: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError> {
        self.0
            .lock()
            .unwrap()
            .insert(*key, Zeroizing::new(secret.to_vec()));
        Ok(())
    }
    fn remove(&self, key: &[u8; 32], _: Interaction) -> Result<(), CredentialError> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Event {
    Connect(u8),
    Revoke,
}

#[derive(Default)]
struct Release {
    open: Mutex<bool>,
    changed: Condvar,
}
impl Release {
    fn unblock(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
    fn wait(&self) {
        let (open, timeout) = self
            .changed
            .wait_timeout_while(self.open.lock().unwrap(), Duration::from_secs(10), |open| {
                !*open
            })
            .unwrap();
        assert!(
            *open && !timeout.timed_out(),
            "test must release every blocked route"
        );
    }
}
struct ReleaseOnDrop(Arc<Release>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.unblock();
    }
}

struct Network {
    events: Sender<Event>,
    release: Arc<Release>,
    blocked_through: u8,
    /// How every connection attempt fails while `admits` is false.
    failure: DirectError,
    /// Whether the Core admits this client's work protocol, as an updated Core does.
    admits: Arc<AtomicBool>,
}
impl DirectNetwork for Network {
    fn connect(
        &self,
        identity: &str,
        route: &DirectRoute,
        _: DeviceCredential,
        _: Arc<AtomicBool>,
    ) -> Result<MachineConnection, DirectError> {
        self.events.send(Event::Connect(route.id[0])).unwrap();
        if route.id[0] <= self.blocked_through {
            self.release.wait();
        }
        if self.admits.load(Ordering::Acquire) {
            return MachineConnection::direct(
                identity,
                route.id,
                route.paired.clone(),
                Arc::new(NoConnection),
            )
            .map_err(DirectError::from);
        }
        Err(self.failure)
    }
    fn revoke(
        &self,
        _: &str,
        actor: &PairedDevice,
        _: &DeviceCredential,
        operation: UnpairKey,
        target: PairingId,
    ) -> Result<direct::RevocationReceipt, DirectError> {
        self.events.send(Event::Revoke).unwrap();
        Ok(direct::RevocationReceipt {
            core: actor.pin.core,
            operation,
            target,
        })
    }
    fn cancel_enrollment(
        &self,
        _: &str,
        _: &direct::MachinePin,
        _: [u8; 32],
        _: &DeviceCredential,
    ) -> Result<Option<direct::RevocationReceipt>, DirectError> {
        panic!("unexpected enrollment")
    }
}

fn saved(id: u8) -> SavedRoute {
    SavedRoute {
        route: DirectRoute {
            id: [id; 32],
            label: format!("Server {id}"),
            endpoint: "quic://127.0.0.1:9231".into(),
            paired: PairedDevice {
                pin: direct::MachinePin {
                    build: "spaceterm-development".into(),
                    core: [id; 32],
                    public_key: [3; 32],
                    certificate: [4; 32],
                },
                pairing: [id + 10; 32],
                client: [id + 20; 32],
            },
        },
        credential: [id; 32],
        active: true,
    }
}

fn fixture(
    route_count: u8,
    blocked_through: u8,
    recovery: bool,
) -> (DirectConnections, Receiver<Event>, ReleaseOnDrop) {
    failing_fixture(
        route_count,
        blocked_through,
        recovery,
        DirectError::PinMismatch,
    )
}

fn failing_fixture(
    route_count: u8,
    blocked_through: u8,
    recovery: bool,
    failure: DirectError,
) -> (DirectConnections, Receiver<Event>, ReleaseOnDrop) {
    let (connections, events, release, _) =
        admitting_fixture(route_count, blocked_through, recovery, failure);
    (connections, events, release)
}

/// A fixture whose routes fail with `failure` until the returned flag is set, and are then
/// admitted.
fn admitting_fixture(
    route_count: u8,
    blocked_through: u8,
    recovery: bool,
    failure: DirectError,
) -> (
    DirectConnections,
    Receiver<Event>,
    ReleaseOnDrop,
    Arc<AtomicBool>,
) {
    let records = Arc::new(MemoryRecords::default());
    let credentials = Arc::new(Credentials::default());
    let secret = DeviceCredential::generate().unwrap();
    let mut document = records.0.lock().unwrap();
    document.routes = (1..=route_count).map(saved).collect();
    if recovery {
        let actor = saved(20);
        let target = actor.route.paired.pairing;
        document
            .pending
            .push(PendingRecord::direct([90; 32], actor, target));
        credentials
            .insert(&[20; 32], secret.expose_bytes())
            .unwrap();
    }
    for route in &document.routes {
        credentials
            .insert(&route.credential, secret.expose_bytes())
            .unwrap();
    }
    drop(document);
    let (events, received) = mpsc::channel();
    let release = Arc::new(Release::default());
    let admits = Arc::new(AtomicBool::new(false));
    let network = Arc::new(Network {
        events,
        release: release.clone(),
        blocked_through,
        failure,
        admits: admits.clone(),
    });
    let connections =
        DirectConnections::new("spaceterm-development", records, credentials, network, None);
    connections.resume_retained();
    (connections, received, ReleaseOnDrop(release), admits)
}

#[test]
fn direct_scheduler_attempts_other_routes_while_one_host_is_blocked() {
    let (_connections, events, _release) = fixture(2, 1, false);
    let mut connected = Vec::new();
    for _ in 0..2 {
        match events.recv_timeout(Duration::from_secs(3)).unwrap() {
            Event::Connect(id) => connected.push(id),
            Event::Revoke => panic!("unexpected revocation"),
        }
    }
    connected.sort();
    assert_eq!(connected, vec![1, 2]);
}

#[test]
fn direct_scheduler_bounds_route_work_and_reserves_revocation_capacity() {
    let (_connections, events, release) = fixture(5, 5, true);
    let mut connected = Vec::new();
    let mut revoked = false;
    for _ in 0..5 {
        match events.recv_timeout(Duration::from_secs(3)).unwrap() {
            Event::Connect(id) => connected.push(id),
            Event::Revoke => revoked = true,
        }
    }
    connected.sort();
    assert_eq!(connected, vec![1, 2, 3, 4]);
    assert!(revoked);
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(350)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release.0.unblock();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(3)).unwrap(),
        Event::Connect(5)
    );
}

/// Waits until no attempt runs for `route`. Retrying a refused route changes no status, so its
/// status can't tell when the attempt ends.
fn settle(connections: &DirectConnections, route: &DirectRouteId) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while connections
        .0
        .state
        .lock()
        .unwrap()
        .route_jobs
        .contains(route)
        || connections.status(route) == Some(DirectRouteStatus::Connecting)
    {
        assert!(std::time::Instant::now() < deadline, "the attempt settled");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn retrying_a_route_now_forgets_its_backoff() {
    let (connections, events, _release) = failing_fixture(1, 0, false, DirectError::Transport);
    let route = [1; 32];
    let connected = |within: Duration| matches!(events.recv_timeout(within), Ok(Event::Connect(1)));
    assert!(connected(Duration::from_secs(15)));
    settle(&connections, &route);
    assert_eq!(
        connections.status(&route),
        Some(DirectRouteStatus::Unavailable(UnavailableReason::Network))
    );
    // The route has earned the longest backoff: its next attempt waits 60 seconds, and its next
    // failure would wait 60 more. Each bound below is far shorter, so load can't decide them.
    {
        let mut state = connections.0.state.lock().unwrap();
        let retry = &mut state.routes.get_mut(&route).unwrap().retry;
        retry.attempt = 5;
        retry.due = std::time::Instant::now() + Duration::from_secs(60);
    }
    // Retrying now skips that wait.
    connections.retry_now(&route);
    assert!(connected(Duration::from_secs(15)));
    // That attempt counts as the first again, so its failure waits 2 seconds, not 60.
    assert!(connected(Duration::from_secs(15)));
}

#[test]
fn retrying_now_leaves_a_disconnected_or_refused_route_alone() {
    for (failure, disconnect) in [
        (DirectError::Transport, true),
        (DirectError::Revoked, false),
        (DirectError::PinMismatch, false),
        (DirectError::AuthenticationRefused, false),
    ] {
        let (connections, events, _release) = failing_fixture(1, 0, false, failure);
        let route = [1; 32];
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(3)),
            Ok(Event::Connect(1))
        ));
        if disconnect {
            connections.disconnect(&route);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while connections.status(&route) == Some(DirectRouteStatus::Connecting) {
            assert!(std::time::Instant::now() < deadline, "the attempt settled");
            std::thread::sleep(Duration::from_millis(5));
        }
        connections.retry_now(&route);
        assert!(matches!(
            events.recv_timeout(Duration::from_millis(500)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
}

/// A Core that refused this desktop's work protocol can be updated through its service lane.
/// The expected restart's retry then checks the saved route once more and admits it once the
/// Core speaks a compatible work protocol.
#[test]
fn retrying_now_checks_an_incompatible_route_again_until_it_is_admitted() {
    let (connections, events, _release, admits) =
        admitting_fixture(1, 0, false, DirectError::IncompatibleProtocol);
    let route = [1; 32];
    let attempted = |within: Duration| matches!(events.recv_timeout(within), Ok(Event::Connect(1)));
    let refused = Some(DirectRouteStatus::Refused(RefusalReason::Incompatible));
    assert!(attempted(Duration::from_secs(15)));
    settle(&connections, &route);
    assert_eq!(connections.status(&route), refused);

    // A Core still on the incompatible version refuses again, and the route stays refused
    // without further attempts.
    connections.retry_now(&route);
    assert!(attempted(Duration::from_secs(15)));
    settle(&connections, &route);
    assert_eq!(connections.status(&route), refused);
    assert!(connections.core(&route).is_none());
    assert!(!attempted(Duration::from_millis(2500)));

    admits.store(true, Ordering::Release);
    connections.retry_now(&route);
    assert!(attempted(Duration::from_secs(15)));
    settle(&connections, &route);
    assert_eq!(
        connections.status(&route),
        Some(DirectRouteStatus::Connected)
    );
    assert!(connections.core(&route).is_some());
}

#[test]
fn direct_subscription_close_waits_for_an_inflight_wake_and_prevents_later_notifications() {
    use std::future::Future;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context, Poll, Wake, Waker};

    struct InFlightWake {
        entered: Sender<()>,
        release: Arc<Release>,
        calls: AtomicUsize,
    }
    impl Wake for InFlightWake {
        fn wake(self: Arc<Self>) {
            self.calls.fetch_add(1, Ordering::AcqRel);
            self.entered.send(()).unwrap();
            self.release.wait();
        }
    }

    let (connections, _events, _network_release) = fixture(0, 0, false);
    let receiver = connections.subscribe();
    let release = Arc::new(Release::default());
    let _release_on_failure = ReleaseOnDrop(release.clone());
    let (entered, waking) = mpsc::channel();
    let wake = Arc::new(InFlightWake {
        entered,
        release: release.clone(),
        calls: AtomicUsize::new(0),
    });
    let waker = Waker::from(wake.clone());
    let mut context = Context::from_waker(&waker);
    let mut receive = Box::pin(receiver.recv());
    assert!(matches!(receive.as_mut().poll(&mut context), Poll::Pending));

    let notifier = connections.clone();
    let notification = std::thread::spawn(move || notifier.0.notify());
    waking.recv_timeout(Duration::from_secs(3)).unwrap();
    let closer = connections.clone();
    let closing_receiver = receiver.clone();
    let (started, closing) = mpsc::channel();
    let (completed, closed) = mpsc::channel();
    let close = std::thread::spawn(move || {
        started.send(()).unwrap();
        closer.close_subscription(&closing_receiver);
        completed.send(()).unwrap();
    });
    closing.recv_timeout(Duration::from_secs(3)).unwrap();
    let waits_for_wake = matches!(
        closed.recv_timeout(Duration::from_millis(150)),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    release.unblock();
    notification.join().unwrap();
    close.join().unwrap();
    assert!(
        waits_for_wake,
        "closing a view must fence a notification that already owns its task waker"
    );

    drop(receive);
    assert!(receiver.is_closed());
    assert_eq!(receiver.try_recv(), Ok(()));
    connections.0.notify();
    assert_eq!(
        receiver.try_recv(),
        Err(async_channel::TryRecvError::Closed)
    );
    assert_eq!(wake.calls.load(Ordering::Acquire), 1);
}
