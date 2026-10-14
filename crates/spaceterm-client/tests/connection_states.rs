mod support;
use spaceterm_client::{
    api::ConnectionState,
    failure::{ClientFailure, ClientFailureClass},
    ports::*,
    registry::ClientRegistry,
};
use std::sync::{
    Arc, Mutex,
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

struct ForegroundOpeningCleanup {
    finish: tokio::sync::Semaphore,
    attempts: Mutex<Vec<ForegroundCancellation>>,
    completed: AtomicUsize,
}
impl Default for ForegroundOpeningCleanup {
    fn default() -> Self {
        Self {
            finish: tokio::sync::Semaphore::new(0),
            attempts: Mutex::new(Vec::new()),
            completed: AtomicUsize::new(0),
        }
    }
}
impl RouteConnector for ForegroundOpeningCleanup {
    fn open<'a>(
        &'a self,
        _: &'a spaceterm_client::api::ConnectionTarget,
        _: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        Box::pin(async { Err(ClientFailure::new(ClientFailureClass::Unavailable)) })
    }
    fn open_foreground<'a>(
        &'a self,
        _: &'a spaceterm_client::api::ConnectionTarget,
        cancellation: ForegroundCancellation,
    ) -> ClientFuture<'a, ConnectedRoute> {
        self.attempts.lock().unwrap().push(cancellation);
        Box::pin(async {
            // Native cleanup remains owned after foreground authority is revoked.
            self.finish.acquire().await.unwrap().forget();
            self.completed.fetch_add(1, Ordering::SeqCst);
            Err(ClientFailure::new(ClientFailureClass::Unavailable))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn shutdown_revokes_foreground_authority_before_dial_cleanup_finishes() {
    let routes = Arc::new(ForegroundOpeningCleanup::default());
    let binding = binding(1);
    let (registry, runtime) =
        ClientRegistry::recover(services(vec![binding.clone()], routes.clone()))
            .await
            .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.retry().await.unwrap();
    settle().await;
    let authority = routes.attempts.lock().unwrap()[0].clone();
    assert!(!authority.is_cancelled());

    let shutdown = tokio::spawn(runtime.shutdown());
    settle().await;
    assert!(
        authority.is_cancelled(),
        "shutdown retained prompt authority"
    );
    assert!(!shutdown.is_finished(), "shutdown detached native cleanup");
    assert_eq!(routes.completed.load(Ordering::SeqCst), 0);

    routes.finish.add_permits(1);
    shutdown.await.unwrap().unwrap();
    assert_eq!(routes.completed.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn replacement_retry_revokes_foreground_authority_before_dial_cleanup_finishes() {
    let routes = Arc::new(ForegroundOpeningCleanup::default());
    let binding = binding(1);
    let (registry, runtime) =
        ClientRegistry::recover(services(vec![binding.clone()], routes.clone()))
            .await
            .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.retry().await.unwrap();
    settle().await;
    let authority = routes.attempts.lock().unwrap()[0].clone();
    assert!(!authority.is_cancelled());

    server.retry().await.unwrap();
    settle().await;
    assert!(
        authority.is_cancelled(),
        "replacement Retry retained prompt authority"
    );
    assert_eq!(routes.completed.load(Ordering::SeqCst), 0);
    assert_eq!(
        routes.attempts.lock().unwrap().len(),
        1,
        "replacement dial started before old native cleanup finished"
    );

    routes.finish.add_permits(1);
    settle().await;
    assert_eq!(routes.completed.load(Ordering::SeqCst), 1);
    let replacement = {
        let attempts = routes.attempts.lock().unwrap();
        assert_eq!(attempts.len(), 2);
        attempts[1].clone()
    };
    assert!(!replacement.is_cancelled());
    assert!(
        authority.is_cancelled(),
        "replacement restored old prompt authority"
    );

    let shutdown = tokio::spawn(runtime.shutdown());
    settle().await;
    assert!(replacement.is_cancelled());
    assert!(!shutdown.is_finished());
    routes.finish.add_permits(1);
    shutdown.await.unwrap().unwrap();
    assert_eq!(routes.completed.load(Ordering::SeqCst), 2);
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

struct InteractiveRoute {
    background: AtomicUsize,
    foreground: AtomicUsize,
}
impl RouteConnector for InteractiveRoute {
    fn open<'a>(
        &'a self,
        _: &'a spaceterm_client::api::ConnectionTarget,
        _: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        self.background.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ClientFailure::new(ClientFailureClass::Unavailable)) })
    }
    fn open_foreground<'a>(
        &'a self,
        _: &'a spaceterm_client::api::ConnectionTarget,
        _: ForegroundCancellation,
    ) -> ClientFuture<'a, ConnectedRoute> {
        self.foreground.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ClientFailure::new(ClientFailureClass::Unavailable)) })
    }
}

#[tokio::test(start_paused = true)]
async fn foreground_retry_authorizes_only_one_dial_not_background_reconnect() {
    let routes = Arc::new(InteractiveRoute {
        background: AtomicUsize::new(0),
        foreground: AtomicUsize::new(0),
    });
    let binding = binding(1);
    let (registry, runtime) =
        ClientRegistry::recover(services(vec![binding.clone()], routes.clone()))
            .await
            .unwrap();
    let server = registry.server(binding.target().server()).unwrap();
    server.connect().await.unwrap();
    settle().await;
    assert_eq!(routes.background.load(Ordering::SeqCst), 1);
    assert_eq!(routes.foreground.load(Ordering::SeqCst), 0);
    server.retry().await.unwrap();
    settle().await;
    assert_eq!(routes.foreground.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(31)).await;
    settle().await;
    assert_eq!(routes.foreground.load(Ordering::SeqCst), 1);
    assert!(routes.background.load(Ordering::SeqCst) >= 2);
    runtime.shutdown().await.unwrap();
}
