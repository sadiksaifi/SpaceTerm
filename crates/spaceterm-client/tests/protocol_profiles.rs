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
