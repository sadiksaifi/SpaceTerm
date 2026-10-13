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
