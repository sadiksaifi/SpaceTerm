mod support;
use spaceterm_client::{
    api::ConnectionState,
    failure::{ClientFailure, ClientFailureClass},
    ports::*,
    registry::ClientRegistry,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use support::*;

struct MissingRoute {
    opens: AtomicUsize,
}
impl RouteConnector for MissingRoute {
    fn open<'a>(
        &'a self,
        _target: &'a spaceterm_client::api::ConnectionTarget,
        _purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ClientFailure::new(ClientFailureClass::Unavailable)) })
    }
}

#[tokio::test(start_paused = true)]
async fn missing_endpoint_is_offline_not_stopped_and_retries_are_bounded() {
    let route = Arc::new(MissingRoute {
        opens: AtomicUsize::new(0),
    });
    let binding = binding(1);
    let (registry, runtime) =
        ClientRegistry::recover(services(vec![binding.clone()], route.clone()))
            .await
            .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    assert_eq!(route.opens.load(Ordering::SeqCst), 1);
    assert!(
        matches!(*server.state().borrow(), ConnectionState::Offline { failure, retry } if failure.class() == ClientFailureClass::Unavailable && retry.failures == 1 && retry.delay.unwrap() >= Duration::from_millis(250))
    );
    tokio::time::advance(Duration::from_millis(249)).await;
    settle().await;
    assert_eq!(route.opens.load(Ordering::SeqCst), 1);
    for _ in 0..10 {
        tokio::time::advance(Duration::from_secs(31)).await;
        settle().await;
    }
    assert!(
        matches!(*server.state().borrow(), ConnectionState::Offline { retry, .. } if retry.delay.unwrap() <= Duration::from_secs(30))
    );
    assert!(route.opens.load(Ordering::SeqCst) <= 12);
    runtime.shutdown().await.unwrap();
}

struct Stopped;
impl TrustedDiscovery for Stopped {
    fn status<'a>(
        &'a self,
        _target: &'a spaceterm_client::api::ConnectionTarget,
    ) -> ClientFuture<'a, DiscoveryState> {
        Box::pin(async { Ok(DiscoveryState::Stopped) })
    }
}
#[tokio::test(start_paused = true)]
async fn confirmed_stopped_never_dials_on_retry() {
    let route = Arc::new(MissingRoute {
        opens: AtomicUsize::new(0),
    });
    let binding = binding(1);
    let mut services = services(vec![binding.clone()], route.clone());
    services.discovery = Some(Arc::new(Stopped));
    let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    assert!(matches!(*server.state().borrow(), ConnectionState::Stopped));
    server.retry().await.unwrap();
    tokio::time::advance(Duration::from_secs(60)).await;
    settle().await;
    assert_eq!(route.opens.load(Ordering::SeqCst), 0);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn register_requires_the_exact_durably_verified_client_binding() {
    let route = Arc::new(MissingRoute {
        opens: AtomicUsize::new(0),
    });
    let (registry, runtime) = ClientRegistry::recover(services(vec![], route.clone()))
        .await
        .unwrap();
    let error = match registry.register(binding(1)).await {
        Ok(_) => panic!("unverified binding admitted"),
        Err(error) => error,
    };
    assert_eq!(error.class(), ClientFailureClass::AuthorizationDenied);
    assert!(registry.servers().is_empty());
    assert_eq!(route.opens.load(Ordering::SeqCst), 0);
    runtime.shutdown().await.unwrap();
}

struct PendingRoute {
    opens: AtomicUsize,
}
impl RouteConnector for PendingRoute {
    fn open<'a>(
        &'a self,
        _target: &'a spaceterm_client::api::ConnectionTarget,
        _purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            tokio::time::sleep(spaceterm_client::limits::DIAL_DEADLINE).await;
            Err(ClientFailure::new(ClientFailureClass::Unavailable))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn connection_actor_limit_counts_pending_native_dials() {
    let route = Arc::new(PendingRoute {
        opens: AtomicUsize::new(0),
    });
    let first = binding(1);
    let second = binding(2);
    let mut services = services(vec![first.clone(), second.clone()], route.clone());
    services.limits.connection_actors = 1;
    let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
    let a = registry.server(first.target().server()).unwrap();
    let b = registry.server(second.target().server()).unwrap();
    a.connect().await.unwrap();
    settle().await;
    b.connect().await.unwrap();
    settle().await;
    assert_eq!(route.opens.load(Ordering::SeqCst), 1);
    assert!(
        matches!(*b.state().borrow(),ConnectionState::Offline { failure,.. } if failure.class() == ClientFailureClass::ResourceLimit)
    );
    tokio::time::advance(Duration::from_secs(10)).await;
    settle().await;
    b.retry().await.unwrap();
    settle().await;
    assert_eq!(route.opens.load(Ordering::SeqCst), 2);
    runtime.shutdown().await.unwrap();
}

struct OpeningCleanup {
    finish: tokio::sync::Semaphore,
    complete: std::sync::atomic::AtomicBool,
    opens: AtomicUsize,
}
impl RouteConnector for OpeningCleanup {
    fn open<'a>(
        &'a self,
        _: &'a spaceterm_client::api::ConnectionTarget,
        _: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            tokio::time::sleep(spaceterm_client::limits::DIAL_DEADLINE).await;
            // The native deadline expires before cancellation cleanup finishes.
            self.finish.acquire().await.unwrap().forget();
            self.complete.store(true, Ordering::SeqCst);
            Err(ClientFailure::new(ClientFailureClass::Unavailable))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn timed_out_open_retains_its_slot_and_shutdown_waits_for_cleanup() {
    let routes = Arc::new(OpeningCleanup {
        finish: tokio::sync::Semaphore::new(0),
        complete: std::sync::atomic::AtomicBool::new(false),
        opens: AtomicUsize::new(0),
    });
    let first = binding(1);
    let second = binding(2);
    let mut services = services(vec![first.clone(), second.clone()], routes.clone());
    services.limits.connection_actors = 1;
    let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
    let a = registry.server(first.target().server()).unwrap();
    let b = registry.server(second.target().server()).unwrap();
    a.connect().await.unwrap();
    settle().await;
    tokio::time::advance(spaceterm_client::limits::DIAL_DEADLINE).await;
    settle().await;
    b.connect().await.unwrap();
    settle().await;
    assert_eq!(
        routes.opens.load(Ordering::SeqCst),
        1,
        "expired native open released its connection slot before cleanup"
    );
    let shutdown = tokio::spawn(runtime.shutdown());
    settle().await;
    assert!(
        !shutdown.is_finished(),
        "shutdown detached expired native opening cleanup"
    );
    routes.finish.add_permits(1);
    shutdown.await.unwrap().unwrap();
    assert!(routes.complete.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn shutdown_during_dialing_waits_for_failed_open_cleanup() {
    let routes = Arc::new(OpeningCleanup {
        finish: tokio::sync::Semaphore::new(0),
        complete: std::sync::atomic::AtomicBool::new(false),
        opens: AtomicUsize::new(0),
    });
    let binding = binding(1);
    let (registry, runtime) =
        ClientRegistry::recover(services(vec![binding.clone()], routes.clone()))
            .await
            .unwrap();
    registry
        .server(binding.target().server())
        .unwrap()
        .connect()
        .await
        .unwrap();
    settle().await;
    let shutdown = tokio::spawn(runtime.shutdown());
    settle().await;
    tokio::time::advance(spaceterm_client::limits::DIAL_DEADLINE).await;
    settle().await;
    assert!(
        !shutdown.is_finished(),
        "shutdown dropped the failed native open before cleanup"
    );
    routes.finish.add_permits(1);
    shutdown.await.unwrap().unwrap();
    assert!(routes.complete.load(Ordering::SeqCst));
}

struct DeniedCredentials;
impl CredentialSource for DeniedCredentials {
    fn signing_access<'a>(
        &'a self,
        _target: &'a spaceterm_client::api::ConnectionTarget,
        _purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, SigningAccess> {
        Box::pin(async { Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied)) })
    }
}
#[tokio::test(start_paused = true)]
async fn denied_credentials_pause_reconnect_without_an_enrollment_capability() {
    use support::peer::{PeerPlan, Peers};
    let binding = binding(1);
    let plan = Arc::new(PeerPlan::new(vec![2, 1]));
    let mut services = services(
        vec![binding.clone()],
        Arc::new(Peers::one(&binding, plan.clone())),
    );
    services.credentials = Arc::new(DeniedCredentials);
    let (registry, runtime) = ClientRegistry::recover(services).await.unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    assert!(
        matches!(*server.state().borrow(),ConnectionState::Offline { failure,retry } if failure.class() == ClientFailureClass::AuthorizationDenied && retry.delay.is_none())
    );
    tokio::time::advance(Duration::from_secs(60)).await;
    settle().await;
    assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
    assert_eq!(plan.queries.load(Ordering::SeqCst), 0);
    runtime.shutdown().await.unwrap();
}
