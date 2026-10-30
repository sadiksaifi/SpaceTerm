use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::credentials::CredentialError;
use super::worker::DirectNetwork;
use super::*;

pub fn uncertain_join(label: &str) -> (DirectConnections, JoinId, DirectRouteId) {
    super::worker::enrollment_tests::uncertain_join(label)
}

pub fn unconfirmed_join(label: &str) -> (DirectConnections, JoinId) {
    super::worker::enrollment_tests::unconfirmed_join(label)
}

pub fn retryable_join(label: &str, stage: EnrollmentStage) -> (DirectConnections, JoinId) {
    super::worker::enrollment_tests::retryable_join(label, stage)
}

pub fn lost_claim_invitation(label: &str) -> (DirectConnections, Zeroizing<String>) {
    super::worker::enrollment_tests::lost_claim_invitation(label)
}

/// An unjoined registry for the real Add Server journey. The network boundary returns
/// a fixed exchange comparison ("000007") and waits for the person's cancellation.
pub fn waiting_invitation(
    label: &str,
) -> (DirectConnections, Zeroizing<String>, direct::ComparisonCode) {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    let pin = saved(1, 5).route.paired.pin;
    let invitation = random_id().unwrap();
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 300;
    let mut exchange_hash = [7; 32];
    exchange_hash[..4].copy_from_slice(&[0, 0, 0, 7]);
    let comparison_code = direct::PairingClaim {
        id: [9; 32],
        invitation_id: invitation,
        server: pin.server,
        device_public_key: [3; 32],
        device_name: "Joining device".into(),
        expires_at_unix_seconds: expires,
        exchange_hash,
    }
    .comparison_code();
    let payload = Zeroizing::new(
        serde_json::to_vec(&serde_json::json!({
            "schema": 1, "id": invitation, "endpoint": "quic://127.0.0.1:9231",
            "pin": pin, "server_name": label, "token": random_id().unwrap(),
            "expires": expires, "valid_for_seconds": 300,
        }))
        .unwrap(),
    );
    let text = Zeroizing::new(format!(
        "spaceterm://pair/{}",
        URL_SAFE_NO_PAD.encode(&payload)
    ));
    let owner = DirectConnections::new(
        "spaceterm-development",
        Arc::new(MemoryRecords::default()),
        Arc::new(Credentials::default()),
        Arc::new(WaitingNetwork {
            comparison_code,
            cleanup: Network::new(Answer::Exact),
        }),
    );
    (owner, text, comparison_code)
}

struct WaitingNetwork {
    comparison_code: direct::ComparisonCode,
    cleanup: Arc<Network>,
}

impl DirectNetwork for WaitingNetwork {
    fn submit_enrollment(
        &self,
        _: &mut direct::Enrollment,
    ) -> Result<direct::ComparisonCode, DirectError> {
        Ok(self.comparison_code)
    }
    fn finish_enrollment(
        &self,
        _: &mut direct::Enrollment,
    ) -> Result<Option<PairedDevice>, DirectError> {
        Ok(None)
    }
    fn cancel_enrollment(
        &self,
        endpoint: &str,
        pin: &direct::ServerPin,
        claim: [u8; 32],
        credential: &DeviceCredential,
    ) -> Result<Option<direct::RevocationReceipt>, DirectError> {
        self.cleanup
            .cancel_enrollment(endpoint, pin, claim, credential)
    }
    fn connect(
        &self,
        identity: &str,
        route: &DirectRoute,
        credential: DeviceCredential,
        gate: Arc<AtomicBool>,
    ) -> Result<ServerConnection, DirectError> {
        self.cleanup.connect(identity, route, credential, gate)
    }
    fn revoke(
        &self,
        endpoint: &str,
        actor: &PairedDevice,
        credential: &DeviceCredential,
        operation: UnpairKey,
        target: PairingId,
    ) -> Result<direct::RevocationReceipt, DirectError> {
        self.cleanup
            .revoke(endpoint, actor, credential, operation, target)
    }
}

/// Records kept in memory. While `failing`, a save publishes nothing, as a full or locked
/// disk does.
#[derive(Default)]
pub(super) struct MemoryRecords(Mutex<Document>, AtomicBool);
impl Records for MemoryRecords {
    fn load(&self) -> Result<Document, spaceterm_server::direct::DirectError> {
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
    fn kind(&self) -> crate::StoreKind {
        crate::StoreKind::Keychain
    }
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
    ) -> Result<ServerConnection, DirectError> {
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

pub fn saved(id: u8, key: u8) -> SavedRoute {
    SavedRoute {
        route: DirectRoute {
            id: [id; 32],
            label: format!("Server {id}"),
            endpoint: "quic://127.0.0.1:9231".into(),
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

#[test]
fn an_empty_saved_inventory_is_distinct_from_one_that_could_not_be_read() {
    struct Inventory(Option<Document>);
    impl Records for Inventory {
        fn load(&self) -> Result<Document, spaceterm_server::direct::DirectError> {
            self.0.clone().ok_or(DirectError::StorageUnavailable)
        }
        fn save(&self, _: &Document) -> Result<Durability, SaveError> {
            panic!("inventory observation must not write records")
        }
    }

    for (document, ready) in [(None, false), (Some(Document::default()), true)] {
        let connections = DirectConnections::new(
            "spaceterm-development",
            Arc::new(Inventory(document)),
            Arc::new(Credentials::default()),
            Network::new(Answer::Lost),
        );
        assert!(!connections.retained_inventory_loaded());
        connections.resume_retained();
        assert!(connections.routes().is_empty());
        assert_eq!(connections.retained_inventory_loaded(), ready);
    }
}

/// A real retained registry with unavailable network I/O for UI journey tests.
pub fn offline(label: &str) -> (DirectConnections, DirectRouteId) {
    let (connections, id, _) = offline_with_lock(label);
    (connections, id)
}

pub struct CredentialLock(Arc<Credentials>);

impl CredentialLock {
    pub fn set_locked(&self, locked: bool) {
        self.0.locked.store(locked, Ordering::Release);
    }
}

/// Whether the offline fixture's records can be saved.
pub struct RecordsStorage(Arc<MemoryRecords>);

impl RecordsStorage {
    pub fn set_failing(&self, failing: bool) {
        self.0.1.store(failing, Ordering::Release);
    }
}

/// The offline fixture with control over whether its records save.
pub fn offline_with_storage(label: &str) -> (DirectConnections, DirectRouteId, RecordsStorage) {
    let (connections, id, _, records) = offline_fixture(label);
    (connections, id, RecordsStorage(records))
}

/// The offline fixture with control over its own in-memory credential boundary.
pub fn offline_with_lock(label: &str) -> (DirectConnections, DirectRouteId, CredentialLock) {
    let (connections, id, lock, _) = offline_fixture(label);
    (connections, id, lock)
}

/// Connected registry entries for UI tests that supply their own protocol writers.
/// Seeds retained routes and handles without starting native maintenance workers, because the
/// scripted writers answer protocol operations and the handles cannot reach a native server.
/// Registry mutations still use DirectConnections.
pub fn connected_routes(labels: &[&str]) -> (DirectConnections, Vec<DirectRouteId>) {
    connected_fixture(labels, None)
}

/// Connected saved routes that all reach the server `server`, as an earlier pairing and a newer
/// one to the same computer do.
pub fn connected_routes_to_one_server(
    labels: &[&str],
    server: [u8; 32],
) -> (DirectConnections, Vec<DirectRouteId>) {
    connected_fixture(labels, Some(server))
}

fn connected_fixture(
    labels: &[&str],
    server: Option<[u8; 32]>,
) -> (DirectConnections, Vec<DirectRouteId>) {
    struct ConnectedNetwork;
    impl DirectNetwork for ConnectedNetwork {
        fn connect(
            &self,
            identity: &str,
            route: &DirectRoute,
            _: DeviceCredential,
            _: Arc<AtomicBool>,
        ) -> Result<ServerConnection, DirectError> {
            struct Unreachable;
            impl direct::DirectConnector for Unreachable {
                fn connect(&self) -> Result<direct::DirectConnection, DirectError> {
                    Err(DirectError::Transport)
                }
            }
            ServerConnection::direct(
                identity,
                route.id,
                route.paired.clone(),
                Arc::new(Unreachable),
            )
            .map_err(DirectError::from)
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
    let records = Arc::new(MemoryRecords::default());
    let credentials = Arc::new(Credentials::default());
    let connections = DirectConnections::new(
        "spaceterm-development",
        records.clone(),
        credentials.clone(),
        Arc::new(ConnectedNetwork),
    );
    let mut state = connections.0.state.lock().unwrap();
    let mut ids = Vec::new();
    for (index, label) in (0_u8..).zip(labels) {
        let mut saved = saved(1 + index, 5 + index);
        saved.route.label = (*label).to_owned();
        saved.active = true;
        if let Some(server) = server {
            saved.route.paired.pin.server = server;
        }
        credentials
            .insert(
                &saved.credential,
                DeviceCredential::generate().unwrap().expose_bytes(),
            )
            .unwrap();
        let gate = Arc::new(AtomicBool::new(true));
        let server = worker::connect(&connections.0, &saved, Interaction::Quiet, gate.clone())
            .expect("the scripted route connects without network I/O");
        ids.push(saved.route.id);
        state.routes.insert(
            saved.route.id,
            RouteState {
                status: DirectRouteStatus::Connected,
                server: Some(server),
                gate,
                retry: Retry::new(Interaction::Quiet),
                health: None,
            },
        );
        state.document.routes.push(saved);
    }
    *records.0.lock().unwrap() = state.document.clone();
    state.loaded = true;
    state.inventory_loaded = true;
    drop(state);
    (connections, ids)
}

/// Disconnected saved routes for picker tests, with no connection intent that could start a
/// native route job and wake GPUI's deterministic scheduler.
pub fn disconnected_routes_at(routes: &[(&str, &str)]) -> (DirectConnections, Vec<DirectRouteId>) {
    let routes: Vec<_> = routes
        .iter()
        .map(|(label, endpoint)| (*label, Some(*endpoint)))
        .collect();
    let (connections, ids, _, _) = offline_fixture_reached(&routes, false);
    (connections, ids)
}

fn offline_fixture(
    label: &str,
) -> (
    DirectConnections,
    DirectRouteId,
    CredentialLock,
    Arc<MemoryRecords>,
) {
    let (connections, ids, lock, records) = offline_fixture_routes(&[label]);
    (connections, ids[0], lock, records)
}

fn offline_fixture_routes(
    labels: &[&str],
) -> (
    DirectConnections,
    Vec<DirectRouteId>,
    CredentialLock,
    Arc<MemoryRecords>,
) {
    let routes: Vec<_> = labels.iter().map(|label| (*label, None)).collect();
    offline_fixture_reached(&routes, true)
}

/// Offline saved routes named as given, each at its own endpoint when one is given.
fn offline_fixture_reached(
    routes: &[(&str, Option<&str>)],
    active: bool,
) -> (
    DirectConnections,
    Vec<DirectRouteId>,
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
        ) -> Result<ServerConnection, DirectError> {
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
    retained_fixture_routes(routes, active, None, Arc::new(OfflineNetwork))
}

fn retained_fixture_routes(
    routes: &[(&str, Option<&str>)],
    active: bool,
    server: Option<[u8; 32]>,
    network: Arc<dyn DirectNetwork>,
) -> (
    DirectConnections,
    Vec<DirectRouteId>,
    CredentialLock,
    Arc<MemoryRecords>,
) {
    let records = Arc::new(MemoryRecords::default());
    let credentials = Arc::new(Credentials::default());
    let mut ids = Vec::new();
    for (index, (label, endpoint)) in (0_u8..).zip(routes) {
        let mut route = saved(1 + index, 5 + index);
        route.route.label = (*label).to_owned();
        if let Some(endpoint) = endpoint {
            route.route.endpoint = (*endpoint).to_owned();
        }
        route.active = active;
        if let Some(server) = server {
            route.route.paired.pin.server = server;
        }
        ids.push(route.route.id);
        records.0.lock().unwrap().routes.push(route);
        credentials
            .insert(
                &[5 + index; 32],
                DeviceCredential::generate().unwrap().expose_bytes(),
            )
            .unwrap();
    }
    let connections = DirectConnections::new(
        "spaceterm-development",
        records.clone(),
        credentials.clone(),
        network,
    );
    connections.resume_retained();
    (connections, ids, CredentialLock(credentials), records)
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
fn relaunch_lets_only_the_first_attempt_of_an_active_route_ask_for_its_key() {
    let (_connections, _, lock, _) = offline_fixture_reached(&[("Studio", None)], true);
    let reads = || lock.0.reads.lock().unwrap().clone();
    await_condition(|| reads().len() >= 2);
    assert_eq!(
        reads()[..2],
        [Interaction::Requested, Interaction::Quiet],
        "a store that no longer trusts this build asks at launch; the offline retry stays quiet"
    );
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
            .is_some_and(|p| p.state == UnpairProgress::Unconfirmed)
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
    await_condition(|| connections.pending_unpairs()[0].state == UnpairProgress::NeedsInteraction);
    drop(connections);
    let network = Network::new(Answer::Exact);
    let reopened = DirectConnections::new(
        "spaceterm-development",
        records,
        credentials.clone(),
        network.clone(),
    );
    reopened.resume_retained();
    await_condition(|| reopened.pending_unpairs()[0].state == UnpairProgress::NeedsInteraction);
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
            "quic://127.0.0.1:9231".into(),
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
        &[("quic://127.0.0.1:9231".into(), pin, [8; 32])]
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
fn a_disconnected_route_stops_following_its_old_link() {
    let (connections, ids) = connected_routes(&["Studio"]);
    let id = ids[0];
    let health = worker::follow_link(&connections.0, id);
    connections
        .0
        .state
        .lock()
        .unwrap()
        .routes
        .get_mut(&id)
        .unwrap()
        .health = Some(health);
    connections.disconnect(&id);
    assert!(
        connections.0.state.lock().unwrap().routes[&id]
            .health
            .is_none(),
        "the next connection's link must be followed instead"
    );
    connections.connect(&id);
    // Once the route connects again, the worker follows the new link and checks the route only
    // when that link ends.
    await_condition(|| {
        let state = connections.0.state.lock().unwrap();
        let route = &state.routes[&id];
        route.status == DirectRouteStatus::Connected && route.health.is_some()
    });
}

/// Reconnect on a connected route drops its link and connects again, rather than doing
/// nothing because a link is live.
#[test]
fn reconnecting_a_connected_route_replaces_its_link() {
    let (connections, ids) = connected_routes(&["Studio"]);
    let id = ids[0];
    let (old_gate, old_server) = {
        let state = connections.0.state.lock().unwrap();
        let route = &state.routes[&id];
        assert_eq!(route.status, DirectRouteStatus::Connected);
        (Arc::clone(&route.gate), route.server.clone().unwrap())
    };
    connections.connect(&id);
    assert!(
        !old_gate.load(Ordering::Acquire),
        "the old link's connector is disabled"
    );
    await_condition(|| {
        let state = connections.0.state.lock().unwrap();
        let route = &state.routes[&id];
        route.status == DirectRouteStatus::Connected
            && !Arc::ptr_eq(&route.gate, &old_gate)
            && route
                .server
                .as_ref()
                .is_some_and(|server| !server.same_handle(&old_server))
    });
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
        ) -> Result<ServerConnection, DirectError> {
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

/// Removing a route runs its cleanup once the removal is saved, so what the client retained for
/// it goes with it. Disconnect keeps the route and what it retained.
#[test]
fn removing_a_route_should_discard_what_it_retained() {
    let (connections, routes) = connected_routes_to_one_server(&["penguin"], [0x5E; 32]);
    let removed = Arc::new(Mutex::new(Vec::new()));
    connections.on_route_removed(Arc::new({
        let removed = Arc::clone(&removed);
        move |route: &DirectRouteId| removed.lock().unwrap().push(*route)
    }));

    connections.disconnect(&routes[0]);
    let after_disconnect = removed.lock().unwrap().clone();
    connections.unpair_self(&routes[0]).unwrap();

    assert_eq!(after_disconnect, Vec::<DirectRouteId>::new());
    assert_eq!(*removed.lock().unwrap(), vec![routes[0]]);
    assert!(connections.route(&routes[0]).is_none());
}

#[derive(Default)]
pub(crate) struct CancellationToken(std::sync::atomic::AtomicBool);
impl CancellationToken {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}
impl crate::setup::Cancellation for CancellationToken {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// File-boundary fixture: a publication failure grants no later write authority.
#[derive(Default)]
pub(crate) struct RecordingFile {
    pub files: Arc<Mutex<FileState>>,
    blocked: std::sync::atomic::AtomicBool,
}
#[derive(Default)]
pub(crate) struct FileState {
    bytes: Option<Vec<u8>>,
    pub commit_outcome: Option<Durability>,
    pub commit_error: Option<()>,
    pub prepare_error: Option<()>,
}
impl RecordingFile {
    pub fn reopen(&self) -> Self {
        Self {
            files: self.files.clone(),
            blocked: Default::default(),
        }
    }
}
impl records::RecordFile for RecordingFile {
    fn read(
        &self,
        accept: &mut dyn FnMut(&[u8]) -> Result<(), spaceterm_server::direct::DirectError>,
    ) -> Result<(), spaceterm_server::direct::DirectError> {
        self.blocked.store(true, Ordering::Release);
        if let Some(bytes) = &self.files.lock().unwrap().bytes {
            accept(bytes)?;
        }
        self.blocked.store(false, Ordering::Release);
        Ok(())
    }
    fn commit(&self, bytes: &[u8]) -> Result<Durability, SaveError> {
        let mut files = self.files.lock().unwrap();
        if self.blocked.load(Ordering::Acquire) {
            return Err(SaveError::PublicationIndeterminate);
        }
        if files.prepare_error.is_some() {
            return Err(SaveError::NotPublished);
        }
        if files.commit_error.is_some() {
            self.blocked.store(true, Ordering::Release);
            return Err(SaveError::PublicationIndeterminate);
        }
        files.bytes = Some(bytes.to_vec());
        Ok(files.commit_outcome.unwrap_or(Durability::Synchronized))
    }
}
