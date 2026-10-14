//! The caller owns the route. These tests inject only the message transport and
//! remote runtime; pairing admission, durable journals and protocol codecs are real.
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding, RouteDescription, RouteParts},
    failure::{ClientFailure, ClientFailureClass},
    journal::{
        ClientJournalChange, JournalController, JournalCredentialSlot, JournalLimits,
        ReceiptVerifier, SavedBinding,
    },
    pairing::journal::{JournalPairingStore, LocalPairingStore},
};
use spaceterm_client_native::{
    journal::NativeClientJournal,
    native::runtime::{RuntimeClientAccess, authenticated_account_runtime_call},
};
use spaceterm_core::{
    authority::{ServerEpoch, SessionGeneration, SessionKey, StoreRevision},
    identity::{
        ClientId, DeviceId, OperationStreamId, OperationStreamKey, PairingId, PaneId, ServerId,
    },
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    operation_ledger::canonical::CanonicalCodec,
    operations::{OperationBinding, OperationId, OperationSequence, RequestDigest},
    pairing::{
        ClientRecord, CredentialReference, LocalPairingGeneration, PairingClientStreams,
        PairingClientStreamsParts, PairingKeys, PairingKeysParts, PairingReceiptBody,
        PairingTarget, PairingTargetParts, RevocationRouteReference, SignedPairingReceipt,
    },
};
use spaceterm_host::{
    account_service::{
        RuntimeAssessment, ServiceFailure,
        runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply, RUNTIME_MESSAGE_LIMIT},
    },
    transport::{
        EnrolledServerKey, MessageTransport, PeerFacts, TransportError, TransportFuture,
        TransportRoute,
    },
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{
        Established, HandshakeConfig, PairingLookup, PairingRecord, Principal, ServerHandshake,
        ServerPin, SigningKey,
    },
    codec::Payload,
    negotiation::CAPABILITY_ACCOUNT_RUNTIME,
    pairing_adapter::ReceiptCodec,
    wire::{common, v1, v2},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Barrier, Mutex as AsyncMutex, mpsc},
    task::JoinHandle,
};

const DEADLINE: Duration = Duration::from_secs(5);

struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
}
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))
    }
}

struct Fixture {
    server: Arc<Key>,
    device: Arc<Key>,
    binding: PairedServerBinding,
    saved: SavedBinding,
}
impl Fixture {
    fn new(server_seed: u8, client: u8) -> Self {
        let server = Arc::new(Key::new(server_seed));
        let device = Arc::new(Key::new(30 + client));
        let id = ServerId::new([server_seed; 16]);
        let local = u64::from(client);
        let pairing = PairingId::new(id, local).unwrap();
        let client_id = ClientId::new(id, local).unwrap();
        let stream = |number| {
            OperationStreamId::new(
                OperationStreamKey::new(id, number).unwrap(),
                pairing,
                client_id,
            )
            .unwrap()
        };
        let binding = PairedServerBinding::new(
            ConnectionTarget::new(
                ServerPin {
                    identity: id.bytes(),
                    public_key: server.public_key(),
                },
                DeviceId::new([60 + client; 16]),
                pairing,
                client_id,
                RouteDescription::new(RouteParts::Wss {
                    address: "192.0.2.10:443".parse().unwrap(),
                    server_name: "runtime.example.test".into(),
                })
                .unwrap(),
                CredentialReference::new([client; 16]),
                LocalPairingGeneration::new(1).unwrap(),
            )
            .unwrap(),
        );
        let streams = PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: client_id,
                pairing,
            },
            ordinary: stream(local * 3),
            self_revocation: stream(local * 3 + 1),
        })
        .unwrap();
        let receipt = ReceiptCodec::new(binding.target().pin())
            .sign(
                PairingReceiptBody::ClientBinding {
                    target: PairingTarget::new(PairingTargetParts {
                        server: id,
                        device: binding.target().device(),
                        pairing,
                    })
                    .unwrap(),
                    keys: PairingKeys::new(PairingKeysParts {
                        device_identity_public_key: Key::new(90 + client).public_key(),
                        pairing_authentication_public_key: device.public_key(),
                    })
                    .unwrap(),
                    client: streams.clone(),
                    operation: OperationBinding {
                        id: OperationId::new(
                            stream(local * 3 + 2),
                            OperationSequence::new(1).unwrap(),
                        ),
                        digest: RequestDigest::from_bytes([client; 32]),
                    },
                    revision: StoreRevision::new(1),
                },
                server.as_ref(),
            )
            .unwrap();
        let saved = SavedBinding::verified(binding.clone(), streams, receipt, &Verifier).unwrap();
        Self {
            server,
            device,
            binding,
            saved,
        }
    }
    async fn retain(&self, journal: &JournalController) {
        let target = self.binding.target();
        journal
            .transact(vec![
                ClientJournalChange::ReserveCredential(JournalCredentialSlot {
                    namespace: "dev.spaceterm.account-runtime-test".into(),
                    server: Some(target.server()),
                    reference: target.credential(),
                    public_key: None,
                    route_reference: None,
                    route: None,
                    ssh_capture: None,
                    generation: target.incarnation(),
                }),
                ClientJournalChange::RecordCredentialPublicKey {
                    reference: target.credential(),
                    public_key: self.device.public_key(),
                },
                ClientJournalChange::BindCredentialRoute {
                    reference: target.credential(),
                    route_reference: RevocationRouteReference::new(target.credential().opaque()),
                    route: target.route().clone(),
                },
                ClientJournalChange::SaveBinding(Box::new(self.saved.clone())),
            ])
            .await
            .unwrap();
        assert!(journal.ordinary_use_allowed(&self.binding).unwrap());
    }
    fn access(&self, journal: &Arc<JournalController>) -> RuntimeClientAccess {
        RuntimeClientAccess {
            binding: self.binding.clone(),
            signer: self.device.clone(),
            journal: journal.clone(),
        }
    }
    fn principal(&self) -> Principal {
        let local = |number: u64| {
            let mut bytes = [0; 16];
            bytes[8..].copy_from_slice(&number.to_be_bytes());
            bytes
        };
        let target = self.binding.target();
        Principal {
            server: target.server().bytes(),
            device: target.device().bytes(),
            pairing: local(target.pairing().local()),
            client: local(target.client().local()),
        }
    }
    fn assessment(&self) -> RuntimeAssessment {
        let server = self.binding.target().server();
        let epoch = ServerEpoch::new(17);
        RuntimeAssessment {
            server,
            epoch: Some(epoch),
            generation: 40 + self.binding.target().client().local(),
            sessions: vec![SessionKey {
                pane: PaneId::new(server, self.binding.target().client().local()).unwrap(),
                epoch,
                generation: SessionGeneration::new(8),
            }],
            token: [u8::try_from(self.binding.target().client().local()).unwrap(); 32],
        }
    }
}

async fn durable_journal() -> (tempfile::TempDir, Arc<JournalController>) {
    // /var/tmp is persistent on Linux, unlike native-journal-ineligible tmpfs.
    let directory = tempfile::Builder::new()
        .prefix("st-account-runtime-")
        .tempdir_in("/var/tmp")
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let native = NativeClientJournal::create(
        directory.path().canonicalize().unwrap(),
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let journal = JournalController::recover(
        Arc::new(native),
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    (directory, Arc::new(journal))
}

fn seal(journal: &Arc<JournalController>, binding: &PairedServerBinding) {
    JournalPairingStore::new(journal.clone())
        .seal_ordinary(&binding.target().local_pairing().unwrap());
    assert!(!journal.ordinary_use_allowed(binding).unwrap());
}

// An uncontrolled remote endpoint, without IPC account facts or native actions.
struct Transport {
    facts: PeerFacts,
    outgoing: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    incoming: AsyncMutex<mpsc::Receiver<Vec<u8>>>,
    sent: AtomicUsize,
}
impl Transport {
    fn pair(pin: ServerPin) -> (Self, Self) {
        let (to_server, server_incoming) = mpsc::channel(1);
        let (to_client, client_incoming) = mpsc::channel(1);
        let facts = PeerFacts {
            route: TransportRoute::SecureWebSocket,
            local_uid: None,
            local_pid: None,
            pinned_server_key: Some(EnrolledServerKey(pin.public_key)),
        };
        (
            Self {
                facts,
                outgoing: Mutex::new(Some(to_server)),
                incoming: AsyncMutex::new(client_incoming),
                sent: AtomicUsize::new(0),
            },
            Self {
                facts,
                outgoing: Mutex::new(Some(to_client)),
                incoming: AsyncMutex::new(server_incoming),
                sent: AtomicUsize::new(0),
            },
        )
    }
    async fn finish(&self, server: JoinHandle<ServerReport>) -> ServerReport {
        self.close().await.unwrap();
        tokio::time::timeout(DEADLINE, server)
            .await
            .expect("test remote endpoint did not finish")
            .unwrap()
    }
}
impl MessageTransport for Transport {
    fn peer_facts(&self) -> PeerFacts {
        self.facts
    }
    fn send<'a>(&'a self, message: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            let sender = self
                .outgoing
                .lock()
                .unwrap()
                .clone()
                .ok_or(TransportError::Closed)?;
            self.sent.fetch_add(1, Ordering::SeqCst);
            sender
                .send(message.to_vec())
                .await
                .map_err(|_| TransportError::Closed)
        })
    }
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.incoming.lock().await.recv().await) })
    }
    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async {
            self.outgoing.lock().unwrap().take();
            self.incoming.lock().await.close();
            Ok(())
        })
    }
}

struct Pairings(PairingRecord);
impl PairingLookup for Pairings {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        if (server, pairing, client)
            != (
                self.0.principal.server,
                self.0.principal.pairing,
                self.0.principal.client,
            )
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(self.0)
    }
}

struct Peer {
    pin: ServerPin,
    signer: Arc<Key>,
    pairings: Pairings,
    major: u32,
    capabilities: Vec<u32>,
    body: Vec<u8>,
    correlation_delta: u64,
    seal_before_ready: Option<(Arc<JournalController>, PairedServerBinding)>,
    seal_before_reply: Option<(Arc<JournalController>, PairedServerBinding)>,
    reply_barrier: Option<Arc<Barrier>>,
}
impl Peer {
    fn new(fixture: &Fixture, major: u32, reply: AccountRuntimeReply) -> Self {
        Self {
            pin: fixture.binding.target().pin(),
            signer: fixture.server.clone(),
            pairings: Pairings(PairingRecord {
                principal: fixture.principal(),
                device_public_key: fixture.device.public_key(),
                active: true,
            }),
            major,
            capabilities: vec![CAPABILITY_ACCOUNT_RUNTIME],
            body: serde_json::to_vec(&reply).unwrap(),
            correlation_delta: 0,
            seal_before_ready: None,
            seal_before_reply: None,
            reply_barrier: None,
        }
    }
    fn start(self) -> (Transport, JoinHandle<ServerReport>) {
        let (client, server) = Transport::pair(self.pin);
        (client, tokio::spawn(self.serve(server)))
    }
    async fn serve(self, transport: Transport) -> ServerReport {
        let mut report = ServerReport::default();
        let Some(hello) = transport.recv().await.unwrap() else {
            return report;
        };
        report.hello_received = true;
        let (handshake, reply) = match ServerHandshake::accept(
            self.pin.identity,
            17,
            HandshakeConfig {
                majors: vec![self.major],
                capabilities: self.capabilities.clone(),
                ..HandshakeConfig::default()
            },
            self.signer,
            &self.pairings,
            &hello,
        ) {
            Ok(accepted) => accepted,
            Err(error) => {
                report.authentication_failure = Some(error);
                return report;
            }
        };
        transport.send(&reply).await.unwrap();
        let Some(proof) = transport.recv().await.unwrap() else {
            return report;
        };
        let (established, ready) = match handshake.receive_device(&self.pairings, &proof) {
            Ok(accepted) => accepted,
            Err(error) => {
                report.authentication_failure = Some(error);
                return report;
            }
        };
        let Established::Ordinary(mut codec) = established else {
            panic!("account runtime requires an ordinary paired connection")
        };
        assert_eq!(codec.major(), self.major);
        assert_eq!(codec.capabilities(), self.capabilities);
        report.principal = Some(codec.principal());
        if let Some((journal, binding)) = &self.seal_before_ready {
            seal(journal, binding);
        }
        transport.send(&ready).await.unwrap();
        let Some(bytes) = transport.recv().await.unwrap() else {
            return report;
        };
        let request = match codec.decode(&bytes).unwrap() {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeRequest(value)),
            })
            | Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeRequest(value)),
            }) => value,
            _ => panic!("dedicated account runtime request required"),
        };
        assert!(request.body.len() <= RUNTIME_MESSAGE_LIMIT);
        report
            .calls
            .push(serde_json::from_slice(&request.body).unwrap());
        if let Some(barrier) = &self.reply_barrier {
            barrier.wait().await;
        }
        if let Some((journal, binding)) = &self.seal_before_reply {
            seal(journal, binding);
        }
        let message = common::AccountRuntimeMessage {
            request: request.request + self.correlation_delta,
            body: self.body,
            ..Default::default()
        };
        let payload = match self.major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeReply(message)),
            }),
            2 => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeReply(message)),
            }),
            _ => unreachable!(),
        };
        let bytes = match codec.encode(&payload) {
            Ok(bytes) => bytes,
            Err(error) => {
                report.reply_encoding_failure = Some(error);
                return report;
            }
        };
        transport.send(&bytes).await.unwrap();
        report
    }
}

#[derive(Default)]
struct ServerReport {
    hello_received: bool,
    principal: Option<Principal>,
    calls: Vec<AccountRuntimeCall>,
    authentication_failure: Option<ProtocolError>,
    reply_encoding_failure: Option<ProtocolError>,
}

#[tokio::test]
async fn nonlocal_runtime_exchange_preserves_typed_calls_and_replies_in_both_majors() {
    for major in [1, 2] {
        let (_directory, journal) = durable_journal().await;
        let fixture = Fixture::new(1, 1);
        fixture.retain(&journal).await;
        let assessment = fixture.assessment();
        let request = NativeManagementRequestRef::new(
            assessment.server,
            [7; 16],
            RequestDigest::from_bytes([8; 32]),
        )
        .unwrap();
        for (call, reply) in [
            (
                AccountRuntimeCall::Assess,
                AccountRuntimeReply::Assessment(assessment.clone()),
            ),
            (
                AccountRuntimeCall::Reserve(assessment.clone()),
                AccountRuntimeReply::Reserved,
            ),
            (
                AccountRuntimeCall::Abort(assessment.clone()),
                AccountRuntimeReply::Aborted,
            ),
            (
                AccountRuntimeCall::Quiesce {
                    request,
                    action: NativeRetirementAction::Stop,
                    assessment: assessment.clone(),
                },
                AccountRuntimeReply::QuiescenceAccepted,
            ),
        ] {
            let access = fixture.access(&journal);
            let (transport, server) = Peer::new(&fixture, major, reply.clone()).start();
            let result = tokio::time::timeout(
                DEADLINE,
                authenticated_account_runtime_call(&access, &transport, &call),
            )
            .await
            .unwrap();
            let report = transport.finish(server).await;
            assert_eq!(result, Ok(reply));
            assert_eq!(report.principal, Some(fixture.principal()));
            assert_eq!(report.calls, vec![call]);
        }
    }
}

#[tokio::test]
async fn unsaved_exact_incarnation_is_denied_before_handshake() {
    let (_directory, journal) = durable_journal().await;
    let fixture = Fixture::new(1, 1);
    fixture.retain(&journal).await;
    let mut access = fixture.access(&journal);
    let target = access.binding.target();
    access.binding = PairedServerBinding::new(
        ConnectionTarget::new(
            target.pin(),
            target.device(),
            target.pairing(),
            target.client(),
            target.route().clone(),
            target.credential(),
            LocalPairingGeneration::new(2).unwrap(),
        )
        .unwrap(),
    );
    let (transport, server) = Peer::new(&fixture, 2, AccountRuntimeReply::Reserved).start();
    let result = tokio::time::timeout(
        DEADLINE,
        authenticated_account_runtime_call(&access, &transport, &AccountRuntimeCall::Assess),
    )
    .await
    .unwrap();
    let report = transport.finish(server).await;
    assert_eq!(result, Err(ServiceFailure::AuthorizationDenied));
    assert_eq!(transport.sent.load(Ordering::SeqCst), 0);
    assert!(!report.hello_received);
}

#[tokio::test]
async fn sealed_saved_binding_is_denied_before_handshake() {
    let (_directory, journal) = durable_journal().await;
    let fixture = Fixture::new(1, 1);
    fixture.retain(&journal).await;
    let access = fixture.access(&journal);
    seal(&journal, &fixture.binding);
    let (transport, server) = Peer::new(&fixture, 2, AccountRuntimeReply::Reserved).start();
    let result = tokio::time::timeout(
        DEADLINE,
        authenticated_account_runtime_call(&access, &transport, &AccountRuntimeCall::Assess),
    )
    .await
    .unwrap();
    let report = transport.finish(server).await;
    assert_eq!(result, Err(ServiceFailure::AuthorizationDenied));
    assert_eq!(transport.sent.load(Ordering::SeqCst), 0);
    assert!(!report.hello_received);
}

#[tokio::test]
async fn authority_sealed_during_handshake_prevents_runtime_request_in_both_majors() {
    for major in [1, 2] {
        let (_directory, journal) = durable_journal().await;
        let fixture = Fixture::new(1, 1);
        fixture.retain(&journal).await;
        let access = fixture.access(&journal);
        let mut peer = Peer::new(&fixture, major, AccountRuntimeReply::Reserved);
        peer.seal_before_ready = Some((journal.clone(), fixture.binding.clone()));
        let (transport, server) = peer.start();
        let result = tokio::time::timeout(
            DEADLINE,
            authenticated_account_runtime_call(&access, &transport, &AccountRuntimeCall::Assess),
        )
        .await
        .unwrap();
        let report = transport.finish(server).await;
        assert_eq!(result, Err(ServiceFailure::AuthorizationDenied));
        assert_eq!(report.principal, Some(fixture.principal()));
        assert!(report.calls.is_empty());
    }
}

#[tokio::test]
async fn authority_sealed_during_reply_prevents_response_release_in_both_majors() {
    for major in [1, 2] {
        let (_directory, journal) = durable_journal().await;
        let fixture = Fixture::new(1, 1);
        fixture.retain(&journal).await;
        let access = fixture.access(&journal);
        let mut peer = Peer::new(
            &fixture,
            major,
            AccountRuntimeReply::Assessment(fixture.assessment()),
        );
        peer.seal_before_reply = Some((journal.clone(), fixture.binding.clone()));
        let (transport, server) = peer.start();
        let result = tokio::time::timeout(
            DEADLINE,
            authenticated_account_runtime_call(&access, &transport, &AccountRuntimeCall::Assess),
        )
        .await
        .unwrap();
        let report = transport.finish(server).await;
        assert_eq!(result, Err(ServiceFailure::AuthorizationDenied));
        assert_eq!(report.calls, vec![AccountRuntimeCall::Assess]);
    }
}

#[tokio::test]
async fn transport_pin_facts_do_not_replace_protocol_server_pin_in_both_majors() {
    for major in [1, 2] {
        let (_directory, journal) = durable_journal().await;
        let fixture = Fixture::new(1, 1);
        fixture.retain(&journal).await;
        let access = fixture.access(&journal);
        let mut peer = Peer::new(&fixture, major, AccountRuntimeReply::Reserved);
        // The supplied route still reports the enrolled pin. Another server's
        // signing key must not authenticate by claiming that server identity.
        peer.signer = Fixture::new(2, 2).server;
        let (transport, server) = peer.start();
        let result = tokio::time::timeout(
            DEADLINE,
            authenticated_account_runtime_call(&access, &transport, &AccountRuntimeCall::Assess),
        )
        .await
        .unwrap();
        let report = transport.finish(server).await;
        assert_eq!(result, Err(ServiceFailure::IdentityMismatch));
        assert!(report.principal.is_none());
        assert!(report.calls.is_empty());
    }
}

#[tokio::test]
async fn another_pairings_signer_cannot_authenticate_the_saved_client_in_both_majors() {
    for major in [1, 2] {
        let (_directory, journal) = durable_journal().await;
        let fixture = Fixture::new(1, 1);
        fixture.retain(&journal).await;
        let mut access = fixture.access(&journal);
        access.signer = Fixture::new(1, 2).device;
        let (transport, server) = Peer::new(&fixture, major, AccountRuntimeReply::Reserved).start();
        let result = tokio::time::timeout(
            DEADLINE,
            authenticated_account_runtime_call(&access, &transport, &AccountRuntimeCall::Assess),
        )
        .await
        .unwrap();
        let report = transport.finish(server).await;
        assert!(
            result.is_err(),
            "another pairing's key authenticated: {result:?}"
        );
        assert!(report.authentication_failure.is_some());
        assert!(report.principal.is_none());
        assert!(report.calls.is_empty());
    }
}

#[tokio::test]
async fn runtime_capability_is_required_before_sending_request_in_both_majors() {
    for major in [1, 2] {
        let (_directory, journal) = durable_journal().await;
        let fixture = Fixture::new(1, 1);
        fixture.retain(&journal).await;
        let access = fixture.access(&journal);
        let mut peer = Peer::new(&fixture, major, AccountRuntimeReply::Reserved);
        peer.capabilities.clear();
        let (transport, server) = peer.start();
        let result = tokio::time::timeout(
            DEADLINE,
            authenticated_account_runtime_call(&access, &transport, &AccountRuntimeCall::Assess),
        )
        .await
        .unwrap();
        let report = transport.finish(server).await;
        assert_eq!(result, Err(ServiceFailure::UnsupportedSchema));
        assert!(report.calls.is_empty());
    }
}

#[tokio::test]
async fn oversized_request_is_rejected_without_runtime_submission_in_both_majors() {
    for major in [1, 2] {
        let (_directory, journal) = durable_journal().await;
        let fixture = Fixture::new(1, 1);
        fixture.retain(&journal).await;
        let access = fixture.access(&journal);
        let mut assessment = fixture.assessment();
        assessment.sessions = vec![assessment.sessions[0]; 1024];
        let call = AccountRuntimeCall::Reserve(assessment);
        assert!(serde_json::to_vec(&call).unwrap().len() > RUNTIME_MESSAGE_LIMIT);
        let (transport, server) = Peer::new(&fixture, major, AccountRuntimeReply::Reserved).start();
        let result = tokio::time::timeout(
            DEADLINE,
            authenticated_account_runtime_call(&access, &transport, &call),
        )
        .await
        .unwrap();
        let report = transport.finish(server).await;
        assert_eq!(result, Err(ServiceFailure::LimitExceeded));
        assert!(report.calls.is_empty());
    }
}

#[tokio::test]
async fn runtime_reply_limit_accepts_exact_bound_and_prevents_oversized_response_release_in_both_majors()
 {
    for major in [1, 2] {
        let (_directory, journal) = durable_journal().await;
        let fixture = Fixture::new(1, 1);
        fixture.retain(&journal).await;
        let access = fixture.access(&journal);
        for (size, expected) in [
            (RUNTIME_MESSAGE_LIMIT, Ok(AccountRuntimeReply::Reserved)),
            (
                RUNTIME_MESSAGE_LIMIT + 1,
                Err(ServiceFailure::SupervisorUnavailable),
            ),
        ] {
            let mut peer = Peer::new(&fixture, major, AccountRuntimeReply::Reserved);
            // JSON whitespace keeps both bodies valid typed replies. The real
            // server codec refuses the oversized body before signing it. Closing
            // that peer must not release an unchecked typed response to the caller.
            peer.body.resize(size, b' ');
            assert_eq!(
                serde_json::from_slice::<AccountRuntimeReply>(&peer.body).unwrap(),
                AccountRuntimeReply::Reserved
            );
            let (transport, server) = peer.start();
            let result = tokio::time::timeout(
                DEADLINE,
                authenticated_account_runtime_call(
                    &access,
                    &transport,
                    &AccountRuntimeCall::Assess,
                ),
            )
            .await
            .unwrap();
            let report = transport.finish(server).await;
            assert_eq!(result, expected, "runtime reply body size {size}");
            if size > RUNTIME_MESSAGE_LIMIT {
                assert!(matches!(
                    report.reply_encoding_failure,
                    Some(ProtocolError::LimitExceeded)
                ));
            } else {
                assert!(report.reply_encoding_failure.is_none());
            }
            assert_eq!(report.calls, vec![AccountRuntimeCall::Assess]);
        }
    }
}

#[tokio::test]
async fn authenticated_reply_with_wrong_request_correlation_is_rejected_in_both_majors() {
    for major in [1, 2] {
        let (_directory, journal) = durable_journal().await;
        let fixture = Fixture::new(1, 1);
        fixture.retain(&journal).await;
        let access = fixture.access(&journal);
        let mut peer = Peer::new(&fixture, major, AccountRuntimeReply::Reserved);
        peer.correlation_delta = 1;
        let (transport, server) = peer.start();
        let result = tokio::time::timeout(
            DEADLINE,
            authenticated_account_runtime_call(&access, &transport, &AccountRuntimeCall::Assess),
        )
        .await
        .unwrap();
        let report = transport.finish(server).await;
        assert_eq!(result, Err(ServiceFailure::IdentityMismatch));
        assert_eq!(report.calls, vec![AccountRuntimeCall::Assess]);
    }
}

#[tokio::test]
async fn typed_server_failure_is_returned_without_reclassification_in_both_majors() {
    for major in [1, 2] {
        let (_directory, journal) = durable_journal().await;
        let fixture = Fixture::new(1, 1);
        fixture.retain(&journal).await;
        let access = fixture.access(&journal);
        let (transport, server) = Peer::new(
            &fixture,
            major,
            AccountRuntimeReply::Failed(ServiceFailure::StaleAssessment),
        )
        .start();
        let call = AccountRuntimeCall::Reserve(fixture.assessment());
        let result = tokio::time::timeout(
            DEADLINE,
            authenticated_account_runtime_call(&access, &transport, &call),
        )
        .await
        .unwrap();
        let report = transport.finish(server).await;
        assert_eq!(result, Err(ServiceFailure::StaleAssessment));
        assert_eq!(report.calls, vec![call]);
    }
}

#[tokio::test]
async fn simultaneous_callers_on_one_server_keep_principals_requests_and_replies_independent() {
    for major in [1, 2] {
        let (_first_directory, first_journal) = durable_journal().await;
        let (_second_directory, second_journal) = durable_journal().await;
        let first = Fixture::new(1, 1);
        let second = Fixture::new(1, 2);
        first.retain(&first_journal).await;
        second.retain(&second_journal).await;
        let first_access = first.access(&first_journal);
        let second_access = second.access(&second_journal);
        let first_call = AccountRuntimeCall::Reserve(first.assessment());
        let second_call = AccountRuntimeCall::Assess;
        let second_reply = AccountRuntimeReply::Assessment(second.assessment());
        let barrier = Arc::new(Barrier::new(2));
        let mut first_peer = Peer::new(&first, major, AccountRuntimeReply::Reserved);
        first_peer.reply_barrier = Some(barrier.clone());
        let mut second_peer = Peer::new(&second, major, second_reply.clone());
        second_peer.reply_barrier = Some(barrier);
        let (first_transport, first_server) = first_peer.start();
        let (second_transport, second_server) = second_peer.start();
        let (first_result, second_result) = tokio::time::timeout(DEADLINE, async {
            tokio::join!(
                authenticated_account_runtime_call(&first_access, &first_transport, &first_call),
                authenticated_account_runtime_call(&second_access, &second_transport, &second_call),
            )
        })
        .await
        .unwrap();
        let first_report = first_transport.finish(first_server).await;
        let second_report = second_transport.finish(second_server).await;
        assert_eq!(first_result, Ok(AccountRuntimeReply::Reserved));
        assert_eq!(second_result, Ok(second_reply));
        assert_eq!(first_report.principal, Some(first.principal()));
        assert_eq!(second_report.principal, Some(second.principal()));
        assert_eq!(first_report.calls, vec![first_call]);
        assert_eq!(second_report.calls, vec![second_call]);
    }
}

#[tokio::test]
async fn sealing_one_inflight_pairing_does_not_deny_an_independent_caller_on_the_same_server() {
    for major in [1, 2] {
        let (_first_directory, first_journal) = durable_journal().await;
        let (_second_directory, second_journal) = durable_journal().await;
        let first = Fixture::new(1, 1);
        let second = Fixture::new(1, 2);
        first.retain(&first_journal).await;
        second.retain(&second_journal).await;
        let first_access = first.access(&first_journal);
        let second_access = second.access(&second_journal);
        let first_call = AccountRuntimeCall::Assess;
        let second_call = AccountRuntimeCall::Reserve(second.assessment());
        let barrier = Arc::new(Barrier::new(2));
        let mut first_peer = Peer::new(
            &first,
            major,
            AccountRuntimeReply::Assessment(first.assessment()),
        );
        first_peer.reply_barrier = Some(barrier.clone());
        first_peer.seal_before_reply = Some((first_journal.clone(), first.binding.clone()));
        let mut second_peer = Peer::new(&second, major, AccountRuntimeReply::Reserved);
        second_peer.reply_barrier = Some(barrier);
        let (first_transport, first_server) = first_peer.start();
        let (second_transport, second_server) = second_peer.start();
        let (first_result, second_result) = tokio::time::timeout(DEADLINE, async {
            tokio::join!(
                authenticated_account_runtime_call(&first_access, &first_transport, &first_call),
                authenticated_account_runtime_call(&second_access, &second_transport, &second_call),
            )
        })
        .await
        .unwrap();
        let first_report = first_transport.finish(first_server).await;
        let second_report = second_transport.finish(second_server).await;
        assert_eq!(first_result, Err(ServiceFailure::AuthorizationDenied));
        assert_eq!(second_result, Ok(AccountRuntimeReply::Reserved));
        assert_eq!(first_report.calls, vec![first_call]);
        assert_eq!(second_report.calls, vec![second_call]);
        assert!(
            second_journal
                .ordinary_use_allowed(&second.binding)
                .unwrap()
        );
    }
}
