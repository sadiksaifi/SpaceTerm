#[path = "support/allocation_probe.rs"]
mod allocation_probe;
#[path = "support/publication.rs"]
mod publication;

use spaceterm_core::{authority::*, identity::*};
use spaceterm_server::{
    contracts::{ConnectionAccessSnapshot, OutboundMessage},
    egress::{ConnectionEgress, EgressBudget},
    publication::PublicationHub,
};

fn access(
    snapshot: &spaceterm_core::organization::OrganizationSnapshot,
) -> ConnectionAccessSnapshot {
    let server = snapshot.state().server;
    let client = ClientId::new(server, 1).unwrap();
    ConnectionAccessSnapshot {
        server,
        device: DeviceId::new([2; 16]),
        pairing: PairingId::new(server, 1).unwrap(),
        client,
        binding: ConnectionBinding::new(client, ConnectionGeneration::new(1)),
        epoch: snapshot.epoch(),
        reconciled_revision: OrganizationRevision::new(0),
        effects_enabled: false,
    }
}

#[test]
fn snapshot_planning_scratch_is_bounded_before_page_conversion() {
    for snapshot in [
        publication::large_snapshot(),
        publication::snapshot_with_workspaces(256),
    ] {
        let count = snapshot.state().workspaces.len() as u64;
        spaceterm_core::reducer::RetainedOrganization::new(
            snapshot.state().clone(),
            IdentityHighWater {
                workspace: count,
                tab: count,
                pane: count * 4,
                creation_ordinal: count * 6,
                client: 1,
                ..Default::default()
            },
            spaceterm_core::organization::ServerDirectory::new(
                snapshot.state().server,
                b"/home".to_vec(),
            )
            .unwrap(),
        )
        .validate(spaceterm_core::reducer::OrganizationLimits::default())
        .unwrap();
        for major in [1, 2] {
            let mut hub = PublicationHub::new(snapshot.clone()).unwrap();
            let (mut transfer, peak) =
                allocation_probe::measure(|| hub.begin_snapshot(major, 1, 1, 64 * 1024).unwrap());
            assert!(
                peak <= 64 * 1024,
                "snapshot planning allocated {peak} bytes before egress reservation"
            );
            let global = EgressBudget::new();
            let mut queue = ConnectionEgress::new(global.clone());
            let mut pages = 0;
            while let Some(page) = transfer.next_page().unwrap() {
                assert_eq!(global.usage().bytes, 0);
                queue
                    .enqueue(OutboundMessage::SnapshotPart(page), None)
                    .unwrap();
                let send = queue.take_next(&access(&snapshot)).unwrap();
                let charged = global.usage().bytes;
                let (mut converted, peak) = allocation_probe::measure(|| {
                    let mut payload = send.snapshot_payload().unwrap();
                    payload.encode().unwrap();
                    payload
                });
                assert!(converted.encode().unwrap().len() <= 64 * 1024);
                assert!(
                    peak <= charged,
                    "page scratch {peak} exceeds reservation {charged}"
                );
                queue.complete_send(converted.id());
                assert_eq!(
                    global.usage().bytes,
                    charged,
                    "converted DTO must retain its charge"
                );
                drop(converted);
                assert_eq!(global.usage().bytes, 0);
                pages += 1;
            }
            assert!(pages > 1);
        }
    }
}

#[path = "support/job_a.rs"]
mod actor_fixture;

// This is an actor-port dispatch regression, not the real-provider acceptance
// vertical. Recovery, ledger and reducer composition are tested separately.
#[tokio::test]
async fn ordinary_wire_dispatch_reaches_actor_and_preserves_lost_reply_identity() {
    use actor_fixture as f;
    use spaceterm_core::{operations::*, organization::*};
    use spaceterm_protocol::{
        codec::Payload,
        convert::operations,
        wire::{v1, v2},
    };
    use spaceterm_server::{
        api::OperationResponse,
        protocol_dispatch::{ActorOrdinaryDispatch, OrdinaryDispatchPort},
        service::RecoveredServer,
    };
    for major in [1, 2] {
        let fixture = f::Fixture::new();
        let principal = fixture.principal(1, 1);
        let host = fixture.host.clone();
        let codec = fixture.codec.clone();
        let database = fixture.db.clone();
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        f::connect(&handle, principal).await;
        let dispatch = ActorOrdinaryDispatch::new(codec.clone());
        let request = f::request(
            &codec,
            1,
            1,
            Command::RenameWorkspace {
                workspace: f::workspace(),
                expected_version: FieldVersion::new(1),
                draft: NameDraft::Custom(NameText::new("from wire".into()).unwrap()),
            },
        );
        let payload = match major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Operation(
                    operations::v1::encode_request(&request).unwrap(),
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Operation(
                    operations::v2::encode_request(&request).unwrap(),
                )),
            }),
        };
        let reply = dispatch
            .dispatch(principal, payload, handle.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            handle.snapshot().state().workspaces[0]
                .name
                .value
                .display
                .as_str(),
            "from wire"
        );
        let OperationResponse::Completed {
            revision, outcome, ..
        } = handle.lookup(principal, request.binding()).await.unwrap()
        else {
            panic!("completed");
        };
        match reply {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Outcome(value)),
            }) => assert_eq!(
                operations::v1::decode_outcome(&value, request.binding().id).unwrap(),
                (revision, outcome.clone())
            ),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Outcome(value)),
            }) => assert_eq!(
                operations::v2::decode_outcome(&value, request.binding().id).unwrap(),
                (revision, outcome.clone())
            ),
            _ => panic!("outcome"),
        }
        let commits = database.lock().unwrap().transactions.len();
        let lookup = match major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::OperationLookupRequest(
                    operations::v1::encode_lookup_request(request.binding()),
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OperationLookupRequest(
                    operations::v2::encode_lookup_request(request.binding()),
                )),
            }),
        };
        let reply = dispatch
            .dispatch(principal, lookup, handle.clone())
            .await
            .unwrap()
            .unwrap();
        let expected = OperationLookup::Completed {
            result: outcome,
            publication_revision: revision,
        };
        match reply {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::OperationLookupResult(value)),
            }) => assert_eq!(
                operations::v1::decode_lookup_result(&value, request.binding()).unwrap(),
                expected
            ),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OperationLookupResult(value)),
            }) => assert_eq!(
                operations::v2::decode_lookup_result(&value, request.binding()).unwrap(),
                expected
            ),
            _ => panic!("lookup"),
        }
        assert_eq!(database.lock().unwrap().transactions.len(), commits);
        f::stop(&handle, host).await.unwrap();
        server.wait().await.unwrap();
    }
}

#[path = "support/job_b.rs"]
mod authentication_fixture;

#[tokio::test(start_paused = true)]
async fn readiness_deadline_race_never_releases_admission_before_native_close() {
    use spaceterm_host::transport::{
        MessageTransport, PeerFacts, PendingConnection, TransportError, TransportFuture,
        TransportRoute,
    };
    use spaceterm_protocol::authentication::Purpose;
    use spaceterm_server::connection::Authenticator;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::sync::{Mutex, Notify, Semaphore, mpsc};

    struct Native {
        sent: mpsc::Sender<Vec<u8>>,
        received: Mutex<mpsc::Receiver<Vec<u8>>>,
        closes: Arc<AtomicUsize>,
        expired: Arc<Notify>,
        closing: Arc<Notify>,
        release: Arc<Semaphore>,
        destroyed: Arc<AtomicBool>,
        server: bool,
        uid: u32,
    }
    impl Drop for Native {
        fn drop(&mut self) {
            if self.server {
                self.destroyed.store(true, Ordering::SeqCst);
            }
        }
    }
    impl MessageTransport for Native {
        fn peer_facts(&self) -> PeerFacts {
            PeerFacts {
                route: TransportRoute::PrivateIpc,
                local_uid: Some(self.uid),
                local_pid: None,
                pinned_server_key: None,
            }
        }
        fn send<'a>(&'a self, value: &'a [u8]) -> TransportFuture<'a, ()> {
            Box::pin(async move {
                self.sent
                    .send(value.to_vec())
                    .await
                    .map_err(|_| TransportError::Closed)?;
                Ok(())
            })
        }
        fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
            Box::pin(async { Ok(self.received.lock().await.recv().await) })
        }
        fn close(&self) -> TransportFuture<'_, ()> {
            Box::pin(async {
                let call = self.closes.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    // The native timer's failed close proves no teardown.
                    self.expired.notify_one();
                    return Err(TransportError::Unavailable);
                }
                self.closing.notify_one();
                self.release.acquire().await.unwrap().forget();
                Err(TransportError::Unavailable)
            })
        }
    }
    let fixture = Arc::new(authentication_fixture::Fixture::new());
    let registration = Arc::new(Semaphore::new(0));
    *fixture.authority.register_gate.lock().unwrap() = Some(registration.clone());
    // Observe the actual native account instead of assuming macOS UID 501.
    tokio::time::resume();
    let directory = authentication_fixture::Directory::new();
    let listener = directory.listener();
    let second_client = directory.connect().await;
    let owning_uid = second_client.peer_facts().local_uid.unwrap();
    tokio::time::pause();
    let mut config = fixture.config(owning_uid);
    config.unauthenticated_limit = 2;
    config.authenticated_limit = 1;
    let authenticator = Authenticator::new(config).unwrap();
    let native_slots = Arc::new(Semaphore::new(1));
    let (to_client, client_messages) = mpsc::channel(2);
    let (to_server, server_messages) = mpsc::channel(2);
    let closing = Arc::new(Notify::new());
    let expired = Arc::new(Notify::new());
    let release = Arc::new(Semaphore::new(0));
    let destroyed = Arc::new(AtomicBool::new(false));
    let closes = Arc::new(AtomicUsize::new(0));
    let server = Native {
        sent: to_client,
        received: Mutex::new(server_messages),
        closes: closes.clone(),
        expired: expired.clone(),
        closing: closing.clone(),
        release: release.clone(),
        destroyed: destroyed.clone(),
        server: true,
        uid: owning_uid,
    };
    let client = Native {
        sent: to_server,
        received: Mutex::new(client_messages),
        closes: Arc::new(AtomicUsize::new(0)),
        expired: expired.clone(),
        closing: closing.clone(),
        release: release.clone(),
        destroyed: destroyed.clone(),
        server: false,
        uid: owning_uid,
    };
    let pending = PendingConnection::test_fixture(
        server,
        native_slots.clone().try_acquire_owned().unwrap(),
        tokio::time::Instant::now() + Duration::from_secs(1),
    );
    let owner = authenticator.clone();
    let permit = authenticator.try_admit().unwrap();
    let second_permit = authenticator.try_admit().unwrap();
    let authentication = tokio::spawn(async move { owner.authenticate(pending, permit).await });
    let client_fixture = fixture.clone();
    let client_handshake = tokio::spawn(async move {
        client_fixture
            .client_handshake(&client, 1, 2, Purpose::Ordinary)
            .await
    });
    // Registration owns an authenticated slot but cannot complete readiness.
    // Expire only the native deadline while that actor boundary is held.
    fixture.authority.register_entered.notified().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    expired.notified().await;
    assert!(native_slots.clone().try_acquire_owned().is_err());
    registration.add_permits(1);
    closing.notified().await;
    assert_eq!(closes.load(Ordering::SeqCst), 2);
    assert!(!authentication.is_finished());
    assert!(!destroyed.load(Ordering::SeqCst));
    assert!(
        authenticator.try_admit().is_err(),
        "server admission released ahead of native teardown"
    );
    assert!(
        native_slots.clone().try_acquire_owned().is_err(),
        "native admission released after an unconfirmed close"
    );
    // A different verified Client cannot borrow the authenticated slot while
    // the first connection's native readiness finalization is still owned.
    // Native socket readiness comes from the OS, not Tokio's virtual clock.
    // Disable paused-clock auto-advance only for this independent IPC exchange.
    tokio::time::resume();
    let second_pending = listener.accept().await.unwrap();
    let (second_server, second_client) = tokio::join!(
        authenticator.authenticate(second_pending, second_permit),
        fixture.client_handshake(&second_client, 2, 2, Purpose::Ordinary)
    );
    assert_eq!(
        second_server.err().unwrap().class,
        spaceterm_server::contracts::ServerFailureClass::Capacity
    );
    assert!(second_client.is_err());
    assert_eq!(fixture.authority.registered.load(Ordering::SeqCst), 1);
    tokio::time::pause();
    assert!(!authentication.is_finished());
    assert!(!destroyed.load(Ordering::SeqCst));
    release.add_permits(1);
    assert!(authentication.await.unwrap().is_err());
    assert!(client_handshake.await.unwrap().is_err());
    assert!(destroyed.load(Ordering::SeqCst));
    assert!(authenticator.try_admit().is_ok());
    assert!(native_slots.try_acquire_owned().is_ok());
    assert!(fixture.authority.clients.lock().unwrap().is_empty());
    // A native close failure remains observable by retained finalization.
    assert!(authenticator.close().await.is_err());
}

struct ClosingListeners {
    opened: tokio::sync::Notify,
    closing: std::sync::Arc<actor_fixture::Gate>,
    closed: std::sync::atomic::AtomicBool,
}
impl spaceterm_server::api::ListenerLifecyclePort for ClosingListeners {
    fn open(
        &self,
        _: spaceterm_server::api::ServerHandle,
        _: std::sync::Arc<spaceterm_core::organization::OrganizationSnapshot>,
    ) -> spaceterm_server::contracts::ServerFuture<
        '_,
        Result<(), spaceterm_server::contracts::ServerFailure>,
    > {
        Box::pin(async move {
            self.opened.notify_one();
            Ok(())
        })
    }
    fn apply_remote_policy(
        &self,
        _: spaceterm_core::organization::Versioned<
            spaceterm_core::remote_access::RemoteAccessPolicy,
        >,
        _: spaceterm_core::storage::CommitReceipt,
    ) -> spaceterm_server::contracts::ServerFuture<
        '_,
        Result<(), spaceterm_server::contracts::ServerFailure>,
    > {
        Box::pin(async { Ok(()) })
    }
    fn close(
        &self,
    ) -> spaceterm_server::contracts::ServerFuture<
        '_,
        Result<(), spaceterm_server::contracts::ServerFailure>,
    > {
        Box::pin(async move {
            if !self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                self.closing.wait().await;
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn shutdown_awaits_listener_closure_before_cancelling_reads_and_draining_writes() {
    use spaceterm_server::{api::ListenerFactories, service::RecoveredServer};
    use std::{
        sync::{Arc, atomic::Ordering},
        time::Duration,
    };
    let fixture = actor_fixture::Fixture::new();
    let principal = fixture.principal(1, 1);
    let host = fixture.host.clone();
    let codec = fixture.codec.clone();
    let external = fixture.external.clone();
    let factory = fixture.factory.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    actor_fixture::connect(&handle, principal).await;
    let grant = actor_fixture::acquire(&handle, principal, &codec).await;
    actor_fixture::start(&handle, principal, &codec, grant).await;
    let listeners = Arc::new(ClosingListeners {
        opened: tokio::sync::Notify::new(),
        closing: actor_fixture::Gate::new(),
        closed: std::sync::atomic::AtomicBool::new(false),
    });
    let started_writes = actor_fixture::Gate::new();
    *external.gate.lock().unwrap() = Some(started_writes.clone());
    let run_listeners = listeners.clone();
    let running = tokio::spawn(async move {
        server
            .run(ListenerFactories {
                lifecycle: run_listeners,
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), listeners.opened.notified())
        .await
        .unwrap();
    let stop_handle = handle.clone();
    let stopping = tokio::spawn(async move { actor_fixture::stop(&stop_handle, host).await });
    tokio::time::timeout(Duration::from_secs(5), listeners.closing.entered())
        .await
        .unwrap();
    assert_eq!(external.reads_cancelled.load(Ordering::SeqCst), 0);
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        0
    );
    listeners.closing.release();
    tokio::time::timeout(Duration::from_secs(5), started_writes.entered())
        .await
        .unwrap();
    assert_eq!(external.reads_cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        0
    );
    assert!(!stopping.is_finished());
    started_writes.release();
    let report = stopping.await.unwrap().unwrap();
    assert!(report.listeners_closed && report.started_writes_finished && report.cleanup_confirmed);
    running.await.unwrap().unwrap();
    assert_eq!(
        factory.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        1
    );
}

struct Key(ring::signature::Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Self {
        Self(ring::signature::Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
    fn pin(&self) -> spaceterm_protocol::authentication::ServerPin {
        use spaceterm_protocol::authentication::SigningKey;
        spaceterm_protocol::authentication::ServerPin {
            identity: [11; 16],
            public_key: self.public_key(),
        }
    }
}
impl spaceterm_protocol::authentication::SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        use ring::signature::KeyPair;
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, value: &[u8]) -> Result<[u8; 64], spaceterm_protocol::ProtocolError> {
        Ok(self.0.sign(value).as_ref().try_into().unwrap())
    }
}
impl spaceterm_server::contracts::ServerCredentialPort for Key {}

#[allow(dead_code)]
#[path = "store_pairing_composition/support.rs"]
mod physical;
struct PhysicalStartup;
impl spaceterm_server::api::StartupAuthorityPort for PhysicalStartup {
    fn validate_startup(&self) -> Result<ServerId, spaceterm_server::contracts::ServerFailure> {
        Ok(physical::identity().server)
    }
    fn validate_credentials(
        &self,
        key: &dyn spaceterm_server::contracts::ServerCredentialPort,
        _: ServerId,
    ) -> Result<(), spaceterm_server::contracts::ServerFailure> {
        assert_eq!(key.public_key(), physical::identity().server_public_key);
        Ok(())
    }
}
struct PhysicalPairings(spaceterm_core::pairing::policy::PairingSnapshot);
impl spaceterm_server::pairing::protocol_lookup::PairingSnapshotPort for &PhysicalPairings {
    fn snapshot(
        &self,
    ) -> Result<spaceterm_core::pairing::policy::PairingSnapshot, spaceterm_protocol::ProtocolError>
    {
        Ok(self.0.clone())
    }
}
impl spaceterm_protocol::authentication::PairingLookup for PhysicalPairings {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<spaceterm_protocol::authentication::PairingRecord, spaceterm_protocol::ProtocolError>
    {
        spaceterm_server::pairing::protocol_lookup::PairingProtocolLookup::new(self)
            .lookup(server, pairing, client)
    }
}
impl spaceterm_server::contracts::PairingAuthorizationPort for PhysicalPairings {}
struct RejectRestricted;
impl spaceterm_server::protocol_dispatch::RestrictedDispatchPort for RejectRestricted {
    fn dispatch(
        &self,
        _: spaceterm_protocol::authentication::Principal,
        _: spaceterm_protocol::wire::common::RestrictedMessage,
    ) -> spaceterm_server::contracts::ServerFuture<
        '_,
        Result<
            spaceterm_protocol::wire::common::RestrictedMessage,
            spaceterm_server::contracts::ServerFailure,
        >,
    > {
        Box::pin(async {
            Err(spaceterm_server::failure::failure(
                spaceterm_server::contracts::ServerFailureClass::Authentication,
            ))
        })
    }
}

#[tokio::test]
async fn committed_organization_reaches_an_authenticated_client_without_a_focused_view() {
    for major in [1, 2] {
        authenticated_publication_case(major, PublicationCase::Subscription).await;
    }
}

#[tokio::test]
async fn live_reply_capacity_rejection_precedes_real_ledger_admission() {
    for major in [1, 2] {
        authenticated_publication_case(major, PublicationCase::ReplyPressure).await;
    }
}

#[tokio::test]
async fn negotiated_organization_pages_fit_reduced_management_and_frame_limits() {
    for major in [1, 2] {
        for limits in [
            spaceterm_protocol::limits::NegotiatedLimits {
                management_bytes: 4096,
                ..Default::default()
            },
            spaceterm_protocol::limits::NegotiatedLimits {
                frame_bytes: 4096,
                management_bytes: 4096,
                ..Default::default()
            },
        ] {
            authenticated_publication_case(major, PublicationCase::Paging(limits)).await;
        }
    }
}

#[tokio::test]
async fn minimum_negotiated_frame_delivers_an_authenticated_empty_organization() {
    for major in [1, 2] {
        authenticated_publication_case(major, PublicationCase::MinimumFrame).await;
    }
}

#[tokio::test]
async fn cancelled_partial_snapshot_requires_fresh_generation_and_bounds_connection_resyncs() {
    for major in [1, 2] {
        authenticated_publication_case(major, PublicationCase::PinCancellation).await;
        authenticated_publication_case(major, PublicationCase::StaleResyncGeneration).await;
    }
}

#[tokio::test]
async fn progressing_native_writes_can_outlive_the_five_second_send_window() {
    for major in [1, 2] {
        authenticated_publication_case(major, PublicationCase::ProgressingWrite).await;
    }
}

#[tokio::test]
async fn stalled_native_writes_close_their_connection_after_five_seconds() {
    for major in [1, 2] {
        authenticated_publication_case(major, PublicationCase::StalledWrite).await;
    }
}

#[tokio::test]
async fn idle_authenticated_connections_have_no_write_deadline() {
    for major in [1, 2] {
        authenticated_publication_case(major, PublicationCase::Idle).await;
    }
}

struct WriteGate {
    transport: std::sync::Arc<dyn spaceterm_host::transport::MessageTransport>,
    progress: tokio::sync::watch::Sender<tokio::time::Instant>,
    stalled: bool,
}
impl spaceterm_host::transport::MessageTransport for WriteGate {
    fn peer_facts(&self) -> spaceterm_host::transport::PeerFacts {
        self.transport.peer_facts()
    }
    fn write_progress(&self) -> Option<tokio::sync::watch::Receiver<tokio::time::Instant>> {
        Some(self.progress.subscribe())
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> spaceterm_host::transport::TransportFuture<'a, ()> {
        Box::pin(async move {
            // Inject native write completions at the transport boundary. Separate
            // host tests verify that only successful socket writes feed this clock.
            if self.stalled {
                std::future::pending::<()>().await;
            }
            for _ in 0..4 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                self.progress.send_replace(tokio::time::Instant::now());
            }
            self.transport.send(bytes).await
        })
    }
    fn recv(&self) -> spaceterm_host::transport::TransportFuture<'_, Option<Vec<u8>>> {
        self.transport.recv()
    }
    fn close(&self) -> spaceterm_host::transport::TransportFuture<'_, ()> {
        self.transport.close()
    }
}

enum PublicationCase {
    Subscription,
    ReplyPressure,
    Paging(spaceterm_protocol::limits::NegotiatedLimits),
    MinimumFrame,
    ProgressingWrite,
    StalledWrite,
    Idle,
    PinCancellation,
    StaleResyncGeneration,
}

struct PageGate {
    transport: std::sync::Arc<dyn spaceterm_host::transport::MessageTransport>,
    armed: std::sync::atomic::AtomicBool,
    blocked: tokio::sync::Notify,
    released: tokio::sync::Semaphore,
}
impl spaceterm_host::transport::MessageTransport for PageGate {
    fn peer_facts(&self) -> spaceterm_host::transport::PeerFacts {
        self.transport.peer_facts()
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> spaceterm_host::transport::TransportFuture<'a, ()> {
        Box::pin(async move {
            self.transport.send(bytes).await?;
            if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                self.blocked.notify_one();
                self.released.acquire().await.unwrap().forget();
            }
            Ok(())
        })
    }
    fn recv(&self) -> spaceterm_host::transport::TransportFuture<'_, Option<Vec<u8>>> {
        self.transport.recv()
    }
    fn close(&self) -> spaceterm_host::transport::TransportFuture<'_, ()> {
        self.transport.close()
    }
}

async fn cancel_snapshot_base(
    handle: &spaceterm_server::api::ServerHandle,
    principal: spaceterm_server::api::VerifiedPrincipal,
    stream: spaceterm_core::identity::OperationStreamId,
    major: u32,
    first_sequence: u64,
) {
    use spaceterm_core::{
        operation_ledger::canonical::CanonicalCodec,
        operation_ledger_contracts::CanonicalOperationRequest, operations::*, organization::*,
    };
    // The test holds a completed native page write while creating later bases.
    // Physical SQLite latency must not turn that barrier into a slow-consumer
    // test. A retained blocking task prevents paused-time auto-advance while
    // the actor's separate store thread commits these mutations.
    let (release_clock, clock_gate) = std::sync::mpsc::channel();
    let clock_hold = tokio::task::spawn_blocking(move || {
        let _ = clock_gate.recv();
    });
    tokio::time::pause();
    let mut pins = Vec::new();
    for offset in 0..3 {
        let workspace = handle.snapshot().state().workspaces[0].clone();
        let request = CanonicalOperationRequest::new(
            OperationId::new(
                stream,
                OperationSequence::new(first_sequence + offset).unwrap(),
            ),
            Command::RenameWorkspace {
                workspace: workspace.id,
                expected_version: workspace.name.version,
                draft: NameDraft::Custom(
                    NameText::new(format!("revision {}", first_sequence + offset)).unwrap(),
                ),
            },
            &CanonicalCodec,
        )
        .unwrap();
        handle.submit(principal, request).await.unwrap();
        if offset < 2 {
            pins.push(
                handle
                    .organization_snapshot(principal, major, 90 + offset, 1, 4096)
                    .await
                    .unwrap(),
            );
        }
    }
    tokio::time::resume();
    release_clock.send(()).unwrap();
    clock_hold.await.unwrap();
}

async fn stop_physical_server(
    handle: &spaceterm_server::api::ServerHandle,
    host: std::sync::Arc<actor_fixture::Host>,
) -> spaceterm_server::shutdown::ShutdownReport {
    // These tests close their wire peers before durably stopping SQLite. The
    // fake-store fixture's ten-second budget is not a transport deadline and
    // cannot bound physical sync latency on the shared build machine.
    tokio::time::timeout(
        std::time::Duration::from_secs(120),
        handle.shutdown(spaceterm_server::shutdown::TrustedShutdownRequest { authority: host }),
    )
    .await
    .expect("bounded physical-store test shutdown")
    .unwrap()
}

async fn authenticated_publication_case(major: u32, case: PublicationCase) {
    let progressing_write = matches!(case, PublicationCase::ProgressingWrite);
    let stalled_write = matches!(case, PublicationCase::StalledWrite);
    let idle = matches!(case, PublicationCase::Idle);
    let empty_base = matches!(
        case,
        PublicationCase::MinimumFrame
            | PublicationCase::ProgressingWrite
            | PublicationCase::StalledWrite
            | PublicationCase::Idle
    );
    let deny_reply_capacity = matches!(case, PublicationCase::ReplyPressure);
    let stale_resync_generation = matches!(case, PublicationCase::StaleResyncGeneration);
    let cancel_pin = matches!(
        case,
        PublicationCase::PinCancellation | PublicationCase::StaleResyncGeneration
    );
    let page_limits = match case {
        PublicationCase::Paging(limits) => Some(limits),
        PublicationCase::MinimumFrame
        | PublicationCase::ProgressingWrite
        | PublicationCase::StalledWrite
        | PublicationCase::Idle => Some(spaceterm_protocol::limits::NegotiatedLimits {
            frame_bytes: 1024,
            management_bytes: 1024,
            ..Default::default()
        }),
        PublicationCase::PinCancellation | PublicationCase::StaleResyncGeneration => {
            Some(spaceterm_protocol::limits::NegotiatedLimits {
                management_bytes: 4096,
                ..Default::default()
            })
        }
        _ => None,
    };
    use spaceterm_core::{
        operation_ledger::canonical::CanonicalCodec,
        operation_ledger_contracts::CanonicalOperationRequest, operations::*, organization::*,
    };
    use spaceterm_host::transport::MessageTransport;
    use spaceterm_protocol::{
        authentication::{self, Established, HandshakeConfig, Purpose},
        codec::Payload,
        convert::organization,
        wire::{v1, v2},
    };
    use spaceterm_server::{
        adapters::core::CorePorts,
        connection::{AuthenticationConfig, Authenticator, PeerConnection},
        listeners::ConnectionDispatchPort,
        protocol_dispatch::{ActorOrdinaryDispatch, ProtocolDispatcher},
        service::RecoveredServer,
    };
    use std::{sync::Arc, time::Duration};
    let directory = physical::Directory::new();
    let mut storage = physical::Fixture::new(&directory.0);
    storage.enroll(42);
    let recovered = storage.recovered();
    let streams = recovered.pairing.as_ref().unwrap().parts().authorizations[0]
        .parts()
        .clients[0]
        .parts();
    let stream = streams.ordinary;
    let pairing = Arc::new(PhysicalPairings(storage.snapshot()));
    let keys = Arc::new(Key::new(71));
    let mut native = actor_fixture::Fixture::new();
    let host = native.host.clone();
    native.config.host.authority = Arc::new(PhysicalStartup);
    native.config.core = CorePorts::production(
        physical::identity(),
        ServerDirectory::new(
            physical::identity().server,
            directory.0.as_os_str().as_encoded_bytes().to_vec(),
        )
        .unwrap(),
        Arc::new(actor_fixture::Clock),
    )
    .unwrap();
    native.dependencies.store = Box::new(storage.store);
    native.dependencies.credentials = keys.clone();
    native.dependencies.pairing = pairing.clone();
    let server = RecoveredServer::recover(native.config, native.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    let ipc = authentication_fixture::Directory::new();
    let listener = ipc.listener();
    let client = Arc::new(ipc.connect().await);
    let handshake = HandshakeConfig {
        majors: vec![major],
        limits: page_limits.unwrap_or_default(),
        capabilities: vec![
            spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
            spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
        ],
    };
    let authenticator = Authenticator::new(AuthenticationConfig {
        server: physical::identity().server,
        epoch: handle.snapshot().epoch(),
        owning_uid: client.peer_facts().local_uid.unwrap(),
        handshake: handshake.clone(),
        credentials: keys.clone(),
        pairing,
        authority: Arc::new(handle.clone()),
        unauthenticated_limit: 16,
        authenticated_limit: 32,
        deadline: Duration::from_secs(5),
    })
    .unwrap();
    let pending = listener.accept().await.unwrap();
    let claimed = authentication::Principal {
        server: physical::identity().server.bytes(),
        device: recovered.pairing.as_ref().unwrap().parts().enrollments[0]
            .parts()
            .target
            .parts()
            .device
            .bytes(),
        pairing: actor_fixture::local(stream.pairing().local()),
        client: actor_fixture::local(stream.client().local()),
    };
    let client_handshake = async {
        let (handshake, hello) = authentication::ClientHandshake::begin(
            keys.pin(),
            claimed,
            Purpose::Ordinary,
            handshake,
            Arc::new(Key::new(43)),
        )
        .unwrap();
        client.send(&hello).await.unwrap();
        let (handshake, proof) = handshake
            .receive_server(&client.recv().await.unwrap().unwrap())
            .unwrap();
        client.send(&proof).await.unwrap();
        let Established::Ordinary(codec) = handshake
            .receive_ready(&client.recv().await.unwrap().unwrap())
            .unwrap()
        else {
            panic!("ordinary");
        };
        codec
    };
    let (peer, mut codec) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        client_handshake
    );
    let mut peer = peer.unwrap();
    let PeerConnection::Ordinary(session) = &peer.connection else {
        panic!("ordinary");
    };
    let principal = session.principal;
    if page_limits.is_some() && !empty_base {
        handle
            .reconcile(principal, handle.snapshot().state().revision)
            .await
            .unwrap();
        for sequence in 1..=24 {
            let request = CanonicalOperationRequest::new(
                OperationId::new(stream, OperationSequence::new(sequence).unwrap()),
                Command::CreateWorkspace {
                    server: physical::identity().server,
                    source: WorkspaceCreationSource::ServerHome {
                        server: physical::identity().server,
                    },
                    name: NameDraft::Custom(
                        NameText::new(format!("workspace {sequence}")).unwrap(),
                    ),
                },
                &CanonicalCodec,
            )
            .unwrap();
            handle.submit(principal, request).await.unwrap();
        }
    }
    if progressing_write || stalled_write {
        peer.transport = Arc::new(WriteGate {
            transport: peer.transport.clone(),
            progress: tokio::sync::watch::channel(tokio::time::Instant::now()).0,
            stalled: stalled_write,
        });
    }
    let page_gate = cancel_pin.then(|| {
        Arc::new(PageGate {
            transport: peer.transport.clone(),
            armed: std::sync::atomic::AtomicBool::new(true),
            blocked: tokio::sync::Notify::new(),
            released: tokio::sync::Semaphore::new(0),
        })
    });
    if let Some(gate) = &page_gate {
        peer.transport = gate.clone();
    }
    let (_, status) = tokio::sync::watch::channel(authentication_fixture::status(1));
    let budget = EgressBudget::new();
    let dispatcher = ProtocolDispatcher::new(
        authenticator.clone(),
        status,
        Arc::new(ActorOrdinaryDispatch::new(Arc::new(CanonicalCodec))),
        Arc::new(RejectRestricted),
    )
    .with_egress_budget(budget.clone());
    let dispatch_handle = handle.clone();
    let running = tokio::spawn(async move {
        let result = dispatcher.run(&mut peer, dispatch_handle).await;
        peer.close().await.unwrap();
        result
    });
    let query = match major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::OrganizationQuery(
                v1::OrganizationQuery {
                    request: 1,
                    request_generation: 1,
                    ..Default::default()
                },
            )),
        }),
        _ => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::OrganizationQuery(
                v2::OrganizationQuery {
                    request: 1,
                    request_generation: 1,
                    ..Default::default()
                },
            )),
        }),
    };
    if idle {
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert!(!running.is_finished());
    }
    let write_started = tokio::time::Instant::now();
    client.send(&codec.encode(&query).unwrap()).await.unwrap();
    if stalled_write {
        assert!(
            tokio::time::timeout(Duration::from_secs(7), client.recv())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        assert!(write_started.elapsed() >= Duration::from_secs(5));
        assert_eq!(
            running.await.unwrap().unwrap_err().class,
            spaceterm_server::contracts::ServerFailureClass::Capacity
        );
        // The actor and listener remain available; only this authenticated peer closed.
        assert_eq!(handle.snapshot().state().workspaces.len(), 0);
        let another = ipc.connect().await;
        let pending = listener.accept().await.unwrap();
        assert_eq!(
            pending.peer_facts().local_uid,
            another.peer_facts().local_uid
        );
        another.close().await.unwrap();
        drop(pending);
        authenticator.close().await.unwrap();
        stop_physical_server(&handle, host).await;
        server.wait().await.unwrap();
        listener.close().unwrap();
        assert_eq!(budget.usage().bytes, 0);
        return;
    }
    if let Some(gate) = page_gate {
        use spaceterm_client::replica::{
            OrganizationCursor, OrganizationResidency, PublicationStamp, Replica, ReplicaUpdate,
            SnapshotPart,
        };
        let mut replica = Replica::new(
            physical::identity().server,
            Arc::new(OrganizationResidency::new(Default::default()).unwrap()),
        );
        let mut token = replica.begin_resync(handle.snapshot().epoch()).unwrap();
        let decode_part = |payload| match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::OrganizationSnapshot(value)),
            }) => organization::v1::decode_snapshot_part(&value).unwrap(),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationSnapshot(value)),
            }) => organization::v2::decode_snapshot_part(&value).unwrap(),
            _ => panic!("organization snapshot"),
        };
        let client_part = |part: organization::SnapshotPart| SnapshotPart {
            stamp: PublicationStamp {
                request: part.part.request,
                request_generation: part.part.generation,
                transfer: part.part.transfer,
                index: part.part.index,
                total: part.part.total,
                cursor: OrganizationCursor {
                    server: part.state.server,
                    epoch: part.part.epoch,
                    revision: part.part.revision,
                },
            },
            state: part.state,
            runtime: part.runtime,
        };
        let bytes = client.recv().await.unwrap().unwrap();
        let part = decode_part(codec.decode(&bytes).unwrap());
        assert!(part.part.total > 1);
        assert_eq!(
            replica.stage_snapshot(token, client_part(part)).unwrap(),
            ReplicaUpdate::Staged
        );
        gate.blocked.notified().await;
        cancel_snapshot_base(&handle, principal, stream, major, 25).await;
        gate.released.add_permits(1);
        let bytes = tokio::time::timeout(Duration::from_secs(5), client.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match codec.decode(&bytes).unwrap() {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::OrganizationResync(marker)),
            }) => assert_eq!((marker.request, marker.request_generation), (1, 1)),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationResync(marker)),
            }) => assert_eq!((marker.request, marker.request_generation), (1, 1)),
            _ => panic!(
                "a cancelled staged transfer needs an explicit resync, not replacement page zero"
            ),
        }
        if stale_resync_generation {
            let query = match major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::OrganizationQuery(
                        v1::OrganizationQuery {
                            request: 2,
                            request_generation: 1,
                            ..Default::default()
                        },
                    )),
                }),
                _ => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::OrganizationQuery(
                        v2::OrganizationQuery {
                            request: 2,
                            request_generation: 1,
                            ..Default::default()
                        },
                    )),
                }),
            };
            client.send(&codec.encode(&query).unwrap()).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(5), client.recv())
                    .await
                    .unwrap()
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                running.await.unwrap().unwrap_err().class,
                spaceterm_server::contracts::ServerFailureClass::StaleAuthority
            );
            authenticator.close().await.unwrap();
            stop_physical_server(&handle, host).await;
            server.wait().await.unwrap();
            listener.close().unwrap();
            assert_eq!(budget.usage().bytes, 0);
            return;
        }
        token = replica.begin_resync(handle.snapshot().epoch()).unwrap();
        let query = match major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::OrganizationQuery(
                    v1::OrganizationQuery {
                        request: 2,
                        request_generation: 2,
                        ..Default::default()
                    },
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationQuery(
                    v2::OrganizationQuery {
                        request: 2,
                        request_generation: 2,
                        ..Default::default()
                    },
                )),
            }),
        };
        client.send(&codec.encode(&query).unwrap()).await.unwrap();
        loop {
            let bytes = tokio::time::timeout(Duration::from_secs(5), client.recv())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let part = decode_part(codec.decode(&bytes).unwrap());
            match replica.stage_snapshot(token, client_part(part)).unwrap() {
                ReplicaUpdate::Installed => break,
                ReplicaUpdate::Staged => assert!(!replica.subscribe().borrow().is_ready()),
                _ => panic!("fresh transfer"),
            }
        }
        let installed = replica
            .subscribe()
            .borrow()
            .retained()
            .unwrap()
            .organization
            .clone();
        assert_eq!(installed, *handle.snapshot());
        // A completed recovery does not reset the connection's resync allowance.
        gate.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let query = match major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::OrganizationQuery(
                    v1::OrganizationQuery {
                        request: 3,
                        request_generation: 3,
                        ..Default::default()
                    },
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationQuery(
                    v2::OrganizationQuery {
                        request: 3,
                        request_generation: 3,
                        ..Default::default()
                    },
                )),
            }),
        };
        client.send(&codec.encode(&query).unwrap()).await.unwrap();
        let bytes = client.recv().await.unwrap().unwrap();
        assert_eq!(decode_part(codec.decode(&bytes).unwrap()).part.index, 0);
        gate.blocked.notified().await;
        cancel_snapshot_base(&handle, principal, stream, major, 28).await;
        gate.released.add_permits(1);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), client.recv())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        assert_eq!(
            running.await.unwrap().unwrap_err().class,
            spaceterm_server::contracts::ServerFailureClass::Capacity
        );
        authenticator.close().await.unwrap();
        stop_physical_server(&handle, host).await;
        server.wait().await.unwrap();
        listener.close().unwrap();
        assert_eq!(budget.usage().bytes, 0);
        return;
    }
    if let Some(limits) = page_limits {
        let mut pages = 0;
        let mut workspaces = 0;
        loop {
            let bytes = tokio::time::timeout(Duration::from_secs(10), client.recv())
                .await
                .unwrap()
                .unwrap()
                .expect("organization transfer must not close");
            assert!(bytes.len() <= limits.frame_bytes);
            let payload = codec.decode(&bytes).unwrap();
            assert!(payload.encode().unwrap().len() <= limits.management_bytes);
            let part = match payload {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::OrganizationSnapshot(value)),
                }) => organization::v1::decode_snapshot_part(&value).unwrap(),
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::OrganizationSnapshot(value)),
                }) => organization::v2::decode_snapshot_part(&value).unwrap(),
                _ => panic!("organization snapshot"),
            };
            assert_eq!(part.part.index, pages);
            pages += 1;
            workspaces += part.state.workspaces.len();
            if pages == part.part.total {
                break;
            }
        }
        if progressing_write {
            assert!(write_started.elapsed() >= Duration::from_secs(8));
        }
        if empty_base {
            assert_eq!(pages, 1);
            assert_eq!(workspaces, 0);
        } else {
            assert!(pages > 1);
            assert_eq!(workspaces, 24);
        }
        client.close().await.unwrap();
        running.await.unwrap().unwrap();
        authenticator.close().await.unwrap();
        stop_physical_server(&handle, host).await;
        server.wait().await.unwrap();
        listener.close().unwrap();
        assert_eq!(budget.usage().bytes, 0);
        return;
    }
    let initial = tokio::time::timeout(Duration::from_secs(5), client.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let initial = match codec.decode(&initial).unwrap() {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::OrganizationSnapshot(value)),
        }) => organization::v1::decode_snapshot_part(&value).unwrap(),
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::OrganizationSnapshot(value)),
        }) => organization::v2::decode_snapshot_part(&value).unwrap(),
        _ => panic!("organization snapshot"),
    };
    assert!(initial.state.workspaces.is_empty());
    handle
        .reconcile(principal, initial.part.revision)
        .await
        .unwrap();
    let request = CanonicalOperationRequest::new(
        OperationId::new(stream, OperationSequence::new(1).unwrap()),
        Command::CreateWorkspace {
            server: physical::identity().server,
            source: WorkspaceCreationSource::ServerHome {
                server: physical::identity().server,
            },
            name: NameDraft::Custom(NameText::new("unfocused workspace".into()).unwrap()),
        },
        &CanonicalCodec,
    )
    .unwrap();
    handle.submit(principal, request).await.unwrap();
    let publication = tokio::time::timeout(Duration::from_secs(5), client.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let publication = match codec.decode(&publication).unwrap() {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::OrganizationSnapshot(value)),
        }) => organization::v1::decode_snapshot_part(&value).unwrap(),
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::OrganizationSnapshot(value)),
        }) => organization::v2::decode_snapshot_part(&value).unwrap(),
        _ => panic!("organization snapshot"),
    };
    assert_eq!(
        publication.state.workspaces[0].name.value.display.as_str(),
        "unfocused workspace"
    );
    assert_eq!(
        publication.part.revision,
        handle.snapshot().state().revision
    );
    assert_eq!(publication.part.request, 1);
    assert_eq!(publication.part.generation, 1);
    let mut reservations = Vec::new();
    let mut rejected = None;
    if deny_reply_capacity {
        while let Ok(reservation) = ConnectionEgress::new(budget.clone()).reserve_operation_reply()
        {
            reservations.push(reservation);
        }
        let request = CanonicalOperationRequest::new(
            OperationId::new(stream, OperationSequence::new(2).unwrap()),
            Command::RenameWorkspace {
                workspace: publication.state.workspaces[0].id,
                expected_version: publication.state.workspaces[0].name.version,
                draft: NameDraft::Custom(NameText::new("must not commit".into()).unwrap()),
            },
            &CanonicalCodec,
        )
        .unwrap();
        rejected = Some(request.binding());
        let payload = match major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Operation(
                    spaceterm_protocol::convert::operations::v1::encode_request(&request).unwrap(),
                )),
            }),
            _ => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Operation(
                    spaceterm_protocol::convert::operations::v2::encode_request(&request).unwrap(),
                )),
            }),
        };
        client.send(&codec.encode(&payload).unwrap()).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), client.recv())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
    } else {
        client.close().await.unwrap();
    }
    let result = running.await.unwrap();
    if deny_reply_capacity {
        assert_eq!(
            result.unwrap_err().class,
            spaceterm_server::contracts::ServerFailureClass::Capacity
        );
    } else {
        result.unwrap();
    }
    authenticator.close().await.unwrap();
    stop_physical_server(&handle, host).await;
    server.wait().await.unwrap();
    listener.close().unwrap();
    if let Some(binding) = rejected {
        use spaceterm_core::storage::OrganizationStore;
        let restored = physical::reopen(&directory.0).unwrap().recover().unwrap();
        let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
            physical::identity().server,
            restored.ledger.unwrap(),
        )
        .unwrap();
        assert_eq!(
            ledger
                .lookup(
                    &spaceterm_core::operation_ledger_contracts::StreamBinding::from_stream(stream),
                    binding.id,
                    Some(binding.digest)
                )
                .unwrap(),
            OperationLookup::NeverAdmitted
        );
        assert_eq!(
            restored.organization.workspaces[0]
                .name
                .value
                .display
                .as_str(),
            "unfocused workspace"
        );
    }
    drop(reservations);
    assert_eq!(budget.usage().bytes, 0);
}

#[path = "support/terminal_dispatch.rs"]
mod terminal_dispatch;
