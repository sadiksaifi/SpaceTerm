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
                local: None,
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
async fn desktop_add_server_publishes_enrolled_handle_into_runtime(cx: &mut TestAppContext) {
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
        #[cfg(all(feature = "desktop-test-support", debug_assertions))]
        fixture: false,
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
        settings: Default::default(),
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
    let binding = h.fixture.storage.journal.snapshot().unwrap().bindings()[0]
        .binding()
        .unwrap();
    let handle = enrollment.handle.as_ref().unwrap();
    let connection = crate::ui::settings_window::server_runtime::SettingsConnection::from_handle(
        binding.clone(),
        crate::ui::server_desktop_contracts::DesktopServerInfo {
            server: binding.target().server(),
            name: crate::ui::server_desktop_contracts::DisplayName::unnamed(),
            local: false,
        },
        handle,
        &spaceterm_client::operations::OperationTracker::new(h.fixture.storage.journal.clone()),
    )
    .unwrap();
    assert!(
        connection.inventory.is_some(),
        "new enrollment must retain authenticated device inventory"
    );
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

#[gpui::test]
async fn desktop_native_direct_invitation_uses_owned_credentials_and_canonical_journal(
    cx: &mut TestAppContext,
) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    cx.executor().allow_parking();
    let route = RouteDescription::new(spaceterm_client::api::RouteParts::Wss {
        address: "127.0.0.1:443".parse().unwrap(),
        server_name: "127.0.0.1".into(),
    })
    .unwrap();
    let fixture = Fixture::new_with_route(2, false, route);
    let delivery = fixture
        .authorization
        .borrow()
        .as_ref()
        .unwrap()
        .delivery_message(&fixture.endpoint.reservation.lock().unwrap())
        .unwrap();
    let invitation = Zeroizing::new(
        serde_json::json!({
            "schema": 1, "server_identity": fixture.prepared.pin.identity,
            "server_public_key": fixture.prepared.pin.public_key,
            "authorization_delivery": STANDARD.encode(delivery.as_slice()),
        })
        .to_string(),
    );
    let pairing = NativePairingRuntime::with_adapters(
        fixture.storage.journal.clone(),
        Arc::new(fixture.credentials.clone()),
        Arc::new(fixture.endpoint.clone()),
        Arc::new(fixture.endpoint.clone()),
        Arc::new(Invalidator),
        HandshakeConfig::default(),
    );
    let preparation = fixture
        .storage
        .runtime
        .block_on(direct::NativeDirectInvitation.prepare(
            "wss://127.0.0.1:443".into(),
            invitation,
            EnrollmentAttempt::new(1),
            &pairing,
        ))
        .unwrap();
    assert_eq!(fixture.endpoint.submissions.load(Ordering::SeqCst), 0);
    assert_eq!(preparation.prepared, fixture.prepared);
    let attempt = EnrollmentAttempt::new(1);
    let mut candidate = DirectCandidate::new(
        attempt,
        preparation.prepared,
        preparation.device,
        fixture.reservation.clone(),
        preparation.authorization,
        preparation.majors,
    )
    .unwrap();
    let verifier = candidate.verifier().unwrap();
    let approval = candidate
        .approve(&verifier, &verifier.fingerprint())
        .unwrap();
    let saved = fixture
        .storage
        .runtime
        .block_on(pairing.enroll(
            EnrollmentAttemptRequest {
                verified: &approval.verified,
                prepared: &approval.prepared,
                reservation: approval.reservation.clone(),
                device: &approval.device,
                authorization: approval.authorization.as_ref(),
                majors: approval.majors.clone(),
            },
            preparation.route,
        ))
        .unwrap();
    assert_eq!(saved, fixture.saved);
    assert_eq!(
        fixture.storage.journal.snapshot().unwrap().bindings().len(),
        1
    );
}

#[derive(Default)]
struct ClaimKeys(std::sync::Mutex<Vec<PreparedCredential>>);
impl spaceterm_client::pairing::credentials::NativeDeviceCredentials for ClaimKeys {
    fn create(
        &self,
        reservation: spaceterm_client::pairing::credentials::CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            use spaceterm_client::pairing::credentials::CredentialPurpose;
            use spaceterm_protocol::authentication::SigningKey;
            let scope = reservation.scope().clone();
            let prepared = PreparedCredential {
                public_key: spaceterm_client::testing::DeterministicKey::new(
                    if scope.parts().purpose == CredentialPurpose::DeviceIdentity {
                        3
                    } else {
                        2
                    },
                )
                .unwrap()
                .public_key(),
                scope,
            };
            self.0.lock().unwrap().push(prepared.clone());
            Ok(prepared)
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a spaceterm_client::pairing::credentials::CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|credential| &credential.scope == scope)
                .cloned())
        })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a spaceterm_client::pairing::credentials::CredentialUse,
    ) -> PairingFuture<
        'a,
        Result<Arc<dyn spaceterm_protocol::authentication::SigningKey>, PairingFailure>,
    > {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            Ok(Arc::new(
                spaceterm_client::testing::DeterministicKey::new(
                    if credential.scope.parts().purpose
                        == spaceterm_client::pairing::credentials::CredentialPurpose::DeviceIdentity
                    {
                        3
                    } else {
                        2
                    },
                )
                .unwrap(),
            )
                as Arc<dyn spaceterm_protocol::authentication::SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<
        'a,
        Result<spaceterm_client::pairing::credentials::CredentialDeletion, PairingFailure>,
    > {
        panic!("claim export cannot delete credentials")
    }
}
struct ClaimVerifier;
impl spaceterm_client::journal::ReceiptVerifier for ClaimVerifier {
    fn verify(
        &self,
        pin: spaceterm_protocol::authentication::ServerPin,
        receipt: &spaceterm_core::pairing::SignedPairingReceipt,
    ) -> Result<(), ClientFailure> {
        spaceterm_protocol::pairing_adapter::ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(Into::into)
    }
}

#[gpui::test]
async fn desktop_first_use_remote_claim_prepares_native_keys_and_exports_only_verified_public_proofs(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let h = Harness::new(2);
    let storage = Arc::new(spaceterm_client::testing::MemoryJournal::default());
    let journal = Arc::new(
        h.fixture
            .storage
            .runtime
            .block_on(spaceterm_client::journal::JournalController::recover(
                storage,
                Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec),
                Arc::new(ClaimVerifier),
                spaceterm_client::journal::JournalLimits::default(),
            ))
            .unwrap(),
    );
    let keys = Arc::new(ClaimKeys::default());
    let pairing = Arc::new(NativePairingRuntime::with_adapters(
        journal.clone(),
        keys.clone(),
        Arc::new(h.fixture.endpoint.clone()),
        Arc::new(h.fixture.endpoint.clone()),
        Arc::new(Invalidator),
        HandshakeConfig::default(),
    ));
    let provider = NativeEnrollmentProvider::new(
        NativeEnrollmentServices {
            pairing,
            direct: Arc::new(direct::NativeDirectInvitation),
            bootstrap: None,
            executor: h.fixture.storage.runtime.handle().clone(),
            local: None,
        },
        h.provider.registry.clone(),
        h.publication.clone(),
    );
    let descriptor = serde_json::json!({"schema": 1, "server_identity": h.fixture.prepared.pin.identity, "server_public_key": h.fixture.prepared.pin.public_key}).to_string();
    let wrong = cx
        .update(|cx| {
            provider.prepare_claim(
                "wss://127.0.0.1:443".into(),
                Zeroizing::new(descriptor.clone()),
                "wrong".into(),
                EnrollmentAttempt::new(1),
                cx,
            )
        })
        .await;
    assert!(matches!(
        wrong,
        Err(EnrollmentFailure::Pairing(
            PairingFailure::FingerprintMismatch
        ))
    ));
    assert!(keys.0.lock().unwrap().is_empty());
    let fingerprint =
        spaceterm_client::pairing::enrollment::fingerprint(h.fixture.prepared.pin.public_key);
    let claim = cx
        .update(|cx| {
            provider.prepare_claim(
                "wss://127.0.0.1:443".into(),
                Zeroizing::new(descriptor.clone()),
                fingerprint.clone(),
                EnrollmentAttempt::new(2),
                cx,
            )
        })
        .await
        .unwrap();
    let public: serde_json::Value = serde_json::from_str(&claim).unwrap();
    assert_eq!(public.as_object().unwrap().len(), 6);
    assert_eq!(public["schema"], 1);
    let proposal: spaceterm_core::pairing::EnrollmentProposal =
        serde_json::from_value(public["proposal"].clone()).unwrap();
    let device_proof: Vec<u8> = serde_json::from_value(public["device_proof"].clone()).unwrap();
    let authentication_proof: Vec<u8> =
        serde_json::from_value(public["authentication_proof"].clone()).unwrap();
    spaceterm_protocol::pairing_adapter::VerifiedDeviceClaim::verify(
        proposal.clone(),
        &device_proof,
        &authentication_proof,
    )
    .unwrap();
    assert_eq!(
        proposal.parts().server.bytes(),
        h.fixture.prepared.pin.identity
    );
    assert_eq!(keys.0.lock().unwrap().len(), 2);
    assert!(journal.snapshot().unwrap().bindings().is_empty());
    assert_eq!(h.fixture.endpoint.submissions.load(Ordering::SeqCst), 0);
    assert_eq!(h.ports.opens.load(Ordering::SeqCst), 0);
    let second = cx
        .update(|cx| {
            provider.prepare_claim(
                "wss://127.0.0.1:443".into(),
                Zeroizing::new(descriptor),
                fingerprint,
                EnrollmentAttempt::new(3),
                cx,
            )
        })
        .await
        .unwrap();
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    let second: spaceterm_core::pairing::EnrollmentProposal =
        serde_json::from_value(second["proposal"].clone()).unwrap();
    assert_eq!(proposal.parts().keys, second.parts().keys);
    assert_eq!(keys.0.lock().unwrap().len(), 2);
}

#[gpui::test]
async fn desktop_explicit_repair_publishes_new_incarnation_after_old_binding_suppression(
    cx: &mut TestAppContext,
) {
    use spaceterm_client::{
        journal::{ClientJournalChange, JournalCredentialSlot, SavedBinding},
        pairing::journal::LocalPairingStore,
    };
    use spaceterm_core::{
        authority::StoreRevision,
        identity::{ClientId, OperationStreamId, OperationStreamKey, PairingId},
        operations::{OperationBinding, OperationId, OperationSequence, RequestDigest},
        pairing::{
            ClientRecord, CredentialReference, LocalPairingGeneration, LocalPairingKeyParts,
            PairingClientStreams, PairingClientStreamsParts, PairingReceiptBody, PairingTarget,
            PairingTargetParts,
        },
    };
    cx.executor().allow_parking();
    let h = Harness::new(2);
    let server = h.fixture.prepared.local_pairing.parts().server;
    let pairing = PairingId::new(server, 2).unwrap();
    let client = ClientId::new(server, 2).unwrap();
    let streams = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: client,
            pairing,
        },
        ordinary: OperationStreamId::new(
            OperationStreamKey::new(server, 11).unwrap(),
            pairing,
            client,
        )
        .unwrap(),
        self_revocation: OperationStreamId::new(
            OperationStreamKey::new(server, 12).unwrap(),
            pairing,
            client,
        )
        .unwrap(),
    })
    .unwrap();
    let route = h
        .fixture
        .storage
        .journal
        .snapshot()
        .unwrap()
        .pairing()
        .slot(h.fixture.prepared.credential.scope.reference())
        .unwrap()
        .route
        .clone()
        .unwrap();
    let old_binding = PairedServerBinding::new(
        ConnectionTarget::new(
            h.fixture.prepared.pin,
            h.fixture.prepared.device,
            pairing,
            client,
            route.clone(),
            h.fixture.prepared.credential.scope.reference(),
            LocalPairingGeneration::new(1).unwrap(),
        )
        .unwrap(),
    );
    let proof = spaceterm_protocol::pairing_adapter::ReceiptCodec::new(h.fixture.prepared.pin)
        .sign(
            PairingReceiptBody::ClientBinding {
                target: PairingTarget::new(PairingTargetParts {
                    server,
                    pairing,
                    device: h.fixture.prepared.device,
                })
                .unwrap(),
                keys: h.fixture.prepared.keys.clone(),
                client: streams.clone(),
                operation: OperationBinding {
                    id: OperationId::new(
                        streams.parts().ordinary,
                        OperationSequence::new(1).unwrap(),
                    ),
                    digest: RequestDigest::from_bytes([13; 32]),
                },
                revision: StoreRevision::new(1),
            },
            h.fixture.endpoint.server.as_ref(),
        )
        .unwrap();
    let old_self_revocation = streams.parts().self_revocation;
    let old_saved =
        SavedBinding::verified(old_binding.clone(), streams, proof, &ClaimVerifier).unwrap();
    h.fixture
        .storage
        .runtime
        .block_on(
            h.fixture
                .storage
                .journal
                .transact(vec![ClientJournalChange::SaveBinding(Box::new(old_saved))]),
        )
        .unwrap();
    let old = h
        .fixture
        .storage
        .runtime
        .block_on(h.provider.registry.register(old_binding.clone()))
        .unwrap();
    let old_key = old_binding.target().local_pairing().unwrap();
    h.fixture.storage.runtime.block_on(async {
        let store = h.fixture.storage.store();
        let authority = store.ordinary_authority(&old_key).await.unwrap();
        spaceterm_client::pairing::revocation::PairingController::new(
            store,
            Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec),
        )
        .self_unpair(
            &authority,
            spaceterm_client::pairing::revocation::ConfirmedSelfUnpair::confirm(
                old_key.clone(),
                OperationId::new(old_self_revocation, OperationSequence::new(1).unwrap()),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    });
    let mut prepared = h.fixture.prepared.clone();
    let mut scope = prepared.credential.scope.parts().clone();
    scope.reference = CredentialReference::new([43; 16]);
    prepared.credential.scope =
        spaceterm_client::pairing::credentials::CredentialScope::new(scope).unwrap();
    prepared.local_pairing = LocalPairingKey::new(LocalPairingKeyParts {
        server,
        pairing: prepared.local_pairing.parts().pairing,
        generation: LocalPairingGeneration::new(3).unwrap(),
    })
    .unwrap();
    h.fixture
        .storage
        .runtime
        .block_on(h.fixture.storage.journal.transact(vec![
            ClientJournalChange::ReserveCredential(JournalCredentialSlot {
                namespace: prepared.credential.scope.parts().namespace.as_str().into(),
                server: Some(server),
                reference: prepared.credential.scope.reference(),
                public_key: None,
                route_reference: None,
                route: None,
                ssh_capture: None,
                generation: LocalPairingGeneration::new(3).unwrap(),
            }),
            ClientJournalChange::RecordCredentialPublicKey {
                reference: prepared.credential.scope.reference(),
                public_key: prepared.credential.public_key,
            },
            ClientJournalChange::BindCredentialRoute {
                reference: prepared.credential.scope.reference(),
                route_reference: prepared.route,
                route: route.clone(),
            },
        ]))
        .unwrap();
    let original = h
        .provider
        .services
        .direct
        .prepare(
            "wss://enrollment.invalid".into(),
            Zeroizing::new("one-use-test-delivery".into()),
            EnrollmentAttempt::new(1),
            &h.provider.services.pairing,
        )
        .await
        .unwrap();
    let mut services = h.provider.services.as_ref().clone();
    services.direct = Arc::new(Preparation(Mutex::new(Some(DirectPreparation {
        prepared: prepared.clone(),
        device: h.fixture.device.clone(),
        authorization: original.authorization,
        route,
        majors: vec![2],
    }))));
    let provider =
        NativeEnrollmentProvider::new(services, h.provider.registry.clone(), h.publication.clone());
    let discovery = cx
        .update(|cx| {
            provider.inspect(
                EnrollmentTarget::Direct {
                    address: "wss://enrollment.invalid".into(),
                    invitation: Zeroizing::new("one-use-test-delivery".into()),
                },
                EnrollmentAttempt::new(2),
                cx,
            )
        })
        .await
        .unwrap();
    let EnrollmentDiscovery::Direct(mut candidate) = discovery else {
        panic!("direct repair expected")
    };
    let verifier = candidate.verifier().unwrap();
    let approval = candidate
        .approve(&verifier, &verifier.fingerprint())
        .unwrap();
    let enrollment = cx
        .update(|cx| provider.enroll_direct(approval, cx))
        .await
        .unwrap();
    assert_eq!(enrollment.saved.key(), &prepared.local_pairing);
    assert_eq!(provider.registry.servers().len(), 1);
    assert!(h.fixture.storage.runtime.block_on(old.connect()).is_err());
    assert_eq!(h.ports.opens.load(Ordering::SeqCst), 0);
    cx.update(|cx| provider.publish(enrollment, cx)).unwrap();
    assert_eq!(
        h.publication.0.borrow().as_slice(),
        &[prepared.local_pairing]
    );
}

mod bootstrap_tests;
