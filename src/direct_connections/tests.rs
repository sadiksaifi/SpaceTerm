use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::credentials::CredentialError;
use super::worker::DirectNetwork;
use super::*;

pub(crate) fn uncertain_join(label: &str) -> (DirectConnections, JoinId, DirectRouteId) {
    super::worker::enrollment_tests::uncertain_join(label)
}

pub(crate) fn unconfirmed_join(label: &str) -> (DirectConnections, JoinId) {
    super::worker::enrollment_tests::unconfirmed_join(label)
}

/// Records kept in memory. While `failing`, a save publishes nothing, as a full or locked
/// disk does.
#[derive(Default)]
struct MemoryRecords(Mutex<Document>, AtomicBool);
impl Records for MemoryRecords {
    fn load(&self) -> Result<Document, ()> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, document: &Document) -> Result<Durability, SaveError> {
        if self.1.load(Ordering::Acquire) {
            return Err(SaveError::NotPublished);
        }
        *self.0.lock().unwrap() = document.clone();
        Ok(Durability::Synchronized)
    }
}

#[derive(Default)]
pub(super) struct Credentials {
    pub(super) values: Mutex<BTreeMap<[u8; 32], Zeroizing<Vec<u8>>>>,
    reads: Mutex<Vec<Interaction>>,
    pub(super) locked: AtomicBool,
}
impl CredentialStore for Credentials {
    fn read(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
        self.reads.lock().unwrap().push(interaction);
        if self.locked.load(Ordering::Acquire) {
            return Err(CredentialError::InteractionRequired);
        }
        Ok(self.values.lock().unwrap().get(key).cloned())
    }
    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError> {
        let mut values = self.values.lock().unwrap();
        if values.contains_key(key) {
            return Err(CredentialError::Unavailable);
        }
        values.insert(*key, Zeroizing::new(secret.to_vec()));
        Ok(())
    }
    fn remove(&self, key: &[u8; 32], _: Interaction) -> Result<(), CredentialError> {
        self.values.lock().unwrap().remove(key);
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Call {
    endpoint: String,
    actor: PairedDevice,
    operation: UnpairKey,
    target: PairingId,
}
pub(super) enum Answer {
    Lost,
    Exact,
    WrongTarget,
    LoseOther,
}
pub(super) struct Network {
    answer: Answer,
    calls: Mutex<Vec<Call>>,
    pub(super) cancellations: Mutex<Vec<(String, direct::ServerPin, [u8; 32])>>,
    other_ready: AtomicBool,
}
impl Network {
    pub(super) fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            answer,
            calls: Mutex::default(),
            cancellations: Mutex::default(),
            other_ready: AtomicBool::new(false),
        })
    }
}
impl DirectNetwork for Network {
    fn cancel_enrollment(
        &self,
        endpoint: &str,
        pin: &direct::ServerPin,
        claim: [u8; 32],
        _: &DeviceCredential,
    ) -> Result<Option<direct::RevocationReceipt>, DirectError> {
        self.cancellations
            .lock()
            .unwrap()
            .push((endpoint.to_owned(), pin.clone(), claim));
        Ok(None)
    }
    fn connect(
        &self,
        _: &str,
        _: &DirectRoute,
        _: DeviceCredential,
        _: Arc<AtomicBool>,
    ) -> Result<LocalServer, DirectError> {
        panic!("inactive or removed routes must not connect")
    }
    fn revoke(
        &self,
        endpoint: &str,
        actor: &PairedDevice,
        _: &DeviceCredential,
        operation: UnpairKey,
        target: PairingId,
    ) -> Result<direct::RevocationReceipt, DirectError> {
        self.calls.lock().unwrap().push(Call {
            endpoint: endpoint.into(),
            actor: actor.clone(),
            operation,
            target,
        });
        match self.answer {
            Answer::Lost => Err(DirectError::Transport),
            Answer::LoseOther
                if target == [44; 32] && !self.other_ready.load(Ordering::Acquire) =>
            {
                Err(DirectError::Transport)
            }
            Answer::Exact | Answer::WrongTarget | Answer::LoseOther => {
                Ok(direct::RevocationReceipt {
                    server: actor.pin.server,
                    operation,
                    target: if matches!(self.answer, Answer::WrongTarget) {
                        [99; 32]
                    } else {
                        target
                    },
                })
            }
        }
    }
}

pub(super) fn saved(id: u8, key: u8) -> SavedRoute {
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
        credential: [key; 32],
        active: false,
    }
}

fn fixture(
    routes: Vec<SavedRoute>,
    answer: Answer,
) -> (
    DirectConnections,
    Arc<MemoryRecords>,
    Arc<Credentials>,
    Arc<Network>,
) {
    let records = Arc::new(MemoryRecords::default());
    records.0.lock().unwrap().routes = routes.clone();
    let credentials = Arc::new(Credentials::default());
    let secret = DeviceCredential::generate().unwrap();
    for route in routes {
        credentials.values.lock().unwrap().insert(
            route.credential,
            Zeroizing::new(secret.expose_bytes().to_vec()),
        );
    }
    let network = Network::new(answer);
    let connections = DirectConnections::new(
        "spaceterm-development",
        records.clone(),
        credentials.clone(),
        network.clone(),
    );
    connections.resume_retained();
    (connections, records, credentials, network)
}

/// A real retained registry with unavailable network I/O for UI journey tests.
pub(crate) fn offline(label: &str) -> (DirectConnections, DirectRouteId) {
    let (connections, id, _) = offline_with_lock(label);
    (connections, id)
}

pub(crate) struct CredentialLock(Arc<Credentials>);

impl CredentialLock {
    pub(crate) fn set_locked(&self, locked: bool) {
        self.0.locked.store(locked, Ordering::Release);
    }
}

/// Whether the offline fixture's records can be saved.
pub(crate) struct RecordsStorage(Arc<MemoryRecords>);

impl RecordsStorage {
    pub(crate) fn set_failing(&self, failing: bool) {
        self.0.1.store(failing, Ordering::Release);
    }
}

/// The offline fixture with control over whether its records save.
pub(crate) fn offline_with_storage(
    label: &str,
) -> (DirectConnections, DirectRouteId, RecordsStorage) {
    let (connections, id, _, records) = offline_fixture(label);
    (connections, id, RecordsStorage(records))
}

/// The offline fixture with control over its own in-memory credential boundary.
pub(crate) fn offline_with_lock(label: &str) -> (DirectConnections, DirectRouteId, CredentialLock) {
    let (connections, id, lock, _) = offline_fixture(label);
    (connections, id, lock)
}

fn offline_fixture(
    label: &str,
) -> (
    DirectConnections,
    DirectRouteId,
    CredentialLock,
    Arc<MemoryRecords>,
) {
    struct OfflineNetwork;
    impl DirectNetwork for OfflineNetwork {
        fn connect(
            &self,
            _: &str,
            _: &DirectRoute,
            _: DeviceCredential,
            _: Arc<AtomicBool>,
        ) -> Result<LocalServer, DirectError> {
            Err(DirectError::Transport)
        }
        fn revoke(
            &self,
            _: &str,
            _: &PairedDevice,
            _: &DeviceCredential,
            _: UnpairKey,
            _: PairingId,
        ) -> Result<direct::RevocationReceipt, DirectError> {
            Err(DirectError::Transport)
        }
        fn cancel_enrollment(
            &self,
            _: &str,
            _: &direct::ServerPin,
            _: [u8; 32],
            _: &DeviceCredential,
        ) -> Result<Option<direct::RevocationReceipt>, DirectError> {
            Err(DirectError::Transport)
        }
    }
    let mut route = saved(1, 5);
    route.route.label = label.to_owned();
    route.active = true;
    let id = route.route.id;
    let records = Arc::new(MemoryRecords::default());
    records.0.lock().unwrap().routes.push(route);
    let credentials = Arc::new(Credentials::default());
    credentials
        .insert(
            &[5; 32],
            DeviceCredential::generate().unwrap().expose_bytes(),
        )
        .unwrap();
    let connections = DirectConnections::new(
        "spaceterm-development",
        records.clone(),
        credentials.clone(),
        Arc::new(OfflineNetwork),
    );
    connections.resume_retained();
    (connections, id, CredentialLock(credentials), records)
}

fn await_condition(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "direct connection worker did not reach the expected outcome"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn offline_self_unpair_removes_only_its_route_and_keeps_exact_recovery_after_relaunch() {
    let (connections, records, credentials, network) =
        fixture(vec![saved(1, 5), saved(2, 6)], Answer::Lost);
    connections.unpair_self(&[1; 32]).unwrap();
    assert!(connections.route(&[1; 32]).is_none());
    assert!(connections.server(&[1; 32]).is_none());
    assert!(connections.route(&[2; 32]).is_some());
    await_condition(|| !network.calls.lock().unwrap().is_empty());
    let original = network.calls.lock().unwrap()[0].clone();
    let pending = connections.pending_unpairs()[0].key;
    connections.dismiss_unpair_notice(pending);
    assert!(connections.pending_unpairs()[0].dismissed);
    drop(connections);
    let recovered_network = Network::new(Answer::Exact);
    let reopened = DirectConnections::new(
        "spaceterm-development",
        records,
        credentials.clone(),
        recovered_network.clone(),
    );
    reopened.resume_retained();
    await_condition(|| reopened.pending_unpairs().is_empty());
    assert!(reopened.route(&[1; 32]).is_none());
    assert!(reopened.route(&[2; 32]).is_some());
    assert_eq!(
        recovered_network.calls.lock().unwrap().as_slice(),
        &[original]
    );
    assert!(!credentials.values.lock().unwrap().contains_key(&[5; 32]));
    assert!(credentials.values.lock().unwrap().contains_key(&[6; 32]));
    assert_eq!(
        credentials.reads.lock().unwrap().last(),
        Some(&Interaction::Quiet)
    );
}

#[test]
fn an_unrelated_revocation_receipt_never_completes_removal_or_retires_credentials() {
    let (connections, _, credentials, network) = fixture(vec![saved(1, 5)], Answer::WrongTarget);
    connections.unpair_self(&[1; 32]).unwrap();
    await_condition(|| {
        connections
            .pending_unpairs()
            .first()
            .is_some_and(|p| p.state == UnpairState::Unconfirmed)
    });
    assert!(!network.calls.lock().unwrap().is_empty());
    assert!(credentials.values.lock().unwrap().contains_key(&[5; 32]));
    assert!(connections.route(&[1; 32]).is_none());
}

#[test]
fn unpairing_another_device_keeps_the_initiating_route_and_native_key() {
    let (connections, _, credentials, _) = fixture(vec![saved(1, 5)], Answer::Exact);
    connections.unpair(&[1; 32], [44; 32]).unwrap();
    await_condition(|| connections.pending_unpairs().is_empty());
    assert!(connections.route(&[1; 32]).is_some());
    assert!(credentials.values.lock().unwrap().contains_key(&[5; 32]));
}

#[test]
fn self_unpair_preserves_a_key_shared_by_a_newer_saved_pairing() {
    let (connections, _, credentials, _) = fixture(vec![saved(1, 5), saved(2, 5)], Answer::Exact);
    connections.unpair_self(&[1; 32]).unwrap();
    await_condition(|| connections.pending_unpairs().is_empty());
    assert!(credentials.values.lock().unwrap().contains_key(&[5; 32]));
    assert!(connections.route(&[2; 32]).is_some());
}

#[test]
fn locked_native_storage_pauses_quiet_recovery_until_explicit_resume() {
    let (connections, records, credentials, _) = fixture(vec![saved(1, 5)], Answer::Lost);
    credentials.locked.store(true, Ordering::Release);
    connections.unpair_self(&[1; 32]).unwrap();
    await_condition(|| connections.pending_unpairs()[0].state == UnpairState::NeedsInteraction);
    drop(connections);
    let network = Network::new(Answer::Exact);
    let reopened = DirectConnections::new(
        "spaceterm-development",
        records,
        credentials.clone(),
        network.clone(),
    );
    reopened.resume_retained();
    await_condition(|| reopened.pending_unpairs()[0].state == UnpairState::NeedsInteraction);
    assert!(network.calls.lock().unwrap().is_empty());
    assert_eq!(
        credentials.reads.lock().unwrap().last(),
        Some(&Interaction::Quiet)
    );
    credentials.locked.store(false, Ordering::Release);
    reopened.resume_unpair(reopened.pending_unpairs()[0].key);
    await_condition(|| reopened.pending_unpairs().is_empty());
    assert_eq!(
        credentials.reads.lock().unwrap().last(),
        Some(&Interaction::Requested)
    );
}

#[test]
fn interrupted_enrollment_resolves_only_its_retained_claim_without_reenrolling_or_connecting() {
    let (connections, records, credentials, _) =
        fixture(vec![saved(1, 5), saved(2, 6)], Answer::Lost);
    drop(connections);
    let pin = saved(1, 5).route.paired.pin;
    {
        let mut document = records.0.lock().unwrap();
        document.routes.retain(|r| r.route.id != [1; 32]);
        document.pending.push(PendingRecord::enrollment(
            [7; 32],
            "Interrupted server".into(),
            "wss://127.0.0.1:9231".into(),
            pin.clone(),
            [8; 32],
            [5; 32],
            false,
        ));
    }
    let network = Network::new(Answer::Exact);
    let reopened = DirectConnections::new(
        "spaceterm-development",
        records,
        credentials.clone(),
        network.clone(),
    );
    reopened.resume_retained();
    await_condition(|| reopened.pending_unpairs().is_empty());
    assert_eq!(
        network.cancellations.lock().unwrap().as_slice(),
        &[("wss://127.0.0.1:9231".into(), pin, [8; 32])]
    );
    assert!(network.calls.lock().unwrap().is_empty());
    assert!(reopened.route(&[1; 32]).is_none());
    assert!(reopened.route(&[2; 32]).is_some());
    assert!(!credentials.values.lock().unwrap().contains_key(&[5; 32]));
    assert!(credentials.values.lock().unwrap().contains_key(&[6; 32]));
    assert_eq!(
        credentials.reads.lock().unwrap().last(),
        Some(&Interaction::Quiet)
    );
}

#[test]
fn confirmed_self_unpair_retires_its_key_after_another_original_finishes_using_it() {
    let (connections, _, credentials, network) = fixture(vec![saved(1, 5)], Answer::LoseOther);
    connections.unpair(&[1; 32], [44; 32]).unwrap();
    let other = connections.pending_unpairs()[0].key;
    connections.unpair_self(&[1; 32]).unwrap();
    await_condition(|| {
        connections
            .pending_unpairs()
            .iter()
            .any(|p| p.self_unpair && p.server_confirmed)
    });
    assert!(credentials.values.lock().unwrap().contains_key(&[5; 32]));
    network.other_ready.store(true, Ordering::Release);
    connections.resume_unpair(other);
    await_condition(|| connections.pending_unpairs().is_empty());
    assert!(!credentials.values.lock().unwrap().contains_key(&[5; 32]));
}

#[test]
fn disconnect_then_connect_ignores_the_previous_attempt_and_never_reopens_its_gate() {
    struct BlockingNetwork {
        started: std::sync::mpsc::Sender<Arc<AtomicBool>>,
        finish: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl DirectNetwork for BlockingNetwork {
        fn connect(
            &self,
            _: &str,
            _: &DirectRoute,
            _: DeviceCredential,
            gate: Arc<AtomicBool>,
        ) -> Result<LocalServer, DirectError> {
            self.started.send(gate).unwrap();
            let _ = self
                .finish
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
            Err(DirectError::PinMismatch)
        }
        fn revoke(
            &self,
            _: &str,
            _: &PairedDevice,
            _: &DeviceCredential,
            _: UnpairKey,
            _: PairingId,
        ) -> Result<direct::RevocationReceipt, DirectError> {
            panic!("unexpected revocation")
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
    let records = Arc::new(MemoryRecords::default());
    records.0.lock().unwrap().routes.push(saved(1, 5));
    let credentials = Arc::new(Credentials::default());
    credentials
        .insert(
            &[5; 32],
            DeviceCredential::generate().unwrap().expose_bytes(),
        )
        .unwrap();
    let (started, attempts) = std::sync::mpsc::channel();
    let (finish, completions) = std::sync::mpsc::channel();
    let network = Arc::new(BlockingNetwork {
        started,
        finish: Mutex::new(completions),
    });
    let connections =
        DirectConnections::new("spaceterm-development", records, credentials, network);
    connections.resume_retained();
    connections.connect(&[1; 32]);
    let old_gate = attempts.recv_timeout(Duration::from_secs(5)).unwrap();
    connections.disconnect(&[1; 32]);
    connections.connect(&[1; 32]);
    assert!(
        !old_gate.load(Ordering::Acquire),
        "a cancelled connector must stay disabled"
    );
    finish.send(()).unwrap();
    let new_gate = attempts.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!Arc::ptr_eq(&old_gate, &new_gate));
    assert_eq!(
        connections.status(&[1; 32]),
        Some(DirectRouteStatus::Connecting)
    );
    connections.disconnect(&[1; 32]);
    finish.send(()).unwrap();
}

#[test]
fn two_confirmed_self_unpairs_sharing_a_key_finish_without_waiting_on_each_other() {
    let records = Arc::new(MemoryRecords::default());
    for (operation, route) in [([7; 32], saved(1, 5)), ([8; 32], saved(2, 5))] {
        let target = route.route.paired.pairing;
        let mut pending = PendingRecord::direct(operation, route, target);
        pending.confirmed = true;
        records.0.lock().unwrap().pending.push(pending);
    }
    let credentials = Arc::new(Credentials::default());
    credentials
        .insert(
            &[5; 32],
            DeviceCredential::generate().unwrap().expose_bytes(),
        )
        .unwrap();
    let network = Network::new(Answer::Exact);
    let connections = DirectConnections::new(
        "spaceterm-development",
        records,
        credentials.clone(),
        network.clone(),
    );
    connections.resume_retained();
    await_condition(|| connections.pending_unpairs().is_empty());
    assert!(!credentials.values.lock().unwrap().contains_key(&[5; 32]));
    assert!(network.calls.lock().unwrap().is_empty());
}

#[test]
fn full_recovery_capacity_refuses_new_operations_but_still_removes_its_own_route() {
    let route = saved(1, 5);
    let records = Arc::new(MemoryRecords::default());
    {
        let mut document = records.0.lock().unwrap();
        document.routes.push(route.clone());
        for operation in 1..128 {
            document.pending.push(PendingRecord::direct(
                [operation; 32],
                route.clone(),
                [44; 32],
            ));
        }
    }
    let credentials = Arc::new(Credentials::default());
    credentials
        .insert(
            &[5; 32],
            DeviceCredential::generate().unwrap().expose_bytes(),
        )
        .unwrap();
    let network = Network::new(Answer::LoseOther);
    let connections = DirectConnections::new(
        "spaceterm-development",
        records,
        credentials.clone(),
        network.clone(),
    );
    connections.resume_retained();
    assert_eq!(
        connections.unpair(&[1; 32], [45; 32]),
        Err(DirectError::Capacity)
    );
    assert_eq!(connections.pending_unpairs().len(), 127);
    connections.unpair_self(&[1; 32]).unwrap();
    assert!(connections.route(&[1; 32]).is_none());
    network.other_ready.store(true, Ordering::Release);
    for pending in connections.pending_unpairs() {
        connections.resume_unpair(pending.key);
    }
    await_condition(|| connections.pending_unpairs().len() < 128);
    assert!(!connections.persistence_error());
}
