#![cfg(all(
    feature = "test-support",
    any(target_os = "macos", target_os = "linux")
))]

#[path = "support/signed_installation.rs"]
mod signed_installation;

use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    identity::ServerId,
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    retirement::SessionCleanupObservation,
};
use spaceterm_host::{
    account_service::{
        AccountStartupAdmission, InstallationStatus, LingerState, RegisteredServer,
        RuntimeAssessment, ServiceAction, ServiceClock, ServiceFailure, ServiceFuture,
        ServiceManager, ServiceOutcome, ServicePhase, ServiceProcesses, ServiceRequestState,
        ServiceSupervisor, ServiceTime, SupervisorState,
        test_support::{self, AccountServiceFixture},
    },
    native_deployment::contracts::NativeRequestId,
    startup::{BuildIdentity, DesiredServerState},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::sync::Semaphore;

struct Clock(AtomicU64);
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: [3; 32],
            millis: self.0.load(Ordering::SeqCst),
        })
    }
}
struct Manager {
    state: Mutex<SupervisorState>,
    effects: Mutex<Vec<&'static str>>,
    stop_entered: Semaphore,
    stop_release: Semaphore,
    delay_stop: bool,
    linger: Mutex<Option<LingerState>>,
    rejected_installation: Mutex<Option<spaceterm_host::startup::InstallationId>>,
    start_failure: Mutex<Option<ServiceFailure>>,
    status_failure: Mutex<Option<ServiceFailure>>,
    starting: AtomicBool,
}
impl Manager {
    fn new(delay_stop: bool) -> Self {
        Self {
            state: Mutex::new(SupervisorState::Running),
            effects: Mutex::new(Vec::new()),
            stop_entered: Semaphore::new(0),
            stop_release: Semaphore::new(0),
            delay_stop,
            rejected_installation: Mutex::new(None),
            start_failure: Mutex::new(None),
            status_failure: Mutex::new(None),
            starting: AtomicBool::new(false),
            linger: Mutex::new(if cfg!(target_os = "linux") {
                Some(LingerState::Enabled)
            } else {
                None
            }),
        }
    }
}
impl ServiceManager for Manager {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async {
            match *self.status_failure.lock().unwrap() {
                Some(failure) => Err(failure),
                None => Ok(*self.state.lock().unwrap()),
            }
        })
    }
    fn scoped_processes<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, Vec<u32>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async { Ok(*self.linger.lock().unwrap()) })
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
            *self.state.lock().unwrap() = if self.starting.load(Ordering::SeqCst) {
                SupervisorState::Starting
            } else {
                SupervisorState::Running
            };
            match *self.start_failure.lock().unwrap() {
                Some(failure) => Err(failure),
                None => Ok(()),
            }
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.lock().unwrap().push("stop");
            self.stop_entered.add_permits(1);
            if self.delay_stop {
                self.stop_release.acquire().await.unwrap().forget();
            }
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
    fn verify_installation(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        if registration.approved_installation().is_some_and(|status| {
            Some(status.installation) == *self.rejected_installation.lock().unwrap()
        }) {
            Err(ServiceFailure::IncompatibleStorage)
        } else {
            Ok(())
        }
    }
}
struct Processes {
    generation: AtomicU64,
    assessments: AtomicU64,
    reserved: AtomicBool,
    advance_on_revalidate: Mutex<Option<Arc<Clock>>>,
    reservation: Mutex<Option<RuntimeAssessment>>,
    aborted: Mutex<std::collections::BTreeSet<[u8; 32]>>,
    sessions: Mutex<Vec<SessionKey>>,
    reject_abort: AtomicBool,
    fail_acceptance: AtomicBool,
    absence: bool,
    calls: Mutex<Vec<NativeManagementRequestRef>>,
}
impl Processes {
    fn new(absence: bool) -> Self {
        Self {
            generation: AtomicU64::new(1),
            assessments: AtomicU64::new(1),
            reserved: AtomicBool::new(false),
            advance_on_revalidate: Mutex::new(None),
            reservation: Mutex::new(None),
            aborted: Mutex::new(std::collections::BTreeSet::new()),
            sessions: Mutex::new(Vec::new()),
            reject_abort: AtomicBool::new(false),
            fail_acceptance: AtomicBool::new(false),
            absence,
            calls: Mutex::new(Vec::new()),
        }
    }
}
impl ServiceProcesses for Processes {
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            let mut token = [8; 32];
            token[..8].copy_from_slice(
                &self
                    .assessments
                    .fetch_add(1, Ordering::SeqCst)
                    .to_be_bytes(),
            );
            Ok(RuntimeAssessment {
                server: ServerId::new(registration.server_pin().identity),
                epoch: Some(ServerEpoch::new(1)),
                generation: self.generation.load(Ordering::SeqCst),
                sessions: self.sessions.lock().unwrap().clone(),
                token,
            })
        })
    }
    fn revalidate<'a>(
        &'a self,
        _: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if assessment.generation != self.generation.load(Ordering::SeqCst)
                || self.aborted.lock().unwrap().contains(&assessment.token)
            {
                Err(ServiceFailure::StaleAssessment)
            } else {
                *self.reservation.lock().unwrap() = Some(assessment.clone());
                self.reserved.store(true, Ordering::SeqCst);
                if self.fail_acceptance.swap(false, Ordering::SeqCst) {
                    test_support::fail_next_mutation_completion_sync();
                }
                if let Some(clock) = self.advance_on_revalidate.lock().unwrap().take() {
                    clock.0.store(30_100, Ordering::SeqCst);
                }
                Ok(())
            }
        })
    }
    fn abort_reservation<'a>(
        &'a self,
        _: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if self.reject_abort.load(Ordering::SeqCst) {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            let mut owner = self.reservation.lock().unwrap();
            if owner
                .as_ref()
                .is_some_and(|original| original != assessment)
            {
                return Err(ServiceFailure::StaleAssessment);
            }
            *owner = None;
            self.aborted.lock().unwrap().insert(assessment.token);
            self.reserved.store(false, Ordering::SeqCst);
            Ok(())
        })
    }
    fn quiesce<'a>(
        &'a self,
        _: &'a RegisteredServer,
        request: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(request);
            self.reserved.store(false, Ordering::SeqCst);
            *self.reservation.lock().unwrap() = None;
            assessment
                .sessions
                .iter()
                .map(|session| {
                    SessionCleanupObservation::new(
                        *session,
                        action.reason(),
                        spaceterm_core::retirement::CleanupResult::Confirmed(
                            spaceterm_core::retirement::ConfirmedCleanupClass::ReapedAndReleased,
                        ),
                    )
                    .map_err(|_| ServiceFailure::InvalidRecord)
                })
                .collect()
        })
    }
    fn verify_absent<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            if self.absence {
                Ok(())
            } else {
                Err(ServiceFailure::CleanupUnresolved)
            }
        })
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: ServerEpoch,
        sessions: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            if self.absence && sessions.is_empty() {
                Ok(Vec::new())
            } else {
                Err(ServiceFailure::CleanupUnresolved)
            }
        })
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    account: AccountServiceFixture,
    manager: Arc<Manager>,
    processes: Arc<Processes>,
    clock: Arc<Clock>,
}
impl Fixture {
    fn for_install() -> Self {
        use std::os::unix::fs::PermissionsExt;

        // First installation opens real organization storage, so a tmpfs tempdir is unsafe.
        let home = spaceterm_host::account_service::TrustedAccountFacts::current().unwrap();
        let root = tempfile::Builder::new()
            .prefix("spaceterm-lifecycle-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(home.home())
            .unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        Self {
            _root: root,
            account,
            manager: Arc::new(Manager::new(false)),
            processes: Arc::new(Processes::new(false)),
            clock: Arc::new(Clock(AtomicU64::new(100))),
        }
    }
    fn new(delay_stop: bool, absence: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        account
            .initialize_with_installation(DesiredServerState::Running, Some(installation(1)))
            .unwrap();
        let registration = account.reopen().discover().unwrap().unwrap();
        let (key, first) =
            signed_installation::fixture("1.0.0", 7, signed_installation::native_machine());
        let store =
            spaceterm_host::server_installation::test_support::store(&registration, key).unwrap();
        let id = store.stage(&first).unwrap();
        store
            .activate(id, store.snapshot().unwrap().generation, 1)
            .unwrap();
        let (_, second) =
            signed_installation::fixture("2.0.0", 7, signed_installation::native_machine());
        store.stage(&second).unwrap();
        drop(store);
        Self {
            _root: root,
            account,
            manager: Arc::new(Manager::new(delay_stop)),
            processes: Arc::new(Processes::new(absence)),
            clock: Arc::new(Clock(AtomicU64::new(100))),
        }
    }
    fn supervisor(&self) -> ServiceSupervisor {
        let (key, _) =
            signed_installation::fixture("1.0.0", 7, signed_installation::native_machine());
        self.account.with_installation_trust(
            self.account.with_providers(
                self.manager.clone(),
                self.processes.clone(),
                self.clock.clone(),
                None,
            ),
            key,
        )
    }
}

#[tokio::test]
async fn first_epoch_zero_cleanup_requires_native_absence_and_no_old_sessions() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let server = ServerId::new(
        supervisor
            .discover()
            .unwrap()
            .unwrap()
            .server_pin()
            .identity,
    );
    assert_eq!(
        supervisor
            .previous_instance_cleanup(server, ServerEpoch::new(0), &[])
            .await,
        Ok(Vec::new())
    );
    let unavailable = Fixture::new(false, false);
    let supervisor = unavailable.supervisor();
    let server = ServerId::new(
        supervisor
            .discover()
            .unwrap()
            .unwrap()
            .server_pin()
            .identity,
    );
    assert_eq!(
        supervisor
            .previous_instance_cleanup(server, ServerEpoch::new(0), &[])
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
}

#[tokio::test]
async fn stop_records_stopped_before_manager_work_and_survives_observer_loss() {
    let fixture = Fixture::new(true, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    let task = tokio::spawn(async move { supervisor.stop(action).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fixture.manager.stop_entered.acquire(),
    )
    .await
    .expect("manager Stop was not reached")
    .unwrap()
    .forget();
    let reopened = fixture.supervisor();
    assert_eq!(
        reopened.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
    task.abort();
    fixture.manager.stop_release.add_permits(1);
    for _ in 0..100 {
        let progress = reopened.resolve(request).await.unwrap();
        if progress.state == ServiceRequestState::Completed(ServiceOutcome::Stopped) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("lost observer cancelled accepted Stop");
}

#[tokio::test]
async fn historical_start_replay_cannot_undo_another_clients_stop() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let start = supervisor.allocate_request().unwrap();
    let original = supervisor.start(start).await.unwrap();
    assert_eq!(
        original.state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    let stop = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(stop, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    assert_eq!(
        supervisor.stop(action).await.unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    let effects = fixture.manager.effects.lock().unwrap().clone();
    assert_eq!(fixture.supervisor().start(start).await.unwrap(), original);
    assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
    assert_eq!(
        fixture
            .supervisor()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
}

#[tokio::test]
async fn restored_assessment_keeps_original_expiry_and_exact_runtime_generation() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let assessment = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap();
    let token = assessment.token();
    fixture.clock.0.store(29_000, Ordering::SeqCst);
    assert!(
        fixture
            .supervisor()
            .restore_assessment(request, token)
            .await
            .is_ok()
    );
    fixture.clock.0.store(30_100, Ordering::SeqCst);
    assert!(matches!(
        fixture
            .supervisor()
            .restore_assessment(request, token)
            .await,
        Err(ServiceFailure::StaleAssessment)
    ));
    assert!(matches!(
        assessment.confirm(),
        Err(ServiceFailure::StaleAssessment)
    ));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stale_runtime_scope_is_rejected_before_stop_intent_or_manager_effects() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    fixture.processes.generation.store(2, Ordering::SeqCst);
    assert_eq!(
        supervisor.stop(action).await,
        Err(ServiceFailure::StaleAssessment)
    );
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

fn installation(id: u64) -> InstallationStatus {
    InstallationStatus {
        installation: spaceterm_host::startup::InstallationId(
            std::num::NonZeroU64::new(id).unwrap(),
        ),
        version: spaceterm_distribution::BuildVersion::parse(&format!("{id}.0.0")).unwrap(),
        artifact_sha256: signed_installation::fixture(
            &format!("{id}.0.0"),
            7,
            signed_installation::native_machine(),
        )
        .1
        .artifact_sha256(),
    }
}

#[tokio::test]
async fn every_original_start_and_stop_phase_resolves_after_manager_interruption() {
    let start_phases = [
        ServicePhase::Accepted,
        ServicePhase::IntentCommitted,
        ServicePhase::Publishing,
        ServicePhase::Starting,
        ServicePhase::Completed,
    ];
    let stop_phases = [
        ServicePhase::Accepted,
        ServicePhase::IntentCommitted,
        ServicePhase::Quiescing,
        ServicePhase::Publishing,
        ServicePhase::Completed,
    ];
    for stop in [false, true] {
        for phase in if stop {
            stop_phases.as_slice()
        } else {
            start_phases.as_slice()
        } {
            let fixture = Fixture::new(false, true);
            let supervisor = fixture.supervisor();
            let request = supervisor.allocate_request().unwrap();
            let action = if stop {
                Some(
                    supervisor
                        .assess(request, ServiceAction::Stop)
                        .await
                        .unwrap()
                        .confirm()
                        .unwrap(),
                )
            } else {
                None
            };
            test_support::interrupt_after_phase(*phase);
            let interrupted = match action {
                Some(action) => supervisor.stop(action).await,
                None => supervisor.start(request).await,
            };
            assert_eq!(
                interrupted,
                Err(ServiceFailure::SupervisorUnavailable),
                "phase {phase:?}"
            );
            let resolved = fixture.supervisor().resolve(request).await.unwrap();
            assert_eq!(resolved.request, request);
            assert_eq!(
                resolved.state,
                ServiceRequestState::Completed(if stop {
                    ServiceOutcome::Stopped
                } else {
                    ServiceOutcome::Started
                }),
                "phase {phase:?}"
            );
            let effects = fixture.manager.effects.lock().unwrap().clone();
            assert_eq!(
                fixture.supervisor().resolve(request).await.unwrap(),
                resolved
            );
            assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
        }
    }
}

#[tokio::test]
async fn uncertain_acceptance_reconciles_exact_original_request_before_start_effects() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    test_support::fail_next_mutation_completion_sync();
    assert_eq!(
        supervisor.start(request).await,
        Err(ServiceFailure::Indeterminate)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert_eq!(
        fixture.supervisor().discover().unwrap_err(),
        ServiceFailure::Indeterminate
    );
    let resolved = fixture.supervisor().resolve(request).await.unwrap();
    assert_eq!(
        resolved.state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    assert_eq!(resolved.request, request);
}

#[tokio::test]
async fn uncertain_stopped_intent_never_calls_the_manager_and_resolves_original_stop() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    test_support::after_next_phase(ServicePhase::Accepted, || {
        test_support::fail_next_mutation_completion_sync();
        Ok(())
    });
    assert_eq!(
        supervisor.stop(action).await,
        Err(ServiceFailure::Indeterminate)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let resolved = fixture.supervisor().resolve(request).await.unwrap();
    assert_eq!(
        resolved.state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    assert!(matches!(
        fixture.supervisor().admit_startup().unwrap(),
        AccountStartupAdmission::Stopped
    ));
}

#[tokio::test]
async fn completed_acknowledgement_retires_service_id_without_new_effects() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    assert_eq!(
        supervisor.start(request).await.unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    supervisor.acknowledge_request(request).unwrap();
    let effects = fixture.manager.effects.lock().unwrap().clone();
    assert_eq!(
        fixture.supervisor().start(request).await,
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        fixture.supervisor().resolve(request).await,
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
}

#[tokio::test]
async fn empty_runtime_registry_without_native_absence_evidence_cannot_complete_stop() {
    let fixture = Fixture::new(false, false);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    assert_eq!(
        supervisor.stop(action).await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    assert_eq!(
        fixture.supervisor().status().await.unwrap().pending,
        Some(request)
    );
    assert_eq!(
        fixture.supervisor().resolve(request).await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    assert_eq!(
        supervisor
            .previous_instance_cleanup(
                ServerId::new(
                    supervisor
                        .discover()
                        .unwrap()
                        .unwrap()
                        .server_pin()
                        .identity
                ),
                ServerEpoch::new(1),
                &[]
            )
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
}

#[tokio::test]
async fn stopped_startup_never_opens_missing_keys_or_organization() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    supervisor.stop(action).await.unwrap();
    std::fs::remove_file(fixture.account.data_directory().join("identity")).unwrap();
    std::fs::remove_dir(fixture.account.data_directory().join("organization")).unwrap();
    assert!(matches!(
        fixture.account.reopen().admit_startup().unwrap(),
        AccountStartupAdmission::Stopped
    ));
    assert!(!fixture.account.data_directory().join("identity").exists());
    assert!(
        !fixture
            .account
            .data_directory()
            .join("organization")
            .exists()
    );
}

#[tokio::test]
async fn instance_lease_does_not_block_management_and_revalidates_a_later_stop() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let AccountStartupAdmission::Ready(startup) = supervisor.admit_startup().unwrap() else {
        panic!("startup not admitted");
    };
    assert_eq!(
        startup.identity().pin(),
        startup.registration().server_pin()
    );
    assert!(matches!(
        fixture.supervisor().admit_startup(),
        Err(ServiceFailure::Pending)
    ));
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    supervisor.stop(action).await.unwrap();
    assert_eq!(startup.revalidate(), Err(ServiceFailure::Pending));
    drop(startup);
    assert!(matches!(
        fixture.supervisor().admit_startup().unwrap(),
        AccountStartupAdmission::Stopped
    ));
}

#[tokio::test]
async fn native_retirement_handoff_requires_the_original_durable_stop_and_epoch() {
    let fixture = Fixture::new(true, true);
    let supervisor = fixture.supervisor();
    let server = ServerId::new(
        supervisor
            .discover()
            .unwrap()
            .unwrap()
            .server_pin()
            .identity,
    );
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    assert_eq!(
        supervisor.verify_retirement_request(server, ServerEpoch::new(1)),
        Err(ServiceFailure::UnknownRequest)
    );
    let observer = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.stop(action).await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fixture.manager.stop_entered.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    let (reference, action) = fixture
        .supervisor()
        .verify_retirement_request(server, ServerEpoch::new(1))
        .unwrap();
    assert_eq!(reference.id_bytes(), *request.as_bytes());
    assert_eq!(action, NativeRetirementAction::Stop);
    assert_eq!(
        fixture
            .supervisor()
            .verify_retirement_request(server, ServerEpoch::new(2)),
        Err(ServiceFailure::IdentityMismatch)
    );
    assert_eq!(
        fixture.processes.calls.lock().unwrap().as_slice(),
        &[reference]
    );
    fixture.manager.stop_release.add_permits(1);
    assert_eq!(
        observer.await.unwrap().unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    assert_eq!(
        fixture
            .supervisor()
            .verify_retirement_request(server, ServerEpoch::new(1)),
        Err(ServiceFailure::UnknownRequest)
    );
}

#[tokio::test]
async fn stopped_activation_selects_only_the_assessed_stage_and_never_starts() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let stop = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(stop, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    supervisor.stop(action).await.unwrap();
    fixture.account.stage_installation(installation(2)).unwrap();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(
            request,
            ServiceAction::Activate(installation(2).installation),
        )
        .await
        .unwrap()
        .confirm()
        .unwrap();
    let progress = supervisor.activate(action).await.unwrap();
    assert_eq!(
        progress.state,
        ServiceRequestState::Completed(ServiceOutcome::Activated(installation(2)))
    );
    let registration = fixture.supervisor().discover().unwrap().unwrap();
    assert_eq!(registration.desired_state(), DesiredServerState::Stopped);
    assert_eq!(registration.approved_installation(), Some(&installation(2)));
    assert!(registration.staged_installation().is_none());
    assert!(!fixture.manager.effects.lock().unwrap().contains(&"start"));
}

#[tokio::test]
async fn original_pending_publication_before_effect_reconstructs_without_substituting_admission() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    test_support::fail_next_publication_before_effect();
    assert_eq!(
        supervisor.start(request).await,
        Err(ServiceFailure::Indeterminate)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let resolved = fixture.supervisor().resolve(request).await.unwrap();
    assert_eq!(resolved.request, request);
    assert_eq!(
        resolved.state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    let effects = fixture.manager.effects.lock().unwrap().clone();
    assert_eq!(
        fixture.supervisor().resolve(request).await.unwrap(),
        resolved
    );
    assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
}

#[tokio::test]
async fn startup_honors_original_accepted_stop_before_keys_or_manager_effects() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    test_support::interrupt_after_phase(ServicePhase::Accepted);
    assert_eq!(
        supervisor.stop(action).await,
        Err(ServiceFailure::SupervisorUnavailable)
    );
    std::fs::remove_file(fixture.account.data_directory().join("identity")).unwrap();
    assert!(matches!(
        fixture.supervisor().admit_startup().unwrap(),
        AccountStartupAdmission::Stopped
    ));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert_eq!(
        fixture
            .supervisor()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
}

#[test]
fn service_mutation_without_tokio_returns_a_typed_failure_before_admission() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let mut future = supervisor.start(request);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert_eq!(
        future.as_mut().poll(&mut context),
        std::task::Poll::Ready(Err(ServiceFailure::SupervisorUnavailable))
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn replacement_of_original_publication_base_cannot_be_overwritten_by_resolution() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    test_support::fail_next_publication_before_effect();
    assert_eq!(
        supervisor.start(request).await,
        Err(ServiceFailure::Indeterminate)
    );
    let path = fixture.account.anchor().join("requests");
    let bytes = std::fs::read(&path).unwrap();
    let replacement = fixture.account.anchor().join("replacement");
    std::fs::write(&replacement, &bytes).unwrap();
    std::fs::set_permissions(
        &replacement,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(
        fixture.supervisor().resolve(request).await,
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn nonce_mismatched_original_proposal_cannot_issue_start() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    test_support::fail_next_publication_before_effect();
    assert_eq!(
        supervisor.start(request).await,
        Err(ServiceFailure::Indeterminate)
    );
    let path = fixture.account.anchor().join(".account-proposal");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[5] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        fixture.supervisor().resolve(request).await,
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn one_request_cannot_change_action_or_become_owner_enrollment() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap();
    assert_eq!(
        supervisor.start(request).await,
        Err(ServiceFailure::RequestConflict)
    );
    assert!(matches!(
        supervisor.assess(request, ServiceAction::Restart).await,
        Err(ServiceFailure::RequestConflict)
    ));
    let server = ServerId::new(
        supervisor
            .discover()
            .unwrap()
            .unwrap()
            .server_pin()
            .identity,
    );
    let claim = spaceterm_core::operations::RequestDigest::from_bytes([7; 32]);
    let digest = supervisor
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    assert_eq!(
        supervisor.record_owner_enrollment(request, digest, server, claim),
        Err(ServiceFailure::RequestConflict)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn restart_uses_approved_code_only_and_never_leaves_stopped() {
    let fixture = Fixture::new(false, true);
    fixture.account.stage_installation(installation(2)).unwrap();
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Restart)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    assert_eq!(
        supervisor.restart(action).await.unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Restarted)
    );
    let registration = fixture.supervisor().discover().unwrap().unwrap();
    assert_eq!(registration.approved_installation(), Some(&installation(1)));
    assert_eq!(registration.staged_installation(), Some(&installation(2)));
    let stop = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(stop, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    supervisor.stop(action).await.unwrap();
    let effects = fixture.manager.effects.lock().unwrap().clone();
    let invalid = supervisor.allocate_request().unwrap();
    assert!(matches!(
        supervisor.assess(invalid, ServiceAction::Restart).await,
        Err(ServiceFailure::StaleAssessment)
    ));
    assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
}

#[tokio::test]
async fn uninstall_retains_identity_organization_and_deliberate_state() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let registration = supervisor.discover().unwrap().unwrap();
    let pin = registration.server_pin();
    let identity = std::fs::read(fixture.account.data_directory().join("identity")).unwrap();
    let organization = registration.organization_directory().to_owned();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Uninstall)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    assert_eq!(
        supervisor.uninstall(action).await.unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Uninstalled)
    );
    let retained = fixture.supervisor().discover().unwrap().unwrap();
    assert_eq!(retained.server_pin(), pin);
    assert!(!retained.is_installed());
    assert!(retained.approved_installation().is_none());
    assert!(retained.staged_installation().is_none());
    assert_eq!(retained.desired_state(), DesiredServerState::Running);
    assert!(std::fs::read(fixture.account.data_directory().join("identity")).unwrap() == identity);
    assert!(organization.is_dir());
    let start = supervisor.allocate_request().unwrap();
    assert_eq!(supervisor.start(start).await, Err(ServiceFailure::Missing));
}

struct Authorization {
    manager: Arc<Manager>,
    calls: AtomicU64,
    failure: Option<ServiceFailure>,
    uid: u32,
}
impl spaceterm_host::account_service::LingerAuthorization for Authorization {
    fn enable_linger<'a>(
        &'a self,
        request: &'a spaceterm_host::account_service::LingerRequest,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            assert_eq!(request.account_uid(), self.uid);
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(failure) = self.failure {
                return Err(failure);
            }
            *self.manager.linger.lock().unwrap() = Some(LingerState::Enabled);
            Ok(())
        })
    }
}

#[tokio::test]
async fn enabled_linger_never_authorizes_and_denied_linger_replays_original_failure() {
    for failure in [
        None,
        Some(ServiceFailure::AuthorizationDenied),
        Some(ServiceFailure::AuthorizationCancelled),
    ] {
        let fixture = Fixture::new(false, true);
        let authorization = Arc::new(Authorization {
            manager: fixture.manager.clone(),
            calls: AtomicU64::new(0),
            failure,
            uid: fixture.supervisor().key().account_uid,
        });
        *fixture.manager.linger.lock().unwrap() = Some(if failure.is_some() {
            LingerState::Disabled
        } else {
            LingerState::Enabled
        });
        let supervisor = fixture.account.with_providers(
            fixture.manager.clone(),
            fixture.processes.clone(),
            fixture.clock.clone(),
            Some(authorization.clone()),
        );
        let request = supervisor.allocate_request().unwrap();
        let progress = supervisor.start(request).await.unwrap();
        let expected = match failure {
            None => ServiceRequestState::Completed(ServiceOutcome::Started),
            Some(failure) => ServiceRequestState::NotApplied(failure),
        };
        assert_eq!(progress.state, expected);
        assert_eq!(
            authorization.calls.load(Ordering::SeqCst),
            u64::from(failure.is_some())
        );
        assert_eq!(supervisor.start(request).await.unwrap(), progress);
        assert_eq!(
            authorization.calls.load(Ordering::SeqCst),
            u64::from(failure.is_some())
        );
        if failure.is_some() {
            assert!(fixture.manager.effects.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn disabled_linger_is_authorized_only_after_durable_original_admission() {
    let fixture = Fixture::new(false, true);
    *fixture.manager.linger.lock().unwrap() = Some(LingerState::Disabled);
    let authorization = Arc::new(Authorization {
        manager: fixture.manager.clone(),
        calls: AtomicU64::new(0),
        failure: None,
        uid: fixture.supervisor().key().account_uid,
    });
    let supervisor = fixture.account.with_providers(
        fixture.manager.clone(),
        fixture.processes.clone(),
        fixture.clock.clone(),
        Some(authorization.clone()),
    );
    let request = supervisor.allocate_request().unwrap();
    test_support::interrupt_after_phase(ServicePhase::Accepted);
    assert_eq!(
        supervisor.start(request).await,
        Err(ServiceFailure::SupervisorUnavailable)
    );
    assert_eq!(authorization.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        supervisor.resolve(request).await.unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    assert_eq!(authorization.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn admission_pressure_retires_only_unsubmitted_ids_not_completed_service_results() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let accepted = supervisor.allocate_request().unwrap();
    let original = supervisor.start(accepted).await.unwrap();
    let unsubmitted = supervisor.allocate_request().unwrap();
    for _ in 0..264 {
        supervisor.allocate_request().unwrap();
    }
    let effects = fixture.manager.effects.lock().unwrap().clone();
    assert_eq!(
        fixture.supervisor().resolve(accepted).await.unwrap(),
        original
    );
    assert_eq!(
        supervisor.start(unsubmitted).await,
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
}

#[tokio::test]
async fn unresolved_original_stop_blocks_new_action_admission_before_effects() {
    let fixture = Fixture::new(true, true);
    let supervisor = fixture.supervisor();
    let stop = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(stop, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    let observer = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.stop(action).await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fixture.manager.stop_entered.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    let next = fixture.supervisor().allocate_request().unwrap();
    assert_eq!(
        fixture.supervisor().start(next).await,
        Err(ServiceFailure::Pending)
    );
    assert!(matches!(
        fixture
            .supervisor()
            .assess(next, ServiceAction::Restart)
            .await,
        Err(ServiceFailure::Pending)
    ));
    assert!(!fixture.manager.effects.lock().unwrap().contains(&"start"));
    fixture.manager.stop_release.add_permits(1);
    assert_eq!(
        observer.await.unwrap().unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
}

#[tokio::test]
async fn damaged_staged_activation_is_rejected_before_retirement_or_manager_effects() {
    let fixture = Fixture::new(false, true);
    fixture.account.stage_installation(installation(2)).unwrap();
    let selected = spaceterm_host::server_installation::test_support::existing_store(
        &fixture.account.reopen().discover().unwrap().unwrap(),
        signed_installation::fixture("1.0.0", 7, signed_installation::native_machine()).0,
    )
    .unwrap();
    let executable = selected
        .retain_compatible_staged(installation(2).installation, 1)
        .unwrap();
    std::fs::write(executable.path(), b"damaged candidate").unwrap();
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(
            request,
            ServiceAction::Activate(installation(2).installation),
        )
        .await
        .unwrap()
        .confirm()
        .unwrap();
    assert_eq!(
        supervisor.activate(action).await,
        Err(ServiceFailure::ArtifactRejected)
    );
    assert!(fixture.processes.calls.lock().unwrap().is_empty());
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let registration = fixture.supervisor().discover().unwrap().unwrap();
    assert_eq!(registration.approved_installation(), Some(&installation(1)));
    assert_eq!(registration.desired_state(), DesiredServerState::Running);
}

#[tokio::test]
async fn runtime_admission_is_reserved_before_durable_stop_intent() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    let processes = fixture.processes.clone();
    test_support::after_next_phase(ServicePhase::Accepted, move || {
        assert!(
            processes.reserved.load(Ordering::SeqCst),
            "new runtime admission was not reserved before Stop intent"
        );
        Ok(())
    });
    assert_eq!(
        supervisor.stop(action).await.unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
}

#[tokio::test]
async fn expired_unsubmitted_assessments_release_capacity_without_reusing_the_original_id() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let assessment = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap();
    let token = assessment.token();
    fixture.clock.0.store(29_000, Ordering::SeqCst);
    supervisor.allocate_request().unwrap();
    assert!(supervisor.restore_assessment(request, token).await.is_ok());
    fixture.clock.0.store(30_100, Ordering::SeqCst);
    let next = supervisor.allocate_request().unwrap();
    assert_ne!(next, request);
    assert!(matches!(
        supervisor.restore_assessment(request, token).await,
        Err(ServiceFailure::RequestRetired)
    ));
    assert!(matches!(
        assessment.confirm(),
        Err(ServiceFailure::StaleAssessment)
    ));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn interrupted_activation_fences_old_startup_until_original_approval_is_reconciled() {
    let fixture = Fixture::new(false, true);
    fixture.account.stage_installation(installation(2)).unwrap();
    let supervisor = fixture.supervisor();
    let AccountStartupAdmission::Ready(startup) = supervisor.admit_startup().unwrap() else {
        panic!("startup not admitted");
    };
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(
            request,
            ServiceAction::Activate(installation(2).installation),
        )
        .await
        .unwrap()
        .confirm()
        .unwrap();
    test_support::interrupt_after_phase(ServicePhase::Publishing);
    assert_eq!(
        supervisor.activate(action).await,
        Err(ServiceFailure::SupervisorUnavailable)
    );
    assert_eq!(
        supervisor
            .discover()
            .unwrap()
            .unwrap()
            .approved_installation(),
        Some(&installation(1))
    );
    assert_eq!(startup.revalidate(), Err(ServiceFailure::Pending));
    drop(startup);
    assert!(matches!(
        fixture.supervisor().admit_startup(),
        Err(ServiceFailure::Pending)
    ));
    assert_eq!(
        fixture.supervisor().resolve(request).await.unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Activated(installation(2)))
    );
    let AccountStartupAdmission::Ready(startup) = fixture.supervisor().admit_startup().unwrap()
    else {
        panic!("reconciled startup not admitted");
    };
    assert_eq!(
        startup.registration().approved_installation(),
        Some(&installation(2))
    );
}

#[tokio::test]
async fn expiry_during_runtime_reservation_releases_only_proven_unaccepted_work() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    *fixture.processes.advance_on_revalidate.lock().unwrap() = Some(fixture.clock.clone());
    assert_eq!(
        supervisor.stop(action).await,
        Err(ServiceFailure::StaleAssessment)
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn uncertain_acceptance_keeps_runtime_reservation_until_original_resolution() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    fixture
        .processes
        .fail_acceptance
        .store(true, Ordering::SeqCst);
    assert_eq!(
        supervisor.stop(action).await,
        Err(ServiceFailure::Indeterminate)
    );
    assert!(fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert_eq!(
        fixture.supervisor().resolve(request).await.unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
}

#[tokio::test]
async fn unresolved_reservation_abort_survives_expiry_and_reconstruction() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let action = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    *fixture.processes.advance_on_revalidate.lock().unwrap() = Some(fixture.clock.clone());
    fixture.processes.reject_abort.store(true, Ordering::SeqCst);
    assert_eq!(
        supervisor.stop(action).await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    fixture.supervisor().allocate_request().unwrap();
    assert_eq!(
        fixture.supervisor().resolve(request).await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    assert!(fixture.processes.reserved.load(Ordering::SeqCst));
    fixture
        .processes
        .reject_abort
        .store(false, Ordering::SeqCst);
    assert_eq!(
        fixture.supervisor().resolve(request).await.unwrap().phase,
        ServicePhase::Reserved
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    fixture.supervisor().allocate_request().unwrap();
    assert_eq!(
        fixture.supervisor().resolve(request).await,
        Err(ServiceFailure::RequestRetired)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn interrupted_completed_acknowledgement_recovers_retirement_without_new_effects() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    supervisor.start(request).await.unwrap();
    let effects = fixture.manager.effects.lock().unwrap().clone();
    test_support::fail_next_mutation_completion_sync();
    assert_eq!(
        supervisor.acknowledge_request(request),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(
        fixture.supervisor().resolve(request).await,
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
}

#[tokio::test]
async fn near_ceiling_journal_reserves_nonempty_scope_and_original_outcome_before_effects() {
    for activate in [true, false] {
        let fixture = Fixture::new(false, true);
        fixture.account.stage_installation(installation(2)).unwrap();
        let supervisor = fixture.supervisor();
        let server = ServerId::new(
            supervisor
                .discover()
                .unwrap()
                .unwrap()
                .server_pin()
                .identity,
        );
        let sessions: Vec<_> = (1..=64)
            .map(|local| SessionKey {
                epoch: ServerEpoch::new(1),
                pane: spaceterm_core::identity::PaneId::new(server, local).unwrap(),
                generation: spaceterm_core::authority::SessionGeneration::new(1),
            })
            .collect();
        *fixture.processes.sessions.lock().unwrap() = sessions.clone();
        // Confirm the full original scope before filling the journal. Keep it
        // unsubmitted until allocation has displaced an expendable bare ID.
        let request = supervisor.allocate_request().unwrap();
        let assessment = supervisor
            .assess(
                request,
                if activate {
                    ServiceAction::Activate(installation(2).installation)
                } else {
                    ServiceAction::Stop
                },
            )
            .await
            .unwrap();
        assert_eq!(assessment.affected_sessions(), 64);
        assert_eq!(assessment.runtime_epoch(), Some(ServerEpoch::new(1)));
        let token = assessment.token();
        let action = assessment.confirm().unwrap();
        let reserved = supervisor.request_progress(request).unwrap();
        assert!(fixture.manager.effects.lock().unwrap().is_empty());
        assert!(fixture.processes.calls.lock().unwrap().is_empty());

        // Only accepted results retain capacity under arbitrary pressure. Real
        // completed Restarts preserve the original Running registration basis.
        // Leave conservative serialization headroom while constructing each filler,
        // so setup cannot retire the still-unsubmitted original assessment.
        let journal_path = fixture.account.anchor().join("requests");
        let mut completed = Vec::new();
        for scope_len in [64, 32, 16, 8, 4, 2, 1] {
            *fixture.processes.sessions.lock().unwrap() = sessions[..scope_len].to_vec();
            for _ in 0..32 {
                let bytes = std::fs::metadata(&journal_path).unwrap().len();
                let filler_headroom = 4096 + 512 * scope_len as u64;
                if bytes + filler_headroom > 256 * 1024 {
                    break;
                }
                let next = supervisor.allocate_request().unwrap();
                let filler = supervisor
                    .assess(next, ServiceAction::Restart)
                    .await
                    .unwrap()
                    .confirm()
                    .unwrap();
                let result = supervisor.restart(filler).await.unwrap();
                assert_eq!(
                    result.state,
                    ServiceRequestState::Completed(ServiceOutcome::Restarted)
                );
                completed.push(result);
                assert_eq!(supervisor.request_progress(request).unwrap(), reserved);
            }
        }
        assert!(!completed.is_empty());
        assert!(std::fs::metadata(&journal_path).unwrap().len() > 240 * 1024);
        assert_eq!(fixture.clock.0.load(Ordering::SeqCst), 100);
        let effects = fixture.manager.effects.lock().unwrap().clone();
        let calls = fixture.processes.calls.lock().unwrap().clone();

        // Bare IDs are evictable, so allocation must recycle one rather than
        // evicting either the original scope or an unacknowledged completed result.
        let unsubmitted = supervisor.allocate_request().unwrap();
        let mut retired = false;
        for _ in 0..256 {
            supervisor.allocate_request().unwrap();
            match supervisor.request_progress(unsubmitted) {
                Err(ServiceFailure::RequestRetired) => {
                    retired = true;
                    break;
                }
                Err(ServiceFailure::Pending) => {}
                result => panic!("unexpected unsubmitted progress {result:?}"),
            }
        }
        assert!(retired, "allocation pressure must retire an unsubmitted ID");
        let journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        assert!(
            journal["reservations"].as_array().unwrap().len() < 256,
            "retirement must be caused by bytes, not the slot ceiling"
        );
        assert!(std::fs::metadata(&journal_path).unwrap().len() <= 256 * 1024);
        let reservations_before_completion = journal["reservations"].as_array().unwrap().len();
        assert_eq!(supervisor.request_progress(request).unwrap(), reserved);
        let restored = fixture
            .supervisor()
            .restore_assessment(request, token)
            .await
            .unwrap();
        assert_eq!(restored.token(), token);
        assert_eq!(restored.runtime_epoch(), Some(ServerEpoch::new(1)));
        assert_eq!(restored.affected_sessions(), 64);
        assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
        assert_eq!(fixture.processes.calls.lock().unwrap().as_slice(), calls);
        assert_eq!(
            fixture.supervisor().start(unsubmitted).await,
            Err(ServiceFailure::RequestRetired)
        );
        assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
        assert_eq!(fixture.processes.calls.lock().unwrap().as_slice(), calls);

        *fixture.processes.sessions.lock().unwrap() = sessions;
        let original = if activate {
            supervisor.activate(action).await
        } else {
            supervisor.stop(action).await
        }
        .unwrap();
        assert_eq!(original.request, request);
        assert_eq!(original.request_digest, reserved.request_digest);
        assert_eq!(original.phase, ServicePhase::Completed);
        assert_eq!(
            original.state,
            ServiceRequestState::Completed(if activate {
                ServiceOutcome::Activated(installation(2))
            } else {
                ServiceOutcome::Stopped
            })
        );

        let journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        assert_eq!(
            journal["reservations"].as_array().unwrap().len(),
            reservations_before_completion,
            "completion must use its reserved budget without retiring another ID"
        );

        // Prove actual byte admission refusal, not merely a file-size threshold.
        // The original is completed now, so no quota probe can evict its outcome.
        let mut exhausted = false;
        for _ in 0..32 {
            let next = supervisor.allocate_request().unwrap();
            let before = std::fs::read(&journal_path).unwrap();
            let effects = fixture.manager.effects.lock().unwrap().clone();
            let calls = fixture.processes.calls.lock().unwrap().clone();
            let registration = supervisor.discover().unwrap().unwrap();
            match supervisor.assess(next, ServiceAction::Stop).await {
                Ok(assessment) => {
                    let result = supervisor
                        .stop(assessment.confirm().unwrap())
                        .await
                        .unwrap();
                    assert_eq!(
                        result.state,
                        ServiceRequestState::Completed(ServiceOutcome::Stopped)
                    );
                    completed.push(result);
                }
                Err(ServiceFailure::LimitExceeded) => {
                    exhausted = true;
                    let journal: serde_json::Value = serde_json::from_slice(&before).unwrap();
                    let reservations = journal["reservations"].as_array().unwrap().len();
                    assert!(reservations < 256);
                    assert!(before.len() > 240 * 1024 && before.len() <= 256 * 1024);
                    eprintln!(
                        "native_journal_quota_refusal: activate={activate} bytes={} reservations={reservations} original_sessions=64 clock_millis=100",
                        before.len()
                    );
                    assert!(
                        std::fs::read(&journal_path).unwrap() == before,
                        "quota refusal changed the native journal"
                    );
                    let after = supervisor.discover().unwrap().unwrap();
                    assert_eq!(after.generation(), registration.generation());
                    assert_eq!(after.desired_state(), registration.desired_state());
                    assert_eq!(
                        after.approved_installation(),
                        registration.approved_installation()
                    );
                    assert_eq!(
                        after.staged_installation(),
                        registration.staged_installation()
                    );
                    assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
                    assert_eq!(fixture.processes.calls.lock().unwrap().as_slice(), calls);
                    break;
                }
                Err(failure) => panic!("unexpected assessment failure {failure:?}"),
            }
        }
        assert!(exhausted, "admission pressure must reach the byte ceiling");
        assert_eq!(fixture.clock.0.load(Ordering::SeqCst), 100);
        let effects = fixture.manager.effects.lock().unwrap().clone();
        let calls = fixture.processes.calls.lock().unwrap().clone();
        assert_eq!(
            fixture.supervisor().resolve(request).await.unwrap(),
            original
        );
        let replay = fixture
            .supervisor()
            .restore_assessment(request, token)
            .await
            .unwrap()
            .confirm()
            .unwrap();
        assert_eq!(
            if activate {
                fixture.supervisor().activate(replay).await
            } else {
                fixture.supervisor().stop(replay).await
            }
            .unwrap(),
            original
        );
        for result in completed {
            assert_eq!(
                fixture.supervisor().resolve(result.request).await.unwrap(),
                result
            );
        }
        assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
        assert_eq!(fixture.processes.calls.lock().unwrap().as_slice(), calls);
    }
}

#[tokio::test]
async fn byte_pressure_retires_unsubmitted_nonempty_scope_without_reexecuting_its_identity() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let server = ServerId::new(
        supervisor
            .discover()
            .unwrap()
            .unwrap()
            .server_pin()
            .identity,
    );
    *fixture.processes.sessions.lock().unwrap() = (1..=64)
        .map(|local| SessionKey {
            epoch: ServerEpoch::new(1),
            pane: spaceterm_core::identity::PaneId::new(server, local).unwrap(),
            generation: spaceterm_core::authority::SessionGeneration::new(1),
        })
        .collect();
    let request = supervisor.allocate_request().unwrap();
    let assessment = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap();
    assert_eq!(assessment.affected_sessions(), 64);
    let token = assessment.token();
    let action = assessment.confirm().unwrap();
    let mut retired = false;
    for _ in 0..32 {
        let next = supervisor.allocate_request().unwrap();
        supervisor.assess(next, ServiceAction::Stop).await.unwrap();
        if supervisor.request_progress(request) == Err(ServiceFailure::RequestRetired) {
            retired = true;
            break;
        }
    }
    assert!(retired, "byte pressure must retire an unsubmitted scope");
    let journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.account.anchor().join("requests")).unwrap())
            .unwrap();
    assert!(journal["reservations"].as_array().unwrap().len() < 256);
    assert_eq!(fixture.clock.0.load(Ordering::SeqCst), 100);
    assert_eq!(
        fixture.supervisor().stop(action).await,
        Err(ServiceFailure::RequestRetired)
    );
    assert!(matches!(
        fixture
            .supervisor()
            .restore_assessment(request, token)
            .await,
        Err(ServiceFailure::RequestRetired)
    ));
    assert_eq!(
        fixture.supervisor().resolve(request).await,
        Err(ServiceFailure::RequestRetired)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert!(fixture.processes.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn original_starting_phase_authorizes_only_its_reconciled_installation() {
    for activate in [false, true] {
        let fixture = Fixture::new(false, true);
        fixture.account.stage_installation(installation(2)).unwrap();
        let supervisor = fixture.supervisor();
        let request = supervisor.allocate_request().unwrap();
        let assessment = supervisor
            .assess(
                request,
                if activate {
                    ServiceAction::Activate(installation(2).installation)
                } else {
                    ServiceAction::Restart
                },
            )
            .await
            .unwrap()
            .confirm()
            .unwrap();
        test_support::interrupt_after_phase(ServicePhase::Starting);
        let result = if activate {
            supervisor.activate(assessment).await
        } else {
            supervisor.restart(assessment).await
        };
        assert_eq!(result, Err(ServiceFailure::SupervisorUnavailable));
        let AccountStartupAdmission::Ready(startup) = fixture.supervisor().admit_startup().unwrap()
        else {
            panic!("original Starting selection not admitted");
        };
        assert_eq!(
            startup.registration().approved_installation(),
            Some(&installation(if activate { 2 } else { 1 }))
        );
        startup.revalidate().unwrap();
        drop(startup);
        fixture.supervisor().resolve(request).await.unwrap();
    }
}

#[tokio::test]
async fn native_process_composition_requires_actor_custody_and_validates_actor_assessments() {
    use spaceterm_host::account_service::{NativeServiceInstallations, NativeServiceProcesses};
    let fixture = Fixture::new(false, true);
    let registration = fixture.supervisor().discover().unwrap().unwrap();
    let native = NativeServiceProcesses::current(Arc::new(NativeServiceInstallations)).unwrap();
    assert_eq!(
        native.assess(&registration).await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    let native = native.with_runtime(fixture.processes.clone());
    let assessment = native.assess(&registration).await.unwrap();
    native.revalidate(&registration, &assessment).await.unwrap();
    assert!(fixture.processes.reserved.load(Ordering::SeqCst));
    native
        .abort_reservation(&registration, &assessment)
        .await
        .unwrap();
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    fixture.processes.generation.store(0, Ordering::SeqCst);
    assert_eq!(
        native.assess(&registration).await,
        Err(ServiceFailure::InvalidRecord)
    );
}

#[tokio::test(start_paused = true)]
async fn install_reports_completed_when_launch_agent_runs_after_start_acknowledgement_failure() {
    let fixture = Fixture::for_install();
    *fixture.manager.start_failure.lock().unwrap() = Some(ServiceFailure::SupervisorUnavailable);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let (_, artifact) =
        signed_installation::fixture("1.0.0", 7, signed_installation::native_machine());
    let progress = supervisor
        .install(request, fixture.account.confirm_install(artifact))
        .await
        .unwrap();
    assert!(matches!(
        progress.state,
        ServiceRequestState::Completed(ServiceOutcome::Installed { reused: false, .. })
    ));
    assert_eq!(supervisor.resolve(request).await.unwrap(), progress);
    assert_eq!(
        *fixture.manager.effects.lock().unwrap(),
        ["configure", "start"]
    );
}

#[tokio::test(start_paused = true)]
async fn assess_completes_original_starting_install_when_launch_agent_is_running_without_restarting()
 {
    let fixture = Fixture::for_install();
    fixture.manager.starting.store(true, Ordering::SeqCst);
    let supervisor = fixture.supervisor();
    let install = supervisor.allocate_request().unwrap();
    let (_, artifact) =
        signed_installation::fixture("1.0.0", 7, signed_installation::native_machine());
    let progress = supervisor
        .install(install, fixture.account.confirm_install(artifact))
        .await
        .unwrap();
    assert_eq!(progress.phase, ServicePhase::Starting);
    assert_eq!(progress.state, ServiceRequestState::Pending);
    let blocked = supervisor.allocate_request().unwrap();
    assert!(matches!(
        supervisor.assess(blocked, ServiceAction::Stop).await,
        Err(ServiceFailure::Pending)
    ));
    *fixture.manager.state.lock().unwrap() = SupervisorState::Running;
    let request = supervisor.allocate_request().unwrap();
    let assessment = supervisor
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap();
    assert_eq!(assessment.request(), request);
    assert_ne!(assessment.token(), [0; 32]);
    assert!(matches!(
        supervisor.request_progress(install).unwrap().state,
        ServiceRequestState::Completed(ServiceOutcome::Installed { reused: false, .. })
    ));
    assert_eq!(
        *fixture.manager.effects.lock().unwrap(),
        ["configure", "start"]
    );
}

#[tokio::test(start_paused = true)]
async fn install_waits_for_launch_agent_readiness_within_the_observation_window() {
    let fixture = Fixture::for_install();
    fixture.manager.starting.store(true, Ordering::SeqCst);
    let manager = fixture.manager.clone();
    let transition = tokio::spawn(async move {
        loop {
            if *manager.state.lock().unwrap() == SupervisorState::Starting {
                break;
            }
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        *manager.state.lock().unwrap() = SupervisorState::Running;
    });
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let (_, artifact) =
        signed_installation::fixture("1.0.0", 7, signed_installation::native_machine());
    let progress = supervisor
        .install(request, fixture.account.confirm_install(artifact))
        .await
        .unwrap();
    assert!(matches!(
        progress.state,
        ServiceRequestState::Completed(ServiceOutcome::Installed { reused: false, .. })
    ));
    transition.await.unwrap();
    assert_eq!(
        *fixture.manager.effects.lock().unwrap(),
        ["configure", "start"]
    );
}

#[tokio::test(start_paused = true)]
async fn install_resolution_preserves_pending_startup_when_manager_observation_is_unavailable() {
    let fixture = Fixture::for_install();
    fixture.manager.starting.store(true, Ordering::SeqCst);
    *fixture.manager.status_failure.lock().unwrap() = Some(ServiceFailure::SupervisorUnavailable);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let (_, artifact) =
        signed_installation::fixture("1.0.0", 7, signed_installation::native_machine());
    let original = supervisor
        .install(request, fixture.account.confirm_install(artifact))
        .await
        .unwrap();
    assert_eq!(original.phase, ServicePhase::Starting);
    assert_eq!(original.state, ServiceRequestState::Pending);
    assert_eq!(supervisor.resolve(request).await.unwrap(), original);
    assert_eq!(
        *fixture.manager.effects.lock().unwrap(),
        ["configure", "start"]
    );
}

fn paired_custody(
    fixture: &Fixture,
) -> spaceterm_host::account_service::runtime_inventory::RuntimeCustody {
    let registration = fixture.supervisor().discover().unwrap().unwrap();
    // This key belongs only to the isolated fixture. No production credential
    // store, native authorization or service-manager process is involved.
    let identity = spaceterm_host::server_keys::ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    spaceterm_host::account_service::runtime_inventory::RuntimeCustody::begin(
        &registration,
        ServerEpoch::new(1),
        identity,
    )
    .unwrap()
}

fn paired_authority(
    fixture: &Fixture,
    custody: &spaceterm_host::account_service::runtime_inventory::RuntimeCustody,
    caller: u64,
    pairing_number: u64,
    device_number: u8,
    version: u64,
) -> spaceterm_host::account_service::RuntimeLifecycleAuthority {
    use spaceterm_core::{
        authority::{FieldVersion, StoreRevision},
        identity::*,
        pairing::{policy::*, *},
    };
    let server = ServerId::new(
        fixture
            .supervisor()
            .discover()
            .unwrap()
            .unwrap()
            .server_pin()
            .identity,
    );
    let pairing = PairingId::new(server, pairing_number).unwrap();
    let client = ClientId::new(server, caller).unwrap();
    let stream = |number| {
        OperationStreamId::new(
            OperationStreamKey::new(server, number).unwrap(),
            pairing,
            client,
        )
        .unwrap()
    };
    let recovery = PairingRecovery::new(PairingRecoveryParts {
        authorizations: vec![
            PairingAuthorization::new(PairingAuthorizationParts {
                record: PairingRecord {
                    id: pairing,
                    device: DeviceId::new([device_number; 16]),
                    state: PairingState::Authorized,
                },
                authorization_version: FieldVersion::new(version),
                keys: PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: [4; 32],
                    pairing_authentication_public_key: [5; 32],
                })
                .unwrap(),
                clients: vec![
                    PairingClientStreams::new(PairingClientStreamsParts {
                        client: ClientRecord {
                            id: client,
                            pairing,
                        },
                        ordinary: stream(1),
                        self_revocation: stream(2),
                    })
                    .unwrap(),
                ],
                revocation_receipt: None,
            })
            .unwrap(),
        ],
        enrollments: vec![],
    })
    .unwrap();
    let state = PairingSnapshot::new(
        server,
        ServerEpoch::new(1),
        StoreRevision::new(1),
        IdentityHighWater {
            client: caller,
            pairing: pairing_number,
            operation_stream: 2,
            ..Default::default()
        },
        recovery,
    )
    .unwrap();
    let principal = PairingPolicy::default()
        .authorize_client(&state, pairing, client)
        .unwrap();
    custody
        .native_lifecycle_authority(principal, FieldVersion::new(version))
        .unwrap()
}

#[tokio::test]
async fn paired_capture_storage_recovery_requires_original_key_pin_and_id_without_refreshing_intent()
 {
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let key = supervisor.key();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let request = supervisor.allocate_request().unwrap();
    test_support::fail_next_mutation_completion_sync();
    assert_eq!(
        supervisor.capture_runtime_request(request, ServiceAction::Stop),
        Err(ServiceFailure::Indeterminate)
    );
    assert!(matches!(
        supervisor.discover(),
        Err(ServiceFailure::Indeterminate)
    ));
    let mut wrong_key = key;
    wrong_key.account_uid += 1;
    assert_eq!(
        supervisor.reconcile_runtime_lifecycle_storage(wrong_key, pin, request),
        Err(ServiceFailure::IdentityMismatch)
    );
    let mut wrong_pin = pin;
    wrong_pin.public_key[0] ^= 1;
    assert_eq!(
        supervisor.reconcile_runtime_lifecycle_storage(key, wrong_pin, request),
        Err(ServiceFailure::IdentityMismatch)
    );
    assert_eq!(
        supervisor.reconcile_runtime_lifecycle_storage(
            key,
            pin,
            NativeRequestId::new([7; 16]).unwrap()
        ),
        Err(ServiceFailure::UnknownRequest)
    );
    assert!(matches!(
        supervisor.discover(),
        Err(ServiceFailure::Indeterminate)
    ));
    fixture.clock.0.store(900, Ordering::SeqCst);
    supervisor
        .reconcile_runtime_lifecycle_storage(key, pin, request)
        .unwrap();
    let capture = supervisor.restore_runtime_request(request).unwrap();
    assert_eq!(
        supervisor
            .capture_runtime_request(request, ServiceAction::Stop)
            .unwrap(),
        capture
    );
    assert_eq!(
        supervisor
            .resolve_runtime_lifecycle(key, pin, request)
            .await,
        Err(ServiceFailure::RequestConflict)
    );
    assert_eq!(fixture.processes.assessments.load(Ordering::SeqCst), 1);
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    assert_eq!(reference.expires_at.millis, 30_100);
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_assessment_lost_reply_restores_exact_original_scope_and_expiry_after_provider_recreation()
 {
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let original = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    fixture.clock.0.store(900, Ordering::SeqCst);
    let recreated = fixture.supervisor();
    assert_eq!(recreated.restore_runtime_request(request).unwrap(), capture);
    let restored = recreated
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    assert_eq!(restored, original);
    assert_eq!(fixture.processes.assessments.load(Ordering::SeqCst), 2);
    assert_eq!(restored.expires_at.millis, 30_100);
    fixture.clock.0.store(40_000, Ordering::SeqCst);
    assert_eq!(
        recreated
            .restore_runtime_lifecycle_reference(capture)
            .unwrap(),
        original
    );
    assert_eq!(fixture.processes.assessments.load(Ordering::SeqCst), 2);
    assert_eq!(
        recreated
            .resolve_runtime_lifecycle(
                recreated.key(),
                recreated.discover().unwrap().unwrap().server_pin(),
                request,
            )
            .await
            .unwrap()
            .phase,
        ServicePhase::Reserved
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_reference_restore_refuses_unknown_unsigned_and_local_requests() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let unsigned = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(unsigned, ServiceAction::Stop)
        .unwrap();
    assert_eq!(
        supervisor.restore_runtime_lifecycle_reference(capture),
        Err(ServiceFailure::RequestConflict)
    );
    let mut unknown = capture;
    unknown.request = NativeRequestId::new([7; 16]).unwrap();
    assert_eq!(
        supervisor.restore_runtime_lifecycle_reference(unknown),
        Err(ServiceFailure::UnknownRequest)
    );
    let local = supervisor.allocate_request().unwrap();
    supervisor.assess(local, ServiceAction::Stop).await.unwrap();
    let mut non_paired = capture;
    non_paired.request = local;
    assert_eq!(
        supervisor.restore_runtime_lifecycle_reference(non_paired),
        Err(ServiceFailure::RequestConflict)
    );
    assert_eq!(
        supervisor.request_progress(local).unwrap().phase,
        ServicePhase::Reserved
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_acceptance_lost_reply_stays_accepted_without_native_effects_until_original_owner_resolution()
 {
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    test_support::interrupt_after_phase(ServicePhase::Accepted);
    assert_eq!(
        supervisor
            .admit_runtime_lifecycle(reference.clone(), &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::SupervisorUnavailable)
    );
    assert!(fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
    let accepted = fixture
        .supervisor()
        .admit_runtime_lifecycle(reference, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    assert_eq!(accepted.phase, ServicePhase::Accepted);
    assert_eq!(accepted.request, request);
    // Acceptance survives its original assessment expiry. Resolution must not
    // reassess, change the action or allocate another native request.
    fixture.clock.0.store(40_000, Ordering::SeqCst);
    assert_eq!(
        supervisor
            .poll_runtime_lifecycle_handoff(
                supervisor.key(),
                supervisor.discover().unwrap().unwrap().server_pin(),
                capture,
            )
            .unwrap(),
        spaceterm_host::account_service::NativeLifecycleHandoff::ResolveOriginal {
            progress: accepted.clone(),
        }
    );
    assert!(fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let completed = fixture.supervisor().resolve(request).await.unwrap();
    assert_eq!(
        completed.state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    assert_eq!(completed.request_digest, accepted.request_digest);
    assert_eq!(
        fixture.supervisor().resolve(request).await.unwrap(),
        completed
    );
    assert_eq!(fixture.processes.calls.lock().unwrap().len(), 1);
    assert_eq!(
        fixture
            .supervisor()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
}

#[tokio::test]
async fn owner_cannot_admit_a_paired_origin_reserved_request_or_substitute_start() {
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Restart)
        .unwrap();
    assert_eq!(
        supervisor.start(request).await,
        Err(ServiceFailure::PairingRequired)
    );
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    assert_eq!(
        supervisor.resolve(request).await.unwrap().phase,
        ServicePhase::Reserved
    );
    assert!(matches!(
        supervisor.assess(request, ServiceAction::Restart).await,
        Err(ServiceFailure::RequestConflict)
    ));
    assert!(matches!(
        supervisor
            .restore_assessment(request, reference.proof_digest.bytes())
            .await,
        Err(ServiceFailure::PairingRequired)
    ));
    assert_eq!(
        supervisor.start(request).await,
        Err(ServiceFailure::PairingRequired)
    );
    assert_eq!(
        supervisor.capture_runtime_request(request, ServiceAction::Stop),
        Err(ServiceFailure::RequestConflict)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_native_admission_rejects_every_public_scope_comparison_substitution() {
    use spaceterm_core::operations::RequestDigest;
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    let mut substitutions = Vec::new();
    let mut changed = reference.clone();
    changed.original.action = ServiceAction::Restart;
    substitutions.push(changed);
    let mut changed = reference.clone();
    changed.original.capture_digest = RequestDigest::from_bytes([91; 32]);
    substitutions.push(changed);
    let mut changed = reference.clone();
    changed.original.request = supervisor.allocate_request().unwrap();
    substitutions.push(changed);
    let mut changed = reference.clone();
    changed.request_digest = RequestDigest::from_bytes([92; 32]);
    substitutions.push(changed);
    let mut changed = reference.clone();
    changed.proof_digest = RequestDigest::from_bytes([93; 32]);
    substitutions.push(changed);
    let mut changed = reference.clone();
    changed.epoch = ServerEpoch::new(2);
    substitutions.push(changed);
    let mut changed = reference.clone();
    changed.affected_sessions += 1;
    substitutions.push(changed);
    let mut changed = reference.clone();
    changed.expires_at.millis += 1;
    substitutions.push(changed);
    let mut changed = reference.clone();
    changed.expires_at.boot = [99; 32];
    substitutions.push(changed);
    for changed in substitutions {
        assert_eq!(
            supervisor
                .admit_runtime_lifecycle(changed, &authority, fixture.processes.as_ref())
                .await,
            Err(ServiceFailure::RequestConflict)
        );
    }
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_assessment_and_admission_reject_different_client_pairing_device_and_authorization_lifetime()
 {
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    for (client, pairing, device, version) in
        [(2, 1, 3, 1), (1, 2, 3, 1), (1, 1, 7, 1), (1, 1, 3, 2)]
    {
        let other = paired_authority(&fixture, &custody, client, pairing, device, version);
        assert_eq!(
            supervisor
                .assess_runtime_lifecycle(capture, &other, fixture.processes.as_ref())
                .await,
            Err(ServiceFailure::PairingRequired)
        );
        assert_eq!(
            supervisor
                .admit_runtime_lifecycle(reference.clone(), &other, fixture.processes.as_ref())
                .await,
            Err(ServiceFailure::PairingRequired)
        );
    }
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_request_original_expiry_is_not_extended_by_capture_restore_or_assessment() {
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    fixture.clock.0.store(30_000, Ordering::SeqCst);
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    assert_eq!(reference.expires_at.millis, 30_100);
    fixture.clock.0.store(30_100, Ordering::SeqCst);
    assert_eq!(
        fixture
            .supervisor()
            .capture_runtime_request(request, ServiceAction::Stop)
            .unwrap(),
        capture
    );
    assert_eq!(
        supervisor
            .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::StaleAssessment)
    );
    assert_eq!(
        supervisor
            .admit_runtime_lifecycle(reference, &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::StaleAssessment)
    );
    assert_eq!(
        supervisor.resolve(request).await.unwrap().phase,
        ServicePhase::Reserved
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_admission_rechecks_original_expiry_after_final_registration_and_lease_validation() {
    struct CrossingClock(AtomicU64);
    impl ServiceClock for CrossingClock {
        fn now(&self) -> Result<ServiceTime, ServiceFailure> {
            let call = self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ServiceTime {
                boot: [3; 32],
                millis: if call < 3 { 30_099 } else { 30_100 },
            })
        }
    }
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    let clock = Arc::new(CrossingClock(AtomicU64::new(0)));
    let (key, _) = signed_installation::fixture("1.0.0", 7, signed_installation::native_machine());
    let crossing = fixture.account.with_installation_trust(
        fixture.account.with_providers(
            fixture.manager.clone(),
            fixture.processes.clone(),
            clock.clone(),
            None,
        ),
        key,
    );
    assert_eq!(
        crossing
            .admit_runtime_lifecycle(reference, &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::StaleAssessment)
    );
    assert_eq!(clock.0.load(Ordering::SeqCst), 4);
    assert_eq!(
        supervisor.request_progress(request).unwrap().phase,
        ServicePhase::Reserved
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_exact_ack_preserves_only_the_original_signed_terminal_receipt_until_pressure_retirement()
 {
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let owner_token = supervisor
        .native_lifecycle_owner_token(supervisor.key(), pin, capture)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    let reserved = supervisor.request_progress(request).unwrap();
    assert_eq!(
        supervisor.acknowledge_runtime_lifecycle_exact(&reserved),
        Err(ServiceFailure::Pending)
    );
    let accepted = supervisor
        .admit_runtime_lifecycle(reference.clone(), &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    assert_eq!(
        supervisor.acknowledge_runtime_lifecycle_exact(&accepted),
        Err(ServiceFailure::Pending)
    );
    for _ in 0..256 {
        supervisor.allocate_request().unwrap();
    }
    assert_eq!(
        supervisor
            .poll_owned_runtime_lifecycle_handoff(&owner_token)
            .unwrap(),
        spaceterm_host::account_service::NativeLifecycleHandoff::ResolveOriginal {
            progress: accepted.clone(),
        }
    );
    assert!(fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let completed = supervisor.resolve(request).await.unwrap();
    let effects = fixture.manager.effects.lock().unwrap().clone();
    assert_eq!(
        supervisor.acknowledge_request(request),
        Err(ServiceFailure::PairingRequired)
    );
    for index in 0..3 {
        let mut substituted = completed.clone();
        match index {
            0 => {
                substituted.request_digest =
                    spaceterm_core::operations::RequestDigest::from_bytes([9; 32])
            }
            1 => substituted.phase = ServicePhase::Accepted,
            _ => substituted.state = ServiceRequestState::Completed(ServiceOutcome::Restarted),
        }
        assert_eq!(
            supervisor.acknowledge_runtime_lifecycle_exact(&substituted),
            Err(ServiceFailure::RequestConflict)
        );
    }
    supervisor
        .acknowledge_runtime_lifecycle_exact(&completed)
        .unwrap();
    let recreated = fixture.supervisor();
    recreated
        .acknowledge_runtime_lifecycle_exact(&completed)
        .unwrap();
    let pin = recreated.discover().unwrap().unwrap().server_pin();
    assert_eq!(
        recreated
            .poll_runtime_lifecycle_handoff(recreated.key(), pin, capture)
            .unwrap(),
        spaceterm_host::account_service::NativeLifecycleHandoff::ResolveOriginal {
            progress: completed.clone(),
        }
    );
    let mut changed_capture = capture;
    changed_capture.action = ServiceAction::Restart;
    assert_eq!(
        recreated.poll_runtime_lifecycle_handoff(recreated.key(), pin, changed_capture),
        Err(ServiceFailure::RequestConflict)
    );
    assert_eq!(
        recreated.request_progress(request),
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        recreated.resolve(request).await,
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        recreated.restore_runtime_request(request),
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        recreated.restore_runtime_lifecycle_reference(capture),
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        recreated
            .admit_runtime_lifecycle(reference, &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::RequestRetired)
    );
    assert!(recreated.retained_service_requests().unwrap().is_empty());
    let local = recreated.allocate_request().unwrap();
    let action = recreated
        .assess(local, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    let local_completed = recreated.stop(action).await.unwrap();
    assert_eq!(
        recreated.acknowledge_runtime_lifecycle_exact(&local_completed),
        Err(ServiceFailure::PairingRequired)
    );
    recreated.acknowledge_request(local).unwrap();
    let effects_after_local = fixture.manager.effects.lock().unwrap().clone();
    for _ in 0..256 {
        recreated.allocate_request().unwrap();
    }
    assert_eq!(
        recreated.acknowledge_runtime_lifecycle_exact(&completed),
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        recreated.poll_runtime_lifecycle_handoff(recreated.key(), pin, capture),
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        recreated
            .poll_owned_runtime_lifecycle_handoff(&owner_token)
            .unwrap(),
        spaceterm_host::account_service::NativeLifecycleHandoff::RetiredOriginal {
            original: capture
        }
    );
    assert_eq!(
        *fixture.manager.effects.lock().unwrap(),
        effects_after_local
    );
    assert!(!effects.is_empty());
}

#[tokio::test]
async fn paired_uninstall_ack_retains_exact_signed_result_after_native_registration_is_uninstalled()
{
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Uninstall)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    let accepted = supervisor
        .admit_runtime_lifecycle(reference, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    assert_eq!(accepted.phase, ServicePhase::Accepted);
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let completed = supervisor.resolve(request).await.unwrap();
    assert_eq!(
        completed.state,
        ServiceRequestState::Completed(ServiceOutcome::Uninstalled)
    );
    let retained = supervisor.discover().unwrap().unwrap();
    assert!(!retained.is_installed());
    assert_eq!(retained.desired_state(), DesiredServerState::Stopped);
    assert_eq!(retained.server_pin(), pin);
    supervisor
        .acknowledge_runtime_lifecycle_exact(&completed)
        .unwrap();
    fixture
        .supervisor()
        .acknowledge_runtime_lifecycle_exact(&completed)
        .unwrap();
    assert_eq!(
        supervisor.resolve(request).await,
        Err(ServiceFailure::RequestRetired)
    );
}

#[tokio::test]
async fn paired_acceptance_uncertain_durability_keeps_barrier_without_effects_until_original_resolution()
 {
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    fixture
        .processes
        .fail_acceptance
        .store(true, Ordering::SeqCst);
    assert_eq!(
        supervisor
            .admit_runtime_lifecycle(reference.clone(), &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::Indeterminate)
    );
    assert!(matches!(
        supervisor.discover(),
        Err(ServiceFailure::Indeterminate)
    ));
    assert!(fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    supervisor
        .reconcile_runtime_lifecycle_storage(supervisor.key(), pin, request)
        .unwrap();
    assert_eq!(
        supervisor
            .restore_runtime_lifecycle_reference(capture)
            .unwrap(),
        reference
    );
    assert_eq!(
        supervisor.request_progress(request).unwrap().phase,
        ServicePhase::Accepted
    );
    assert!(fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let completed = supervisor
        .resolve_runtime_lifecycle(supervisor.key(), pin, request)
        .await
        .unwrap();
    assert_eq!(
        completed.state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert_eq!(fixture.processes.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn paired_assessments_for_simultaneous_callers_preserve_exact_request_and_original_scope() {
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let first = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let second = paired_authority(&fixture, &custody, 2, 2, 7, 1);
    let supervisor = fixture.supervisor();
    let first_request = supervisor.allocate_request().unwrap();
    let second_request = supervisor.allocate_request().unwrap();
    let first_capture = supervisor
        .capture_runtime_request(first_request, ServiceAction::Stop)
        .unwrap();
    let second_capture = supervisor
        .capture_runtime_request(second_request, ServiceAction::Restart)
        .unwrap();
    let first_ref = supervisor
        .assess_runtime_lifecycle(first_capture, &first, fixture.processes.as_ref())
        .await
        .unwrap();
    let second_ref = supervisor
        .assess_runtime_lifecycle(second_capture, &second, fixture.processes.as_ref())
        .await
        .unwrap();
    assert_ne!(first_ref.request_digest, second_ref.request_digest);
    assert_ne!(first_ref.proof_digest, second_ref.proof_digest);
    assert_eq!(
        supervisor
            .admit_runtime_lifecycle(first_ref.clone(), &second, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::PairingRequired)
    );
    let accepted = supervisor
        .admit_runtime_lifecycle(second_ref.clone(), &second, fixture.processes.as_ref())
        .await
        .unwrap();
    assert_eq!(accepted.request, second_request);
    assert_eq!(accepted.request_digest, second_ref.request_digest);
    assert_eq!(
        supervisor
            .admit_runtime_lifecycle(first_ref, &first, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::Pending)
    );
    assert_eq!(
        supervisor.request_progress(first_request).unwrap().phase,
        ServicePhase::Reserved
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_activation_rejects_staged_artifact_changed_after_original_capture() {
    let fixture = Fixture::new(false, true);
    fixture.account.stage_installation(installation(2)).unwrap();
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(
            request,
            ServiceAction::Activate(installation(2).installation),
        )
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    let mut changed = installation(2);
    changed.artifact_sha256 = [91; 32];
    fixture.account.stage_installation(changed).unwrap();
    assert_eq!(
        supervisor
            .admit_runtime_lifecycle(reference, &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::StaleAssessment)
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_origin_signature_tampering_cannot_be_resolved_or_imported_as_native_authority() {
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    let path = fixture.account.anchor().join("requests");
    let mut journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let signature = journal["reservations"][0]["service"]["runtime_origin"]["signature"][0]
        .as_u64()
        .unwrap();
    journal["reservations"][0]["service"]["runtime_origin"]["signature"][0] =
        serde_json::json!(signature ^ 1);
    journal.sort_all_objects();
    std::fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
    assert_eq!(
        fixture.supervisor().resolve(request).await,
        Err(ServiceFailure::InvalidRecord)
    );
    assert_eq!(
        fixture.supervisor().request_progress(request),
        Err(ServiceFailure::InvalidRecord)
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn original_paired_non_admission_requires_exact_pinned_signed_abort_before_owner_resolution()
{
    use spaceterm_host::account_service::runtime_inventory::RetainedRuntimeProcesses;
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    *fixture.processes.advance_on_revalidate.lock().unwrap() = Some(fixture.clock.clone());
    fixture.processes.reject_abort.store(true, Ordering::SeqCst);
    assert_eq!(
        supervisor
            .admit_runtime_lifecycle(reference, &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    let registration = supervisor.discover().unwrap().unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let (original, scope) = supervisor
        .runtime_origin_abort_request(server, ServerEpoch::new(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        supervisor
            .poll_runtime_lifecycle_handoff(supervisor.key(), registration.server_pin(), capture)
            .unwrap(),
        spaceterm_host::account_service::NativeLifecycleHandoff::ResolveOriginal {
            progress: supervisor.request_progress(request).unwrap(),
        }
    );
    assert!(fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let retained = RetainedRuntimeProcesses;
    assert_eq!(
        retained
            .abort_native_origin(&registration, original, &scope)
            .await,
        Err(ServiceFailure::Pending)
    );
    fixture
        .processes
        .reject_abort
        .store(false, Ordering::SeqCst);
    fixture
        .processes
        .abort_reservation(&registration, &scope)
        .await
        .unwrap();
    custody
        .reservation_aborted(&registration, original, &scope)
        .unwrap();
    let replacement = NativeManagementRequestRef::new(
        server,
        original.id_bytes(),
        spaceterm_core::operations::RequestDigest::from_bytes([97; 32]),
    )
    .unwrap();
    assert_eq!(
        retained
            .abort_native_origin(&registration, replacement, &scope)
            .await,
        Err(ServiceFailure::RequestConflict)
    );
    let mut changed = scope.clone();
    changed.generation += 1;
    assert_eq!(
        retained
            .abort_native_origin(&registration, original, &changed)
            .await,
        Err(ServiceFailure::RequestConflict)
    );
    let owner = fixture.account.with_providers(
        fixture.manager.clone(),
        Arc::new(retained),
        fixture.clock.clone(),
        None,
    );
    assert_eq!(
        owner
            .resolve_runtime_lifecycle(owner.key(), registration.server_pin(), request)
            .await
            .unwrap()
            .phase,
        ServicePhase::Reserved
    );
    assert!(
        owner
            .runtime_origin_abort_request(server, ServerEpoch::new(1))
            .unwrap()
            .is_none()
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert_eq!(
        owner.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
}

#[tokio::test]
async fn uncertain_signed_abort_publication_recovers_only_original_non_admission_without_native_effects()
 {
    use spaceterm_host::account_service::runtime_inventory::RetainedRuntimeProcesses;
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    *fixture.processes.advance_on_revalidate.lock().unwrap() = Some(fixture.clock.clone());
    fixture.processes.reject_abort.store(true, Ordering::SeqCst);
    assert_eq!(
        supervisor
            .admit_runtime_lifecycle(reference, &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    let registration = supervisor.discover().unwrap().unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let (original, scope) = supervisor
        .runtime_origin_abort_request(server, ServerEpoch::new(1))
        .unwrap()
        .unwrap();
    fixture
        .processes
        .reject_abort
        .store(false, Ordering::SeqCst);
    fixture
        .processes
        .abort_reservation(&registration, &scope)
        .await
        .unwrap();
    test_support::fail_next_mutation_completion_sync();
    assert_eq!(
        custody.reservation_aborted(&registration, original, &scope),
        Err(ServiceFailure::Indeterminate)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let owner = fixture.account.with_providers(
        fixture.manager.clone(),
        Arc::new(RetainedRuntimeProcesses),
        fixture.clock.clone(),
        None,
    );
    assert_eq!(
        owner
            .resolve_runtime_lifecycle(owner.key(), registration.server_pin(), request)
            .await
            .unwrap()
            .phase,
        ServicePhase::Reserved
    );
    assert_eq!(
        owner.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert_eq!(
        owner.start(request).await,
        Err(ServiceFailure::PairingRequired)
    );
}

#[tokio::test]
async fn native_full_absence_needs_no_approved_executable_when_no_root_is_exempted() {
    struct MissingApprovedCode;
    impl spaceterm_host::account_service::ServiceInstallations for MissingApprovedCode {
        fn approved_executable(
            &self,
            _: &RegisteredServer,
        ) -> Result<std::path::PathBuf, ServiceFailure> {
            Err(ServiceFailure::ArtifactRejected)
        }
        fn uninstall(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
            Err(ServiceFailure::ArtifactRejected)
        }
    }
    let fixture = Fixture::new(false, true);
    let registration = fixture.supervisor().discover().unwrap().unwrap();
    let native = spaceterm_host::account_service::NativeServiceProcesses::current(Arc::new(
        MissingApprovedCode,
    ))
    .unwrap()
    .with_manager(fixture.manager.clone())
    .with_runtime(fixture.processes.clone());
    native.verify_absent(&registration).await.unwrap();
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn signed_clean_custody_cannot_hide_a_live_exact_owner_outside_manager_scope() {
    use spaceterm_host::account_service::{
        NativeServiceProcesses, runtime_inventory::RetainedRuntimeProcesses,
    };
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let registration = fixture.supervisor().discover().unwrap().unwrap();
    custody.completed_instance(&registration).unwrap();
    assert!(RetainedRuntimeProcesses::is_clean(&registration).unwrap());
    let native = NativeServiceProcesses::current(Arc::new(AbortInstallation))
        .unwrap()
        .with_manager(fixture.manager.clone())
        .with_runtime(Arc::new(RetainedRuntimeProcesses));
    assert_eq!(
        native.verify_absent(&registration).await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    // A trusted native boot change discharges only the old signed process;
    // fixed manager scope, clean custody and private listener checks still run.
    struct NewBootClock([u8; 32]);
    impl ServiceClock for NewBootClock {
        fn now(&self) -> Result<ServiceTime, ServiceFailure> {
            Ok(ServiceTime {
                boot: self.0,
                millis: 100,
            })
        }
    }
    let mut boot = ServiceTime::current().unwrap().boot;
    boot[0] ^= 1;
    let next_boot = native.with_clock(Arc::new(NewBootClock(boot)));
    next_boot.verify_absent(&registration).await.unwrap();
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

struct AbortInstallation;
impl spaceterm_host::account_service::ServiceInstallations for AbortInstallation {
    fn approved_executable(
        &self,
        _: &RegisteredServer,
    ) -> Result<std::path::PathBuf, ServiceFailure> {
        std::env::current_exe().map_err(|_| ServiceFailure::StorageUnavailable)
    }
    fn uninstall(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        Err(ServiceFailure::CleanupUnresolved)
    }
}
struct AbortInstanceObserver {
    live: AtomicBool,
    observations: Mutex<Vec<(u32, u64)>>,
}
impl spaceterm_host::account_service::InstanceProcessObserver for AbortInstanceObserver {
    fn verify_absent(&self, pid: u32, birth: u64, _: &[[u64; 2]]) -> Result<(), ServiceFailure> {
        self.observations.lock().unwrap().push((pid, birth));
        if self.live.load(Ordering::SeqCst) {
            Err(ServiceFailure::CleanupUnresolved)
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn crashed_runtime_non_admission_requires_exact_native_instance_absence_before_owner_resolution()
 {
    use spaceterm_host::account_service::{
        NativeServiceProcesses, runtime_inventory::RetainedRuntimeProcesses,
    };
    for stale_witness in [false, true] {
        let fixture = Fixture::new(false, true);
        let custody = paired_custody(&fixture);
        let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
        let supervisor = fixture.supervisor();
        let previous_request = supervisor.allocate_request().unwrap();
        let request = supervisor.allocate_request().unwrap();
        let capture = supervisor
            .capture_runtime_request(request, ServiceAction::Stop)
            .unwrap();
        let reference = supervisor
            .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
            .await
            .unwrap();
        *fixture.processes.advance_on_revalidate.lock().unwrap() = Some(fixture.clock.clone());
        fixture.processes.reject_abort.store(true, Ordering::SeqCst);
        assert_eq!(
            supervisor
                .admit_runtime_lifecycle(reference, &authority, fixture.processes.as_ref())
                .await,
            Err(ServiceFailure::CleanupUnresolved)
        );
        if stale_witness {
            let registration = supervisor.discover().unwrap().unwrap();
            let (original, mut previous_scope) = supervisor
                .runtime_origin_abort_request(
                    ServerId::new(registration.server_pin().identity),
                    ServerEpoch::new(1),
                )
                .unwrap()
                .unwrap();
            previous_scope.token = [101; 32];
            let previous = NativeManagementRequestRef::new(
                original.server(),
                *previous_request.as_bytes(),
                spaceterm_core::operations::RequestDigest::from_bytes([103; 32]),
            )
            .unwrap();
            // Another request's pinned signed witness grants no authority here. The
            // native adapter must still prove this exact old instance absent.
            custody
                .reservation_aborted(&registration, previous, &previous_scope)
                .unwrap();
        }
        let observer = Arc::new(AbortInstanceObserver {
            live: AtomicBool::new(true),
            observations: Mutex::new(Vec::new()),
        });
        let processes = NativeServiceProcesses::current(Arc::new(AbortInstallation))
            .unwrap()
            .with_manager(fixture.manager.clone())
            .with_runtime(Arc::new(RetainedRuntimeProcesses))
            .with_instance_observer(observer.clone());
        let owner = fixture.account.with_providers(
            fixture.manager.clone(),
            Arc::new(processes),
            fixture.clock.clone(),
            None,
        );
        assert_eq!(
            owner.resolve(request).await,
            Err(ServiceFailure::CleanupUnresolved)
        );
        let observations = observer.observations.lock().unwrap().clone();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].0, std::process::id());
        assert_ne!(observations[0].1, 0);
        observer.live.store(false, Ordering::SeqCst);
        assert_eq!(
            owner.resolve(request).await.unwrap().phase,
            ServicePhase::Reserved
        );
        assert!(fixture.manager.effects.lock().unwrap().is_empty());
        assert_eq!(
            owner.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Running
        );
        assert_eq!(observer.observations.lock().unwrap()[1], observations[0]);
    }
}

#[tokio::test]
async fn paired_owner_handoff_preserves_unsigned_capture_and_uses_only_original_native_expiry() {
    use spaceterm_host::account_service::NativeLifecycleHandoff;
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let poll = || supervisor.poll_runtime_lifecycle_handoff(supervisor.key(), pin, capture);
    assert_eq!(
        poll().unwrap(),
        NativeLifecycleHandoff::Awaiting {
            expires_at: ServiceTime {
                boot: [3; 32],
                millis: 30_100
            },
        }
    );
    let mut wrong_key = supervisor.key();
    wrong_key.account_uid += 1;
    assert_eq!(
        supervisor.poll_runtime_lifecycle_handoff(wrong_key, pin, capture),
        Err(ServiceFailure::IdentityMismatch)
    );
    let mut wrong_pin = pin;
    wrong_pin.public_key[0] ^= 1;
    assert_eq!(
        supervisor.poll_runtime_lifecycle_handoff(supervisor.key(), wrong_pin, capture),
        Err(ServiceFailure::IdentityMismatch)
    );
    let mut changed = capture;
    changed.action = ServiceAction::Uninstall;
    assert_eq!(
        supervisor.poll_runtime_lifecycle_handoff(supervisor.key(), pin, changed),
        Err(ServiceFailure::RequestConflict)
    );
    fixture.clock.0.store(99, Ordering::SeqCst);
    assert_eq!(poll(), Err(ServiceFailure::SupervisorUnavailable));
    fixture.clock.0.store(30_099, Ordering::SeqCst);
    assert!(matches!(
        poll().unwrap(),
        NativeLifecycleHandoff::Awaiting { .. }
    ));
    fixture.clock.0.store(30_100, Ordering::SeqCst);
    assert_eq!(
        poll().unwrap(),
        NativeLifecycleHandoff::ExpiredNonAdmission {
            original: capture,
            progress: None,
        }
    );
    let recreated = fixture.supervisor();
    assert_eq!(recreated.restore_runtime_request(request).unwrap(), capture);
    assert_eq!(
        recreated
            .capture_runtime_request(request, ServiceAction::Stop)
            .unwrap(),
        capture
    );
    assert_eq!(
        recreated
            .poll_runtime_lifecycle_handoff(recreated.key(), pin, capture)
            .unwrap(),
        NativeLifecycleHandoff::ExpiredNonAdmission {
            original: capture,
            progress: None
        }
    );
    assert_eq!(
        recreated
            .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::StaleAssessment)
    );
    assert_eq!(fixture.processes.assessments.load(Ordering::SeqCst), 1);
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_owner_handoff_seals_only_exact_signed_unreserved_expiry_without_effects() {
    use spaceterm_host::account_service::NativeLifecycleHandoff;
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    assert!(matches!(
        supervisor
            .poll_runtime_lifecycle_handoff(supervisor.key(), pin, capture)
            .unwrap(),
        NativeLifecycleHandoff::Awaiting { .. }
    ));
    fixture
        .clock
        .0
        .store(reference.expires_at.millis, Ordering::SeqCst);
    let NativeLifecycleHandoff::ExpiredNonAdmission {
        original,
        progress: Some(progress),
    } = supervisor
        .poll_runtime_lifecycle_handoff(supervisor.key(), pin, capture)
        .unwrap()
    else {
        panic!("exact signed expiry must become terminal non-admission")
    };
    assert_eq!(original, capture);
    assert_eq!(progress.request, request);
    assert_eq!(progress.request_digest, reference.request_digest);
    assert_eq!(progress.phase, ServicePhase::Completed);
    assert_eq!(
        progress.state,
        ServiceRequestState::NotApplied(ServiceFailure::StaleAssessment)
    );
    assert_eq!(
        supervisor.restore_runtime_request(request).unwrap(),
        capture
    );
    assert_eq!(
        supervisor
            .restore_runtime_lifecycle_reference(capture)
            .unwrap(),
        reference
    );
    assert_eq!(
        supervisor
            .admit_runtime_lifecycle(reference.clone(), &authority, fixture.processes.as_ref())
            .await
            .unwrap(),
        progress
    );
    assert_eq!(
        fixture
            .supervisor()
            .poll_runtime_lifecycle_handoff(supervisor.key(), pin, capture)
            .unwrap(),
        NativeLifecycleHandoff::ResolveOriginal {
            progress: progress.clone()
        }
    );
    assert_eq!(
        supervisor
            .resolve_runtime_lifecycle(supervisor.key(), pin, request)
            .await
            .unwrap(),
        progress
    );
    supervisor
        .acknowledge_runtime_lifecycle_exact(&progress)
        .unwrap();
    assert_eq!(
        supervisor
            .poll_runtime_lifecycle_handoff(supervisor.key(), pin, capture)
            .unwrap(),
        NativeLifecycleHandoff::ResolveOriginal { progress }
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_owner_handoff_uncertain_expiry_publication_requires_original_recovery() {
    use spaceterm_host::account_service::NativeLifecycleHandoff;
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(capture, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    fixture
        .clock
        .0
        .store(reference.expires_at.millis, Ordering::SeqCst);
    test_support::fail_next_mutation_completion_sync();
    assert_eq!(
        supervisor.poll_runtime_lifecycle_handoff(supervisor.key(), pin, capture),
        Err(ServiceFailure::Indeterminate)
    );
    let NativeLifecycleHandoff::ResolveOriginal { progress } = supervisor
        .poll_runtime_lifecycle_handoff(supervisor.key(), pin, capture)
        .unwrap()
    else {
        panic!("original durable expiry publication must be reconciled")
    };
    assert_eq!(progress.request_digest, reference.request_digest);
    assert_eq!(progress.phase, ServicePhase::Completed);
    assert_eq!(
        progress.state,
        ServiceRequestState::NotApplied(ServiceFailure::StaleAssessment)
    );
    assert_eq!(
        supervisor
            .restore_runtime_lifecycle_reference(capture)
            .unwrap(),
        reference
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_owned_handoff_drains_only_prior_durable_originals_retired_by_expiry_or_pressure() {
    use spaceterm_host::account_service::NativeLifecycleHandoff;
    let fixture = Fixture::new(false, true);
    let custody = paired_custody(&fixture);
    let authority = paired_authority(&fixture, &custody, 1, 1, 3, 1);
    let supervisor = fixture.supervisor();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let unsigned_id = supervisor.allocate_request().unwrap();
    let unsigned = supervisor
        .capture_runtime_request(unsigned_id, ServiceAction::Stop)
        .unwrap();
    let unsigned_owner = supervisor
        .native_lifecycle_owner_token(supervisor.key(), pin, unsigned)
        .unwrap();
    let signed_id = supervisor.allocate_request().unwrap();
    let signed = supervisor
        .capture_runtime_request(signed_id, ServiceAction::Restart)
        .unwrap();
    let signed_owner = supervisor
        .native_lifecycle_owner_token(supervisor.key(), pin, signed)
        .unwrap();
    let reference = supervisor
        .assess_runtime_lifecycle(signed, &authority, fixture.processes.as_ref())
        .await
        .unwrap();
    fixture
        .clock
        .0
        .store(reference.expires_at.millis, Ordering::SeqCst);
    // Another helper's ordinary reservation reclaims only the expired unreserved
    // signed original. The owner never claims a particular result from its absence.
    let other = fixture.supervisor();
    other.allocate_request().unwrap();
    assert_eq!(
        other.poll_runtime_lifecycle_handoff(other.key(), pin, signed),
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        other
            .poll_owned_runtime_lifecycle_handoff(&signed_owner)
            .unwrap(),
        NativeLifecycleHandoff::RetiredOriginal { original: signed }
    );
    for _ in 0..256 {
        other.allocate_request().unwrap();
    }
    assert_eq!(
        other.poll_runtime_lifecycle_handoff(other.key(), pin, unsigned),
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        other
            .poll_owned_runtime_lifecycle_handoff(&unsigned_owner)
            .unwrap(),
        NativeLifecycleHandoff::RetiredOriginal { original: unsigned }
    );
    assert_eq!(
        other
            .assess_runtime_lifecycle(signed, &authority, fixture.processes.as_ref())
            .await,
        Err(ServiceFailure::RequestRetired)
    );
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_owned_handoff_refuses_forwarded_capture_foreign_namespace_and_replaced_native_object()
 {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let token = supervisor
        .native_lifecycle_owner_token(supervisor.key(), pin, capture)
        .unwrap();
    let mut forwarded = capture;
    forwarded.action = ServiceAction::Restart;
    assert!(matches!(
        supervisor.native_lifecycle_owner_token(supervisor.key(), pin, forwarded),
        Err(ServiceFailure::RequestConflict)
    ));
    forwarded = capture;
    forwarded.request = supervisor.allocate_request().unwrap();
    assert!(matches!(
        supervisor.native_lifecycle_owner_token(supervisor.key(), pin, forwarded),
        Err(ServiceFailure::RequestConflict)
    ));
    let mut foreign_id = *request.as_bytes();
    foreign_id[0] ^= 1;
    forwarded.request = NativeRequestId::new(foreign_id).unwrap();
    assert!(matches!(
        supervisor.native_lifecycle_owner_token(supervisor.key(), pin, forwarded),
        Err(ServiceFailure::UnknownRequest)
    ));
    let foreign = Fixture::new(false, true);
    assert_eq!(
        foreign
            .supervisor()
            .poll_owned_runtime_lifecycle_handoff(&token),
        Err(ServiceFailure::IdentityMismatch)
    );
    let old_anchor = fixture
        .account
        .anchor()
        .with_file_name("retained-owner-old-anchor");
    std::fs::rename(fixture.account.anchor(), &old_anchor).unwrap();
    std::fs::create_dir(fixture.account.anchor()).unwrap();
    assert_eq!(
        supervisor.poll_owned_runtime_lifecycle_handoff(&token),
        Err(ServiceFailure::UnsafeStorage)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn paired_owned_handoff_cannot_mint_or_drain_from_uncertain_retirement_publication() {
    let fixture = Fixture::new(false, true);
    let supervisor = fixture.supervisor();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let request = supervisor.allocate_request().unwrap();
    let capture = supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let token = supervisor
        .native_lifecycle_owner_token(supervisor.key(), pin, capture)
        .unwrap();
    for _ in 0..255 {
        supervisor.allocate_request().unwrap();
    }
    test_support::fail_next_mutation_completion_sync();
    assert_eq!(
        supervisor.allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert!(matches!(
        supervisor.native_lifecycle_owner_token(supervisor.key(), pin, capture),
        Err(ServiceFailure::Indeterminate)
    ));
    assert!(matches!(
        supervisor.poll_owned_runtime_lifecycle_handoff(&token),
        Err(ServiceFailure::Indeterminate | ServiceFailure::RequestRetired)
    ));
    assert!(matches!(
        supervisor.discover(),
        Err(ServiceFailure::Indeterminate)
    ));
    assert!(!fixture.processes.reserved.load(Ordering::SeqCst));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}
