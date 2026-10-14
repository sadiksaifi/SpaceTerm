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
        Box::pin(async { Ok(*self.state.lock().unwrap()) })
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
            *self.state.lock().unwrap() = SupervisorState::Running;
            Ok(())
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
        *fixture.processes.sessions.lock().unwrap() = (1..=64)
            .map(|local| SessionKey {
                epoch: ServerEpoch::new(1),
                pane: spaceterm_core::identity::PaneId::new(server, local).unwrap(),
                generation: spaceterm_core::authority::SessionGeneration::new(1),
            })
            .collect();
        // Bind the target's exact scope and completion budget before admission
        // pressure. It must survive later allocations without a replacement ID.
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
        let action = assessment.confirm().unwrap();
        // Retain exact nonempty scopes until another assessment no longer fits.
        for _ in 0..32 {
            let next = match supervisor.allocate_request() {
                Ok(next) => next,
                Err(ServiceFailure::LimitExceeded) => break,
                Err(failure) => panic!("unexpected allocation failure {failure:?}"),
            };
            match supervisor.assess(next, ServiceAction::Stop).await {
                Ok(_) => {}
                Err(ServiceFailure::LimitExceeded) => break,
                Err(failure) => panic!("unexpected assessment failure {failure:?}"),
            }
        }
        // Later allocations must not consume capacity already owed to this outcome.
        // Allocation may retire old unsubmitted IDs rather than fail. Padding
        // is bounded by the journal's 256 slots, not an unreachable byte target.
        for _ in 0..256 {
            if std::fs::metadata(fixture.account.anchor().join("requests"))
                .unwrap()
                .len()
                >= (256 * 1024 - 120)
            {
                break;
            }
            match supervisor.allocate_request() {
                Ok(_) => {}
                Err(ServiceFailure::LimitExceeded) => break,
                Err(failure) => panic!("unexpected allocation failure {failure:?}"),
            }
        }
        assert!(
            std::fs::metadata(fixture.account.anchor().join("requests"))
                .unwrap()
                .len()
                > 240 * 1024
        );
        let result = if activate {
            supervisor.activate(action).await
        } else {
            supervisor.stop(action).await
        }
        .unwrap();
        assert_eq!(result.request, request);
        assert_eq!(
            result.state,
            ServiceRequestState::Completed(if activate {
                ServiceOutcome::Activated(installation(2))
            } else {
                ServiceOutcome::Stopped
            })
        );
        assert_eq!(fixture.supervisor().resolve(request).await.unwrap(), result);
    }
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
