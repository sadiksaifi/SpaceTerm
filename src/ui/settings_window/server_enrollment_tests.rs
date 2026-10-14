use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use gpui::{App, Task, TestAppContext, VisualTestContext, accesskit};
use spaceterm_client::{
    api::{RouteDescription, RouteParts},
    failure::{ClientFailure, ClientFailureClass},
    journal::{
        ClientJournalChange, JournalController, JournalCredentialSlot, JournalLimits,
        ReceiptVerifier,
    },
    pairing::{credentials::*, enrollment::*, journal::*},
    testing::{DeterministicKey, MemoryJournal},
};
use spaceterm_core::{
    authority::{ServerEpoch, StoreRevision},
    identity::*,
    operation_ledger::canonical::CanonicalCodec,
    operations::*,
    pairing::*,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ServerPin, SigningKey},
    pairing_adapter::{
        EnrollmentLookup, EnrollmentServer, ReceiptCodec, operation_to_wire, receipt_to_wire,
        reservation_to_wire,
    },
    wire::pairing,
};
use spaceterm_ui::a11y_testing::{A11yTree, perform, perform_with};
use zeroize::Zeroizing;

use super::super::{
    SettingsSectionId,
    tests::{click, open_settings_with_registry, sample_registry, select_section},
};
use super::*;

struct CryptoVerifier;
impl ReceiptVerifier for CryptoVerifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))
    }
}
// Shared UI tests inject storage. The native-client package suite owns SQLite durability.
// The canonical controller, transaction validator, receipt cryptography and protocol stay real.
pub(super) struct TestStore {
    pub(super) journal: Arc<JournalController>,
    pub(super) runtime: Arc<tokio::runtime::Runtime>,
    pub(super) storage: Arc<MemoryJournal>,
}
impl TestStore {
    fn new() -> Self {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
        );
        let storage = Arc::new(MemoryJournal::default());
        let journal = runtime
            .block_on(JournalController::recover(
                storage.clone(),
                Arc::new(CanonicalCodec),
                Arc::new(CryptoVerifier),
                JournalLimits::default(),
            ))
            .unwrap();
        Self {
            journal: Arc::new(journal),
            runtime,
            storage,
        }
    }
    pub(super) fn store(&self) -> JournalPairingStore<JournalController> {
        JournalPairingStore::new(self.journal.clone())
    }
    fn reopened_controller(&self) -> Arc<JournalController> {
        Arc::new(
            self.runtime
                .block_on(JournalController::recover(
                    self.storage.clone(),
                    Arc::new(CanonicalCodec),
                    Arc::new(CryptoVerifier),
                    JournalLimits::default(),
                ))
                .unwrap(),
        )
    }
    fn reopened(&self) -> JournalPairingStore<JournalController> {
        JournalPairingStore::new(self.reopened_controller())
    }
}
pub(super) struct Fixture {
    pub(super) prepared: PreparedEnrollment,
    pub(super) device: PreparedCredential,
    pub(super) credentials: Keys,
    pub(super) saved: SavedPairing,
    pub(super) reservation: PublicEnrollmentReservation,
    pub(super) authorization: RefCell<Option<EnrollmentAuthorization>>,
    pub(super) endpoint: Endpoint,
    pub(super) storage: TestStore,
}
impl Fixture {
    pub(super) fn new(major: u32, lose_reply: bool) -> Self {
        Self::new_with_route(
            major,
            lose_reply,
            RouteDescription::new(RouteParts::Wss {
                address: "127.0.0.1:443".parse().unwrap(),
                server_name: "enrollment.invalid".into(),
            })
            .unwrap(),
        )
    }
    pub(super) fn new_with_route(major: u32, lose_reply: bool, route: RouteDescription) -> Self {
        let storage = TestStore::new();
        let server_key = Arc::new(DeterministicKey::new(1).unwrap());
        let device_key = Arc::new(DeterministicKey::new(3).unwrap());
        let auth_key = Arc::new(DeterministicKey::new(2).unwrap());
        let server = ServerId::new([1; 16]);
        let pairing = PairingId::new(server, 1).unwrap();
        let client = ClientId::new(server, 1).unwrap();
        let stream = |number| {
            OperationStreamId::new(
                OperationStreamKey::new(server, number).unwrap(),
                pairing,
                client,
            )
            .unwrap()
        };
        let enrollment = OperationBinding {
            id: OperationId::new(stream(3), OperationSequence::new(1).unwrap()),
            digest: RequestDigest::from_bytes([5; 32]),
        };
        let pin = ServerPin {
            identity: server.bytes(),
            public_key: server_key.public_key(),
        };
        let scope = |purpose, reference| {
            CredentialScope::new(CredentialScopeParts {
                namespace: CredentialNamespace::new("dev.spaceterm.enrollment-test".into())
                    .unwrap(),
                purpose,
                reference: CredentialReference::new([reference; 16]),
            })
            .unwrap()
        };
        let credential = PreparedCredential {
            scope: scope(CredentialPurpose::PairingAuthentication { server }, 2),
            public_key: auth_key.public_key(),
        };
        let device = PreparedCredential {
            scope: scope(CredentialPurpose::DeviceIdentity, 3),
            public_key: device_key.public_key(),
        };
        let keys = PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key,
            pairing_authentication_public_key: credential.public_key,
        })
        .unwrap();
        let target = PairingTarget::new(PairingTargetParts {
            server,
            pairing,
            device: DeviceId::new([8; 16]),
        })
        .unwrap();
        let authorization = EnrollmentAuthorization::generate().unwrap();
        let reservation = EnrollmentReservation::new(EnrollmentReservationParts {
            ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
                server,
                nonce: [3; 16],
            })
            .unwrap(),
            target: target.clone(),
            keys: keys.clone(),
            initial_client: client,
            enrollment_stream: enrollment.id.stream(),
            claim_digest: RequestDigest::from_bytes([4; 32]),
            authorization_digest: authorization.digest(),
            enrollment_request_digest: enrollment.digest,
            issuer: EnrollmentIssuer::AccountOwner {
                native_request: [8; 16],
                native_request_digest: RequestDigest::from_bytes([9; 32]),
            },
            issued_epoch: ServerEpoch::new(1),
            issued_revision: StoreRevision::new(1),
            issued_at_millis: 0,
            expires_at_millis: 1,
            state: EnrollmentReservationState::Reserved,
        })
        .unwrap();
        let public =
            PublicEnrollmentReservation::from_wire(&reservation_to_wire(&reservation)).unwrap();
        let prepared = PreparedEnrollment {
            local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
                server,
                pairing,
                generation: LocalPairingGeneration::new(1).unwrap(),
            })
            .unwrap(),
            device: target.parts().device,
            pin,
            keys: keys.clone(),
            client,
            credential,
            route: RevocationRouteReference::new([6; 16]),
            enrollment,
            reservation: Some(public.clone()),
        };
        let receipt = ReceiptCodec::new(pin)
            .sign(
                PairingReceiptBody::Enrollment {
                    target,
                    keys,
                    client: PairingClientStreams::new(PairingClientStreamsParts {
                        client: ClientRecord {
                            id: client,
                            pairing,
                        },
                        ordinary: stream(4),
                        self_revocation: stream(5),
                    })
                    .unwrap(),
                    enrollment,
                    revision: StoreRevision::new(7),
                },
                server_key.as_ref(),
            )
            .unwrap();
        let saved = verify_binding(&prepared, receipt.clone(), false).unwrap();
        storage.runtime.block_on(async {
            let mut changes = Vec::new();
            for (generation, credential) in [(1, &prepared.credential), (2, &device)] {
                changes.push(ClientJournalChange::ReserveCredential(
                    JournalCredentialSlot {
                        namespace: credential.scope.parts().namespace.as_str().to_owned(),
                        server: match credential.scope.parts().purpose {
                            CredentialPurpose::PairingAuthentication { server } => Some(server),
                            _ => None,
                        },
                        reference: credential.scope.reference(),
                        public_key: None,
                        route_reference: None,
                        route: None,
                        ssh_capture: None,
                        generation: LocalPairingGeneration::new(generation).unwrap(),
                    },
                ));
                changes.push(ClientJournalChange::RecordCredentialPublicKey {
                    reference: credential.scope.reference(),
                    public_key: credential.public_key,
                });
            }
            changes.push(ClientJournalChange::BindCredentialRoute {
                reference: prepared.credential.scope.reference(),
                route_reference: prepared.route,
                route,
            });
            storage.journal.transact(changes).await.unwrap();
        });
        let endpoint = Endpoint {
            server: server_key,
            reservation: Arc::new(Mutex::new(reservation)),
            receipt,
            major,
            submissions: Arc::new(AtomicU64::new(0)),
            lookups: Arc::new(AtomicU64::new(0)),
            lose_reply: Arc::new(AtomicBool::new(lose_reply)),
        };
        Self {
            prepared,
            device,
            credentials: Keys {
                device: device_key,
                auth: auth_key,
            },
            saved,
            reservation: public,
            authorization: RefCell::new(Some(authorization)),
            endpoint,
            storage,
        }
    }
    fn candidate(&self, attempt: EnrollmentAttempt) -> DirectCandidate {
        DirectCandidate::new(
            attempt,
            self.prepared.clone(),
            self.device.clone(),
            self.reservation.clone(),
            self.authorization.borrow_mut().take(),
            vec![self.endpoint.major],
        )
        .unwrap()
    }
    fn enroll(&self, approval: DirectApproval) -> Result<DurableEnrollment, EnrollmentFailure> {
        let driver = EnrollmentDriver::new(
            self.storage.store(),
            self.credentials.clone(),
            self.endpoint.clone(),
        );
        self.storage
            .runtime
            .block_on(approval.enroll(&driver, &self.storage.store()))
    }
}
#[derive(Clone)]
pub(super) struct Keys {
    device: Arc<DeterministicKey>,
    auth: Arc<DeterministicKey>,
}
impl NativeDeviceCredentials for Keys {
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            let public_key = match scope.parts().purpose {
                CredentialPurpose::DeviceIdentity => self.device.public_key(),
                CredentialPurpose::PairingAuthentication { .. } => self.auth.public_key(),
            };
            Ok(Some(PreparedCredential {
                scope: scope.clone(),
                public_key,
            }))
        })
    }
    fn create(
        &self,
        _: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        panic!("enrollment cannot replace prepared keys")
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        lease: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            lease.validate(&credential.scope)?;
            Ok(match credential.scope.parts().purpose {
                CredentialPurpose::DeviceIdentity => self.device.clone(),
                _ => self.auth.clone(),
            } as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        panic!("enrollment cannot delete keys")
    }
}
#[derive(Clone)]
pub(super) struct Endpoint {
    pub(super) server: Arc<DeterministicKey>,
    pub(super) reservation: Arc<Mutex<EnrollmentReservation>>,
    pub(super) receipt: SignedPairingReceipt,
    pub(super) major: u32,
    pub(super) submissions: Arc<AtomicU64>,
    pub(super) lookups: Arc<AtomicU64>,
    pub(super) lose_reply: Arc<AtomicBool>,
}
impl EnrollmentLookup for Endpoint {
    fn lookup(&self, ticket: &EnrollmentTicketId) -> Result<EnrollmentReservation, ProtocolError> {
        let reservation = self.reservation.lock().unwrap();
        if &reservation.parts().ticket != ticket {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(reservation.clone())
    }
}
impl EnrollmentConnector for Endpoint {
    fn open<'a>(
        &'a self,
        pin: ServerPin,
        route: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async move {
            assert_eq!(pin.public_key, self.server.public_key());
            assert_eq!(route, RevocationRouteReference::new([6; 16]));
            Ok(Box::new(Transport {
                endpoint: self.clone(),
                handshake: None,
                connection: None,
                replies: VecDeque::new(),
            }) as Box<dyn PairingTransport>)
        })
    }
}
struct Transport {
    endpoint: Endpoint,
    handshake: Option<EnrollmentServer>,
    connection: Option<spaceterm_protocol::pairing_adapter::EnrollmentConnection>,
    replies: VecDeque<Vec<u8>>,
}
impl PairingTransport for Transport {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            if let Some(connection) = &mut self.connection {
                let reserved = matches!(
                    self.endpoint.reservation.lock().unwrap().parts().state,
                    EnrollmentReservationState::Reserved
                );
                if reserved {
                    let submission = connection.receive_submission(bytes).unwrap();
                    let mut parts = self.endpoint.reservation.lock().unwrap().parts().clone();
                    assert_eq!(
                        submission.authorization_digest(),
                        parts.authorization_digest
                    );
                    self.endpoint.submissions.fetch_add(1, Ordering::Relaxed);
                    parts.state = EnrollmentReservationState::Consumed {
                        enrollment: OperationBinding {
                            id: OperationId::new(
                                parts.enrollment_stream,
                                OperationSequence::new(1).unwrap(),
                            ),
                            digest: parts.enrollment_request_digest,
                        },
                        receipt: Box::new(self.endpoint.receipt.clone()),
                    };
                    *self.endpoint.reservation.lock().unwrap() =
                        EnrollmentReservation::new(parts).unwrap();
                    if self.endpoint.lose_reply.swap(false, Ordering::AcqRel) {
                        return Ok(());
                    }
                } else {
                    let message = connection.decode(bytes).unwrap();
                    match message.kind {
                        Some(pairing::enrollment_message::Kind::Acknowledgement(_)) => {
                            return Ok(());
                        }
                        Some(pairing::enrollment_message::Kind::Lookup(_)) => {
                            self.endpoint.lookups.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => panic!("no new authorization submission after consumption"),
                    }
                }
                let parts = self.endpoint.reservation.lock().unwrap().parts().clone();
                let message = pairing::EnrollmentMessage {
                    kind: Some(pairing::enrollment_message::Kind::Result(
                        pairing::EnrollmentResult {
                            operation: Some(operation_to_wire(OperationId::new(
                                parts.enrollment_stream,
                                OperationSequence::new(1).unwrap(),
                            ))),
                            request_digest: parts.enrollment_request_digest.bytes().to_vec(),
                            receipt: Some(receipt_to_wire(&self.endpoint.receipt)),
                            failure: None,
                        },
                    )),
                };
                self.replies.push_back(connection.encode(&message).unwrap());
            } else if let Some(handshake) = self.handshake.take() {
                let (connection, reply) = handshake.receive_proof(&self.endpoint, bytes).unwrap();
                self.connection = Some(connection);
                self.replies.push_back(reply);
            } else {
                let (handshake, reply) = EnrollmentServer::accept(
                    [1; 16],
                    vec![self.endpoint.major],
                    self.endpoint.server.clone(),
                    &self.endpoint,
                    bytes,
                )
                .unwrap();
                self.handshake = Some(handshake);
                self.replies.push_back(reply);
            }
            Ok(())
        })
    }
    fn receive(&mut self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, PairingFailure>> {
        Box::pin(async {
            self.replies
                .pop_front()
                .map(Zeroizing::new)
                .ok_or(PairingFailure::Offline)
        })
    }
}

struct Provider {
    fixture: Fixture,
    inspections: Cell<usize>,
    claim_exports: Cell<usize>,
    publications: RefCell<Vec<SavedPairing>>,
    authorizations: RefCell<Vec<bool>>,
    ssh_facts: RefCell<Option<BootstrapHostFacts>>,
    starts: Cell<usize>,
    bootstraps: Cell<usize>,
    hold_inspection: RefCell<Option<async_channel::Sender<EnrollmentDiscovery>>>,
    hold: Cell<bool>,
    resolutions: Cell<usize>,
    native_pending: RefCell<Option<BootstrapRequest>>,
    stale_completion: Cell<bool>,
    hold_enrollment: Cell<bool>,
    held_approval: RefCell<Option<DirectApproval>>,
}
impl Provider {
    fn new(major: u32, lose_reply: bool) -> Rc<Self> {
        Rc::new(Self {
            fixture: Fixture::new(major, lose_reply),
            inspections: Cell::new(0),
            claim_exports: Cell::new(0),
            publications: RefCell::new(Vec::new()),
            authorizations: RefCell::new(Vec::new()),
            ssh_facts: RefCell::new(None),
            starts: Cell::new(0),
            bootstraps: Cell::new(0),
            hold_inspection: RefCell::new(None),
            hold: Cell::new(false),
            resolutions: Cell::new(0),
            native_pending: RefCell::new(None),
            stale_completion: Cell::new(false),
            hold_enrollment: Cell::new(false),
            held_approval: RefCell::new(None),
        })
    }
}
impl EnrollmentProvider for Provider {
    fn prepare_claim(
        &self,
        address: String,
        descriptor: Zeroizing<String>,
        compared_fingerprint: String,
        attempt: EnrollmentAttempt,
        _: &mut App,
    ) -> Task<Result<String, EnrollmentFailure>> {
        if let Err(error) = attempt.validate() {
            return Task::ready(Err(error));
        }
        if address != "wss://enrollment.invalid"
            || descriptor.as_str() != "public-server-identity"
            || compared_fingerprint != fingerprint(self.fixture.prepared.pin.public_key)
        {
            return Task::ready(Err(PairingFailure::FingerprintMismatch.into()));
        }
        self.claim_exports.set(self.claim_exports.get() + 1);
        // Native key custody and proof verification are covered by the public
        // native-provider suite. This boundary supplies public clipboard bytes.
        Task::ready(Ok("verified-public-device-claim".into()))
    }
    fn inspect(
        &self,
        target: EnrollmentTarget,
        attempt: EnrollmentAttempt,
        cx: &mut App,
    ) -> Task<Result<EnrollmentDiscovery, EnrollmentFailure>> {
        self.inspections.set(self.inspections.get() + 1);
        let result = match target {
            EnrollmentTarget::Direct {
                address,
                invitation,
            } => {
                assert_eq!(address, "wss://enrollment.invalid");
                assert!(
                    invitation.is_empty() || invitation.as_str() == "single-use-secret-invitation"
                );
                Ok(EnrollmentDiscovery::Direct(Box::new(
                    self.fixture.candidate(attempt),
                )))
            }
            EnrollmentTarget::Ssh { host } => {
                assert_eq!(host, "build-box");
                SshCandidate::new(attempt, self.ssh_facts.borrow().unwrap())
                    .and_then(|candidate| {
                        if let Some(request) = *self.native_pending.borrow() {
                            candidate.retain_request(request)
                        } else {
                            Ok(candidate)
                        }
                    })
                    .map(|candidate| EnrollmentDiscovery::Ssh(Box::new(candidate)))
            }
        };
        if self.hold.get() {
            let (send, receive) = async_channel::bounded(1);
            *self.hold_inspection.borrow_mut() = Some(send);
            // Holding the real discovery result tests observer cancellation without keeping the UI alive.
            return cx.spawn(async move |_| {
                let _held = result;
                receive
                    .recv()
                    .await
                    .map_err(|_| PairingFailure::Cancelled.into())
            });
        }
        Task::ready(result)
    }
    fn enroll_direct(
        &self,
        approval: DirectApproval,
        cx: &mut App,
    ) -> Task<Result<DurableEnrollment, EnrollmentFailure>> {
        self.authorizations
            .borrow_mut()
            .push(approval.authorization.is_some());
        if self.hold_enrollment.get() {
            *self.held_approval.borrow_mut() = Some(approval);
            return cx.spawn(async move |_| std::future::pending().await);
        }
        let result = self.fixture.enroll(approval).map(|mut enrollment| {
            if self.stale_completion.get() {
                enrollment.attempt = EnrollmentAttempt::new(1);
            }
            enrollment
        });
        Task::ready(result)
    }
    fn bootstrap(
        &self,
        _: DeliberateBootstrap,
        _: SshCandidate,
        _: &mut App,
    ) -> Task<Result<DurableEnrollment, EnrollmentFailure>> {
        self.bootstraps.set(self.bootstraps.get() + 1);
        let request = BootstrapRequest::new([7; 16], [6; 32]).unwrap();
        *self.native_pending.borrow_mut() = Some(request);
        Task::ready(Err(EnrollmentFailure::BootstrapUnresolved(request)))
    }
    fn resolve_bootstrap(
        &self,
        candidate: SshCandidate,
        request: BootstrapRequest,
        _: &mut App,
    ) -> Task<Result<DurableEnrollment, EnrollmentFailure>> {
        assert_eq!(candidate.attempt().generation(), 1);
        candidate.attempt().validate().unwrap();
        assert_eq!(candidate.facts().route.account_uid(), 1000);
        assert_eq!(Some(request), *self.native_pending.borrow());
        assert_eq!(request.request(), [7; 16]);
        assert_eq!(request.artifact(), [6; 32]);
        self.resolutions.set(self.resolutions.get() + 1);
        Task::ready(Err(EnrollmentFailure::BootstrapUnresolved(request)))
    }
    fn start(&self, candidate: SshCandidate, _: &mut App) -> Task<Result<(), EnrollmentFailure>> {
        assert!(matches!(
            candidate.facts.registration,
            BootstrapRegistration::Stopped { .. }
        ));
        candidate.attempt.validate().unwrap();
        self.starts.set(self.starts.get() + 1);
        Task::ready(Ok(()))
    }
    fn publish(&self, enrollment: DurableEnrollment, _: &mut App) -> Result<(), EnrollmentFailure> {
        assert_eq!(enrollment.saved(), &self.fixture.saved);
        self.publications
            .borrow_mut()
            .push(enrollment.saved().clone());
        Ok(())
    }
}
fn write_field(label: &str, value: &str, cx: &mut VisualTestContext) {
    let tree = A11yTree::read(cx);
    perform_with(
        cx,
        tree.node(label),
        accesskit::Action::SetValue,
        Some(accesskit::ActionData::Value(value.into())),
    );
    cx.run_until_parked();
}
fn open(provider: Rc<Provider>, cx: &mut TestAppContext) -> &mut VisualTestContext {
    cx.update(|cx| install(provider, cx));
    let (_, _, cx) = open_settings_with_registry(cx, sample_registry());
    select_section(SettingsSectionId::Servers, cx);
    let tree = A11yTree::read(cx);
    perform(cx, tree.node("Add Server…"), accesskit::Action::Focus);
    cx.run_until_parked();
    click("settings-server-add", cx);
    cx
}
fn direct_ready(cx: &mut VisualTestContext) {
    write_field("Server address or SSH host", "wss://enrollment.invalid", cx);
    write_field(
        "Single-use pairing invitation",
        "single-use-secret-invitation",
        cx,
    );
    click("settings-server-enrollment-check", cx);
}
fn facts(registration: BootstrapRegistration) -> BootstrapHostFacts {
    use spaceterm_client::pairing::bootstrap::*;
    let route =
        BootstrapRoute::capture([1; 32], [2; 32], [3; 32], 1000, BuildChannel::Release).unwrap();
    spaceterm_client::testing::bootstrap_host_facts(route, registration)
}

#[gpui::test]
fn add_server_is_a_foreground_sheet_with_no_automatic_enrollment(cx: &mut TestAppContext) {
    let provider = Provider::new(2, false);
    let cx = open(provider.clone(), cx);
    assert!(cx.debug_bounds("settings-server-enrollment").is_some());
    assert!(
        cx.debug_bounds("settings-server-enrollment-address")
            .is_some()
    );
    assert_eq!(provider.inspections.get(), 0);
    assert!(provider.publications.borrow().is_empty());
    let tree = A11yTree::read(cx);
    assert_eq!(
        tree.focused().unwrap()["aria"]["label"],
        "Server address or SSH host"
    );
    assert_eq!(
        tree.node("Single-use pairing invitation")["aria"]["role"],
        "PasswordInput"
    );
}

#[gpui::test]
fn fingerprint_comparison_gates_real_enrollment_and_durable_publication(cx: &mut TestAppContext) {
    let provider = Provider::new(2, false);
    let fingerprint = fingerprint(provider.fixture.prepared.pin.public_key);
    let cx = open(provider.clone(), cx);
    direct_ready(cx);
    click("settings-server-enrollment-confirm", cx);
    assert!(provider.authorizations.borrow().is_empty());
    write_field(
        "Fingerprint from the server's owner",
        "SHA256:another-server",
        cx,
    );
    click("settings-server-enrollment-confirm", cx);
    assert!(provider.publications.borrow().is_empty());
    write_field("Fingerprint from the server's owner", &fingerprint, cx);
    let tree = A11yTree::read(cx);
    perform(
        cx,
        tree.node("Confirm Fingerprint and Pair"),
        accesskit::Action::Click,
    );
    cx.run_until_parked();
    assert_eq!(provider.authorizations.borrow().as_slice(), &[true]);
    assert_eq!(
        provider.publications.borrow().as_slice(),
        std::slice::from_ref(&provider.fixture.saved)
    );
    let recovered = provider
        .fixture
        .storage
        .runtime
        .block_on(provider.fixture.storage.reopened().recover())
        .unwrap();
    assert_eq!(
        recovered.saved.as_slice(),
        std::slice::from_ref(&provider.fixture.saved)
    );
    assert!(!tree_json(cx).contains("single-use-secret-invitation"));
}
fn tree_json(cx: &mut VisualTestContext) -> String {
    let _ = A11yTree::read(cx);
    cx.update(|window, _| window.debug_a11y_tree_json().unwrap())
}

#[gpui::test]
fn lost_reply_checks_original_request_without_new_authorization_or_inspection(
    cx: &mut TestAppContext,
) {
    let provider = Provider::new(1, true);
    let fingerprint = fingerprint(provider.fixture.prepared.pin.public_key);
    let cx = open(provider.clone(), cx);
    direct_ready(cx);
    write_field("Fingerprint from the server's owner", &fingerprint, cx);
    click("settings-server-enrollment-confirm", cx);
    assert!(provider.publications.borrow().is_empty());
    let recovered = provider
        .fixture
        .storage
        .runtime
        .block_on(provider.fixture.storage.reopened().recover())
        .unwrap();
    assert_eq!(
        recovered.prepared.as_slice(),
        std::slice::from_ref(&provider.fixture.prepared)
    );
    assert!(
        cx.debug_bounds("settings-server-enrollment-confirm")
            .is_none(),
        "a submitted enrollment offers one Check Original Enrollment control"
    );
    click("settings-server-enrollment-check", cx);
    assert_eq!(provider.inspections.get(), 1);
    assert_eq!(provider.authorizations.borrow().as_slice(), &[true, false]);
    assert_eq!(
        provider
            .fixture
            .endpoint
            .submissions
            .load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        provider.publications.borrow().as_slice(),
        std::slice::from_ref(&provider.fixture.saved)
    );
}

#[gpui::test]
fn cancelled_inspection_never_enrolls_and_restores_focus(cx: &mut TestAppContext) {
    let provider = Provider::new(2, false);
    provider.hold.set(true);
    let cx = open(provider.clone(), cx);
    direct_ready(cx);
    assert_eq!(provider.inspections.get(), 1);
    click("modal-action-settings-server-enrollment-done", cx);
    cx.run_until_parked();
    let send = provider.hold_inspection.borrow_mut().take().unwrap();
    assert!(
        send.is_closed(),
        "closing the sheet drops its observation task"
    );
    assert!(provider.publications.borrow().is_empty());
    assert!(provider.authorizations.borrow().is_empty());
    let tree = A11yTree::read(cx);
    assert!(tree.with_role("Dialog").is_empty());
    assert_eq!(tree.focused().unwrap()["aria"]["label"], "Add Server…");
}

#[gpui::test]
fn stopped_ssh_server_needs_separate_start_and_check_again(cx: &mut TestAppContext) {
    let provider = Provider::new(2, false);
    *provider.ssh_facts.borrow_mut() = Some(facts(BootstrapRegistration::Stopped {
        pin: provider.fixture.prepared.pin,
        generation: 7,
    }));
    let cx = open(provider.clone(), cx);
    click("settings-server-enrollment-ssh", cx);
    write_field("Server address or SSH host", "build-box", cx);
    click("settings-server-enrollment-check", cx);
    assert_eq!(provider.starts.get(), 0);
    assert_eq!(provider.bootstraps.get(), 0);
    assert!(
        cx.debug_bounds("settings-server-enrollment-confirm")
            .is_none()
    );
    click("settings-server-enrollment-start", cx);
    assert_eq!(provider.starts.get(), 1);
    assert_eq!(provider.bootstraps.get(), 0);
    assert!(provider.publications.borrow().is_empty());
    click("settings-server-enrollment-check", cx);
    assert_eq!(provider.inspections.get(), 2);
    assert_eq!(provider.bootstraps.get(), 0);
}

#[test]
fn comparison_approval_binds_route_pin_attempt_and_native_credential() {
    let fixture = Fixture::new(2, false);
    let attempt = EnrollmentAttempt::new(1);
    let mut candidate = fixture.candidate(attempt.clone());
    let verifier = candidate.verifier().unwrap();
    let comparison = verifier.fingerprint();
    assert!(
        verifier
            .confirm(
                fixture.prepared.pin,
                RevocationRouteReference::new([9; 16]),
                1,
                &comparison
            )
            .is_err()
    );
    assert!(
        verifier
            .confirm(
                ServerPin {
                    identity: [9; 16],
                    ..fixture.prepared.pin
                },
                fixture.prepared.route,
                1,
                &comparison
            )
            .is_err()
    );
    assert!(
        verifier
            .confirm(fixture.prepared.pin, fixture.prepared.route, 2, &comparison)
            .is_err()
    );
    let approval = candidate.approve(&verifier, &comparison).unwrap();
    assert!(approval.authorization.is_some());
    assert!(
        candidate
            .approve(&verifier, &comparison)
            .unwrap()
            .authorization
            .is_none()
    );
    attempt.cancel();
    assert!(matches!(
        fixture.enroll(approval),
        Err(EnrollmentFailure::Pairing(PairingFailure::Cancelled))
    ));
    assert_eq!(fixture.endpoint.submissions.load(Ordering::Relaxed), 0);
    let mut wrong = fixture.device.clone();
    wrong.public_key = [9; 32];
    assert!(matches!(
        DirectCandidate::new(
            EnrollmentAttempt::new(1),
            fixture.prepared.clone(),
            wrong,
            fixture.reservation.clone(),
            None,
            vec![2]
        ),
        Err(EnrollmentFailure::Pairing(PairingFailure::IdentityMismatch))
    ));
}

#[test]
fn historical_or_wrong_binding_receipt_never_becomes_a_publication_input() {
    let fixture = Fixture::new(2, false);
    let attempt = EnrollmentAttempt::new(1);
    let result = fixture.storage.runtime.block_on(DurableEnrollment::verify(
        attempt.clone(),
        &fixture.storage.store(),
        fixture.prepared.clone(),
        fixture.saved.clone(),
    ));
    assert!(matches!(
        result,
        Err(EnrollmentFailure::Pairing(
            PairingFailure::InvalidDurableState
        ))
    ));
    let mut wrong = fixture.prepared.clone();
    wrong.route = RevocationRouteReference::new([9; 16]);
    // A valid signature alone cannot stand in for the original native route/preparation in storage.
    let result = fixture.storage.runtime.block_on(DurableEnrollment::verify(
        attempt,
        &fixture.storage.store(),
        wrong,
        fixture.saved.clone(),
    ));
    assert!(matches!(
        result,
        Err(EnrollmentFailure::Pairing(PairingFailure::IdentityMismatch))
    ));
}

#[gpui::test]
fn changing_route_discards_the_old_fingerprint_approval(cx: &mut TestAppContext) {
    let provider = Provider::new(2, false);
    let cx = open(provider.clone(), cx);
    direct_ready(cx);
    write_field("Server address or SSH host", "wss://other.invalid", cx);
    assert!(
        cx.debug_bounds("settings-server-enrollment-fingerprint")
            .is_none()
    );
    assert!(
        cx.debug_bounds("settings-server-enrollment-confirm")
            .is_none()
    );
    assert!(provider.authorizations.borrow().is_empty());
}

#[gpui::test]
fn closing_an_approved_attempt_revokes_its_prepared_native_signing_lease(cx: &mut TestAppContext) {
    let provider = Provider::new(2, false);
    provider.hold_enrollment.set(true);
    let fingerprint = fingerprint(provider.fixture.prepared.pin.public_key);
    let cx = open(provider.clone(), cx);
    direct_ready(cx);
    write_field("Fingerprint from the server's owner", &fingerprint, cx);
    click("settings-server-enrollment-confirm", cx);
    let approval = provider.held_approval.borrow_mut().take().unwrap();
    let lease = approval.verified.credential_use(&approval.device).unwrap();
    click("modal-action-settings-server-enrollment-done", cx);
    assert!(lease.validate(&approval.device.scope).is_err());
    assert!(approval.verified.credential_use(&approval.device).is_err());
    assert!(provider.publications.borrow().is_empty());
    assert_eq!(
        provider
            .fixture
            .endpoint
            .submissions
            .load(Ordering::Relaxed),
        0
    );
}

#[gpui::test]
fn a_same_counter_completion_from_another_attempt_cannot_publish(cx: &mut TestAppContext) {
    let provider = Provider::new(2, false);
    provider.stale_completion.set(true);
    let fingerprint = fingerprint(provider.fixture.prepared.pin.public_key);
    let cx = open(provider.clone(), cx);
    direct_ready(cx);
    write_field("Fingerprint from the server's owner", &fingerprint, cx);
    click("settings-server-enrollment-confirm", cx);
    assert!(provider.publications.borrow().is_empty());
    assert!(
        A11yTree::read(cx)
            .find_text(
                "The attempt, route, server or credential changed. Check the selected server again."
            )
            .is_some()
    );
}

#[gpui::test]
fn lost_ssh_reply_and_reopened_sheet_resolve_only_the_retained_native_request(
    cx: &mut TestAppContext,
) {
    let provider = Provider::new(2, false);
    *provider.ssh_facts.borrow_mut() = Some(facts(BootstrapRegistration::Running {
        pin: provider.fixture.prepared.pin,
        generation: 7,
        artifact_identity: [6; 32],
        version: spaceterm_client::pairing::bootstrap::HostVersion::new(1, 0, 0),
        protocol_majors: [1, 2],
    }));
    let cx = open(provider.clone(), cx);
    click("settings-server-enrollment-ssh", cx);
    write_field("Server address or SSH host", "build-box", cx);
    click("settings-server-enrollment-check", cx);
    assert_eq!(provider.bootstraps.get(), 0);
    click("settings-server-enrollment-confirm", cx);
    assert!(
        cx.debug_bounds("settings-server-enrollment-confirm")
            .is_none(),
        "a submitted setup offers one Check Original Setup control"
    );
    click("settings-server-enrollment-check", cx);
    assert_eq!(provider.bootstraps.get(), 1);
    assert_eq!(provider.resolutions.get(), 1);
    click("modal-action-settings-server-enrollment-done", cx);
    click("settings-server-add", cx);
    click("settings-server-enrollment-ssh", cx);
    write_field("Server address or SSH host", "build-box", cx);
    click("settings-server-enrollment-check", cx);
    // The reopened sheet found the retained setup, so Check Original Setup is its one control.
    click("settings-server-enrollment-check", cx);
    assert_eq!(provider.bootstraps.get(), 1);
    assert_eq!(provider.resolutions.get(), 2);
    assert!(provider.publications.borrow().is_empty());
}

#[test]
fn bootstrap_completion_requires_connected_result_matching_the_durable_pinned_server() {
    use spaceterm_client::pairing::bootstrap::*;
    let fixture = Fixture::new(2, false);
    let attempt = EnrollmentAttempt::new(1);
    let mut candidate = fixture.candidate(attempt.clone());
    let verifier = candidate.verifier().unwrap();
    let approval = candidate
        .approve(&verifier, &verifier.fingerprint())
        .unwrap();
    let durable = fixture.enroll(approval).unwrap();
    let facts = facts(BootstrapRegistration::Running {
        pin: fixture.prepared.pin,
        generation: 7,
        artifact_identity: [6; 32],
        version: HostVersion::new(1, 0, 0),
        protocol_majors: [1, 2],
    });
    let mut outcome = BootstrapOutcome {
        server: BootstrapServer {
            route: facts.route,
            pin: fixture.prepared.pin,
            registration_generation: 7,
            approved_artifact_identity: [6; 32],
            protocol_majors: [1, 2],
            running: true,
        },
        reused: true,
        phase: BootstrapPhase::Enrolled,
    };
    assert!(matches!(
        fixture
            .storage
            .runtime
            .block_on(DurableEnrollment::verify_bootstrap(
                attempt.clone(),
                &fixture.storage.store(),
                durable.prepared.clone(),
                durable.saved.clone(),
                outcome
            )),
        Err(EnrollmentFailure::Pairing(PairingFailure::IdentityMismatch))
    ));
    outcome.phase = BootstrapPhase::Connected;
    let result = fixture
        .storage
        .runtime
        .block_on(DurableEnrollment::verify_bootstrap(
            attempt,
            &fixture.storage.reopened(),
            durable.prepared,
            durable.saved,
            outcome,
        ))
        .unwrap();
    assert_eq!(result.bootstrap.unwrap().server.pin, fixture.prepared.pin);
}

#[test]
fn native_locked_storage_has_distinct_content_free_foreground_guidance() {
    assert_eq!(
        failure_message(EnrollmentFailure::CredentialStorageLocked),
        "Unlock the native credential store, then Check Again."
    );
    let mut facts = facts(BootstrapRegistration::Missing);
    facts.storage = spaceterm_client::pairing::bootstrap::BootstrapPrerequisite::Locked;
    assert!(matches!(
        SshCandidate::new(EnrollmentAttempt::new(1), facts),
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::StorageLocked
        ))
    ));
}

#[gpui::test]
fn unavailable_provider_keeps_enrollment_disabled_without_a_production_fake(
    cx: &mut TestAppContext,
) {
    let (_, _, cx) = open_settings_with_registry(cx, sample_registry());
    select_section(SettingsSectionId::Servers, cx);
    click("settings-server-add", cx);
    let tree = A11yTree::read(cx);
    assert!(
        tree.find_text(
            "Server enrollment is unavailable in this build. No server has been changed."
        )
        .is_some()
    );
    assert_eq!(tree.node("Check Server")["aria"]["disabled"], true);
    assert!(
        cx.debug_bounds("settings-server-enrollment-confirm")
            .is_none()
    );
}

#[gpui::test]
fn keyboard_return_in_the_comparison_field_does_not_approve_and_escape_cancels(
    cx: &mut TestAppContext,
) {
    let provider = Provider::new(2, false);
    let fingerprint = fingerprint(provider.fixture.prepared.pin.public_key);
    let cx = open(provider.clone(), cx);
    direct_ready(cx);
    write_field("Fingerprint from the server's owner", &fingerprint, cx);
    let tree = A11yTree::read(cx);
    perform(
        cx,
        tree.node("Fingerprint from the server's owner"),
        accesskit::Action::Focus,
    );
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(provider.authorizations.borrow().is_empty());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(provider.publications.borrow().is_empty());
    assert!(A11yTree::read(cx).with_role("Dialog").is_empty());
}

#[gpui::test]
fn unsupported_ssh_host_refuses_before_start_or_setup(cx: &mut TestAppContext) {
    let provider = Provider::new(2, false);
    let mut host = facts(BootstrapRegistration::Missing);
    host.architecture = spaceterm_client::pairing::bootstrap::BootstrapArchitecture::Other;
    *provider.ssh_facts.borrow_mut() = Some(host);
    let cx = open(provider.clone(), cx);
    click("settings-server-enrollment-ssh", cx);
    write_field("Server address or SSH host", "build-box", cx);
    click("settings-server-enrollment-check", cx);
    assert!(
        cx.debug_bounds("settings-server-enrollment-confirm")
            .is_none()
    );
    assert!(
        cx.debug_bounds("settings-server-enrollment-start")
            .is_none()
    );
    assert_eq!(provider.starts.get(), 0);
    assert_eq!(provider.bootstraps.get(), 0);
    assert!(A11yTree::read(cx).find_text("This host's operating system, architecture or libc is unsupported. Nothing was installed.").is_some());
}

#[test]
fn native_bootstrap_lookup_cannot_be_rebound_to_a_different_signed_artifact() {
    use spaceterm_client::pairing::bootstrap::*;
    let fixture = Fixture::new(2, false);
    let host = facts(BootstrapRegistration::Running {
        pin: fixture.prepared.pin,
        generation: 7,
        artifact_identity: [6; 32],
        version: HostVersion::new(1, 0, 0),
        protocol_majors: [1, 2],
    });
    let candidate = SshCandidate::new(EnrollmentAttempt::new(1), host).unwrap();
    assert!(matches!(
        candidate
            .clone()
            .retain_request(BootstrapRequest::new([7; 16], [9; 32]).unwrap()),
        Err(EnrollmentFailure::Pairing(PairingFailure::IdentityMismatch))
    ));
    let pending = candidate
        .retain_request(BootstrapRequest::new([7; 16], [6; 32]).unwrap())
        .unwrap();
    assert!(matches!(
        pending.confirm(),
        Err(EnrollmentFailure::Pairing(PairingFailure::RecoveryRequired))
    ));
}

#[test]
fn reopening_the_canonical_journal_looks_up_the_original_prepared_enrollment() {
    let fixture = Fixture::new(2, true);
    let mut first = fixture.candidate(EnrollmentAttempt::new(1));
    let verifier = first.verifier().unwrap();
    let approval = first.approve(&verifier, &verifier.fingerprint()).unwrap();
    assert!(matches!(
        fixture.enroll(approval),
        Err(EnrollmentFailure::Pairing(PairingFailure::Offline))
    ));
    drop(verifier);
    let journal = fixture.storage.reopened_controller();
    let store = JournalPairingStore::new(journal.clone());
    let recovered = fixture.storage.runtime.block_on(store.recover()).unwrap();
    assert_eq!(
        recovered.prepared.as_slice(),
        std::slice::from_ref(&fixture.prepared)
    );
    let original = recovered.prepared[0].clone();
    let mut candidate = DirectCandidate::new(
        EnrollmentAttempt::new(2),
        original.clone(),
        fixture.device.clone(),
        original.reservation.clone().unwrap(),
        None,
        vec![2],
    )
    .unwrap();
    let comparison = candidate.verifier().unwrap();
    let approval = candidate
        .approve(&comparison, &comparison.fingerprint())
        .unwrap();
    let driver = EnrollmentDriver::new(
        JournalPairingStore::new(journal),
        fixture.credentials.clone(),
        fixture.endpoint.clone(),
    );
    let result = fixture
        .storage
        .runtime
        .block_on(approval.enroll(&driver, &store))
        .unwrap();
    assert_eq!(result.saved(), &fixture.saved);
    assert_eq!(result.prepared, original);
    assert_eq!(fixture.endpoint.submissions.load(Ordering::Relaxed), 1);
}

#[test]
fn self_unpair_fence_rejects_a_historical_receipt_before_the_tombstone_is_written() {
    let fixture = Fixture::new(2, false);
    let attempt = EnrollmentAttempt::new(1);
    let mut candidate = fixture.candidate(attempt.clone());
    let verifier = candidate.verifier().unwrap();
    let approval = candidate
        .approve(&verifier, &verifier.fingerprint())
        .unwrap();
    let durable = fixture.enroll(approval).unwrap();
    let store = fixture.storage.store();
    store.seal_ordinary(durable.saved.key());
    let result = fixture.storage.runtime.block_on(DurableEnrollment::verify(
        attempt,
        &store,
        durable.prepared,
        durable.saved,
    ));
    assert!(matches!(
        result,
        Err(EnrollmentFailure::Pairing(
            PairingFailure::AuthorizationDenied
        ))
    ));
}

#[gpui::test]
async fn foreground_enrollment_production_composition(cx: &mut TestAppContext) {
    super::native::tests::foreground_composition(cx).await;
}

#[gpui::test]
fn add_server_public_claim_copy_requires_a_deliberate_fingerprint_action_and_never_enrolls(
    cx: &mut TestAppContext,
) {
    let provider = Provider::new(2, false);
    let fingerprint = fingerprint(provider.fixture.prepared.pin.public_key);
    let cx = open(provider.clone(), cx);
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("unchanged".into())));
    click("settings-server-enrollment-copy-claim", cx);
    assert_eq!(provider.claim_exports.get(), 0);
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "unchanged"
    );
    write_field("Server address or SSH host", "wss://enrollment.invalid", cx);
    write_field(
        "Single-use pairing invitation",
        "public-server-identity",
        cx,
    );
    write_field("Fingerprint from the server's owner", &fingerprint, cx);
    let tree = A11yTree::read(cx);
    perform(
        cx,
        tree.node("Copy Public Device Claim"),
        accesskit::Action::Click,
    );
    cx.run_until_parked();
    assert_eq!(provider.claim_exports.get(), 1);
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "verified-public-device-claim"
    );
    assert_eq!(provider.inspections.get(), 0);
    assert!(provider.authorizations.borrow().is_empty());
    assert!(provider.publications.borrow().is_empty());
    assert_eq!(
        provider.fixture.endpoint.submissions.load(Ordering::SeqCst),
        0
    );
    assert!(!tree_json(cx).contains("public-server-identity"));
}
