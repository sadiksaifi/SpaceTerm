mod support;
use spaceterm_client::{
    api::{ConnectedInfo, ConnectionState, ConnectionTarget, PairedServerBinding},
    failure::{ClientFailure, ClientFailureClass},
    ports::*,
    registry::ClientRegistry,
    replica::ReplicaSnapshot,
};
use spaceterm_protocol::{codec::Payload, wire::v2};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use support::{peer::*, *};

#[tokio::test(start_paused = true)]
async fn incompatible_server_does_not_disable_compatible_server() {
    let first = binding(1);
    let second = binding(2);
    let first_plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let mut incompatible = PeerPlan::new(vec![2, 1]);
    incompatible.capabilities.clear();
    let second_plan = Arc::new(incompatible);
    let peers = Peers(BTreeMap::from([
        (first.target().server(), first_plan),
        (second.target().server(), second_plan),
    ]));
    let (registry, runtime) = ClientRegistry::recover(services(
        vec![first.clone(), second.clone()],
        Arc::new(peers),
    ))
    .await
    .unwrap();
    let a = registry.server(first.target().server()).unwrap();
    let b = registry.server(second.target().server()).unwrap();
    a.connect().await.unwrap();
    b.connect().await.unwrap();
    settle().await;
    assert!(matches!(
        *a.state().borrow(),
        ConnectionState::Connected { .. }
    ));
    assert!(matches!(
        *b.state().borrow(),
        ConnectionState::Incompatible { .. }
    ));
    assert_eq!(registry.servers().len(), 2);
    assert_eq!(
        a.replica()
            .borrow()
            .retained()
            .unwrap()
            .organization
            .state()
            .server,
        first.target().server()
    );
    assert!(b.replica().borrow().retained().is_none());
    runtime.shutdown().await.unwrap();
}

struct SecondAdmissionDenied(AtomicUsize);
impl ConnectionDelegate for SecondAdmissionDenied {
    fn invalidate(&self, _binding: &PairedServerBinding, _reason: ConnectionInvalidation) {}
    fn reconcile<'a>(
        &'a self,
        _info: ConnectedInfo,
        _replica: &'a ReplicaSnapshot,
        _channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn publication(
        &self,
        _info: ConnectedInfo,
        _replica: &ReplicaSnapshot,
    ) -> Result<(), ClientFailure> {
        Ok(())
    }
    fn admit(
        &self,
        _info: ConnectedInfo,
        _message: &Payload,
    ) -> Result<OutboundClass, ClientFailure> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(OutboundClass::Ordinary)
        } else {
            Err(ClientFailure::new(ClientFailureClass::StaleAuthority))
        }
    }
    fn receive<'a>(
        &'a self,
        _info: ConnectedInfo,
        _message: Payload,
        _channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test(start_paused = true)]
async fn final_dispatch_rejection_does_not_disconnect_a_current_connection() {
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let mut services = services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    );
    services.delegate = Arc::new(SecondAdmissionDenied(AtomicUsize::new(0)));
    let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    let info = match *server.state().borrow() {
        ConnectionState::Connected { info } => info,
        _ => panic!("not ready"),
    };
    let message = Payload::Major2(v2::Message {
        kind: Some(v2::message::Kind::StatusQuery(v2::StatusQuery {
            request: 10,
            ..Default::default()
        })),
    });
    let error = server.dispatch(info, message).await.unwrap_err();
    assert_eq!(error.class(), ClientFailureClass::StaleAuthority);
    assert!(matches!(
        *server.state().borrow(),
        ConnectionState::Connected { .. }
    ));
    assert_eq!(plan.received.lock().unwrap().len(), 2);
    runtime.shutdown().await.unwrap();
}

struct LifecycleDelegate(Delegate);
impl ConnectionDelegate for LifecycleDelegate {
    fn invalidate(&self, binding: &PairedServerBinding, reason: ConnectionInvalidation) {
        self.0.invalidate(binding, reason);
    }
    fn reconcile<'a>(
        &'a self,
        info: ConnectedInfo,
        replica: &'a ReplicaSnapshot,
        channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        self.0.reconcile(info, replica, channel)
    }
    fn publication(
        &self,
        info: ConnectedInfo,
        replica: &ReplicaSnapshot,
    ) -> Result<(), ClientFailure> {
        self.0.publication(info, replica)
    }
    fn admit(
        &self,
        _info: ConnectedInfo,
        message: &Payload,
    ) -> Result<OutboundClass, ClientFailure> {
        let lifecycle = matches!(message,Payload::Major2(v2::Message { kind: Some(v2::message::Kind::StatusQuery(query)) }) if query.request == 999);
        Ok(if lifecycle {
            OutboundClass::Lifecycle
        } else {
            OutboundClass::Ordinary
        })
    }
    fn receive<'a>(
        &'a self,
        info: ConnectedInfo,
        message: Payload,
        channel: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        self.0.receive(info, message, channel)
    }
}
fn query(request: u64) -> Payload {
    Payload::Major2(v2::Message {
        kind: Some(v2::message::Kind::StatusQuery(v2::StatusQuery {
            request,
            ..Default::default()
        })),
    })
}

#[tokio::test(start_paused = true)]
async fn ordinary_queue_saturation_preserves_lifecycle_reserve() {
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let mut services = services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    );
    services.delegate = Arc::new(LifecycleDelegate(Delegate::default()));
    let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    let info = match *server.state().borrow() {
        ConnectionState::Connected { info } => info,
        _ => panic!("not ready"),
    };
    plan.client_send_gate
        .forget_permits(plan.client_send_gate.available_permits());
    let mut ordinary = tokio::task::JoinSet::new();
    for request in 0..121 {
        let server = server.clone();
        ordinary.spawn(async move { server.dispatch(info, query(request + 10)).await });
    }
    settle().await;
    let error = ordinary.join_next().await.unwrap().unwrap().unwrap_err();
    assert_eq!(error.class(), ClientFailureClass::ResourceLimit);
    let lifecycle = tokio::spawn(async move { server.dispatch(info, query(999)).await });
    settle().await;
    assert!(!lifecycle.is_finished());
    plan.client_send_gate.add_permits(200);
    while let Some(result) = ordinary.join_next().await {
        result.unwrap().unwrap();
    }
    lifecycle.await.unwrap().unwrap();
    settle().await;
    assert_eq!(plan.received.lock().unwrap().len(), 123);
    runtime.shutdown().await.unwrap();
}

struct CleanupGate {
    closing: tokio::sync::Semaphore,
    finish: tokio::sync::Semaphore,
    complete: std::sync::atomic::AtomicBool,
}
impl MessageSender for CleanupGate {
    fn send<'a>(&'a self, _: &'a [u8]) -> ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> ClientFuture<'_, ()> {
        Box::pin(async {
            self.closing.add_permits(1);
            self.finish.acquire().await.unwrap().forget();
            self.complete.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
    fn abort(&self) {}
}
struct CleanupRoutes(Arc<CleanupGate>);
struct PendingReceiver;
impl MessageReceiver for PendingReceiver {
    fn receive(&mut self) -> ClientFuture<'_, Option<Vec<u8>>> {
        Box::pin(std::future::pending())
    }
}
impl RouteConnector for CleanupRoutes {
    fn open<'a>(
        &'a self,
        _: &'a ConnectionTarget,
        _: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        Box::pin(async {
            Ok(ConnectedRoute {
                sender: self.0.clone(),
                receiver: Box::new(PendingReceiver),
                peer: PeerFacts {
                    route: RouteKind::PrivateIpc,
                    local_account_verified: true,
                    pinned_server_key: None,
                },
            })
        })
    }
}

async fn suppressed_cleanup_remains_runtime_owned(cancel_removal: bool) {
    let binding = binding(1);
    let key = binding.target().local_pairing().unwrap();
    let gate = Arc::new(CleanupGate {
        closing: tokio::sync::Semaphore::new(0),
        finish: tokio::sync::Semaphore::new(0),
        complete: std::sync::atomic::AtomicBool::new(false),
    });
    let journal = Arc::new(MutableJournal(std::sync::Mutex::new(vec![binding.clone()])));
    let mut services = services(vec![], Arc::new(CleanupRoutes(gate.clone())));
    services.journal = journal.clone();
    let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    assert!(matches!(
        *server.state().borrow(),
        ConnectionState::Connecting {
            phase: spaceterm_client::api::ConnectingPhase::Authenticating
        }
    ));
    journal.0.lock().unwrap().clear();
    let removal = tokio::spawn(async move { registry.remove_suppressed(&key).await });
    gate.closing.acquire().await.unwrap().forget();
    assert!(!removal.is_finished());
    if cancel_removal {
        removal.abort();
        settle().await;
    }
    let shutdown = tokio::spawn(runtime.shutdown());
    settle().await;
    assert!(
        !shutdown.is_finished(),
        "shutdown lost the suppressed worker's cleanup"
    );
    gate.finish.add_permits(1);
    shutdown.await.unwrap().unwrap();
    assert!(gate.complete.load(Ordering::SeqCst));
    if cancel_removal {
        assert!(removal.await.unwrap_err().is_cancelled());
    } else {
        assert!(removal.await.unwrap().unwrap());
    }
}

#[tokio::test(start_paused = true)]
async fn shutdown_waits_for_concurrent_suppression_removal_cleanup() {
    suppressed_cleanup_remains_runtime_owned(false).await;
}

#[tokio::test(start_paused = true)]
async fn cancelled_suppression_removal_keeps_cleanup_owned_by_runtime() {
    suppressed_cleanup_remains_runtime_owned(true).await;
}

struct MutableJournal(std::sync::Mutex<Vec<PairedServerBinding>>);
impl ConnectionJournal for MutableJournal {
    fn recover_bindings(&self) -> ClientFuture<'_, Vec<PairedServerBinding>> {
        Box::pin(async { Ok(self.0.lock().unwrap().clone()) })
    }
    fn ordinary_use_allowed<'a>(
        &'a self,
        binding: &'a PairedServerBinding,
    ) -> ClientFuture<'a, bool> {
        Box::pin(async move { Ok(self.0.lock().unwrap().contains(binding)) })
    }
}

#[tokio::test(start_paused = true)]
async fn old_suppression_cannot_remove_a_newer_pairing_to_the_same_server() {
    use spaceterm_core::{
        identity::{ClientId, PairingId},
        pairing::{CredentialReference, LocalPairingGeneration},
    };
    let original = binding(1);
    let key = original.target().local_pairing().unwrap();
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let journal = Arc::new(MutableJournal(std::sync::Mutex::new(vec![
        original.clone(),
    ])));
    let mut services = services(vec![], Arc::new(Peers::one(&original, plan)));
    services.journal = journal.clone();
    let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
    let old = registry.server(original.target().server()).unwrap();
    old.connect().await.unwrap();
    settle().await;
    assert!(registry.remove_suppressed(&key).await.is_err());
    let target = original.target();
    let newer = PairedServerBinding::new(
        ConnectionTarget::new(
            target.pin(),
            target.device(),
            PairingId::new(target.server(), 2).unwrap(),
            ClientId::new(target.server(), 2).unwrap(),
            target.route().clone(),
            CredentialReference::new([8; 16]),
            LocalPairingGeneration::new(2).unwrap(),
        )
        .unwrap(),
    );
    *journal.0.lock().unwrap() = vec![newer.clone()];
    assert!(registry.remove_suppressed(&key).await.unwrap());
    assert!(old.retry().await.is_err());
    let current = registry.register(newer.clone()).await.unwrap();
    current.connect().await.unwrap();
    settle().await;
    assert!(!registry.remove_suppressed(&key).await.unwrap());
    assert!(
        matches!(*current.state().borrow(),ConnectionState::Connected { info } if info.client == newer.target().client())
    );
    assert_eq!(registry.servers().len(), 1);
    runtime.shutdown().await.unwrap();
}
