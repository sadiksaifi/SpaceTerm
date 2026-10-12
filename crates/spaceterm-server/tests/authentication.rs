#[path = "support/job_b.rs"]
mod support;
use spaceterm_core::identity::ClientId;
use spaceterm_host::transport::MessageTransport;
use spaceterm_protocol::{
    ProtocolError,
    authentication::{Established, Purpose},
    codec::Payload,
    wire,
};
use spaceterm_server::{connection::*, contracts::ServerFailureClass};
use std::{sync::atomic::Ordering, time::Duration};
use support::*;

#[tokio::test]
async fn wrong_peer_account_rejected_before_handshake() {
    let fixture = Fixture::new();
    let directory = Directory::new();
    let listener = directory.listener();
    let client = directory.connect().await;
    let authenticator =
        Authenticator::new(fixture.config(client.peer_facts().local_uid.unwrap().wrapping_add(1)))
            .unwrap();
    let pending = listener.accept().await.unwrap();
    let error = authenticator
        .authenticate(pending, authenticator.try_admit().unwrap())
        .await
        .err()
        .unwrap();
    assert_eq!(error.class, ServerFailureClass::Authentication);
    assert_eq!(fixture.pairings.reads.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.key.signs.load(Ordering::SeqCst), 0);
    assert_eq!(client.recv().await.unwrap(), None);
}

macro_rules! ordinary {
    ($name:ident, $major:expr) => {
        #[tokio::test]
        async fn $name() {
            let fixture = Fixture::new();
            let directory = Directory::new();
            let listener = directory.listener();
            let client = directory.connect().await;
            let authenticator =
                Authenticator::new(fixture.config(client.peer_facts().local_uid.unwrap())).unwrap();
            let pending = listener.accept().await.unwrap();
            let (peer, codec) = tokio::join!(
                authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
                fixture.client_handshake(&client, 1, $major, Purpose::Ordinary)
            );
            let peer = peer.unwrap();
            let Established::Ordinary(mut codec) = codec.unwrap() else {
                panic!("ordinary client");
            };
            let PeerConnection::Ordinary(session) = &peer.connection else {
                panic!("ordinary server");
            };
            assert_eq!(codec.major(), $major);
            authenticator.check_current(session.principal).unwrap();
            assert_eq!(fixture.authority.registered.load(Ordering::SeqCst), 1);
            let payload = if $major == 1 {
                Payload::Major1(wire::v1::Message {
                    kind: Some(wire::v1::message::Kind::OrganizationQuery(
                        wire::v1::OrganizationQuery::default(),
                    )),
                })
            } else {
                Payload::Major2(wire::v2::Message {
                    kind: Some(wire::v2::message::Kind::OrganizationQuery(
                        wire::v2::OrganizationQuery::default(),
                    )),
                })
            };
            let bytes = codec.encode(&payload).unwrap();
            client.send(&bytes).await.unwrap();
            let mut peer = peer;
            let bytes = peer.transport.recv().await.unwrap().unwrap();
            let PeerConnection::Ordinary(session) = &mut peer.connection else {
                panic!("ordinary server");
            };
            assert_eq!(session.codec.decode(&bytes).unwrap(), payload);
            peer.close().await.unwrap();
            assert_eq!(client.recv().await.unwrap(), None);
            assert!(fixture.authority.clients.lock().unwrap().is_empty());
            authenticator.close().await.unwrap();
        }
    };
}
ordinary!(
    real_ipc_authentication_and_authenticated_envelopes_major_1,
    1
);
ordinary!(
    real_ipc_authentication_and_authenticated_envelopes_major_2,
    2
);

macro_rules! control_wire_tests {
    ($name:ident, $p:ident, $variant:ident, $major:expr) => {
        #[tokio::test]
        async fn $name() {
            use spaceterm_core::{authority::*, control::*, identity::*, operations::*};
            use spaceterm_protocol::convert::{control, identity::encode_operation};
            let fixture = Fixture::new();
            let directory = Directory::new();
            let listener = directory.listener();
            let client = directory.connect().await;
            let authenticator =
                Authenticator::new(fixture.config(client.peer_facts().local_uid.unwrap())).unwrap();
            let pending = listener.accept().await.unwrap();
            let (peer, codec) = tokio::join!(
                authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
                fixture.client_handshake(&client, 1, $major, Purpose::Ordinary)
            );
            let mut peer = peer.unwrap();
            let Established::Ordinary(mut codec) = codec.unwrap() else {
                panic!("ordinary client");
            };
            let client_id = ClientId::new(server_id(), 1).unwrap();
            let pane = PaneId::new(server_id(), 3).unwrap();
            let stream = OperationStreamId::new(
                OperationStreamKey::new(server_id(), 4).unwrap(),
                PairingId::new(server_id(), 1).unwrap(),
                client_id,
            )
            .unwrap();
            let epoch = ServerEpoch::new(2);
            let requests = [
                Command::CompareAcquire(CompareAcquireRequest {
                    epoch,
                    pane,
                    client: client_id,
                    expected_generation: ControlGeneration::new(0),
                }),
                Command::TakeOver(TakeOverRequest {
                    epoch,
                    pane,
                    client: client_id,
                }),
                Command::ReleaseControl(ReleaseControlRequest {
                    epoch,
                    grant: ControlGrant::new(pane, client_id, ControlGeneration::new(5)).unwrap(),
                }),
            ];
            for (index, command) in requests.iter().enumerate() {
                let request = control::$p::encode_request(
                    command,
                    std::num::NonZeroU64::new(index as u64 + 17).unwrap(),
                )
                .unwrap();
                let correlation = request.request;
                for identified in [false, true] {
                    let kind = if identified {
                        let id = OperationId::new(
                            stream,
                            OperationSequence::new(index as u64 + 1).unwrap(),
                        );
                        wire::$p::message::Kind::Operation(wire::$p::Operation {
                            id: Some(encode_operation(id)),
                            action: Some(wire::$p::operation::Action::IdentifiedControl(
                                request.clone(),
                            )),
                        })
                    } else {
                        wire::$p::message::Kind::ControlRequest(request.clone())
                    };
                    let payload = Payload::$variant(wire::$p::Message { kind: Some(kind) });
                    payload
                        .encode()
                        .expect("converted control passes structural admission");
                    client.send(&codec.encode(&payload).unwrap()).await.unwrap();
                    let bytes = peer.transport.recv().await.unwrap().unwrap();
                    let PeerConnection::Ordinary(session) = &mut peer.connection else {
                        panic!("ordinary server");
                    };
                    let Payload::$variant(decoded) = session.codec.decode(&bytes).unwrap() else {
                        panic!("same major");
                    };
                    let request = match decoded.kind.unwrap() {
                        wire::$p::message::Kind::ControlRequest(request) => request,
                        wire::$p::message::Kind::Operation(operation) => {
                            let Some(wire::$p::operation::Action::IdentifiedControl(request)) =
                                operation.action
                            else {
                                panic!("identified control");
                            };
                            request
                        }
                        _ => panic!("control"),
                    };
                    assert_ne!(correlation, 0);
                    assert_eq!(request.request, correlation);
                    assert_eq!(
                        control::$p::decode_request(&request, client_id).unwrap(),
                        *command
                    );
                }
            }
            peer.close().await.unwrap();
            authenticator.close().await.unwrap();
        }
    };
}
control_wire_tests!(
    control_correlations_pass_authenticated_standalone_and_identified_wire_major_1,
    v1,
    Major1,
    1
);
control_wire_tests!(
    control_correlations_pass_authenticated_standalone_and_identified_wire_major_2,
    v2,
    Major2,
    2
);

#[tokio::test]
async fn revoked_pairing_cannot_open_ordinary_session() {
    let fixture = Fixture::new();
    fixture.pairings.active.store(false, Ordering::SeqCst);
    let directory = Directory::new();
    let listener = directory.listener();
    let client = directory.connect().await;
    let authenticator =
        Authenticator::new(fixture.config(client.peer_facts().local_uid.unwrap())).unwrap();
    let pending = listener.accept().await.unwrap();
    let (server, client_result) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        fixture.client_handshake(&client, 1, 2, Purpose::Ordinary)
    );
    assert_eq!(
        server.err().unwrap().class,
        ServerFailureClass::Authentication
    );
    assert_eq!(client_result.err(), Some(ProtocolError::Authentication));
    assert_eq!(fixture.authority.registered.load(Ordering::SeqCst), 0);
    authenticator.close().await.unwrap();
}

#[tokio::test]
async fn restricted_revocation_channel_cannot_dispatch_terminal_commands() {
    let fixture = Fixture::new();
    fixture.pairings.active.store(false, Ordering::SeqCst);
    let directory = Directory::new();
    let listener = directory.listener();
    let client = directory.connect().await;
    let authenticator =
        Authenticator::new(fixture.config(client.peer_facts().local_uid.unwrap())).unwrap();
    let pending = listener.accept().await.unwrap();
    let (server, client_result) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        fixture.client_handshake(&client, 1, 1, Purpose::RevocationStatus)
    );
    let peer = server.unwrap();
    assert!(matches!(peer.connection, PeerConnection::Restricted(_)));
    assert!(matches!(client_result.unwrap(), Established::Restricted(_)));
    assert_eq!(fixture.authority.registered.load(Ordering::SeqCst), 0);
    peer.close().await.unwrap();
    authenticator.close().await.unwrap();
}

#[tokio::test]
async fn new_verified_binding_supersedes_old_client_connection() {
    let fixture = Fixture::new();
    let directory = Directory::new();
    let listener = directory.listener();
    let client = directory.connect().await;
    let authenticator =
        Authenticator::new(fixture.config(client.peer_facts().local_uid.unwrap())).unwrap();
    let pending = listener.accept().await.unwrap();
    let (old, _) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        fixture.client_handshake(&client, 1, 2, Purpose::Ordinary)
    );
    let old = old.unwrap();
    let PeerConnection::Ordinary(old_session) = &old.connection else {
        panic!("ordinary");
    };
    let old_principal = old_session.principal;
    let replacement = directory.connect().await;
    let pending = listener.accept().await.unwrap();
    let (new, _) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        fixture.client_handshake(&replacement, 1, 2, Purpose::Ordinary)
    );
    let new = new.unwrap();
    assert_eq!(client.recv().await.unwrap(), None);
    assert_eq!(
        authenticator
            .check_current(old_principal)
            .unwrap_err()
            .class,
        ServerFailureClass::StaleAuthority
    );
    let PeerConnection::Ordinary(session) = &new.connection else {
        panic!("ordinary");
    };
    authenticator.check_current(session.principal).unwrap();
    assert!(session.principal.binding().generation > old_principal.binding().generation);
    old.close().await.unwrap();
    new.close().await.unwrap();
    authenticator.close().await.unwrap();
}

#[tokio::test]
async fn authorized_connection_limit_plus_one_rejected() {
    let fixture = Fixture::new();
    let directory = Directory::new();
    let listener = directory.listener();
    let client = directory.connect().await;
    let mut config = fixture.config(client.peer_facts().local_uid.unwrap());
    config.authenticated_limit = 1;
    let authenticator = Authenticator::new(config).unwrap();
    let pending = listener.accept().await.unwrap();
    let (peer, _) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        fixture.client_handshake(&client, 1, 2, Purpose::Ordinary)
    );
    let peer = peer.unwrap();
    let second = directory.connect().await;
    let pending = listener.accept().await.unwrap();
    let (refused, _) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        fixture.client_handshake(&second, 2, 2, Purpose::Ordinary)
    );
    assert_eq!(refused.err().unwrap().class, ServerFailureClass::Capacity);
    assert_eq!(fixture.authority.registered.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.authority.disconnected.load(Ordering::SeqCst), 0);
    peer.close().await.unwrap();
    let retry = directory.connect().await;
    let pending = listener.accept().await.unwrap();
    let (peer, _) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        fixture.client_handshake(&retry, 2, 2, Purpose::Ordinary)
    );
    peer.unwrap().close().await.unwrap();
    authenticator.close().await.unwrap();
}

#[tokio::test]
async fn registration_cleanup_of_absent_or_superseded_binding_preserves_replacement() {
    use spaceterm_server::service::RecoveredServer;

    let fixture = actor::Fixture::new();
    empty_retained(&fixture);
    let first = actor::principal(&fixture.pairings, fixture.key.clone(), 1, 1, 2);
    let replacement = actor::principal(&fixture.pairings, fixture.key.clone(), 1, 2, 2);
    let absent = actor::principal(&fixture.pairings, fixture.key.clone(), 2, 3, 2);
    let host = fixture.host.clone();
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    ConnectionAuthorityPort::disconnect(&handle, absent)
        .await
        .unwrap();
    handle.register(first).await.unwrap();
    handle.register(replacement).await.unwrap();
    ConnectionAuthorityPort::disconnect(&handle, first)
        .await
        .unwrap();
    handle
        .reconcile(replacement, handle.snapshot().state().revision)
        .await
        .unwrap();
    ConnectionAuthorityPort::disconnect(&handle, replacement)
        .await
        .unwrap();
    ConnectionAuthorityPort::disconnect(&handle, replacement)
        .await
        .unwrap();
    actor::stop(&handle, host).await.unwrap();
    assert!(server.wait().await.unwrap().cleanup_confirmed);
}

macro_rules! capacity_cleanup {
    ($name:ident, $major:expr) => {
        #[tokio::test]
        async fn $name() {
            use spaceterm_server::service::RecoveredServer;
            use std::sync::Arc;

            let actor = actor::Fixture::new();
            empty_retained(&actor);
            let host = actor.host.clone();
            let server = RecoveredServer::recover(actor.config, actor.dependencies)
                .await
                .unwrap();
            let handle = server.handle();
            let fixture = Fixture::with_seeds(41, 42);
            let directory = Directory::new();
            let listener = directory.listener();
            let client = directory.connect().await;
            let mut config = fixture.config(client.peer_facts().local_uid.unwrap());
            config.authority = Arc::new(handle.clone());
            config.authenticated_limit = 1;
            let authenticator = Authenticator::new(config).unwrap();
            let pending = listener.accept().await.unwrap();
            let (peer, codec) = tokio::join!(
                authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
                fixture.client_handshake(&client, 1, $major, Purpose::Ordinary)
            );
            let peer = peer.unwrap();
            codec.unwrap();
            let PeerConnection::Ordinary(session) = &peer.connection else {
                panic!("ordinary server");
            };
            let first = session.principal;

            // Client 2 has never registered with the real actor. A full native
            // admission pool must not attempt to disconnect that absent identity.
            let second = directory.connect().await;
            let pending = listener.accept().await.unwrap();
            let (refused, codec) = tokio::join!(
                authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
                fixture.client_handshake(&second, 2, $major, Purpose::Ordinary)
            );
            assert_eq!(refused.err().unwrap().class, ServerFailureClass::Capacity);
            assert!(matches!(codec, Err(ProtocolError::Authentication)));
            assert_eq!(second.recv().await.unwrap(), None);
            authenticator.check_current(first).unwrap();
            peer.close().await.unwrap();

            let retry = directory.connect().await;
            let pending = listener.accept().await.unwrap();
            let (peer, codec) = tokio::join!(
                authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
                fixture.client_handshake(&retry, 2, $major, Purpose::Ordinary)
            );
            codec.unwrap();
            peer.unwrap().close().await.unwrap();
            authenticator.close().await.unwrap();
            listener.close().unwrap();
            actor::stop(&handle, host).await.unwrap();
            assert!(server.wait().await.unwrap().cleanup_confirmed);
        }
    };
}
capacity_cleanup!(
    capacity_rejection_then_clean_shutdown_with_real_authority_major_1,
    1
);
capacity_cleanup!(
    capacity_rejection_then_clean_shutdown_with_real_authority_major_2,
    2
);

struct DelayedRegistration {
    authority: spaceterm_server::api::ServerHandle,
    entered: tokio::sync::Notify,
    gate: tokio::sync::Semaphore,
    installed: std::sync::Mutex<Option<spaceterm_server::api::VerifiedPrincipal>>,
}
impl ConnectionAuthorityPort for DelayedRegistration {
    fn register(
        &self,
        principal: spaceterm_server::api::VerifiedPrincipal,
    ) -> spaceterm_server::contracts::ServerFuture<
        '_,
        Result<(), spaceterm_server::contracts::ServerFailure>,
    > {
        Box::pin(async move {
            self.entered.notify_one();
            self.gate.acquire().await.unwrap().forget();
            self.authority.register(principal).await?;
            *self.installed.lock().unwrap() = Some(principal);
            Ok(())
        })
    }
    fn disconnect(
        &self,
        principal: spaceterm_server::api::VerifiedPrincipal,
    ) -> spaceterm_server::contracts::ServerFuture<
        '_,
        Result<(), spaceterm_server::contracts::ServerFailure>,
    > {
        ConnectionAuthorityPort::disconnect(&self.authority, principal)
    }
}
macro_rules! cancelled_registration {
    ($name:ident, $major:expr) => {
        #[tokio::test]
        async fn $name() {
            use spaceterm_server::service::RecoveredServer;
            use std::sync::Arc;

            let actor = actor::Fixture::new();
            empty_retained(&actor);
            let host = actor.host.clone();
            let server = RecoveredServer::recover(actor.config, actor.dependencies)
                .await
                .unwrap();
            let handle = server.handle();
            let fixture = Arc::new(Fixture::with_seeds(41, 42));
            let directory = Directory::new();
            let listener = directory.listener();
            let client = Arc::new(directory.connect().await);
            let authority = Arc::new(DelayedRegistration {
                authority: handle.clone(),
                entered: tokio::sync::Notify::new(),
                gate: tokio::sync::Semaphore::new(0),
                installed: std::sync::Mutex::new(None),
            });
            let mut config = fixture.config(client.peer_facts().local_uid.unwrap());
            config.authority = authority.clone();
            config.unauthenticated_limit = 1;
            let authenticator = Authenticator::new(config).unwrap();
            let pending = listener.accept().await.unwrap();
            let owner = authenticator.clone();
            let permit = authenticator.try_admit().unwrap();
            let authentication =
                tokio::spawn(async move { owner.authenticate(pending, permit).await });
            let device = fixture.clone();
            let transport = client.clone();
            let handshake = tokio::spawn(async move {
                device
                    .client_handshake(transport.as_ref(), 1, $major, Purpose::Ordinary)
                    .await
            });
            tokio::time::timeout(Duration::from_secs(5), authority.entered.notified())
                .await
                .unwrap();
            authentication.abort();
            let _ = authentication.await;
            assert!(matches!(
                handshake.await.unwrap(),
                Err(ProtocolError::Authentication)
            ));
            assert_eq!(client.recv().await.unwrap(), None);
            // Cancellation closes native transport first, but cannot abandon a
            // submitted registration or return admission before its settlement.
            assert_eq!(
                authenticator.try_admit().err().unwrap().class,
                ServerFailureClass::Capacity
            );
            let owner = authenticator.clone();
            let mut closing = tokio::spawn(async move { owner.close().await });
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut closing)
                    .await
                    .is_err()
            );
            authority.gate.add_permits(1);
            tokio::time::timeout(Duration::from_secs(5), closing)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let principal = authority
                .installed
                .lock()
                .unwrap()
                .expect("registration must settle");
            assert_eq!(
                handle
                    .reconcile(principal, handle.snapshot().state().revision)
                    .await
                    .unwrap_err()
                    .class,
                ServerFailureClass::StaleAuthority
            );
            listener.close().unwrap();
            actor::stop(&handle, host).await.unwrap();
            assert!(server.wait().await.unwrap().cleanup_confirmed);
        }
    };
}
cancelled_registration!(
    cancelled_in_flight_registration_settles_before_admission_release_major_1,
    1
);
cancelled_registration!(
    cancelled_in_flight_registration_settles_before_admission_release_major_2,
    2
);

#[tokio::test]
async fn unauthenticated_limit_plus_one_rejected_without_leak() {
    let fixture = Fixture::new();
    let directory = Directory::new();
    let listener = directory.listener();
    let client = directory.connect().await;
    let mut config = fixture.config(client.peer_facts().local_uid.unwrap());
    config.unauthenticated_limit = 1;
    let authenticator = Authenticator::new(config).unwrap();
    let permit = authenticator.try_admit().unwrap();
    assert_eq!(
        authenticator.try_admit().err().unwrap().class,
        ServerFailureClass::Capacity
    );
    let pending = listener.accept().await.unwrap();
    let task = tokio::spawn({
        let authenticator = authenticator.clone();
        async move { authenticator.authenticate(pending, permit).await }
    });
    let (_, hello) = spaceterm_protocol::authentication::ClientHandshake::begin(
        spaceterm_protocol::authentication::ServerPin {
            identity: server_id().bytes(),
            public_key: spaceterm_protocol::authentication::SigningKey::public_key(
                fixture.key.as_ref(),
            ),
        },
        principal(1),
        Purpose::Ordinary,
        Default::default(),
        fixture.device.clone(),
    )
    .unwrap();
    client.send(&hello).await.unwrap();
    assert!(client.recv().await.unwrap().is_some());
    task.abort();
    let _ = task.await;
    authenticator.close().await.unwrap();
    // The retained handshake owner, not the cancelled caller, closes this socket.
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), client.recv())
            .await
            .unwrap()
            .unwrap(),
        None
    );
}

macro_rules! revocation_readiness_race {
    ($name:ident, $major:expr) => {
        #[tokio::test]
        async fn $name() {
            use std::sync::Arc;
            let fixture = Arc::new(Fixture::new());
            let directory = Directory::new();
            let listener = directory.listener();
            let client = Arc::new(directory.connect().await);
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            *fixture.authority.register_gate.lock().unwrap() = Some(gate.clone());
            let authenticator =
                Authenticator::new(fixture.config(client.peer_facts().local_uid.unwrap())).unwrap();
            let pending = listener.accept().await.unwrap();
            let owner = authenticator.clone();
            let permit = authenticator.try_admit().unwrap();
            let authentication =
                tokio::spawn(async move { owner.authenticate(pending, permit).await });
            let device = fixture.clone();
            let transport = client.clone();
            let handshake = tokio::spawn(async move {
                device
                    .client_handshake(transport.as_ref(), 1, $major, Purpose::Ordinary)
                    .await
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                while fixture.authority.registered.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            // The actor has installed the exact binding, but has not returned
            // registration to the readiness sender. Commit and notify revocation.
            fixture.pairings.active.store(false, Ordering::SeqCst);
            let revoked = ClientId::new(server_id(), 1).unwrap();
            let owner = authenticator.clone();
            let mut closing = tokio::spawn(async move { owner.close_clients(&[revoked]).await });
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), client.recv())
                    .await
                    .unwrap()
                    .unwrap(),
                None
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut closing)
                    .await
                    .is_err()
            );
            gate.add_permits(1);
            tokio::time::timeout(Duration::from_secs(5), closing)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let result = authentication.await.unwrap();
            assert_eq!(
                result
                    .err()
                    .expect("revocation must cancel readiness")
                    .class,
                ServerFailureClass::Authentication
            );
            assert!(matches!(
                handshake.await.unwrap(),
                Err(ProtocolError::Authentication)
            ));
            assert!(fixture.authority.clients.lock().unwrap().is_empty());
            assert_eq!(client.recv().await.unwrap(), None);
            // Joined teardown returns admission without making the pool unusable.
            let permit = authenticator.try_admit().unwrap();
            drop(permit);
            authenticator.close().await.unwrap();
        }
    };
}
revocation_readiness_race!(
    pairing_revocation_between_actor_registration_and_readiness_cancels_and_joins_major_1,
    1
);
revocation_readiness_race!(
    pairing_revocation_between_actor_registration_and_readiness_cancels_and_joins_major_2,
    2
);

#[tokio::test]
async fn durable_revocation_closes_ordinary_transports_without_upgrading_restricted_access() {
    let fixture = Fixture::new();
    let directory = Directory::new();
    let listener = directory.listener();
    let client = directory.connect().await;
    let authenticator =
        Authenticator::new(fixture.config(client.peer_facts().local_uid.unwrap())).unwrap();
    let pending = listener.accept().await.unwrap();
    let (peer, _) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        fixture.client_handshake(&client, 1, 2, Purpose::Ordinary)
    );
    let peer = peer.unwrap();
    fixture.pairings.active.store(false, Ordering::SeqCst);
    authenticator
        .close_clients(&[ClientId::new(server_id(), 1).unwrap()])
        .await
        .unwrap();
    assert_eq!(client.recv().await.unwrap(), None);
    assert!(fixture.authority.clients.lock().unwrap().is_empty());
    peer.close().await.unwrap();
    authenticator.close().await.unwrap();
}

#[tokio::test]
async fn handshake_deadline_is_absolute_despite_partial_proof_progress() {
    use spaceterm_protocol::authentication::{
        ClientHandshake, HandshakeConfig, ServerPin, SigningKey,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::new();
    let directory = Directory::new();
    let listener = directory.listener();
    let mut client =
        tokio::net::UnixStream::connect(directory.directory.endpoint("s").unwrap().path())
            .await
            .unwrap();
    let pending = listener.accept().await.unwrap();
    let authenticator =
        Authenticator::new(fixture.config(pending.peer_facts().local_uid.unwrap())).unwrap();
    let server = tokio::spawn({
        let auth = authenticator.clone();
        let permit = auth.try_admit().unwrap();
        async move { auth.authenticate(pending, permit).await }
    });
    let (_, hello) = ClientHandshake::begin(
        ServerPin {
            identity: server_id().bytes(),
            public_key: fixture.key.public_key(),
        },
        principal(1),
        Purpose::Ordinary,
        HandshakeConfig::default(),
        fixture.device.clone(),
    )
    .unwrap();
    client
        .write_all(&(hello.len() as u32).to_be_bytes())
        .await
        .unwrap();
    client.write_all(&hello).await.unwrap();
    let mut prefix = [0; 4];
    client.read_exact(&mut prefix).await.unwrap();
    let mut reply = vec![0; u32::from_be_bytes(prefix) as usize];
    client.read_exact(&mut reply).await.unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(4)).await;
    client.write_all(&64_u32.to_be_bytes()).await.unwrap();
    client.write_all(&[1]).await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        server.await.unwrap().err().unwrap().class,
        ServerFailureClass::Authentication
    );
    assert_eq!(fixture.authority.registered.load(Ordering::SeqCst), 0);
    authenticator.try_admit().unwrap();
    authenticator.close().await.unwrap();
}

#[path = "support/job_a.rs"]
mod actor;

struct RejectOrdinary(std::sync::atomic::AtomicUsize);
impl spaceterm_server::protocol_dispatch::OrdinaryDispatchPort for RejectOrdinary {
    fn dispatch(
        &self,
        _: spaceterm_server::api::VerifiedPrincipal,
        _: Payload,
        _: spaceterm_server::api::ServerHandle,
    ) -> spaceterm_server::contracts::ServerFuture<
        '_,
        Result<Option<Payload>, spaceterm_server::contracts::ServerFailure>,
    > {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(spaceterm_server::failure::failure(
                ServerFailureClass::ScopeMismatch,
            ))
        })
    }
}
struct RejectRestricted;
impl spaceterm_server::protocol_dispatch::RestrictedDispatchPort for RejectRestricted {
    fn dispatch(
        &self,
        _: spaceterm_protocol::authentication::Principal,
        _: wire::common::RestrictedMessage,
    ) -> spaceterm_server::contracts::ServerFuture<
        '_,
        Result<wire::common::RestrictedMessage, spaceterm_server::contracts::ServerFailure>,
    > {
        Box::pin(async {
            Err(spaceterm_server::failure::failure(
                ServerFailureClass::Authentication,
            ))
        })
    }
}

macro_rules! status_tests {
    ($name:ident,$p:ident,$variant:ident,$major:expr) => {
        #[tokio::test]
        async fn $name() {
            use spaceterm_server::{listeners::ConnectionDispatchPort,protocol_dispatch::ProtocolDispatcher,service::RecoveredServer};
            use spaceterm_core::management::StatusPublication;
            use std::sync::Arc;
            let server_fixture = actor::Fixture::new(); let host = server_fixture.host.clone();
            let server = RecoveredServer::recover(server_fixture.config,server_fixture.dependencies).await.unwrap(); let handle = server.handle();
            let fixture = Fixture::new(); let directory = Directory::new(); let listener = directory.listener(); let client = directory.connect().await;
            let authenticator = Authenticator::new(fixture.config(client.peer_facts().local_uid.unwrap())).unwrap(); let pending = listener.accept().await.unwrap();
            let (peer,codec) = tokio::join!(authenticator.authenticate(pending,authenticator.try_admit().unwrap()),fixture.client_handshake(&client,1,$major,Purpose::Ordinary));
            let mut peer = peer.unwrap(); let Established::Ordinary(mut codec) = codec.unwrap() else { panic!("ordinary"); };
            let ordinary = Arc::new(RejectOrdinary(std::sync::atomic::AtomicUsize::new(0)));
            let (feed,status_feed) = tokio::sync::watch::channel(status(7));
            let dispatcher = ProtocolDispatcher::new(authenticator.clone(),status_feed,ordinary.clone(),Arc::new(RejectRestricted));
            let connection = tokio::spawn(async move { let result = dispatcher.run(&mut peer,handle).await; peer.close().await.unwrap(); result });
            let query = Payload::$variant(wire::$p::Message { kind: Some(wire::$p::message::Kind::StatusQuery(wire::$p::StatusQuery { request: 1,request_generation: 2,subscribe: true,..Default::default() })) });
            client.send(&codec.encode(&query).unwrap()).await.unwrap();
            let bytes = client.recv().await.unwrap().unwrap(); let reply = codec.decode(&bytes).unwrap();
            let Payload::$variant(reply) = reply else { panic!("matching major"); }; let Some(wire::$p::message::Kind::StatusPublication(value)) = reply.kind else { panic!("status"); };
            assert_eq!(spaceterm_protocol::convert::management::$p::decode_publication(&value,&|_| Err(ProtocolError::UnexpectedMessage)).unwrap(),StatusPublication::Snapshot { request: std::num::NonZeroU64::new(1).unwrap(),generation: 2,status: status(7) });
            // A stale cancellation cannot unsubscribe a later generation.
            let cancel = Payload::$variant(wire::$p::Message { kind: Some(wire::$p::message::Kind::StatusQuery(wire::$p::StatusQuery { request: 1,request_generation: 1,cancel: true,..Default::default() })) });
            client.send(&codec.encode(&cancel).unwrap()).await.unwrap();
            feed.send_replace(status(8));
            let bytes = client.recv().await.unwrap().unwrap(); let reply = codec.decode(&bytes).unwrap();
            let Payload::$variant(reply) = reply else { panic!("matching major"); }; let Some(wire::$p::message::Kind::StatusPublication(value)) = reply.kind else { panic!("status"); };
            assert!(matches!(spaceterm_protocol::convert::management::$p::decode_publication(&value,&|_| Err(ProtocolError::UnexpectedMessage)).unwrap(),StatusPublication::Changed { base,.. } if base.get()==7));
            feed.send_replace(status(12));
            let bytes = client.recv().await.unwrap().unwrap(); let reply = codec.decode(&bytes).unwrap();
            let Payload::$variant(reply) = reply else { panic!("matching major"); }; let Some(wire::$p::message::Kind::StatusPublication(value)) = reply.kind else { panic!("status"); };
            assert!(matches!(spaceterm_protocol::convert::management::$p::decode_publication(&value,&|_| Err(ProtocolError::UnexpectedMessage)).unwrap(),StatusPublication::Snapshot { status,.. } if status.parts().revision.get()==12));
            assert_eq!(ordinary.0.load(Ordering::SeqCst),0);
            client.close().await.unwrap(); connection.await.unwrap().unwrap(); authenticator.close().await.unwrap();
            actor::stop(&server.handle(),host).await.unwrap(); server.wait().await.unwrap();
        }
    }
}
// The enum variant names differ from profile module names.
status_tests!(
    authenticated_status_query_subscription_and_gap_major_1,
    v1,
    Major1,
    1
);
status_tests!(
    authenticated_status_query_subscription_and_gap_major_2,
    v2,
    Major2,
    2
);

struct NativeAuthFactory {
    uid: u32,
    credentials: std::sync::Arc<actor::TestKey>,
    pairings: std::sync::Arc<actor::Pairings>,
}
impl spaceterm_server::listeners::AuthenticationFactoryPort for NativeAuthFactory {
    fn config(
        &self,
        handle: spaceterm_server::api::ServerHandle,
        snapshot: &spaceterm_core::organization::OrganizationSnapshot,
    ) -> Result<AuthenticationConfig, spaceterm_server::contracts::ServerFailure> {
        Ok(AuthenticationConfig {
            server: snapshot.state().server,
            epoch: snapshot.epoch(),
            owning_uid: self.uid,
            handshake: spaceterm_protocol::authentication::HandshakeConfig {
                capabilities: vec![spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN],
                ..Default::default()
            },
            credentials: self.credentials.clone(),
            pairing: self.pairings.clone(),
            authority: std::sync::Arc::new(handle),
            unauthenticated_limit: 16,
            authenticated_limit: 32,
            deadline: Duration::from_secs(5),
        })
    }
}
struct TrackingDispatchFactory {
    principals: std::sync::Arc<std::sync::Mutex<Vec<spaceterm_server::api::VerifiedPrincipal>>>,
    status: tokio::sync::watch::Receiver<spaceterm_core::management::ServerStatus>,
}
struct TrackingDispatch {
    principals: std::sync::Arc<std::sync::Mutex<Vec<spaceterm_server::api::VerifiedPrincipal>>>,
    dispatcher: spaceterm_server::protocol_dispatch::ProtocolDispatcher,
}
impl spaceterm_server::listeners::ConnectionDispatchFactoryPort for TrackingDispatchFactory {
    fn create(
        &self,
        authenticator: Authenticator,
    ) -> Result<
        std::sync::Arc<dyn spaceterm_server::listeners::ConnectionDispatchPort>,
        spaceterm_server::contracts::ServerFailure,
    > {
        Ok(std::sync::Arc::new(TrackingDispatch {
            principals: self.principals.clone(),
            dispatcher: spaceterm_server::protocol_dispatch::ProtocolDispatcher::new(
                authenticator,
                self.status.clone(),
                std::sync::Arc::new(RejectOrdinary(std::sync::atomic::AtomicUsize::new(0))),
                std::sync::Arc::new(RejectRestricted),
            ),
        }))
    }
}
impl spaceterm_server::listeners::ConnectionDispatchPort for TrackingDispatch {
    fn run<'a>(
        &'a self,
        peer: &'a mut AuthenticatedPeer,
        handle: spaceterm_server::api::ServerHandle,
    ) -> spaceterm_server::contracts::ServerFuture<
        'a,
        Result<(), spaceterm_server::contracts::ServerFailure>,
    > {
        if let PeerConnection::Ordinary(session) = &peer.connection {
            self.principals.lock().unwrap().push(session.principal);
        }
        self.dispatcher.run(peer, handle)
    }
}

fn empty_retained(fixture: &actor::Fixture) {
    use spaceterm_core::{
        authority::FieldVersion, organization::Versioned, organization_retained::*,
        remote_access::RemoteAccessPolicy,
    };
    let mut db = fixture.db.lock().unwrap();
    let state = &mut db.recovered.organization;
    state.workspaces.clear();
    state.tabs.clear();
    state.panes.clear();
    state.retained = Some(
        OrganizationRetainedFacts::new(OrganizationRetainedParts {
            server: actor::server(),
            workspace_names: vec![],
            creation_defaults: vec![],
            pane_generations: vec![],
            tab_membership_versions: vec![],
            repository_inventory_versions: vec![],
            worktree_inventory: vec![],
            remote_access: Versioned {
                version: FieldVersion::new(1),
                value: RemoteAccessPolicy::disabled(actor::server()),
            },
        })
        .unwrap(),
    );
}

macro_rules! wss_tests {
    ($name:ident,$major:expr) => {
        #[tokio::test]
        async fn $name() {
            use spaceterm_core::{
                authority::FieldVersion, management::ListenerState, operations::Command,
                remote_access::RemoteAccessPolicy,
            };
            use spaceterm_host::{
                transport::{EnrolledServerKey, MessageLimit},
                wss::WssTransport,
            };
            use spaceterm_protocol::authentication::SigningKey;
            use spaceterm_server::{
                api::{ListenerFactories, OperationResponse},
                listeners::{ListenerConfig, ServerListeners},
                service::RecoveredServer,
            };
            use std::sync::{Arc, Mutex};
            let fixture = actor::Fixture::new();
            empty_retained(&fixture);
            let key = fixture.key.clone();
            let pairings = fixture.pairings.clone();
            let host = fixture.host.clone();
            let codec = fixture.codec.clone();
            let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
                .await
                .unwrap();
            let handle = server.handle();
            let (stream, _peer) = tokio::net::UnixStream::pair().unwrap();
            let uid = spaceterm_host::peer_credentials::same_account_peer(&stream)
                .unwrap()
                .local_uid
                .unwrap();
            let directory = Directory::new();
            let endpoint = directory.directory.endpoint("s").unwrap();
            let (tls, pin, upgrade_client) = tls_with_client(41);
            assert_eq!(pin, EnrolledServerKey(key.public_key()));
            let principals = Arc::new(Mutex::new(vec![]));
            let (_status, status_feed) = tokio::sync::watch::channel(status(1));
            let listeners = Arc::new(ServerListeners::new(ListenerConfig {
                ipc: endpoint.clone(),
                tls,
                authentication: Arc::new(NativeAuthFactory {
                    uid,
                    credentials: key,
                    pairings,
                }),
                dispatch: Arc::new(TrackingDispatchFactory {
                    principals: principals.clone(),
                    status: status_feed,
                }),
            }));
            let running = tokio::spawn(server.run(ListenerFactories {
                lifecycle: listeners.clone(),
            }));
            tokio::time::timeout(Duration::from_secs(5), async {
                while listeners.authenticator().is_err() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(listeners.subscribe_status().borrow().is_empty());
            let client = directory.connect().await;
            let network_fixture = Fixture::with_seeds(41, 42);
            assert!(matches!(
                network_fixture
                    .client_handshake(&client, 1, $major, Purpose::Ordinary)
                    .await
                    .unwrap(),
                Established::Ordinary(_)
            ));
            let principal = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(principal) = principals.lock().unwrap().first().copied() {
                        break principal;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            handle
                .reconcile(principal, handle.snapshot().state().revision)
                .await
                .unwrap();
            let policy = RemoteAccessPolicy::enabled(
                actor::server(),
                vec!["127.0.0.1:0".parse().unwrap(), "[::1]:0".parse().unwrap()],
            )
            .unwrap();
            let response = handle
                .submit(
                    principal,
                    actor::request(
                        &codec,
                        1,
                        1,
                        Command::SetRemoteAccess {
                            server: actor::server(),
                            expected_version: FieldVersion::new(1),
                            value: policy,
                        },
                    ),
                )
                .await
                .unwrap();
            assert!(matches!(response, OperationResponse::Completed { .. }));
            let facts = listeners.subscribe_status().borrow().clone();
            assert_eq!(facts.len(), 2);
            assert!(
                facts
                    .iter()
                    .all(|v| v.state == ListenerState::Listening && v.generation.get() == 2)
            );
            // An unauthenticated peer can finish TLS and abandon the WebSocket
            // upgrade. Neither address nor existing IPC may lose admission.
            let address = facts[0].endpoint;
            tokio::task::spawn_blocking(move || {
                let socket = std::net::TcpStream::connect(address).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
                let connection = rustls::ClientConnection::new(upgrade_client, name).unwrap();
                let mut tls = rustls::StreamOwned::new(connection, socket);
                while tls.conn.is_handshaking() {
                    tls.conn.complete_io(&mut tls.sock).unwrap();
                }
                // EOF after a verified TLS exchange, before the HTTP upgrade.
                tls.sock.shutdown(std::net::Shutdown::Both).unwrap();
            })
            .await
            .unwrap();
            // The two routes use one paired proof and one operation-independent
            // actor registration policy. Authenticate over actual IPv4 WSS.
            let wss = WssTransport::connect(
                facts[0].endpoint,
                "localhost",
                pin,
                MessageLimit::envelope(),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
            assert!(matches!(
                network_fixture
                    .client_handshake(&wss, 2, $major, Purpose::Ordinary)
                    .await
                    .unwrap(),
                Established::Ordinary(_)
            ));
            let ipv6 = WssTransport::connect(
                facts[1].endpoint,
                "localhost",
                pin,
                MessageLimit::envelope(),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
            assert!(matches!(
                network_fixture
                    .client_handshake(&ipv6, 1, $major, Purpose::RevocationStatus)
                    .await
                    .unwrap(),
                Established::Restricted(_)
            ));
            handle
                .submit(
                    principal,
                    actor::request(
                        &codec,
                        1,
                        2,
                        Command::SetRemoteAccess {
                            server: actor::server(),
                            expected_version: FieldVersion::new(2),
                            value: RemoteAccessPolicy::disabled(actor::server()),
                        },
                    ),
                )
                .await
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), wss.recv())
                    .await
                    .unwrap()
                    .unwrap(),
                None
            );
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), ipv6.recv())
                    .await
                    .unwrap()
                    .unwrap(),
                None
            );
            for fact in &facts {
                assert!(tokio::net::TcpStream::connect(fact.endpoint).await.is_err());
            }
            assert!(listeners.subscribe_status().borrow().is_empty());
            // Disabling a WSS generation does not close the IPC client.
            listeners
                .authenticator()
                .unwrap()
                .check_current(principal)
                .unwrap();
            actor::stop(&handle, host).await.unwrap();
            running.await.unwrap().unwrap();
            assert!(!endpoint.path().exists());
            assert_eq!(client.recv().await.unwrap(), None);
        }
    };
}
wss_tests!(
    durable_wss_enable_disable_ipv4_ipv6_and_ipc_isolation_major_1,
    1
);
wss_tests!(
    durable_wss_enable_disable_ipv4_ipv6_and_ipc_isolation_major_2,
    2
);

struct ListenerHarness {
    handle: spaceterm_server::api::ServerHandle,
    listeners: std::sync::Arc<spaceterm_server::listeners::ServerListeners>,
    running: tokio::task::JoinHandle<
        Result<
            spaceterm_server::shutdown::ShutdownReport,
            spaceterm_server::contracts::ServerFailure,
        >,
    >,
    host: std::sync::Arc<actor::Host>,
    db: std::sync::Arc<std::sync::Mutex<actor::Database>>,
    codec: std::sync::Arc<actor::Codec>,
    principal: spaceterm_server::api::VerifiedPrincipal,
    directory: Directory,
    ipc: spaceterm_host::ipc::IpcTransport,
    ipc_codec: spaceterm_protocol::connection::AuthenticatedConnection,
    _status: tokio::sync::watch::Sender<spaceterm_core::management::ServerStatus>,
}
impl ListenerHarness {
    async fn start() -> Self {
        Self::start_with_policy(None).await
    }
    async fn start_with_policy(
        policy: Option<spaceterm_core::remote_access::RemoteAccessPolicy>,
    ) -> Self {
        use spaceterm_server::{
            api::ListenerFactories,
            listeners::{ListenerConfig, ServerListeners},
            service::RecoveredServer,
        };
        use std::sync::{Arc, Mutex};
        let fixture = actor::Fixture::new();
        empty_retained(&fixture);
        if let Some(policy) = policy {
            let mut db = fixture.db.lock().unwrap();
            let retained = db.recovered.organization.retained.as_mut().unwrap();
            let mut parts = retained.parts().clone();
            parts.remote_access.value = policy;
            *retained =
                spaceterm_core::organization_retained::OrganizationRetainedFacts::new(parts)
                    .unwrap();
        }
        let credentials = fixture.key.clone();
        let pairings = fixture.pairings.clone();
        let host = fixture.host.clone();
        let db = fixture.db.clone();
        let codec = fixture.codec.clone();
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        let (stream, _peer) = tokio::net::UnixStream::pair().unwrap();
        let uid = spaceterm_host::peer_credentials::same_account_peer(&stream)
            .unwrap()
            .local_uid
            .unwrap();
        let directory = Directory::new();
        let (tls, _) = tls(41);
        let principals = Arc::new(Mutex::new(vec![]));
        let (status, status_feed) = tokio::sync::watch::channel(support::status(1));
        let listeners = Arc::new(ServerListeners::new(ListenerConfig {
            ipc: directory.directory.endpoint("s").unwrap(),
            tls,
            authentication: Arc::new(NativeAuthFactory {
                uid,
                credentials,
                pairings,
            }),
            dispatch: Arc::new(TrackingDispatchFactory {
                principals: principals.clone(),
                status: status_feed,
            }),
        }));
        let running = tokio::spawn(server.run(ListenerFactories {
            lifecycle: listeners.clone(),
        }));
        tokio::time::timeout(Duration::from_secs(5), async {
            while listeners.authenticator().is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let ipc = directory.connect().await;
        let network = Fixture::with_seeds(41, 42);
        let Established::Ordinary(ipc_codec) = network
            .client_handshake(&ipc, 1, 2, Purpose::Ordinary)
            .await
            .unwrap()
        else {
            panic!("ordinary IPC connection");
        };
        let principal = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(value) = principals.lock().unwrap().first().copied() {
                    break value;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        handle
            .reconcile(principal, handle.snapshot().state().revision)
            .await
            .unwrap();
        Self {
            handle,
            listeners,
            running,
            host,
            db,
            codec,
            principal,
            directory,
            ipc,
            ipc_codec,
            _status: status,
        }
    }
    async fn stop(self) {
        actor::stop(&self.handle, self.host).await.unwrap();
        self.running.await.unwrap().unwrap();
        assert!(
            !self
                .directory
                .directory
                .endpoint("s")
                .unwrap()
                .path()
                .exists()
        );
        assert_eq!(self.ipc.recv().await.unwrap(), None);
    }
}

#[tokio::test]
async fn failed_enable_commit_binds_nothing_and_keeps_ipc_available() {
    use spaceterm_core::{
        authority::FieldVersion,
        operations::Command,
        remote_access::RemoteAccessPolicy,
        storage::{CommitDisposition, StoreFailure},
    };
    let harness = ListenerHarness::start().await;
    let reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    harness.db.lock().unwrap().next = Some(CommitDisposition::NotApplied(StoreFailure::Io));
    let response = harness
        .handle
        .submit(
            harness.principal,
            actor::request(
                &harness.codec,
                1,
                1,
                Command::SetRemoteAccess {
                    server: actor::server(),
                    expected_version: FieldVersion::new(1),
                    value: RemoteAccessPolicy::enabled(actor::server(), vec![address]).unwrap(),
                },
            ),
        )
        .await;
    assert_eq!(
        response.err().unwrap().class,
        ServerFailureClass::StorageUnavailable
    );
    assert!(harness.listeners.subscribe_status().borrow().is_empty());
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    assert!(
        !harness
            .handle
            .snapshot()
            .state()
            .retained
            .as_ref()
            .unwrap()
            .remote_access()
            .value
            .is_enabled()
    );
    harness
        .listeners
        .authenticator()
        .unwrap()
        .check_current(harness.principal)
        .unwrap();
    harness.stop().await;
}

#[tokio::test]
async fn recovered_enabled_policy_with_unavailable_wss_keeps_authenticated_ipc_status_and_disable()
{
    use spaceterm_core::{
        authority::FieldVersion, management::*, operations::Command,
        remote_access::RemoteAccessPolicy,
    };
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let available = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
    let endpoints = vec![
        available.local_addr().unwrap(),
        occupied.local_addr().unwrap(),
    ];
    drop(available);
    let policy = RemoteAccessPolicy::enabled(actor::server(), endpoints.clone()).unwrap();
    let mut harness = ListenerHarness::start_with_policy(Some(policy.clone())).await;
    assert_eq!(
        harness.handle.phase(),
        spaceterm_server::api::ServicePhase::Serving
    );
    let listeners = harness.listeners.subscribe_status().borrow().clone();
    assert_eq!(listeners.len(), 2);
    assert!(
        listeners
            .iter()
            .all(|v| v.state == ListenerState::Unavailable)
    );
    assert!(tokio::net::TcpStream::connect(endpoints[0]).await.is_err());
    tokio::net::TcpStream::connect(endpoints[1]).await.unwrap();

    // The native/status producer is a frozen-port fake; listener and actor facts
    // above come from their real recovery path and populate its bounded feed.
    let mut parts = status(1).parts().clone();
    parts.remote_access = harness
        .handle
        .snapshot()
        .state()
        .retained
        .as_ref()
        .unwrap()
        .remote_access()
        .clone();
    parts.listeners = listeners;
    let expected = ServerStatus::new(parts).unwrap();
    harness._status.send_replace(expected.clone());
    let query = Payload::Major2(wire::v2::Message {
        kind: Some(wire::v2::message::Kind::StatusQuery(
            wire::v2::StatusQuery {
                request: 7,
                ..Default::default()
            },
        )),
    });
    harness
        .ipc
        .send(&harness.ipc_codec.encode(&query).unwrap())
        .await
        .unwrap();
    let bytes = harness.ipc.recv().await.unwrap().unwrap();
    let Payload::Major2(reply) = harness.ipc_codec.decode(&bytes).unwrap() else {
        panic!("major 2");
    };
    let Some(wire::v2::message::Kind::StatusPublication(reply)) = reply.kind else {
        panic!("status");
    };
    let publication =
        spaceterm_protocol::convert::management::v2::decode_publication(&reply, &|_| {
            Err(ProtocolError::UnexpectedMessage)
        })
        .unwrap();
    assert!(
        matches!(publication, StatusPublication::Snapshot { status, .. } if status == expected)
    );
    assert_eq!(expected.parts().remote_access.value, policy);
    harness
        .handle
        .submit(
            harness.principal,
            actor::request(
                &harness.codec,
                1,
                1,
                Command::SetRemoteAccess {
                    server: actor::server(),
                    expected_version: FieldVersion::new(1),
                    value: RemoteAccessPolicy::disabled(actor::server()),
                },
            ),
        )
        .await
        .unwrap();
    assert!(harness.listeners.subscribe_status().borrow().is_empty());
    harness
        .listeners
        .authenticator()
        .unwrap()
        .check_current(harness.principal)
        .unwrap();
    harness.stop().await;
}

#[tokio::test]
async fn partial_bind_failure_preserves_enabled_intent_and_closes_the_partial_scope() {
    use spaceterm_core::{
        authority::FieldVersion, management::ListenerState, operations::Command,
        remote_access::RemoteAccessPolicy,
    };
    let harness = ListenerHarness::start().await;
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reservation = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
    let available = reservation.local_addr().unwrap();
    drop(reservation);
    let addresses = vec![available, occupied.local_addr().unwrap()];
    let policy = RemoteAccessPolicy::enabled(actor::server(), addresses.clone()).unwrap();
    let response = harness
        .handle
        .submit(
            harness.principal,
            actor::request(
                &harness.codec,
                1,
                1,
                Command::SetRemoteAccess {
                    server: actor::server(),
                    expected_version: FieldVersion::new(1),
                    value: policy.clone(),
                },
            ),
        )
        .await;
    assert_eq!(
        response.err().unwrap().class,
        ServerFailureClass::RuntimeUnavailable
    );
    assert_eq!(
        harness
            .handle
            .snapshot()
            .state()
            .retained
            .as_ref()
            .unwrap()
            .remote_access()
            .value,
        policy
    );
    let facts = harness.listeners.subscribe_status().borrow().clone();
    assert_eq!(facts.len(), 2);
    assert!(
        facts
            .iter()
            .all(|v| v.state == ListenerState::Unavailable && v.generation.get() == 2)
    );
    assert!(tokio::net::TcpStream::connect(available).await.is_err());
    // The failed bind does not alter the foreign listener occupying the other endpoint.
    tokio::net::TcpStream::connect(addresses[1]).await.unwrap();
    harness
        .handle
        .submit(
            harness.principal,
            actor::request(
                &harness.codec,
                1,
                2,
                Command::SetRemoteAccess {
                    server: actor::server(),
                    expected_version: FieldVersion::new(2),
                    value: RemoteAccessPolicy::disabled(actor::server()),
                },
            ),
        )
        .await
        .unwrap();
    assert!(harness.listeners.subscribe_status().borrow().is_empty());
    harness.stop().await;
}

#[tokio::test]
async fn same_account_unpaired_peer_is_not_authorized_or_enrolled() {
    let fixture = Fixture::new();
    let directory = Directory::new();
    let listener = directory.listener();
    let client = directory.connect().await;
    let authenticator =
        Authenticator::new(fixture.config(client.peer_facts().local_uid.unwrap())).unwrap();
    let pending = listener.accept().await.unwrap();
    let (server, client_result) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        fixture.client_handshake(&client, 99, 2, Purpose::Ordinary)
    );
    assert_eq!(
        server.err().unwrap().class,
        ServerFailureClass::Authentication
    );
    assert_eq!(client_result.err(), Some(ProtocolError::Authentication));
    assert_eq!(fixture.key.signs.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.authority.registered.load(Ordering::SeqCst), 0);
    authenticator.close().await.unwrap();
}

macro_rules! crossed {
    ($name:ident,$major:expr) => {
        #[tokio::test]
        async fn $name() {
            let fixture = Fixture::new();
            let directory = Directory::new();
            let listener = directory.listener();
            let first = directory.connect().await;
            let authenticator =
                Authenticator::new(fixture.config(first.peer_facts().local_uid.unwrap())).unwrap();
            let pending = listener.accept().await.unwrap();
            let (one, codec) = tokio::join!(
                authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
                fixture.client_handshake(&first, 1, $major, Purpose::Ordinary)
            );
            let one = one.unwrap();
            let Established::Ordinary(mut codec) = codec.unwrap() else {
                panic!("ordinary");
            };
            let second = directory.connect().await;
            let pending = listener.accept().await.unwrap();
            let (two, _) = tokio::join!(
                authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
                fixture.client_handshake(&second, 2, $major, Purpose::Ordinary)
            );
            let mut two = two.unwrap();
            let payload = if $major == 1 {
                Payload::Major1(wire::v1::Message {
                    kind: Some(wire::v1::message::Kind::OrganizationQuery(
                        Default::default(),
                    )),
                })
            } else {
                Payload::Major2(wire::v2::Message {
                    kind: Some(wire::v2::message::Kind::OrganizationQuery(
                        Default::default(),
                    )),
                })
            };
            second.send(&codec.encode(&payload).unwrap()).await.unwrap();
            let bytes = two.transport.recv().await.unwrap().unwrap();
            let PeerConnection::Ordinary(session) = &mut two.connection else {
                panic!("ordinary");
            };
            assert_eq!(
                session.codec.decode(&bytes),
                Err(ProtocolError::Authentication)
            );
            one.close().await.unwrap();
            two.close().await.unwrap();
            authenticator.close().await.unwrap();
        }
    };
}
crossed!(
    cross_connection_envelope_is_rejected_over_native_ipc_major_1,
    1
);
crossed!(
    cross_connection_envelope_is_rejected_over_native_ipc_major_2,
    2
);
