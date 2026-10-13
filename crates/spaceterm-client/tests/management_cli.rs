use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_client::management_cli::{
    status::{StatusResync, StatusUpdate},
    *,
};
use spaceterm_core::{
    authority::*, identity::*, management::*, observations::*, operations::*,
    organization::Versioned, remote_access::RemoteAccessPolicy,
};
use spaceterm_protocol::{ProtocolError, authentication::*, connection::AuthenticatedConnection};
use std::{
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

struct Key(Ed25519KeyPair);
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
struct Pairings(PairingRecord);
impl PairingLookup for Pairings {
    fn lookup(
        &self,
        s: [u8; 16],
        p: [u8; 16],
        c: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        if (s, p, c)
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
fn local(v: u64) -> [u8; 16] {
    let mut b = [0; 16];
    b[8..].copy_from_slice(&v.to_be_bytes());
    b
}
fn connection(major: u32, client: u64) -> AuthenticatedConnection {
    let server: Arc<dyn SigningKey> =
        Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[41; 32]).unwrap()));
    let device: Arc<dyn SigningKey> =
        Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap()));
    let principal = Principal {
        server: [1; 16],
        device: [2; 16],
        pairing: local(3),
        client: local(client),
    };
    let pairs = Pairings(PairingRecord {
        principal,
        device_public_key: device.public_key(),
        active: true,
    });
    let (client, hello) = ClientHandshake::begin(
        ServerPin {
            identity: principal.server,
            public_key: server.public_key(),
        },
        principal,
        Purpose::Ordinary,
        HandshakeConfig {
            majors: vec![major],
            ..Default::default()
        },
        device,
    )
    .unwrap();
    let (server, reply) = ServerHandshake::accept(
        principal.server,
        7,
        HandshakeConfig::default(),
        server,
        &pairs,
        &hello,
    )
    .unwrap();
    let (client, proof) = client.receive_server(&reply).unwrap();
    let (_, ready) = server.receive_device(&pairs, &proof).unwrap();
    let Established::Ordinary(client) = client.receive_ready(&ready).unwrap() else {
        panic!("ordinary")
    };
    client
}
fn stream() -> OperationStreamId {
    let s = ServerId::new([1; 16]);
    OperationStreamId::new(
        OperationStreamKey::new(s, 5).unwrap(),
        PairingId::new(s, 3).unwrap(),
        ClientId::new(s, 4).unwrap(),
    )
    .unwrap()
}
fn operation(n: u64) -> OperationId {
    OperationId::new(stream(), OperationSequence::new(n).unwrap())
}
fn assessment(epoch: u64) -> ManagementAssessment {
    let s = stream().server();
    let close = CloseAssessment::new(
        AssessmentRef {
            operation: operation(1),
            digest: RequestDigest::from_bytes([1; 32]),
        },
        ServerEpoch::new(epoch),
        AssessmentSummary::new(AssessmentTarget::EndAllSessions(s), 1, 1, 2, 2, true).unwrap(),
    )
    .unwrap();
    ManagementAssessment::new(
        AssessmentToken::new("token_1".to_owned()).unwrap(),
        ManagementAction::EndAllSessions { server: s },
        AssessmentEvidence::EndAllSessions(close),
        MonotonicTick::new(100),
        MonotonicTick::new(30_100),
    )
    .unwrap()
}
fn progress(n: u64) -> ManagementProgress {
    ManagementProgress {
        request: ManagementRequestReference::Operation(OperationBinding {
            id: operation(n),
            digest: RequestDigest::from_bytes([9; 32]),
        }),
        state: ManagementProgressState::Completed,
        completed_units: 2,
        total_units: 2,
    }
}
fn status(revision: u64, epoch: u64) -> ServerStatus {
    ServerStatus::new(ServerStatusParts {
        runtime: Default::default(),
        server: stream().server(),
        epoch: ServerEpoch::new(epoch),
        revision: NonZeroU64::new(revision).unwrap(),
        running: None,
        approved: None,
        staged: None,
        native_verification: NativeVerification::Unverified,
        capabilities: vec![],
        remote_access: Versioned {
            version: FieldVersion::new(1),
            value: RemoteAccessPolicy::disabled(stream().server()),
        },
        listeners: vec![],
        progress: vec![],
    })
    .unwrap()
}
#[derive(Default)]
struct Resync(AtomicUsize);
impl StatusResync for Resync {
    fn request_resync(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
fn status_query() -> StatusQuery {
    StatusQuery {
        connection: ConnectionBinding {
            client: stream().client(),
            generation: ConnectionGeneration::new(1),
        },
        request: NonZeroU64::new(1).unwrap(),
        generation: 1,
        known_epoch: None,
        known_revision: None,
        subscribe: true,
    }
}
fn subscription(resync: Arc<Resync>) -> StatusSubscription {
    StatusSubscription::new(stream().server(), status_query(), resync).unwrap()
}
fn snapshot(status: ServerStatus) -> StatusPublication {
    StatusPublication::Snapshot {
        request: NonZeroU64::new(1).unwrap(),
        generation: 1,
        status,
    }
}
fn changed(base: u64, revision: u64) -> StatusPublication {
    StatusPublication::Changed {
        request: NonZeroU64::new(1).unwrap(),
        generation: 1,
        base: NonZeroU64::new(base).unwrap(),
        status: status(revision, 7),
    }
}
#[test]
fn status_subscription_coalesces_and_resyncs_once_on_a_gap() {
    let resync = Arc::new(Resync::default());
    let sub = subscription(resync.clone());
    sub.apply(snapshot(status(1, 7))).unwrap();
    for rev in 2..=1000 {
        assert_eq!(
            sub.apply(changed(rev - 1, rev)).unwrap(),
            StatusUpdate::Replaced
        );
    }
    assert_eq!(sub.latest().unwrap().unwrap().parts().revision.get(), 1000);
    for _ in 0..10 {
        assert_eq!(
            sub.apply(changed(1001, 1002)).unwrap(),
            StatusUpdate::ResyncRequired
        );
    }
    assert_eq!(resync.0.load(Ordering::SeqCst), 1);
    sub.apply(snapshot(status(1002, 7))).unwrap();
    assert!(!sub.needs_resync().unwrap());
}
#[test]
fn delayed_resync_snapshot_must_reach_the_highest_observed_revision() {
    resync_snapshot_must_reach_observed_status(7, 12, 13, 11);
}
#[test]
fn delayed_resync_snapshot_must_reach_the_highest_observed_epoch() {
    resync_snapshot_must_reach_observed_status(8, 2, 3, 11);
}
fn resync_snapshot_must_reach_observed_status(
    epoch: u64,
    base: u64,
    revision: u64,
    delayed_revision: u64,
) {
    let resync = Arc::new(Resync::default());
    let sub = subscription(resync.clone());
    let usable = status(10, 7);
    sub.apply(snapshot(usable.clone())).unwrap();
    let gap = StatusPublication::Changed {
        request: status_query().request,
        generation: status_query().generation,
        base: NonZeroU64::new(base).unwrap(),
        status: status(revision, epoch),
    };
    for _ in 0..10 {
        assert_eq!(
            sub.apply(gap.clone()).unwrap(),
            StatusUpdate::ResyncRequired
        );
    }
    assert_eq!(resync.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        sub.apply(snapshot(status(delayed_revision, 7))).unwrap(),
        StatusUpdate::ResyncRequired
    );
    assert_eq!(sub.latest().unwrap(), Some(usable.clone()));
    assert!(sub.needs_resync().unwrap());
    assert_eq!(resync.0.load(Ordering::SeqCst), 2);

    // Publications during resync advance the watermark but do not queue queries.
    let next = StatusPublication::Changed {
        request: status_query().request,
        generation: status_query().generation,
        base: NonZeroU64::new(revision).unwrap(),
        status: status(revision + 1, epoch),
    };
    assert_eq!(sub.apply(next).unwrap(), StatusUpdate::ResyncRequired);
    assert_eq!(resync.0.load(Ordering::SeqCst), 2);
    assert_eq!(
        sub.apply(snapshot(status(revision, epoch))).unwrap(),
        StatusUpdate::ResyncRequired
    );
    assert_eq!(sub.latest().unwrap(), Some(usable));
    assert!(sub.needs_resync().unwrap());
    assert_eq!(resync.0.load(Ordering::SeqCst), 3);
    assert_eq!(
        sub.apply(snapshot(status(revision + 1, epoch))).unwrap(),
        StatusUpdate::Replaced
    );
    assert!(!sub.needs_resync().unwrap());
    assert_eq!(sub.latest().unwrap(), Some(status(revision + 1, epoch)));
    assert_eq!(resync.0.load(Ordering::SeqCst), 3);
}
#[test]
fn delayed_status_delta_does_not_replace_a_newer_snapshot_or_request_resync() {
    let resync = Arc::new(Resync::default());
    let sub = subscription(resync.clone());
    sub.apply(snapshot(status(10, 7))).unwrap();
    assert_eq!(sub.apply(changed(8, 9)).unwrap(), StatusUpdate::Ignored);
    assert_eq!(sub.latest().unwrap().unwrap().parts().revision.get(), 10);
    assert_eq!(resync.0.load(Ordering::SeqCst), 0);
}
#[test]
fn wrong_status_correlation_is_ignored_and_wrong_server_is_rejected() {
    let sub = subscription(Arc::new(Resync::default()));
    let mut publication = snapshot(status(1, 7));
    if let StatusPublication::Snapshot { generation, .. } = &mut publication {
        *generation = 2;
    }
    assert_eq!(sub.apply(publication).unwrap(), StatusUpdate::Ignored);
    assert!(sub.latest().unwrap().is_none());
    let mut parts = status(1, 7).parts().clone();
    parts.server = ServerId::new([3; 16]);
    parts.remote_access.value = RemoteAccessPolicy::disabled(parts.server);
    assert_eq!(
        sub.apply(snapshot(ServerStatus::new(parts).unwrap())),
        Err(ManagementFailure::UnsafeNativeState)
    );
}
struct Clock(AtomicU64);
impl RuntimeClock for Clock {
    fn now(&self) -> Result<MonotonicTick, ObservationError> {
        Ok(MonotonicTick::new(self.0.load(Ordering::SeqCst)))
    }
}
struct Backend {
    context: Mutex<Result<AuthenticatedManagementContext, ManagementFailure>>,
    assessment: ManagementAssessment,
    commands: Mutex<Vec<Command>>,
    resolved: Mutex<Vec<OperationId>>,
    progress: Mutex<ManagementProgress>,
    hang_status: bool,
    hang_submit: bool,
    hang_resolve: bool,
    subscription_query: StatusQuery,
    retained_subscription: Option<StatusSubscription>,
}
struct BackendPort(Arc<Backend>);
impl std::ops::Deref for BackendPort {
    type Target = Backend;
    fn deref(&self) -> &Backend {
        &self.0
    }
}
impl ManagementBackend for BackendPort {
    fn context(&self) -> Result<AuthenticatedManagementContext, ManagementFailure> {
        *self.context.lock().unwrap()
    }
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        Box::pin(async {
            if self.hang_status {
                std::future::pending::<()>().await;
            }
            Ok(status(1, 7))
        })
    }
    fn assess(&self, _: ManagementAction) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async { Ok(self.assessment.clone()) })
    }
    fn restore_assessment(
        &self,
        _: ManagementAction,
        _: AssessmentToken,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async { Ok(self.assessment.clone()) })
    }
    fn submit(&self, c: Command) -> ManagementFuture<'_, ManagementProgress> {
        self.commands.lock().unwrap().push(c);
        Box::pin(async {
            if self.hang_submit {
                std::future::pending::<()>().await;
            }
            Ok(*self.progress.lock().unwrap())
        })
    }
    fn resolve(&self, id: OperationId) -> ManagementFuture<'_, ManagementProgress> {
        self.resolved.lock().unwrap().push(id);
        Box::pin(async {
            if self.hang_resolve {
                std::future::pending::<()>().await;
            }
            Ok(*self.progress.lock().unwrap())
        })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(async {
            if let Some(subscription) = &self.0.retained_subscription {
                return Ok(subscription.clone());
            }
            StatusSubscription::new(
                self.0.subscription_query.connection.client.server(),
                self.0.subscription_query,
                Arc::new(Resync::default()),
            )
        })
    }
}
fn backend(kind: ManagementClientKind) -> Arc<Backend> {
    Arc::new(Backend {
        context: Mutex::new(Ok(AuthenticatedManagementContext::from_connection(
            &connection(2, 4),
            stream(),
            kind,
        )
        .unwrap()
        .with_connection_binding(status_query().connection)
        .unwrap())),
        assessment: assessment(7),
        commands: Mutex::new(vec![]),
        resolved: Mutex::new(vec![]),
        progress: Mutex::new(progress(2)),
        hang_status: false,
        hang_submit: false,
        hang_resolve: false,
        subscription_query: status_query(),
        retained_subscription: None,
    })
}
fn confirmed(a: ManagementAssessment) -> ConfirmedManagementAction {
    let r = ConfirmationReply::new(a.token().clone());
    confirm_exact(a, r).unwrap()
}
#[tokio::test]
async fn stale_epoch_or_expired_assessment_rejects_before_tracker_submission() {
    for (epoch, tick) in [(8, 101), (7, 30_100)] {
        let mut b = backend(ManagementClientKind::Cli);
        Arc::get_mut(&mut b).unwrap().assessment = assessment(epoch);
        let facade = TrackedProtocolManagement::new(
            BackendPort(b.clone()),
            Arc::new(Clock(AtomicU64::new(tick))),
        );
        assert_eq!(
            facade.execute(confirmed(assessment(epoch))).await,
            Err(ManagementFailure::StaleAssessment)
        );
        assert!(b.commands.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn end_all_uses_the_exact_assessed_command_and_resolves_original_identity() {
    let b = backend(ManagementClientKind::Cli);
    b.progress.lock().unwrap().state = ManagementProgressState::WaitingForCleanup;
    let facade = TrackedProtocolManagement::new(
        BackendPort(b.clone()),
        Arc::new(Clock(AtomicU64::new(101))),
    );
    let value = facade
        .assess(ManagementAction::EndAllSessions {
            server: stream().server(),
        })
        .await
        .unwrap();
    let result = facade.execute(confirmed(value.clone())).await.unwrap();
    assert_eq!(*b.commands.lock().unwrap(), [value.command()]);
    b.progress.lock().unwrap().state = ManagementProgressState::Completed;
    let ManagementRequestReference::Operation(binding) = result.request else {
        panic!("operation")
    };
    assert_eq!(
        facade.resolve(binding.id).await.unwrap().state,
        ManagementProgressState::Completed
    );
    assert_eq!(*b.resolved.lock().unwrap(), [binding.id]);
    assert_eq!(b.commands.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn companion_cannot_assess_execute_or_resolve_mutations() {
    let b = backend(ManagementClientKind::StatusCompanion);
    let facade = TrackedProtocolManagement::new(
        BackendPort(b.clone()),
        Arc::new(Clock(AtomicU64::new(101))),
    );
    assert!(facade.status().await.is_ok());
    assert_eq!(
        facade.assess(assessment(7).action().clone()).await,
        Err(ManagementFailure::AuthorizationDenied)
    );
    assert_eq!(
        facade.execute(confirmed(assessment(7))).await,
        Err(ManagementFailure::AuthorizationDenied)
    );
    assert_eq!(
        facade.resolve(operation(2)).await,
        Err(ManagementFailure::AuthorizationDenied)
    );
    assert!(b.commands.lock().unwrap().is_empty());
    assert!(b.resolved.lock().unwrap().is_empty());
}
#[tokio::test]
async fn offline_management_does_not_queue_a_command() {
    let b = backend(ManagementClientKind::Cli);
    *b.context.lock().unwrap() = Err(ManagementFailure::Offline);
    let facade = TrackedProtocolManagement::new(
        BackendPort(b.clone()),
        Arc::new(Clock(AtomicU64::new(101))),
    );
    assert_eq!(
        facade.execute(confirmed(assessment(7))).await,
        Err(ManagementFailure::Offline)
    );
    assert!(b.commands.lock().unwrap().is_empty());
}
#[tokio::test]
async fn wrong_retained_operation_cannot_be_reported_as_the_requested_outcome() {
    let b = backend(ManagementClientKind::Cli);
    let facade =
        TrackedProtocolManagement::new(BackendPort(b), Arc::new(Clock(AtomicU64::new(101))));
    assert_eq!(
        facade.resolve(operation(3)).await,
        Err(ManagementFailure::UnsafeNativeState)
    );
}
#[tokio::test(start_paused = true)]
async fn status_deadline_does_not_create_authority_or_mutation() {
    let mut b = backend(ManagementClientKind::Cli);
    Arc::get_mut(&mut b).unwrap().hang_status = true;
    let facade = TrackedProtocolManagement::new(
        BackendPort(b.clone()),
        Arc::new(Clock(AtomicU64::new(101))),
    );
    assert_eq!(facade.status().await, Err(ManagementFailure::Offline));
    assert!(b.commands.lock().unwrap().is_empty());
}
#[test]
fn management_context_requires_exact_authenticated_client_binding_in_both_majors() {
    for major in [1, 2] {
        let actual = connection(major, 4);
        assert_eq!(
            AuthenticatedManagementContext::from_connection(
                &actual,
                stream(),
                ManagementClientKind::Cli
            )
            .unwrap()
            .major(),
            major
        );
        let wrong = connection(major, 6);
        assert_eq!(
            AuthenticatedManagementContext::from_connection(
                &wrong,
                stream(),
                ManagementClientKind::Cli
            ),
            Err(ManagementFailure::UnsafeNativeState)
        );
    }
}
#[test]
fn management_context_rejects_invalid_canonical_connection_bindings_in_both_majors() {
    for major in [1, 2] {
        let context = AuthenticatedManagementContext::from_connection(
            &connection(major, 4),
            stream(),
            ManagementClientKind::Cli,
        )
        .unwrap();
        let valid = status_query().connection;
        assert_eq!(
            context
                .with_connection_binding(valid)
                .unwrap()
                .connection_binding(),
            Ok(valid)
        );
        for binding in [
            ConnectionBinding::new(stream().client(), ConnectionGeneration::new(0)),
            ConnectionBinding::new(
                ClientId::new(stream().server(), 6).unwrap(),
                ConnectionGeneration::new(1),
            ),
            ConnectionBinding::new(
                ClientId::new(ServerId::new([8; 16]), 4).unwrap(),
                ConnectionGeneration::new(1),
            ),
        ] {
            assert_eq!(
                context.with_connection_binding(binding),
                Err(ManagementFailure::UnsafeNativeState)
            );
        }
    }
}
#[test]
fn confirmation_tokens_cannot_be_substituted_or_extend_the_original_expiry() {
    let value = assessment(7);
    assert!(matches!(
        confirm_exact(
            value.clone(),
            ConfirmationReply::new(AssessmentToken::new("other".to_owned()).unwrap())
        ),
        Err(ManagementFailure::ConfirmationRequired)
    ));
    assert_eq!(
        value.validate_at(MonotonicTick::new(30_100)),
        Err(ManagementFailure::StaleAssessment)
    );
    assert_eq!(
        value.validate_at(MonotonicTick::new(99)),
        Err(ManagementFailure::StaleAssessment)
    );
}

#[tokio::test]
async fn confirmation_wait_does_not_reset_the_assessment_deadline() {
    let backend = backend(ManagementClientKind::Cli);
    let clock = Arc::new(Clock(AtomicU64::new(101)));
    let facade = TrackedProtocolManagement::new(BackendPort(backend.clone()), clock.clone());
    let value = facade.assess(assessment(7).action().clone()).await.unwrap();
    clock.0.store(30_100, Ordering::SeqCst);
    assert_eq!(
        facade.execute(confirmed(value)).await,
        Err(ManagementFailure::StaleAssessment)
    );
    assert!(backend.commands.lock().unwrap().is_empty());
}
#[tokio::test]
async fn one_active_mutation_does_not_admit_a_second_command() {
    let mut backend = backend(ManagementClientKind::Cli);
    Arc::get_mut(&mut backend).unwrap().hang_submit = true;
    let facade = TrackedProtocolManagement::new(
        BackendPort(backend.clone()),
        Arc::new(Clock(AtomicU64::new(101))),
    );
    let first = facade.execute(confirmed(assessment(7)));
    tokio::pin!(first);
    tokio::select! { _ = &mut first => panic!("submission should be pending"), _ = tokio::task::yield_now() => {} }
    assert_eq!(backend.commands.lock().unwrap().len(), 1);
    assert_eq!(
        facade.execute(confirmed(assessment(7))).await,
        Err(ManagementFailure::OutcomePending)
    );
    assert_eq!(backend.commands.lock().unwrap().len(), 1);
}

async fn stays_pending(subscription: &mut StatusSubscription) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(60), subscription.changed())
        .await
        .is_err()
}
#[tokio::test(start_paused = true)]
async fn status_subscription_wakes_a_waiter_once_per_coalesced_change() {
    let resync = Arc::new(Resync::default());
    let publisher = subscription(resync.clone());
    let mut waiter = publisher.clone();
    assert!(stays_pending(&mut waiter).await);
    publisher.apply(snapshot(status(1, 7))).unwrap();
    for rev in 2..=100 {
        publisher.apply(changed(rev - 1, rev)).unwrap();
    }
    waiter.changed().await.unwrap();
    assert_eq!(
        waiter.latest().unwrap().unwrap().parts().revision.get(),
        100
    );
    assert!(stays_pending(&mut waiter).await);
    assert_eq!(
        publisher.apply(changed(8, 9)).unwrap(),
        StatusUpdate::Ignored
    );
    assert!(stays_pending(&mut waiter).await);
    for _ in 0..3 {
        publisher.apply(changed(200, 201)).unwrap();
    }
    waiter.changed().await.unwrap();
    assert!(waiter.needs_resync().unwrap());
    assert!(stays_pending(&mut waiter).await);
    assert_eq!(resync.0.load(Ordering::SeqCst), 1);
}
struct StatusPort(Arc<Backend>);
impl StatusBackend for StatusPort {
    fn context(&self) -> Result<AuthenticatedManagementContext, ManagementFailure> {
        *self.0.context.lock().unwrap()
    }
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        BackendPort(self.0.clone()).status_owned()
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(async {
            if let Some(subscription) = &self.0.retained_subscription {
                return Ok(subscription.clone());
            }
            StatusSubscription::new(
                self.0.subscription_query.connection.client.server(),
                self.0.subscription_query,
                Arc::new(Resync::default()),
            )
        })
    }
}
impl BackendPort {
    fn status_owned(self) -> ManagementFuture<'static, ServerStatus> {
        Box::pin(async move {
            if self.hang_status {
                std::future::pending::<()>().await;
            }
            Ok(status(1, 7))
        })
    }
}
#[tokio::test]
async fn status_only_facade_serves_only_the_companion_client_identity() {
    let companion =
        StatusOnlyManagement::new(StatusPort(backend(ManagementClientKind::StatusCompanion)));
    let facade: &dyn StatusManagement = &companion;
    assert_eq!(facade.status().await.unwrap().parts().revision.get(), 1);
    assert_eq!(
        facade.subscribe_status().await.unwrap().server(),
        stream().server()
    );
    let cli = StatusOnlyManagement::new(StatusPort(backend(ManagementClientKind::Cli)));
    assert_eq!(
        cli.status().await,
        Err(ManagementFailure::AuthorizationDenied)
    );
    assert!(matches!(
        cli.subscribe_status().await,
        Err(ManagementFailure::AuthorizationDenied)
    ));
}
#[tokio::test(start_paused = true)]
async fn status_only_facade_bounds_reads_and_rejects_lost_context() {
    let mut hanging = backend(ManagementClientKind::StatusCompanion);
    Arc::get_mut(&mut hanging).unwrap().hang_status = true;
    let facade = StatusOnlyManagement::new(StatusPort(hanging));
    assert_eq!(facade.status().await, Err(ManagementFailure::Offline));
    let offline = backend(ManagementClientKind::StatusCompanion);
    *offline.context.lock().unwrap() = Err(ManagementFailure::Revoked);
    let facade = StatusOnlyManagement::new(StatusPort(offline));
    assert_eq!(facade.status().await, Err(ManagementFailure::Revoked));
    assert!(matches!(
        facade.subscribe_status().await,
        Err(ManagementFailure::Revoked)
    ));
}

#[tokio::test]
async fn status_subscription_cannot_borrow_another_server_or_client_binding() {
    for wrong_server in [false, true] {
        for kind in [
            ManagementClientKind::Cli,
            ManagementClientKind::StatusCompanion,
        ] {
            let mut b = backend(kind);
            let server = if wrong_server {
                ServerId::new([8; 16])
            } else {
                stream().server()
            };
            Arc::get_mut(&mut b)
                .unwrap()
                .subscription_query
                .connection
                .client = ClientId::new(server, 6).unwrap();
            let result = match kind {
                ManagementClientKind::Cli => {
                    TrackedProtocolManagement::new(
                        BackendPort(b),
                        Arc::new(Clock(AtomicU64::new(101))),
                    )
                    .subscribe_status()
                    .await
                }
                ManagementClientKind::StatusCompanion => {
                    StatusOnlyManagement::new(StatusPort(b))
                        .subscribe_status()
                        .await
                }
            };
            assert!(matches!(result, Err(ManagementFailure::UnsafeNativeState)));
        }
    }
}

async fn facade_subscription(
    kind: ManagementClientKind,
    backend: Arc<Backend>,
) -> Result<StatusSubscription, ManagementFailure> {
    match kind {
        ManagementClientKind::Cli => {
            TrackedProtocolManagement::new(
                BackendPort(backend),
                Arc::new(Clock(AtomicU64::new(101))),
            )
            .subscribe_status()
            .await
        }
        ManagementClientKind::StatusCompanion => {
            StatusOnlyManagement::new(StatusPort(backend))
                .subscribe_status()
                .await
        }
    }
}

#[tokio::test]
async fn tracked_management_rejects_a_retained_subscription_after_same_client_reconnect() {
    retained_subscription_is_rejected_after_reconnect(ManagementClientKind::Cli).await;
}
#[tokio::test]
async fn status_only_management_rejects_a_retained_subscription_after_same_client_reconnect() {
    retained_subscription_is_rejected_after_reconnect(ManagementClientKind::StatusCompanion).await;
}
async fn retained_subscription_is_rejected_after_reconnect(kind: ManagementClientKind) {
    for major in [1, 2] {
        let mut b = backend(kind);
        let retained = subscription(Arc::new(Resync::default()));
        retained.apply(snapshot(status(10, 7))).unwrap();
        Arc::get_mut(&mut b).unwrap().retained_subscription = Some(retained.clone());
        *b.context.lock().unwrap() = Ok(AuthenticatedManagementContext::from_connection(
            &connection(major, 4),
            stream(),
            kind,
        )
        .unwrap()
        .with_connection_binding(status_query().connection)
        .unwrap());
        let first = facade_subscription(kind, b.clone()).await.unwrap();
        assert_eq!(first.connection(), status_query().connection);
        assert_eq!(first.latest().unwrap(), Some(status(10, 7)));

        // The same stable Client authenticated again, but its backend retained
        // the subscription and cached value belonging to the retired connection.
        *b.context.lock().unwrap() = Ok(AuthenticatedManagementContext::from_connection(
            &connection(major, 4),
            stream(),
            kind,
        )
        .unwrap()
        .with_connection_binding(ConnectionBinding::new(
            stream().client(),
            ConnectionGeneration::new(2),
        ))
        .unwrap());
        retained.apply(changed(10, 11)).unwrap();
        assert!(matches!(
            facade_subscription(kind, b.clone()).await,
            Err(ManagementFailure::UnsafeNativeState)
        ));

        // Replacing only the retired subscription restores service to the same Client.
        let mut current_query = status_query();
        current_query.connection.generation = ConnectionGeneration::new(2);
        Arc::get_mut(&mut b).unwrap().retained_subscription = Some(
            StatusSubscription::new(
                stream().server(),
                current_query,
                Arc::new(Resync::default()),
            )
            .unwrap(),
        );
        assert_eq!(
            facade_subscription(kind, b).await.unwrap().connection(),
            current_query.connection
        );
    }
}

#[tokio::test]
async fn subscriptions_fail_closed_without_a_canonical_connection_binding() {
    for kind in [
        ManagementClientKind::Cli,
        ManagementClientKind::StatusCompanion,
    ] {
        let b = backend(kind);
        *b.context.lock().unwrap() =
            Ok(
                AuthenticatedManagementContext::from_connection(&connection(2, 4), stream(), kind)
                    .unwrap(),
            );
        assert!(matches!(
            facade_subscription(kind, b).await,
            Err(ManagementFailure::UnsafeNativeState)
        ));
    }
}

#[test]
fn status_subscription_rejects_invalid_query_generations_and_cache_bindings() {
    let mut queries = [status_query(); 4];
    queries[0].generation = 0;
    queries[1].connection.generation = ConnectionGeneration::new(0);
    queries[2].known_epoch = Some(ServerEpoch::new(0));
    queries[3].known_revision = NonZeroU64::new(5);
    for query in queries {
        assert!(matches!(
            StatusSubscription::new(stream().server(), query, Arc::new(Resync::default())),
            Err(ManagementFailure::UnsafeNativeState)
        ));
    }
}

#[test]
fn status_subscription_preserves_the_initial_known_epoch_and_revision_watermark() {
    let mut query = status_query();
    query.known_epoch = Some(ServerEpoch::new(7));
    query.known_revision = NonZeroU64::new(10);
    let sub =
        StatusSubscription::new(stream().server(), query, Arc::new(Resync::default())).unwrap();
    for stale in [status(100, 6), status(9, 7)] {
        assert_eq!(sub.apply(snapshot(stale)).unwrap(), StatusUpdate::Ignored);
        assert!(sub.latest().unwrap().is_none());
    }
    assert_eq!(
        sub.apply(snapshot(status(1, 8))).unwrap(),
        StatusUpdate::Replaced
    );
    assert_eq!(sub.latest().unwrap().unwrap().parts().epoch.get(), 8);
}

#[tokio::test(start_paused = true)]
async fn outcome_lookup_deadline_retains_the_original_identity_without_resubmission() {
    let mut b = backend(ManagementClientKind::Cli);
    Arc::get_mut(&mut b).unwrap().hang_resolve = true;
    let facade = TrackedProtocolManagement::new(
        BackendPort(b.clone()),
        Arc::new(Clock(AtomicU64::new(101))),
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        facade.resolve(operation(2)),
    )
    .await;
    assert_eq!(result, Ok(Err(ManagementFailure::OutcomePending)));
    assert_eq!(*b.resolved.lock().unwrap(), [operation(2)]);
    assert!(b.commands.lock().unwrap().is_empty());
}

fn unpair_assessment(device: DeviceId, pairing: PairingId) -> ManagementAssessment {
    use spaceterm_core::pairing::{
        ClientRecord, PairingTarget, PairingTargetParts, UnpairAssessment, UnpairAssessmentParts,
    };
    let target = PairingTarget::new(PairingTargetParts {
        server: stream().server(),
        device,
        pairing,
    })
    .unwrap();
    ManagementAssessment::new(
        AssessmentToken::new("unpair_1".to_owned()).unwrap(),
        ManagementAction::Unpair {
            target: target.clone(),
        },
        AssessmentEvidence::Unpair(
            UnpairAssessment::new(UnpairAssessmentParts {
                reference: AssessmentRef {
                    operation: operation(1),
                    digest: RequestDigest::from_bytes([1; 32]),
                },
                target,
                authorization_version: FieldVersion::new(1),
                clients: vec![ClientRecord {
                    id: stream().client(),
                    pairing,
                }],
            })
            .unwrap(),
        ),
        MonotonicTick::new(100),
        MonotonicTick::new(30_100),
    )
    .unwrap()
}
#[tokio::test]
async fn ordinary_management_cannot_bypass_the_marker_first_self_unpair_workflow() {
    let b = backend(ManagementClientKind::Cli);
    let value = unpair_assessment(DeviceId::new([2; 16]), stream().pairing());
    let facade = TrackedProtocolManagement::new(
        BackendPort(b.clone()),
        Arc::new(Clock(AtomicU64::new(101))),
    );
    assert_eq!(
        facade.execute(confirmed(value)).await,
        Err(ManagementFailure::AuthorizationDenied)
    );
    assert!(b.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn other_device_revocation_still_submits_the_exact_assessed_lifetime() {
    let b = backend(ManagementClientKind::Cli);
    let value = unpair_assessment(
        DeviceId::new([3; 16]),
        PairingId::new(stream().server(), 6).unwrap(),
    );
    let command = value.command();
    let facade = TrackedProtocolManagement::new(
        BackendPort(b.clone()),
        Arc::new(Clock(AtomicU64::new(101))),
    );
    assert_eq!(facade.execute(confirmed(value)).await.unwrap(), progress(2));
    assert_eq!(*b.commands.lock().unwrap(), [command]);
}
