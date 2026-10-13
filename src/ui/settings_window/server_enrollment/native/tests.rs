use super::*;
use crate::ui::settings_window::server_enrollment::tests::{Endpoint, Fixture};
use gpui::TestAppContext;
use spaceterm_client::{
    api::{ConnectedInfo, ConnectionTarget, PairedServerBinding},
    failure::{ClientFailure, ClientFailureClass},
    limits::ClientLimits,
    pairing::{journal::LocalPairingStore, revocation::RestrictedConnector},
    pending::LocalUseInvalidator,
    ports::*,
    replica::ReplicaSnapshot,
};
use spaceterm_core::{
    authority::GrantedControl,
    identity::WorkspaceId,
    pairing::{LocalPairingKey, PendingRevocationTask},
};
use spaceterm_protocol::{authentication::HandshakeConfig, codec::Payload};
use std::{
    cell::RefCell,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Preparation(Mutex<Option<DirectPreparation>>);
impl DirectInvitationPreparation for Preparation {
    fn prepare<'a>(
        &'a self,
        address: String,
        invitation: Zeroizing<String>,
        attempt: EnrollmentAttempt,
        _: &'a NativePairingRuntime,
    ) -> PairingFuture<'a, Result<DirectPreparation, EnrollmentFailure>> {
        Box::pin(async move {
            attempt.validate()?;
            assert_eq!(address, "wss://enrollment.invalid");
            assert_eq!(invitation.as_str(), "one-use-test-delivery");
            self.0
                .lock()
                .unwrap()
                .take()
                .ok_or(PairingFailure::InvalidRequest.into())
        })
    }
}
impl RestrictedConnector for Endpoint {
    fn open<'a>(
        &'a self,
        _: &'a PendingRevocationTask,
    ) -> PairingFuture<
        'a,
        Result<Box<dyn spaceterm_client::pairing::enrollment::PairingTransport>, PairingFailure>,
    > {
        Box::pin(async { panic!("foreground enrollment cannot use restricted revocation") })
    }
}
struct Invalidator;
impl LocalUseInvalidator for Invalidator {
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        panic!("enrollment cannot detach work")
    }
    fn invalidate_attachment(
        &self,
        _: &LocalPairingKey,
        _: spaceterm_client::view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        panic!("enrollment cannot detach work")
    }
    fn invalidate_pairing(&self, _: &LocalPairingKey) {
        panic!("enrollment cannot revoke")
    }
    fn remove_pairing<'a>(&'a self, _: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
        Box::pin(async { panic!("enrollment cannot remove") })
    }
}

#[derive(Default)]
struct OfflinePorts {
    opens: AtomicUsize,
}
fn unavailable() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::Unavailable)
}
impl RouteConnector for OfflinePorts {
    fn open<'a>(
        &'a self,
        _: &'a ConnectionTarget,
        _: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(unavailable()) })
    }
}
impl CredentialSource for OfflinePorts {
    fn signing_access<'a>(
        &'a self,
        _: &'a ConnectionTarget,
        _: ConnectionPurpose,
    ) -> ClientFuture<'a, SigningAccess> {
        Box::pin(async { Err(unavailable()) })
    }
}
impl OrganizationDecoder for OfflinePorts {
    fn decode(&self, _: Payload) -> Result<OrganizationPublication, ClientFailure> {
        panic!("offline route cannot publish")
    }
}
impl ConnectionDelegate for OfflinePorts {
    fn invalidate(&self, _: &PairedServerBinding, _: ConnectionInvalidation) {}
    fn reconcile<'a>(
        &'a self,
        _: ConnectedInfo,
        _: &'a ReplicaSnapshot,
        _: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async { panic!("offline route cannot reconcile") })
    }
    fn publication(&self, _: ConnectedInfo, _: &ReplicaSnapshot) -> Result<(), ClientFailure> {
        panic!("offline route cannot publish")
    }
    fn admit(&self, _: ConnectedInfo, _: &Payload) -> Result<OutboundClass, ClientFailure> {
        panic!("offline route cannot send")
    }
    fn receive<'a>(
        &'a self,
        _: ConnectedInfo,
        _: Payload,
        _: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async { panic!("offline route cannot receive") })
    }
}
#[derive(Default)]
struct Publication(RefCell<Vec<LocalPairingKey>>);
impl EnrollmentPublication for Publication {
    fn publish(
        &self,
        enrollment: &DurableEnrollment,
        handle: ServerHandle,
        _: &mut App,
    ) -> Result<(), EnrollmentFailure> {
        assert_eq!(handle.identity(), enrollment.saved.key().parts().server);
        self.0.borrow_mut().push(enrollment.saved.key().clone());
        Ok(())
    }
}

struct Harness {
    fixture: Fixture,
    provider: Rc<NativeEnrollmentProvider>,
    publication: Rc<Publication>,
    ports: Arc<OfflinePorts>,
    _runtime: spaceterm_client::registry::ClientRuntime,
}
impl Harness {
    fn new(major: u32) -> Self {
        let fixture = Fixture::new(major, false);
        let pairing = Arc::new(NativePairingRuntime::with_adapters(
            fixture.storage.journal.clone(),
            Arc::new(fixture.credentials.clone()),
            Arc::new(fixture.endpoint.clone()),
            Arc::new(fixture.endpoint.clone()),
            Arc::new(Invalidator),
            HandshakeConfig::default(),
        ));
        let ports = Arc::new(OfflinePorts::default());
        let (registry, runtime) = fixture
            .storage
            .runtime
            .block_on(ClientRegistry::recover(ClientServices {
                routes: ports.clone(),
                credentials: ports.clone(),
                journal: fixture.storage.journal.clone(),
                organization: ports.clone(),
                delegate: ports.clone(),
                discovery: None,
                limits: ClientLimits::default(),
                additional_capabilities: vec![],
            }))
            .unwrap();
        let preparation = DirectPreparation {
            prepared: fixture.prepared.clone(),
            device: fixture.device.clone(),
            authorization: fixture.authorization.borrow_mut().take(),
            majors: vec![major],
            route: fixture
                .storage
                .journal
                .snapshot()
                .unwrap()
                .pairing()
                .slot(fixture.prepared.credential.scope.reference())
                .unwrap()
                .route
                .clone()
                .unwrap(),
        };
        let publication = Rc::new(Publication::default());
        let provider = Rc::new(NativeEnrollmentProvider::new(
            NativeEnrollmentServices {
                pairing,
                direct: Arc::new(Preparation(Mutex::new(Some(preparation)))),
                bootstrap: None,
                executor: fixture.storage.runtime.handle().clone(),
            },
            registry,
            publication.clone(),
        ));
        Self {
            fixture,
            provider,
            publication,
            ports,
            _runtime: runtime,
        }
    }
    async fn candidate(
        &self,
        attempt: EnrollmentAttempt,
        cx: &mut TestAppContext,
    ) -> DirectCandidate {
        let task = cx.update(|cx| {
            self.provider.inspect(
                EnrollmentTarget::Direct {
                    address: "wss://enrollment.invalid".into(),
                    invitation: Zeroizing::new("one-use-test-delivery".into()),
                },
                attempt,
                cx,
            )
        });
        let EnrollmentDiscovery::Direct(candidate) = task.await.unwrap() else {
            panic!("wrong enrollment method")
        };
        *candidate
    }
}

pub(crate) async fn foreground_composition(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    for major in [1, 2] {
        let h = Harness::new(major);
        cx.update(|cx| super::super::install(h.provider.clone(), cx));
        let mut candidate = h.candidate(EnrollmentAttempt::new(major.into()), cx).await;
        let verifier = candidate.verifier().unwrap();
        assert!(matches!(
            candidate.approve(&verifier, "wrong"),
            Err(EnrollmentFailure::Pairing(
                PairingFailure::FingerprintMismatch
            ))
        ));
        assert_eq!(h.fixture.endpoint.submissions.load(Ordering::SeqCst), 0);
        let approval = candidate
            .approve(&verifier, &verifier.fingerprint())
            .unwrap();
        let enrollment = cx
            .update(|cx| h.provider.enroll_direct(approval, cx))
            .await
            .unwrap();
        assert_eq!(h.ports.opens.load(Ordering::SeqCst), 0);
        assert!(h.publication.0.borrow().is_empty());
        assert_eq!(h.fixture.endpoint.submissions.load(Ordering::SeqCst), 1);
        assert_eq!(h.provider.registry.servers().len(), 1);
        assert_eq!(
            h.fixture
                .storage
                .runtime
                .block_on(h.provider.services.pairing.inventory())
                .unwrap()
                .saved,
            vec![h.fixture.saved.clone()]
        );
        cx.update(|cx| h.provider.publish(enrollment, cx)).unwrap();
        assert_eq!(
            h.publication.0.borrow().as_slice(),
            &[h.fixture.saved.key().clone()]
        );
        h.fixture.storage.runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while h.ports.opens.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
    }
}

#[gpui::test]
async fn native_enrollment_canceled_before_approval_has_no_server_effects(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let h = Harness::new(2);
    let attempt = EnrollmentAttempt::new(1);
    let mut candidate = h.candidate(attempt.clone(), cx).await;
    let verifier = candidate.verifier().unwrap();
    attempt.cancel();
    assert!(matches!(
        candidate.approve(&verifier, &verifier.fingerprint()),
        Err(EnrollmentFailure::Pairing(PairingFailure::Cancelled))
    ));
    assert_eq!(h.fixture.endpoint.submissions.load(Ordering::SeqCst), 0);
    assert_eq!(h.ports.opens.load(Ordering::SeqCst), 0);
    assert!(h.provider.registry.servers().is_empty());
}

#[gpui::test]
async fn native_enrollment_synchronous_unpair_fence_blocks_publication(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let h = Harness::new(2);
    let mut candidate = h.candidate(EnrollmentAttempt::new(1), cx).await;
    let verifier = candidate.verifier().unwrap();
    let approval = candidate
        .approve(&verifier, &verifier.fingerprint())
        .unwrap();
    let enrollment = cx
        .update(|cx| h.provider.enroll_direct(approval, cx))
        .await
        .unwrap();
    h.fixture
        .storage
        .store()
        .seal_ordinary(enrollment.saved.key());
    assert!(cx.update(|cx| h.provider.publish(enrollment, cx)).is_err());
    assert!(h.publication.0.borrow().is_empty());
    assert_eq!(h.ports.opens.load(Ordering::SeqCst), 0);
}

struct EmptyView;
impl spaceterm_client::view::ViewStore for EmptyView {
    type Version = u64;
    fn load(
        &mut self,
        _: usize,
    ) -> Result<
        Option<spaceterm_client::view::StoredView<u64>>,
        spaceterm_client::view::ViewStoreFailure,
    > {
        Ok(None)
    }
    fn replace(
        &mut self,
        _: Option<&u64>,
        _: &spaceterm_client::view::EncodedView,
    ) -> spaceterm_client::view::ViewCommit<u64> {
        panic!("enrollment cannot overwrite the client's personal view")
    }
}

#[gpui::test]
async fn native_enrollment_publishes_into_existing_desktop_and_settings(cx: &mut TestAppContext) {
    use crate::platform::desktop_client_startup::{
        DesktopClientStartup, NativeDesktopOwners, NativeDesktopStartup,
    };
    use spaceterm_client::{
        projection::ProjectionBudgets,
        testing::{FakePaneConnection, FakeServer},
    };
    cx.executor().allow_parking();
    let h = Harness::new(2);
    let server = FakeServer::new(9, vec![2, 1]).unwrap();
    let peer = h
        .fixture
        .storage
        .runtime
        .block_on(FakePaneConnection::new(server.clone(), 1, 2))
        .unwrap();
    let channels = peer
        .channels(ProjectionBudgets::supported().unwrap())
        .unwrap();
    let owners = NativeDesktopOwners {
        services: ClientServices {
            routes: h.ports.clone(),
            credentials: h.ports.clone(),
            journal: h.fixture.storage.journal.clone(),
            organization: h.ports.clone(),
            delegate: h.ports.clone(),
            discovery: None,
            limits: ClientLimits::default(),
            additional_capabilities: vec![],
        },
        journal: h.fixture.storage.journal.clone(),
        channels,
        operations: server,
        invalidator: Arc::new(Invalidator),
        local_server: None,
        tracker: None,
        registry_binding: None,
    };
    let startup = NativeDesktopStartup::new(owners, EmptyView, h.fixture.storage.runtime.clone())
        .with_enrollment(h.provider.services.as_ref().clone());
    let recovered = cx.update(|cx| startup.recover(cx)).await.unwrap();
    let runtime = cx.update(|cx| recovered.install(cx)).unwrap();
    assert!(cx.update(|cx| runtime.read(cx).servers().is_empty()));
    let provider = cx.update(|cx| cx.global::<super::super::InstalledEnrollment>().0.clone());
    let inspected = cx
        .update(|cx| {
            provider.inspect(
                EnrollmentTarget::Direct {
                    address: "wss://enrollment.invalid".into(),
                    invitation: Zeroizing::new("one-use-test-delivery".into()),
                },
                EnrollmentAttempt::new(1),
                cx,
            )
        })
        .await
        .unwrap();
    let EnrollmentDiscovery::Direct(mut candidate) = inspected else {
        panic!("wrong method")
    };
    let verifier = candidate.verifier().unwrap();
    let approval = candidate
        .approve(&verifier, &verifier.fingerprint())
        .unwrap();
    let enrollment = cx
        .update(|cx| provider.enroll_direct(approval, cx))
        .await
        .unwrap();
    assert_eq!(h.ports.opens.load(Ordering::SeqCst), 0);
    cx.update(|cx| provider.publish(enrollment, cx)).unwrap();
    assert_eq!(
        cx.update(|cx| runtime.read(cx).servers()[0].server),
        h.fixture.saved.key().parts().server
    );
    cx.update(|cx| {
        let settings = &cx
            .global::<crate::ui::settings_window::servers::model::InstalledServers>()
            .0;
        assert_eq!(settings.read(cx).snapshot().servers.len(), 1);
    });
}

#[gpui::test]
async fn native_bootstrap_completion_checks_running_registration_before_publication(
    cx: &mut TestAppContext,
) {
    use spaceterm_client::pairing::bootstrap::{BootstrapPhase, BootstrapRoute, BootstrapServer};
    cx.executor().allow_parking();
    let h = Harness::new(2);
    let mut direct = h.candidate(EnrollmentAttempt::new(1), cx).await;
    let verifier = direct.verifier().unwrap();
    let approval = direct.approve(&verifier, &verifier.fingerprint()).unwrap();
    let _enrolled = cx
        .update(|cx| h.provider.enroll_direct(approval, cx))
        .await
        .unwrap();
    let route = BootstrapRoute::capture(
        [1; 32],
        [2; 32],
        [3; 32],
        501,
        spaceterm_distribution::BuildChannel::Development,
    )
    .unwrap();
    let facts = spaceterm_client::testing::bootstrap_host_facts(
        route,
        super::super::BootstrapRegistration::Running {
            pin: h.fixture.prepared.pin,
            generation: 7,
            artifact_identity: [4; 32],
            protocol_majors: [2, 1],
            version: spaceterm_client::pairing::bootstrap::HostVersion::new(1, 0, 0),
        },
    );
    let candidate = SshCandidate::new(EnrollmentAttempt::new(2), facts).unwrap();
    let mut outcome = BootstrapOutcome {
        server: BootstrapServer {
            route,
            pin: h.fixture.prepared.pin,
            registration_generation: 7,
            approved_artifact_identity: [4; 32],
            protocol_majors: [2, 1],
            running: false,
        },
        reused: true,
        phase: BootstrapPhase::Connected,
    };
    let completion = |outcome| BootstrapCompletion {
        prepared: h.fixture.prepared.clone(),
        saved: h.fixture.saved.clone(),
        outcome,
    };
    let result = h.fixture.storage.runtime.block_on(verify_bootstrap(
        &h.provider.services,
        &h.provider.registry,
        candidate.clone(),
        completion(outcome),
    ));
    assert!(matches!(
        result,
        Err(EnrollmentFailure::Pairing(
            PairingFailure::ExplicitStartRequired
        ))
    ));
    outcome.server.running = true;
    outcome.server.registration_generation = 8;
    let result = h.fixture.storage.runtime.block_on(verify_bootstrap(
        &h.provider.services,
        &h.provider.registry,
        candidate.clone(),
        completion(outcome),
    ));
    assert!(matches!(
        result,
        Err(EnrollmentFailure::Pairing(PairingFailure::IdentityMismatch))
    ));
    outcome.server.registration_generation = 7;
    h.fixture
        .storage
        .runtime
        .block_on(verify_bootstrap(
            &h.provider.services,
            &h.provider.registry,
            candidate,
            completion(outcome),
        ))
        .unwrap();
    assert!(h.publication.0.borrow().is_empty());
    assert_eq!(h.ports.opens.load(Ordering::SeqCst), 0);
}

#[gpui::test]
async fn native_bootstrap_rejects_another_confirmation_with_the_same_counter(
    cx: &mut TestAppContext,
) {
    use spaceterm_client::pairing::bootstrap::{BootstrapRoute, ForegroundBootstrap, HostVersion};
    cx.executor().allow_parking();
    let h = Harness::new(2);
    let route = BootstrapRoute::capture(
        [1; 32],
        [2; 32],
        [3; 32],
        501,
        spaceterm_distribution::BuildChannel::Development,
    )
    .unwrap();
    let facts = spaceterm_client::testing::bootstrap_host_facts(
        route,
        super::super::BootstrapRegistration::Running {
            pin: h.fixture.prepared.pin,
            generation: 7,
            artifact_identity: [4; 32],
            protocol_majors: [2, 1],
            version: HostVersion::new(1, 0, 0),
        },
    );
    let candidate = SshCandidate::new(EnrollmentAttempt::new(1), facts).unwrap();
    let original = candidate.confirm().unwrap();
    let unrelated = ForegroundBootstrap::confirm_observed(facts, 1).unwrap();
    let result = cx
        .update(|cx| h.provider.bootstrap(unrelated, candidate.clone(), cx))
        .await;
    assert!(matches!(
        result,
        Err(EnrollmentFailure::Pairing(PairingFailure::IdentityMismatch))
    ));
    assert!(matches!(
        candidate.confirm(),
        Err(EnrollmentFailure::Pairing(PairingFailure::RecoveryRequired))
    ));
    let result = cx
        .update(|cx| h.provider.bootstrap(original, candidate, cx))
        .await;
    assert!(matches!(
        result,
        Err(EnrollmentFailure::ProviderUnavailable)
    ));
    assert_eq!(h.fixture.endpoint.submissions.load(Ordering::SeqCst), 0);
    assert_eq!(h.ports.opens.load(Ordering::SeqCst), 0);
}
