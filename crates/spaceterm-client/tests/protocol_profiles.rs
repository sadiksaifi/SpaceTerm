mod support;
use spaceterm_client::{
    api::{ConnectingPhase, ConnectionState},
    registry::ClientRegistry,
};
use spaceterm_protocol::{
    codec::Payload,
    wire::{v1, v2},
};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use support::{peer::*, *};

#[tokio::test(start_paused = true)]
async fn connected_requires_authentication_snapshot_and_control_reconciliation() {
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    plan.snapshot_gate.forget_permits(100);
    plan.reconciliation_gate.forget_permits(100);
    let mut services = services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    );
    let delegate = Arc::new(Delegate::default());
    services.delegate = delegate.clone();
    let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    assert!(matches!(
        *server.state().borrow(),
        ConnectionState::Connecting {
            phase: ConnectingPhase::Reconciling
        }
    ));
    assert_eq!(plan.queries.load(Ordering::SeqCst), 1);
    assert!(!server.replica().borrow().is_ready());
    plan.snapshot_gate.add_permits(1);
    settle().await;
    assert!(server.replica().borrow().is_ready());
    assert!(matches!(
        *server.state().borrow(),
        ConnectionState::Connecting {
            phase: ConnectingPhase::Reconciling
        }
    ));
    assert_eq!(delegate.reconciliations.load(Ordering::SeqCst), 0);
    plan.reconciliation_gate.add_permits(1);
    settle().await;
    assert!(matches!(
        *server.state().borrow(),
        ConnectionState::Connected { .. }
    ));
    assert_eq!(delegate.reconciliations.load(Ordering::SeqCst), 1);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn newest_common_major_is_used_for_every_operation() {
    for majors in [vec![2, 1], vec![1]] {
        let expected = majors[0];
        let binding = binding(1);
        let plan = Arc::new(PeerPlan::new(majors));
        let (registry, runtime) = ClientRegistry::recover(services(
            vec![binding.clone()],
            Arc::new(Peers::one(&binding, plan.clone())),
        ))
        .await
        .unwrap();
        let server = registry.server(binding.target().server()).unwrap();
        server.connect().await.unwrap();
        settle().await;
        let info = match *server.state().borrow() {
            ConnectionState::Connected { info } => info,
            ref state => panic!("unexpected state {state:?}"),
        };
        assert_eq!(info.major, expected);
        let message = if expected == 1 {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::StatusQuery(v1::StatusQuery {
                    request: 10,
                    ..Default::default()
                })),
            })
        } else {
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::StatusQuery(v2::StatusQuery {
                    request: 10,
                    ..Default::default()
                })),
            })
        };
        server.dispatch(info, message).await.unwrap();
        settle().await;
        assert_eq!(
            *plan.received.lock().unwrap(),
            vec![expected, expected, expected]
        );
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn revision_gap_requests_one_resync_and_disables_shared_edits() {
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let (registry, runtime) = ClientRegistry::recover(services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    ))
    .await
    .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    let old = match *server.state().borrow() {
        ConnectionState::Connected { info } => info,
        _ => panic!("not ready"),
    };
    plan.snapshot_gate.forget_permits(99);
    plan.control(PeerControl::Gap {
        base: 7,
        revision: 8,
    })
    .await;
    settle().await;
    assert!(!server.replica().borrow().is_ready());
    assert!(matches!(
        *server.state().borrow(),
        ConnectionState::Connecting {
            phase: ConnectingPhase::Reconciling
        }
    ));
    assert_eq!(plan.queries.load(Ordering::SeqCst), 2);
    let message = Payload::Major2(v2::Message {
        kind: Some(v2::message::Kind::StatusQuery(v2::StatusQuery {
            request: 10,
            ..Default::default()
        })),
    });
    assert!(server.dispatch(old, message).await.is_err());
    plan.snapshot_gate.add_permits(1);
    settle().await;
    assert!(
        matches!(*server.state().borrow(), ConnectionState::Connected { info } if info.organization_revision.get() == 8)
    );
    assert_eq!(plan.queries.load(Ordering::SeqCst), 2);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn malformed_authenticated_peer_pauses_automatic_retry() {
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let (registry, runtime) = ClientRegistry::recover(services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    ))
    .await
    .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    plan.control(PeerControl::InvalidEnvelope).await;
    settle().await;
    assert!(
        matches!(*server.state().borrow(), ConnectionState::Offline { retry, .. } if retry.delay.is_none())
    );
    tokio::time::advance(Duration::from_secs(60)).await;
    settle().await;
    assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn old_attempt_completion_cannot_replace_new_connection() {
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    plan.snapshot_gate.forget_permits(100);
    let (registry, runtime) = ClientRegistry::recover(services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    ))
    .await
    .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    server.retry().await.unwrap();
    settle().await;
    assert_eq!(plan.opens.load(Ordering::SeqCst), 2);
    plan.snapshot_gate.add_permits(2);
    settle().await;
    assert!(
        matches!(*server.state().borrow(), ConnectionState::Connected { info } if info.attempt == 2 && info.binding.generation.get() == 2)
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn client_shutdown_sends_no_release_or_server_shutdown() {
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let (registry, runtime) = ClientRegistry::recover(services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    ))
    .await
    .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    assert_eq!(plan.received.lock().unwrap().len(), 2);
    runtime.shutdown().await.unwrap();
    settle().await;
    // Only organization and reconciliation requests crossed the authenticated
    // route. No shutdown, release or terminal mutation is manufactured on quit.
    assert_eq!(plan.received.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn blocked_outbound_send_does_not_delay_authoritative_publication() {
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let (registry, runtime) = ClientRegistry::recover(services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    ))
    .await
    .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    let info = match *server.state().borrow() {
        ConnectionState::Connected { info } => info,
        _ => panic!("not ready"),
    };
    plan.client_send_gate
        .forget_permits(plan.client_send_gate.available_permits());
    let sender = server.clone();
    let pending = tokio::spawn(async move {
        sender
            .dispatch(
                info,
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::StatusQuery(v2::StatusQuery {
                        request: 10,
                        ..Default::default()
                    })),
                }),
            )
            .await
    });
    settle().await;
    plan.control(PeerControl::Gap {
        base: 1,
        revision: 2,
    })
    .await;
    settle().await;
    assert_eq!(
        server
            .replica()
            .borrow()
            .retained()
            .unwrap()
            .cursor
            .revision
            .get(),
        2
    );
    assert!(!pending.is_finished());
    plan.client_send_gate.add_permits(1);
    pending.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn disconnect_discards_pending_dispatches_without_replaying_the_old_queue() {
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let (registry, runtime) = ClientRegistry::recover(services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    ))
    .await
    .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    let info = match *server.state().borrow() {
        ConnectionState::Connected { info } => info,
        _ => panic!("not ready"),
    };
    plan.client_send_gate
        .forget_permits(plan.client_send_gate.available_permits());
    let mut pending = tokio::task::JoinSet::new();
    for request in 10..12 {
        let server = server.clone();
        pending.spawn(async move {
            server
                .dispatch(
                    info,
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::StatusQuery(v2::StatusQuery {
                            request,
                            ..Default::default()
                        })),
                    }),
                )
                .await
        });
    }
    settle().await;
    plan.control(PeerControl::Disconnect).await;
    settle().await;
    assert!(matches!(
        *server.state().borrow(),
        ConnectionState::Offline { .. }
    ));
    while let Some(result) = pending.join_next().await {
        assert!(result.unwrap().is_err());
    }
    plan.client_send_gate.add_permits(100);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert!(
        matches!(*server.state().borrow(),ConnectionState::Connected { info: current } if current.binding != info.binding)
    );
    assert_eq!(plan.received.lock().unwrap().len(), 4);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn declared_extensions_are_negotiated_only_when_the_client_installs_them() {
    use spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT;
    for installed in [false, true] {
        let binding = binding(1);
        let mut peer = PeerPlan::new(vec![2, 1]);
        peer.capabilities.push(CAPABILITY_SERVER_GIT);
        peer.capabilities.sort_unstable();
        let plan = Arc::new(peer);
        let mut services = services(vec![binding.clone()], Arc::new(Peers::one(&binding, plan)));
        if installed {
            services.additional_capabilities.push(CAPABILITY_SERVER_GIT);
        }
        let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
        let server = registry.server(binding.target().server()).unwrap();
        server.connect().await.unwrap();
        settle().await;
        let info = match *server.state().borrow() {
            ConnectionState::Connected { info } => info,
            _ => panic!("not ready"),
        };
        assert_eq!(info.capabilities.supports(CAPABILITY_SERVER_GIT), installed);
        assert_eq!(
            info.limits,
            spaceterm_protocol::limits::NegotiatedLimits::default()
        );
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn authenticated_stopped_status_pauses_reconnect_without_native_discovery() {
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let (registry, runtime) = ClientRegistry::recover(services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    ))
    .await
    .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    plan.control(PeerControl::Stopped).await;
    settle().await;
    assert!(matches!(*server.state().borrow(), ConnectionState::Stopped));
    tokio::time::advance(Duration::from_secs(60)).await;
    settle().await;
    assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
    assert_eq!(registry.servers().len(), 1);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn snapshot_delta_suffix_is_reconciled_in_both_majors() {
    for major in [1, 2] {
        let binding = binding(1);
        let plan = Arc::new(PeerPlan::new(vec![major]));
        *plan.suffix_before_reconciliation.lock().unwrap() = Some((1, 2));
        let (registry, runtime) = ClientRegistry::recover(services(
            vec![binding.clone()],
            Arc::new(Peers::one(&binding, plan.clone())),
        ))
        .await
        .unwrap();
        let server = registry.server(binding.target().server()).unwrap();
        server.connect().await.unwrap();
        settle().await;
        assert!(
            matches!(*server.state().borrow(), ConnectionState::Connected { info } if info.major == major && info.organization_revision.get() == 2),
            "{:?}",
            *server.state().borrow()
        );
        assert_eq!(
            server
                .replica()
                .borrow()
                .retained()
                .unwrap()
                .cursor
                .revision
                .get(),
            2
        );
        assert_eq!(*plan.reconciliation_revisions.lock().unwrap(), [1, 2]);
        assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn reconciliation_suffix_gap_requests_a_fresh_base_in_both_majors() {
    for major in [1, 2] {
        let binding = binding(1);
        let plan = Arc::new(PeerPlan::new(vec![major]));
        *plan.suffix_before_reconciliation.lock().unwrap() = Some((7, 8));
        let (registry, runtime) = ClientRegistry::recover(services(
            vec![binding.clone()],
            Arc::new(Peers::one(&binding, plan.clone())),
        ))
        .await
        .unwrap();
        let server = registry.server(binding.target().server()).unwrap();
        server.connect().await.unwrap();
        settle().await;
        assert!(
            matches!(*server.state().borrow(), ConnectionState::Connected { info } if info.organization_revision.get() == 8),
            "{:?}",
            *server.state().borrow()
        );
        assert_eq!(plan.queries.load(Ordering::SeqCst), 2);
        assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
        runtime.shutdown().await.unwrap();
    }
}

#[derive(Default)]
struct AcknowledgingDelegate {
    admitted: std::sync::atomic::AtomicBool,
}
impl spaceterm_client::ports::ConnectionDelegate for AcknowledgingDelegate {
    fn invalidate(
        &self,
        _: &spaceterm_client::api::PairedServerBinding,
        _: spaceterm_client::ports::ConnectionInvalidation,
    ) {
    }
    fn reconcile<'a>(
        &'a self,
        _: spaceterm_client::api::ConnectedInfo,
        _: &'a spaceterm_client::replica::ReplicaSnapshot,
        _: &'a mut dyn spaceterm_client::ports::ProtocolChannel,
    ) -> spaceterm_client::ports::ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn publication(
        &self,
        _: spaceterm_client::api::ConnectedInfo,
        _: &spaceterm_client::replica::ReplicaSnapshot,
    ) -> Result<(), spaceterm_client::failure::ClientFailure> {
        Ok(())
    }
    fn admit(
        &self,
        _: spaceterm_client::api::ConnectedInfo,
        _: &Payload,
    ) -> Result<spaceterm_client::ports::OutboundClass, spaceterm_client::failure::ClientFailure>
    {
        Ok(spaceterm_client::ports::OutboundClass::Lifecycle)
    }
    fn receive<'a>(
        &'a self,
        info: spaceterm_client::api::ConnectedInfo,
        _: Payload,
        channel: &'a mut dyn spaceterm_client::ports::ProtocolChannel,
    ) -> spaceterm_client::ports::ClientFuture<'a, ()> {
        Box::pin(async move {
            let identity = || {
                Some(spaceterm_protocol::wire::common::Identity {
                    value: {
                        let mut bytes = vec![0; 16];
                        bytes[15] = 1;
                        bytes
                    },
                })
            };
            let message = match info.major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::Acknowledgement(v1::Acknowledgement {
                        pairing: identity(),
                        client: identity(),
                        stream: identity(),
                        contiguous_sequence: 1,
                    })),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::Acknowledgement(v2::Acknowledgement {
                        pairing: identity(),
                        client: identity(),
                        stream: identity(),
                        contiguous_sequence: 1,
                    })),
                }),
                _ => unreachable!(),
            };
            channel.send(&message).await?;
            self.admitted.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn callback_acknowledgement_uses_the_ordered_writer_without_blocking_publications() {
    for major in [1, 2] {
        let binding = binding(1);
        let plan = Arc::new(PeerPlan::new(vec![major]));
        let delegate = Arc::new(AcknowledgingDelegate::default());
        let mut services = services(
            vec![binding.clone()],
            Arc::new(Peers::one(&binding, plan.clone())),
        );
        services.delegate = delegate.clone();
        let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
        let server = registry.server(binding.target().server()).unwrap();
        server.connect().await.unwrap();
        until(|| matches!(*server.state().borrow(), ConnectionState::Connected { .. })).await;
        let ConnectionState::Connected { info } = *server.state().borrow() else {
            unreachable!()
        };
        let before = plan.sends_started.load(Ordering::SeqCst);
        plan.client_send_gate
            .forget_permits(plan.client_send_gate.available_permits());
        let sender = server.clone();
        let pending = tokio::spawn(async move {
            let message = match major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::StatusQuery(v1::StatusQuery {
                        request: 10,
                        ..Default::default()
                    })),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::StatusQuery(v2::StatusQuery {
                        request: 10,
                        ..Default::default()
                    })),
                }),
                _ => unreachable!(),
            };
            sender.dispatch(info, message).await
        });
        until(|| plan.sends_started.load(Ordering::SeqCst) > before).await;
        plan.control(PeerControl::Callback).await;
        until(|| delegate.admitted.load(Ordering::SeqCst)).await;
        plan.control(PeerControl::Gap {
            base: 1,
            revision: 2,
        })
        .await;
        until(|| {
            server
                .replica()
                .borrow()
                .retained()
                .unwrap()
                .cursor
                .revision
                .get()
                == 2
        })
        .await;
        assert!(!pending.is_finished());
        assert_eq!(plan.sends_started.load(Ordering::SeqCst), before + 1);
        plan.client_send_gate.add_permits(2);
        pending.await.unwrap().unwrap();
        until(|| plan.dispatch_and_ack.lock().unwrap().len() == 2).await;
        assert_eq!(*plan.dispatch_and_ack.lock().unwrap(), ["dispatch", "ack"]);
        assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
        runtime.shutdown().await.unwrap();
    }
}

#[cfg(feature = "test-support")]
mod fake_profiles {
    use spaceterm_client::{
        ports::*,
        testing::{FakeFault, FakeServer},
    };
    use spaceterm_core::{
        control::TakeOverRequest,
        operations::{Command, WorkspaceCreationSource},
        organization::{ControlRecord, NameDraft},
    };
    use spaceterm_protocol::{
        authentication::{ClientHandshake, Established, HandshakeConfig, Principal, Purpose},
        codec::Payload,
        connection::AuthenticatedConnection,
        convert::{control, identity::*},
        negotiation::{CAPABILITY_CANONICAL_DOMAIN, CAPABILITY_NORMALIZED_CREATION_TOPOLOGY},
        wire::{v1, v2},
    };
    use std::{num::NonZeroU64, sync::atomic::Ordering, time::Duration};

    async fn connect(
        server: &FakeServer,
        major: u32,
        client: u64,
    ) -> (ConnectedRoute, AuthenticatedConnection) {
        let binding = server.add_client(client).unwrap();
        let target = binding.target();
        let access = server
            .signing_access(target, ConnectionPurpose::Ordinary)
            .await
            .unwrap();
        let principal = Principal {
            server: target.server().bytes(),
            device: target.device().bytes(),
            pairing: encode_local(target.pairing().local())
                .value
                .try_into()
                .unwrap(),
            client: encode_local(target.client().local())
                .value
                .try_into()
                .unwrap(),
        };
        let config = HandshakeConfig {
            majors: vec![major],
            capabilities: vec![
                CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
                CAPABILITY_CANONICAL_DOMAIN,
            ],
            ..Default::default()
        };
        let (handshake, hello) =
            ClientHandshake::begin(target.pin(), principal, Purpose::Ordinary, config, access.0)
                .unwrap();
        let mut route = server
            .open(target, ConnectionPurpose::Ordinary)
            .await
            .unwrap();
        route.sender.send(&hello).await.unwrap();
        let reply = route.receiver.receive().await.unwrap().unwrap();
        let (handshake, proof) = handshake.receive_server(&reply).unwrap();
        route.sender.send(&proof).await.unwrap();
        let ready = route.receiver.receive().await.unwrap().unwrap();
        let Established::Ordinary(codec) = handshake.receive_ready(&ready).unwrap() else {
            panic!("ordinary codec required")
        };
        (route, codec)
    }
    async fn send(route: &ConnectedRoute, codec: &mut AuthenticatedConnection, payload: &Payload) {
        route
            .sender
            .send(&codec.encode(payload).unwrap())
            .await
            .unwrap();
    }
    async fn receive(route: &mut ConnectedRoute, codec: &mut AuthenticatedConnection) -> Payload {
        let bytes = tokio::time::timeout(Duration::from_secs(2), route.receiver.receive())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        codec.decode(&bytes).unwrap()
    }
    fn create_workspace(server: &FakeServer) {
        server
            .command(
                server.next_operation(1).unwrap(),
                Command::CreateWorkspace {
                    server: server.server(),
                    source: WorkspaceCreationSource::ServerHome {
                        server: server.server(),
                    },
                    name: NameDraft::Automatic,
                },
            )
            .unwrap();
    }
    #[tokio::test]
    async fn fake_server_authenticates_both_profiles_and_retries_real_policy_once() {
        for major in [1, 2] {
            let server = FakeServer::new(1, vec![2, 1]).unwrap();
            create_workspace(&server);
            let (mut route, mut codec) = connect(&server, major, 2).await;
            let snapshot = server.snapshot().unwrap();
            let pane = snapshot.state().panes[0].id;
            let client = server.binding(2).unwrap().target().client();
            let id = server.next_operation(2).unwrap();
            let command = Command::TakeOver(TakeOverRequest {
                epoch: snapshot.epoch(),
                pane,
                client,
            });
            let request = NonZeroU64::new(1).unwrap();
            let payload = match major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::Operation(v1::Operation {
                        id: Some(encode_operation(id)),
                        action: Some(v1::operation::Action::IdentifiedControl(
                            control::v1::encode_request(&command, request).unwrap(),
                        )),
                    })),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::Operation(v2::Operation {
                        id: Some(encode_operation(id)),
                        action: Some(v2::operation::Action::IdentifiedControl(
                            control::v2::encode_request(&command, request).unwrap(),
                        )),
                    })),
                }),
                _ => unreachable!(),
            };
            server
                .faults
                .drop_next_outcome
                .store(true, Ordering::Release);
            send(&route, &mut codec, &payload).await;
            tokio::time::timeout(Duration::from_secs(2), async {
                while server.received_operations().unwrap().len() < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let committed = server.snapshot().unwrap();
            send(&route, &mut codec, &payload).await;
            let outcome = receive(&mut route, &mut codec).await;
            let grant = match outcome {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::Outcome(outcome)),
                }) => {
                    assert_eq!(
                        decode_operation(outcome.operation.as_ref().unwrap()).unwrap(),
                        id
                    );
                    assert_eq!(
                        outcome.publication_revision,
                        committed.state().revision.get()
                    );
                    let Some(v1::outcome::Result::ControlGranted(grant)) = outcome.result else {
                        panic!("grant expected")
                    };
                    control::v1::decode_grant(&grant).unwrap()
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::Outcome(outcome)),
                }) => {
                    assert_eq!(
                        decode_operation(outcome.operation.as_ref().unwrap()).unwrap(),
                        id
                    );
                    assert_eq!(
                        outcome.publication_revision,
                        committed.state().revision.get()
                    );
                    let Some(v2::outcome::Result::ControlGranted(grant)) = outcome.result else {
                        panic!("grant expected")
                    };
                    control::v2::decode_grant(&grant).unwrap()
                }
                _ => panic!("identified outcome required"),
            };
            assert_eq!(
                committed.runtime()[0].control,
                ControlRecord::Held {
                    client,
                    generation: grant.grant().generation()
                }
            );
            assert_eq!(
                server.snapshot().unwrap(),
                committed,
                "retry returns the retained ledger result without another takeover"
            );
            assert_eq!(server.negotiated_majors().unwrap(), [major]);
            server.fault(FakeFault::Disconnect).unwrap();
            assert!(route.receiver.receive().await.unwrap().is_none());
        }
    }
    #[tokio::test]
    async fn fake_server_does_not_acknowledge_foreign_reconciliation_facts() {
        for major in [1, 2] {
            let server = FakeServer::new(1, vec![2, 1]).unwrap();
            let (mut route, mut codec) = connect(&server, major, 1).await;
            let client = server.binding(1).unwrap().target().client();
            let revision = server.snapshot().unwrap().state().revision.get();
            let payload = match major {
                1 => Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::Reconciliation(v1::Reconciliation {
                        request: 1,
                        request_generation: 1,
                        server: Some(encode_global([2; 16])),
                        epoch: 1,
                        client: Some(encode_client(client)),
                        organization_revision: revision,
                        ..Default::default()
                    })),
                }),
                2 => Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::Reconciliation(v2::Reconciliation {
                        request: 1,
                        request_generation: 1,
                        server: Some(encode_global([2; 16])),
                        epoch: 1,
                        client: Some(encode_client(client)),
                        organization_revision: revision,
                        ..Default::default()
                    })),
                }),
                _ => unreachable!(),
            };
            send(&route, &mut codec, &payload).await;
            assert!(
                route.receiver.receive().await.unwrap().is_none(),
                "foreign facts must not receive a success acknowledgement"
            );
        }
    }
    fn startup_payload(
        major: u32,
        id: spaceterm_core::operations::OperationId,
        command: &Command,
    ) -> Payload {
        use spaceterm_protocol::convert::startup;
        match major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Operation(v1::Operation {
                    id: Some(encode_operation(id)),
                    action: Some(v1::operation::Action::IdentifiedStartup(
                        startup::v1::encode_request(command).unwrap(),
                    )),
                })),
            }),
            2 => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Operation(v2::Operation {
                    id: Some(encode_operation(id)),
                    action: Some(v2::operation::Action::IdentifiedStartup(
                        startup::v2::encode_request(command).unwrap(),
                    )),
                })),
            }),
            _ => unreachable!(),
        }
    }
    fn assert_stale_startup_outcome(payload: Payload) {
        match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Outcome(outcome)),
            }) => assert!(matches!(outcome.result,
                Some(v1::outcome::Result::OperationFailure(failure)) if failure.kind == v1::OperationFailureKind::OpStaleAuthority as i32)),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Outcome(outcome)),
            }) => assert!(matches!(outcome.result,
                Some(v2::outcome::Result::OperationFailure(failure)) if failure.kind == v2::OperationFailureKind::OpStaleAuthority as i32)),
            _ => panic!("tracked stale startup outcome required"),
        }
    }
    fn ended_startup_pane(server: &FakeServer) {
        use spaceterm_core::{authority::*, organization::*, reducer::OrganizationEvent};
        create_workspace(server);
        let runtime = server.snapshot().unwrap().runtime()[0];
        let session = SessionKey {
            pane: runtime.pane,
            epoch: ServerEpoch::new(1),
            generation: SessionGeneration::new(1),
        };
        server
            .event(OrganizationEvent::StartupReserved(
                StartupReservation::new(
                    runtime.pane,
                    runtime.session_generation,
                    runtime.control_generation,
                    runtime.control,
                    session,
                )
                .unwrap(),
            ))
            .unwrap();
        server
            .event(OrganizationEvent::StartupCancelled { session })
            .unwrap();
    }
    #[tokio::test]
    async fn fake_server_authenticated_startup_rejects_never_admitted_old_connection_in_both_majors()
     {
        use spaceterm_core::{authority::*, control::*};
        for major in [1, 2] {
            let server = FakeServer::new(1, vec![2, 1]).unwrap();
            ended_startup_pane(&server);
            let (old_route, _) = connect(&server, major, 1).await;
            old_route.sender.close().await.unwrap();
            let (mut route, mut codec) = connect(&server, major, 1).await;
            let snapshot = server.snapshot().unwrap();
            let runtime = snapshot.runtime()[0];
            let client = server.binding(1).unwrap().target().client();
            let spaceterm_core::organization::ControlRecord::Held { generation, .. } =
                runtime.control
            else {
                panic!("held control required")
            };
            let fence = EffectFence::new(
                SessionKey {
                    pane: runtime.pane,
                    epoch: snapshot.epoch(),
                    generation: runtime.session_generation,
                },
                ControlGrant::new(runtime.pane, client, generation).unwrap(),
                ConnectionBinding::new(client, ConnectionGeneration::new(1)),
                ContextGeneration::new(1),
                None,
            )
            .unwrap();
            let id = server.next_operation(1).unwrap();
            let intent = StartupIntent::new(id, fence, StartupSource::StartSession).unwrap();
            send(
                &route,
                &mut codec,
                &startup_payload(major, id, &Command::BeginStartup(intent)),
            )
            .await;
            assert_stale_startup_outcome(receive(&mut route, &mut codec).await);
            assert_eq!(server.snapshot().unwrap().runtime()[0].phase, runtime.phase);
            assert_eq!(
                server.snapshot().unwrap().runtime()[0].session_generation,
                runtime.session_generation
            );
        }
    }

    fn startup_fence(
        server: &FakeServer,
        connection: u64,
        context: u64,
    ) -> spaceterm_core::authority::EffectFence {
        use spaceterm_core::{authority::*, organization::ControlRecord};
        let snapshot = server.snapshot().unwrap();
        let runtime = snapshot.runtime()[0];
        let ControlRecord::Held { client, generation } = runtime.control else {
            panic!("held control required")
        };
        EffectFence::new(
            SessionKey {
                pane: runtime.pane,
                epoch: snapshot.epoch(),
                generation: runtime.session_generation,
            },
            ControlGrant::new(runtime.pane, client, generation).unwrap(),
            ConnectionBinding::new(client, ConnectionGeneration::new(connection)),
            ContextGeneration::new(context),
            None,
        )
        .unwrap()
    }
    fn assert_awaiting_measurement(payload: Payload) {
        match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Outcome(outcome)),
            }) => assert!(matches!(
                outcome.result,
                Some(v1::outcome::Result::StartupAwaitingMeasurement(_))
            )),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Outcome(outcome)),
            }) => assert!(matches!(
                outcome.result,
                Some(v2::outcome::Result::StartupAwaitingMeasurement(_))
            )),
            _ => panic!("startup intent required"),
        }
    }
    #[tokio::test]
    async fn fake_server_authenticated_startup_rejects_old_context_begin_and_completion_in_both_majors()
     {
        use spaceterm_core::{authority::ContextGeneration, control::*, geometry::*};
        for major in [1, 2] {
            for complete in [false, true] {
                for measured in [false, true] {
                    let server = FakeServer::new(1, vec![2, 1]).unwrap();
                    ended_startup_pane(&server);
                    let (mut route, mut codec) = connect(&server, major, 1).await;
                    let fence = startup_fence(&server, 1, 1);
                    let before = server.snapshot().unwrap().runtime()[0];
                    let id = server.next_operation(1).unwrap();
                    let intent =
                        StartupIntent::new(id, fence, StartupSource::StartSession).unwrap();
                    if complete {
                        send(
                            &route,
                            &mut codec,
                            &startup_payload(major, id, &Command::BeginStartup(intent)),
                        )
                        .await;
                        assert_awaiting_measurement(receive(&mut route, &mut codec).await);
                    }
                    server
                        .update_context(
                            fence.connection(),
                            fence.session().pane,
                            ContextGeneration::new(2),
                        )
                        .unwrap();
                    let command = if complete {
                        let legacy = StartupMeasurement {
                            intent: id,
                            fence,
                            grid: GridSize::new(
                                30,
                                100,
                                CellPixels::new(8, 16).unwrap(),
                                SUPPORTED_GRID_LIMITS,
                            )
                            .unwrap(),
                        };
                        if measured {
                            Command::CompleteMeasuredStartup(legacy.into())
                        } else {
                            Command::CompleteStartup(legacy)
                        }
                    } else {
                        Command::BeginStartup(intent)
                    };
                    let operation = server.next_operation(1).unwrap();
                    send(
                        &route,
                        &mut codec,
                        &startup_payload(major, operation, &command),
                    )
                    .await;
                    assert_stale_startup_outcome(receive(&mut route, &mut codec).await);
                    assert_eq!(
                        server.snapshot().unwrap().runtime()[0],
                        before,
                        "stale authority cannot reserve or start a Session"
                    );
                }
            }
        }
    }
    #[tokio::test]
    async fn fake_server_authenticated_startup_rejects_old_connection_completion_in_both_majors() {
        use spaceterm_core::{control::*, geometry::*};
        for major in [1, 2] {
            for measured in [false, true] {
                let server = FakeServer::new(1, vec![2, 1]).unwrap();
                ended_startup_pane(&server);
                let (mut route, mut codec) = connect(&server, major, 1).await;
                let fence = startup_fence(&server, 1, 1);
                let before = server.snapshot().unwrap().runtime()[0];
                let id = server.next_operation(1).unwrap();
                let intent = StartupIntent::new(id, fence, StartupSource::StartSession).unwrap();
                send(
                    &route,
                    &mut codec,
                    &startup_payload(major, id, &Command::BeginStartup(intent)),
                )
                .await;
                assert_awaiting_measurement(receive(&mut route, &mut codec).await);
                route.sender.close().await.unwrap();
                let (mut route, mut codec) = connect(&server, major, 1).await;
                let legacy = StartupMeasurement {
                    intent: id,
                    fence,
                    grid: GridSize::new(
                        30,
                        100,
                        CellPixels::new(8, 16).unwrap(),
                        SUPPORTED_GRID_LIMITS,
                    )
                    .unwrap(),
                };
                let command = if measured {
                    Command::CompleteMeasuredStartup(legacy.into())
                } else {
                    Command::CompleteStartup(legacy)
                };
                let operation = server.next_operation(1).unwrap();
                send(
                    &route,
                    &mut codec,
                    &startup_payload(major, operation, &command),
                )
                .await;
                assert_stale_startup_outcome(receive(&mut route, &mut codec).await);
                assert_eq!(server.snapshot().unwrap().runtime()[0], before);
            }
        }
    }
    #[tokio::test]
    async fn fake_server_startup_checks_authenticated_dispatch_not_only_the_claimed_current_fence()
    {
        use spaceterm_core::control::*;
        for major in [1, 2] {
            let server = FakeServer::new(1, vec![2, 1]).unwrap();
            ended_startup_pane(&server);
            let (mut old_route, mut old_codec) = connect(&server, major, 1).await;
            let (_current_route, _) = connect(&server, major, 1).await;
            let fence = startup_fence(&server, 2, 1);
            let before = server.snapshot().unwrap().runtime()[0];
            let id = server.next_operation(1).unwrap();
            let intent = StartupIntent::new(id, fence, StartupSource::StartSession).unwrap();
            send(
                &old_route,
                &mut old_codec,
                &startup_payload(major, id, &Command::BeginStartup(intent)),
            )
            .await;
            assert_stale_startup_outcome(receive(&mut old_route, &mut old_codec).await);
            assert_eq!(server.snapshot().unwrap().runtime()[0], before);
        }
    }
    #[tokio::test]
    async fn fake_server_authenticated_startup_accepts_current_connection_and_context_in_both_majors()
     {
        use spaceterm_core::{authority::*, control::*, geometry::*, organization::SessionPhase};
        for major in [1, 2] {
            let server = FakeServer::new(1, vec![2, 1]).unwrap();
            ended_startup_pane(&server);
            let (mut route, mut codec) = connect(&server, major, 1).await;
            let fence = startup_fence(&server, 1, 2);
            server
                .update_context(
                    fence.connection(),
                    fence.session().pane,
                    ContextGeneration::new(2),
                )
                .unwrap();
            let id = server.next_operation(1).unwrap();
            let intent = StartupIntent::new(id, fence, StartupSource::StartSession).unwrap();
            send(
                &route,
                &mut codec,
                &startup_payload(major, id, &Command::BeginStartup(intent)),
            )
            .await;
            assert_awaiting_measurement(receive(&mut route, &mut codec).await);
            let command = Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
                intent: id,
                fence,
                grid: MeasuredGrid::new(
                    1024,
                    1024,
                    CellPixels::new(64, 16).unwrap(),
                    SUPPORTED_GRID_LIMITS,
                )
                .unwrap(),
            });
            let operation = server.next_operation(1).unwrap();
            send(
                &route,
                &mut codec,
                &startup_payload(major, operation, &command),
            )
            .await;
            let started = match receive(&mut route, &mut codec).await {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::Outcome(outcome)),
                }) => {
                    let Some(v1::outcome::Result::SessionStarted(session)) = outcome.result else {
                        panic!("started Session required")
                    };
                    decode_session(&session).unwrap()
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::Outcome(outcome)),
                }) => {
                    let Some(v2::outcome::Result::SessionStarted(session)) = outcome.result else {
                        panic!("started Session required")
                    };
                    decode_session(&session).unwrap()
                }
                _ => panic!("startup outcome required"),
            };
            assert_eq!(
                started,
                SessionKey {
                    generation: fence.session().generation.checked_successor().unwrap(),
                    ..fence.session()
                }
            );
            assert!(matches!(
                server.snapshot().unwrap().runtime()[0].phase,
                SessionPhase::Running { .. }
            ));
        }
    }
}
