use std::sync::Condvar;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use super::credentials::CredentialError;
use super::worker::DirectNetwork;
use super::*;

#[derive(Default)]
struct MemoryRecords(Mutex<Document>);
impl Records for MemoryRecords {
    fn load(&self) -> Result<Document, ()> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, document: &Document) -> Result<(), ()> {
        *self.0.lock().unwrap() = document.clone();
        Ok(())
    }
}

#[derive(Default)]
struct Credentials(Mutex<BTreeMap<[u8; 32], Zeroizing<Vec<u8>>>>);
impl CredentialStore for Credentials {
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
}
impl DirectNetwork for Network {
    fn connect(
        &self,
        _: &str,
        route: &DirectRoute,
        _: DeviceCredential,
        _: Arc<AtomicBool>,
    ) -> Result<LocalServer, DirectError> {
        self.events.send(Event::Connect(route.id[0])).unwrap();
        if route.id[0] <= self.blocked_through {
            self.release.wait();
        }
        Err(DirectError::PinMismatch)
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
            server: actor.pin.server,
            operation,
            target,
        })
    }
    fn cancel_enrollment(
        &self,
        _: &str,
        _: &direct::ServerPin,
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
            endpoint: "wss://127.0.0.1:9231".into(),
            paired: PairedDevice {
                pin: direct::ServerPin {
                    build: "spaceterm-development".into(),
                    server: [id; 32],
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
    let network = Arc::new(Network {
        events,
        release: release.clone(),
        blocked_through,
    });
    let connections =
        DirectConnections::new("spaceterm-development", records, credentials, network);
    connections.resume_retained();
    (connections, received, ReleaseOnDrop(release))
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
