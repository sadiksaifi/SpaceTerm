//! The selected-server facade retains the host's native assessment and request.
//! Account storage and lifecycle journals are real; only OS/runtime effects are injected.
#![cfg(any(target_os = "macos", target_os = "linux"))]

#[path = "../../spaceterm-host/tests/support/signed_installation.rs"]
mod signed_installation;

use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding, RouteDescription, RouteParts},
    management_cli::ManagementFailure,
};
use spaceterm_client_native::{
    NativeClientProvider,
    native::{
        NativeServiceManagement,
        host_services::{HostServiceAssessment, HostServiceManagement, LocalHostServices},
    },
    route::LocalRouteAuthority,
};
use spaceterm_core::{
    authority::{ServerEpoch, SessionGeneration, SessionKey},
    identity::{ClientId, DeviceId, PairingId, PaneId, ServerId},
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    pairing::{CredentialReference, LocalPairingGeneration},
    retirement::{CleanupResult, ConfirmedCleanupClass, SessionCleanupObservation},
};
use spaceterm_distribution::UpdateKey;
use spaceterm_host::{
    account_service::{
        InstallationStatus, LingerState, RegisteredServer, RuntimeAssessment, ServiceAction,
        ServiceClock, ServiceFailure, ServiceFuture, ServiceManager, ServiceOutcome,
        ServiceProcesses, ServiceRequestState, ServiceTime, SupervisorState,
        test_support::AccountServiceFixture,
    },
    server_installation::test_support,
    startup::{BuildIdentity, DesiredServerState, InstallationId},
};
use std::{
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

struct Clock;
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: [1; 32],
            millis: 100,
        })
    }
}

struct Manager {
    state: Mutex<SupervisorState>,
    effects: Mutex<Vec<&'static str>>,
    running: Arc<AtomicBool>,
}
impl ServiceManager for Manager {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async { Ok(*self.state.lock().unwrap()) })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async { Ok(cfg!(target_os = "linux").then_some(LingerState::Enabled)) })
    }
    fn configure<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.effects.lock().unwrap().push("configure");
            if registration.desired_state() == DesiredServerState::Stopped {
                *self.state.lock().unwrap() = SupervisorState::Disabled;
            }
            Ok(())
        })
    }
    fn start<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.lock().unwrap().push("start");
            self.running.store(true, Ordering::SeqCst);
            *self.state.lock().unwrap() = SupervisorState::Running;
            Ok(())
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.lock().unwrap().push("stop");
            self.running.store(false, Ordering::SeqCst);
            *self.state.lock().unwrap() = SupervisorState::Inactive;
            Ok(())
        })
    }
    fn uninstall<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.lock().unwrap().push("uninstall");
            *self.state.lock().unwrap() = SupervisorState::NotInstalled;
            Ok(())
        })
    }
    fn verify_installation(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        Ok(())
    }
}

struct Processes {
    running: Arc<AtomicBool>,
    assessments: AtomicU64,
    revalidations: AtomicUsize,
    cleanup: Mutex<
        Vec<(
            NativeManagementRequestRef,
            NativeRetirementAction,
            RuntimeAssessment,
        )>,
    >,
}
impl ServiceProcesses for Processes {
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            let server = ServerId::new(registration.server_pin().identity);
            let running = self.running.load(Ordering::SeqCst);
            let epoch = ServerEpoch::new(17);
            let mut token = [7; 32];
            token[..8].copy_from_slice(
                &self
                    .assessments
                    .fetch_add(1, Ordering::SeqCst)
                    .to_be_bytes(),
            );
            Ok(RuntimeAssessment {
                server,
                epoch: running.then_some(epoch),
                generation: 4,
                sessions: if running {
                    [1, 2]
                        .map(|pane| SessionKey {
                            pane: PaneId::new(server, pane).unwrap(),
                            epoch,
                            generation: SessionGeneration::new(3),
                        })
                        .to_vec()
                } else {
                    vec![]
                },
                token,
            })
        })
    }
    fn revalidate<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            assert_eq!(
                assessment.server.bytes(),
                registration.server_pin().identity
            );
            self.revalidations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn abort_reservation<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn quiesce<'a>(
        &'a self,
        _: &'a RegisteredServer,
        request: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            self.cleanup
                .lock()
                .unwrap()
                .push((request, action, assessment.clone()));
            self.running.store(false, Ordering::SeqCst);
            assessment
                .sessions
                .iter()
                .map(|session| {
                    SessionCleanupObservation::new(
                        *session,
                        action.reason(),
                        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
                    )
                    .map_err(|_| ServiceFailure::InvalidRecord)
                })
                .collect()
        })
    }
    fn verify_absent<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: ServerEpoch,
        _: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async { Err(ServiceFailure::CleanupUnresolved) })
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    account: AccountServiceFixture,
    manager: Arc<Manager>,
    processes: Arc<Processes>,
    binding: PairedServerBinding,
    trust: Option<UpdateKey>,
}
impl Fixture {
    fn new(desired: DesiredServerState) -> Self {
        Self::with_installation(
            desired,
            serde_json::from_value(serde_json::json!({
                "installation": 1, "version": "1.2.3", "artifact_sha256": vec![3; 32]
            }))
            .unwrap(),
        )
    }
    fn with_installation(desired: DesiredServerState, installation: InstallationStatus) -> Self {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registration = account
            .initialize_with_installation(desired, Some(installation))
            .unwrap();
        let server = ServerId::new(registration.server_pin().identity);
        let local = LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap();
        let binding = PairedServerBinding::new(
            ConnectionTarget::new(
                registration.server_pin(),
                DeviceId::new([4; 16]),
                PairingId::new(server, 1).unwrap(),
                ClientId::new(server, 1).unwrap(),
                local.description().clone(),
                CredentialReference::new([5; 16]),
                LocalPairingGeneration::new(1).unwrap(),
            )
            .unwrap(),
        );
        let running = Arc::new(AtomicBool::new(desired == DesiredServerState::Running));
        Self {
            _root: root,
            account,
            manager: Arc::new(Manager {
                state: Mutex::new(if desired == DesiredServerState::Running {
                    SupervisorState::Running
                } else {
                    SupervisorState::Disabled
                }),
                effects: Mutex::new(vec![]),
                running: running.clone(),
            }),
            processes: Arc::new(Processes {
                running,
                assessments: AtomicU64::new(1),
                revalidations: AtomicUsize::new(0),
                cleanup: Mutex::new(vec![]),
            }),
            binding,
            trust: None,
        }
    }
    fn management(&self) -> Arc<NativeServiceManagement> {
        let supervisor = self.account.with_providers(
            self.manager.clone(),
            self.processes.clone(),
            Arc::new(Clock),
            None,
        );
        let supervisor = match &self.trust {
            Some(key) => self
                .account
                .with_installation_trust(supervisor, key.clone()),
            None => supervisor,
        };
        NativeClientProvider::from_supervisor(supervisor).management()
    }
    fn services(&self) -> LocalHostServices {
        LocalHostServices::new(self.management(), self.binding.clone()).unwrap()
    }
}

#[tokio::test]
async fn local_facade_rejects_remote_routes_and_wrong_server_pin_without_native_effects() {
    let fixture = Fixture::new(DesiredServerState::Stopped);
    let target = fixture.binding.target();
    for route in [
        RouteDescription::new(RouteParts::Wss {
            address: "192.0.2.10:443".parse().unwrap(),
            server_name: "server.example.test".into(),
        })
        .unwrap(),
        RouteDescription::new(RouteParts::Ssh {
            destination: "guest".into(),
            remote_socket: "/guest/server".into(),
        })
        .unwrap(),
    ] {
        let binding = PairedServerBinding::new(
            ConnectionTarget::new(
                target.pin(),
                target.device(),
                target.pairing(),
                target.client(),
                route,
                target.credential(),
                target.incarnation(),
            )
            .unwrap(),
        );
        assert!(matches!(
            LocalHostServices::new(fixture.management(), binding),
            Err(ManagementFailure::AuthorizationDenied)
        ));
    }
    let mut wrong_pin = target.pin();
    wrong_pin.public_key = [99; 32];
    let binding = PairedServerBinding::new(
        ConnectionTarget::new(
            wrong_pin,
            target.device(),
            target.pairing(),
            target.client(),
            target.route().clone(),
            target.credential(),
            target.incarnation(),
        )
        .unwrap(),
    );
    assert!(matches!(
        LocalHostServices::new(fixture.management(), binding),
        Err(ManagementFailure::AuthorizationDenied)
    ));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert_eq!(fixture.processes.assessments.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn exact_assessment_survives_facade_recreation_and_submits_the_original_native_request() {
    let fixture = Fixture::new(DesiredServerState::Running);
    let services = fixture.services();
    let observed = services.inspect().await.unwrap();
    let registration = fixture.account.reopen().discover().unwrap().unwrap();
    assert_eq!(
        observed.status.server,
        Some(fixture.binding.target().server())
    );
    assert_eq!(observed.generation.get(), registration.generation());
    let request = services.reserve_request().await.unwrap();
    let original = services.assess(request, ServiceAction::Stop).await.unwrap();
    assert_eq!(original.prompt.request, request);
    assert_eq!(original.prompt.affected_sessions, 2);
    assert_eq!(original.runtime_epoch, Some(ServerEpoch::new(17)));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    drop(services);

    let reopened = fixture.services();
    let restored = reopened
        .restore_assessment(ServiceAction::Stop, original.prompt.token.clone())
        .await
        .unwrap();
    assert_eq!(restored.prompt.request, original.prompt.request);
    assert_eq!(restored.prompt.action, original.prompt.action);
    assert_eq!(
        restored.prompt.affected_sessions,
        original.prompt.affected_sessions
    );
    assert_eq!(
        restored.prompt.token.as_str(),
        original.prompt.token.as_str()
    );
    assert_eq!(restored.runtime_epoch, original.runtime_epoch);
    assert_eq!(fixture.processes.assessments.load(Ordering::SeqCst), 2);
    let completed = reopened.submit(restored).await.unwrap();
    assert_eq!(completed.request, request);
    assert_eq!(
        completed.state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    let cleanup = fixture.processes.cleanup.lock().unwrap();
    assert_eq!(cleanup.len(), 1);
    assert_eq!(cleanup[0].0.id_bytes(), *request.as_bytes());
    assert_eq!(cleanup[0].1, NativeRetirementAction::Stop);
    assert_eq!(cleanup[0].2.sessions.len(), 2);
    assert_eq!(
        fixture
            .manager
            .effects
            .lock()
            .unwrap()
            .iter()
            .filter(|effect| **effect == "stop")
            .count(),
        1
    );
}

#[tokio::test]
async fn changing_public_assessment_comparisons_cannot_submit_the_retained_native_action() {
    let fixture = Fixture::new(DesiredServerState::Running);
    let services = fixture.services();
    let request = services.reserve_request().await.unwrap();
    let original = services.assess(request, ServiceAction::Stop).await.unwrap();
    let different_request = services.reserve_request().await.unwrap();
    let mut changed_request = original.clone();
    changed_request.prompt.request = different_request;
    let mut changed_action = original.clone();
    changed_action.prompt.action = ServiceAction::Restart;
    let mut changed_count = original.clone();
    changed_count.prompt.affected_sessions += 1;
    let changed_epoch = HostServiceAssessment {
        prompt: original.prompt.clone(),
        runtime_epoch: Some(ServerEpoch::new(18)),
    };
    let missing_epoch = HostServiceAssessment {
        prompt: original.prompt.clone(),
        runtime_epoch: None,
    };
    for (comparison, tampered) in [
        ("request", changed_request),
        ("action", changed_action),
        ("affected sessions", changed_count),
        ("runtime epoch", changed_epoch),
        ("missing runtime epoch", missing_epoch),
    ] {
        assert_eq!(
            services.submit(tampered).await,
            Err(ManagementFailure::StaleAssessment),
            "tampered {comparison} was accepted"
        );
    }
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert!(fixture.processes.cleanup.lock().unwrap().is_empty());
    assert_eq!(fixture.processes.revalidations.load(Ordering::SeqCst), 0);
    // Rejected decoded comparisons do not consume the original native assessment.
    assert_eq!(
        services.submit(original).await.unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
}

#[tokio::test]
async fn lost_completed_start_reply_resolves_original_request_after_stop_without_reexecution() {
    let fixture = Fixture::new(DesiredServerState::Stopped);
    let services = fixture.services();
    let request = services.reserve_request().await.unwrap();
    // Discard the completed response, as a caller whose reply delivery was lost.
    drop(services.start(request).await.unwrap());
    drop(services);

    let reopened = fixture.services();
    let stop_request = reopened.reserve_request().await.unwrap();
    let stopping = reopened
        .assess(stop_request, ServiceAction::Stop)
        .await
        .unwrap();
    let stopped = reopened.submit(stopping).await.unwrap();
    assert_eq!(
        stopped.state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    let effects = fixture.manager.effects.lock().unwrap().clone();
    let cleanup_count = fixture.processes.cleanup.lock().unwrap().len();
    drop(reopened);

    let recovered = fixture.services();
    let resolved = recovered.resolve(request).await.unwrap();
    assert_eq!(resolved.request, request);
    assert_eq!(
        resolved.state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    assert_eq!(recovered.resolve(request).await.unwrap(), resolved);
    let retained = recovered.retained_requests().await.unwrap();
    assert!(retained.iter().any(|progress| progress == &resolved));
    assert!(retained.iter().any(|progress| progress == &stopped));
    assert_eq!(*fixture.manager.effects.lock().unwrap(), effects);
    assert_eq!(
        fixture.processes.cleanup.lock().unwrap().len(),
        cleanup_count
    );
    assert_eq!(
        recovered.inspect().await.unwrap().status.desired,
        Some(DesiredServerState::Stopped)
    );
}

#[tokio::test]
async fn stopped_local_server_cannot_be_restarted_by_assessment_or_read_only_facade_actions() {
    let fixture = Fixture::new(DesiredServerState::Stopped);
    let services = fixture.services();
    let request = services.reserve_request().await.unwrap();
    assert!(matches!(
        services.assess(request, ServiceAction::Restart).await,
        Err(ManagementFailure::StaleAssessment)
    ));
    assert_eq!(
        services.inspect().await.unwrap().status.desired,
        Some(DesiredServerState::Stopped)
    );
    services.retained_requests().await.unwrap();
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert_eq!(
        fixture
            .account
            .reopen()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
}

#[tokio::test]
async fn facade_staging_rejects_untrusted_signed_code_without_changing_stopped_or_approved_installation()
 {
    let (key, original) =
        signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
    let approved = InstallationStatus {
        installation: InstallationId(NonZeroU64::MIN),
        version: original.manifest().version.clone(),
        artifact_sha256: original.artifact_sha256(),
    };
    let mut fixture = Fixture::with_installation(DesiredServerState::Stopped, approved.clone());
    let registration = fixture.account.reopen().discover().unwrap().unwrap();
    let store = test_support::store(&registration, key.clone()).unwrap();
    let original_id = store.stage(&original).unwrap();
    store
        .activate(original_id, store.snapshot().unwrap().generation, 1)
        .unwrap();
    drop(store);
    fixture.trust = Some(key);
    let (_, candidate) =
        signed_installation::fixture("1.2.4", 7, signed_installation::native_machine());
    let services = fixture.services();
    let request = services.reserve_request().await.unwrap();
    // The artifact's real signature is valid under the fixture key. Native
    // staging must still require the compiled production trust root.
    assert_eq!(
        services.stage(request, candidate).await,
        Err(ManagementFailure::UnsafeNativeState)
    );
    drop(services);
    let reopened = fixture.services();
    let observed = reopened.inspect().await.unwrap();
    assert_eq!(observed.status.desired, Some(DesiredServerState::Stopped));
    assert_eq!(observed.status.approved, Some(approved));
    assert!(observed.status.staged.is_none());
    assert!(reopened.retained_requests().await.unwrap().is_empty());
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert!(fixture.processes.cleanup.lock().unwrap().is_empty());
}
