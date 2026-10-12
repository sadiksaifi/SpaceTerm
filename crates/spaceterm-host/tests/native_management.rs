use native_management::request::{ManagementRequest, OperationTag, authorization_right};
use spaceterm_host::startup::BuildIdentity;
use spaceterm_host::{native_deployment::contracts::NativeFailure, native_management};

fn frame(operation: OperationTag) -> [u8; 160] {
    let mut bytes = [0u8; 160];
    bytes[..4].copy_from_slice(b"STMG");
    bytes[4] = 1;
    bytes[5] = operation as u8;
    bytes[6] = 3;
    bytes[8..12].copy_from_slice(&160u32.to_be_bytes());
    bytes[12..16].copy_from_slice(&501u32.to_be_bytes());
    bytes[16..32].fill(1);
    bytes[32..40].copy_from_slice(&7u64.to_be_bytes());
    bytes[40..48].copy_from_slice(&8u64.to_be_bytes());
    bytes[48..56].fill(0); // reserved
    bytes[56..88].fill(2);
    bytes[88..120].fill(3);
    bytes[120..152].fill(4);
    bytes
}

#[test]
fn request_roundtrip_preserves_comparison_facts_without_authority() {
    let bytes = frame(OperationTag::Stop);
    let request = ManagementRequest::decode(&bytes).unwrap();
    assert_eq!(request.encode(), bytes);
    assert_eq!(request.key().account_uid, 501);
    assert_eq!(request.operation(), OperationTag::Stop);
    assert_eq!(request.request_bytes(), &[1; 16]);
    assert_eq!(request.registration_generation(), 7);
    assert_eq!(request.lifecycle_generation(), 8);
    assert_eq!(request.account_binding(), &[2; 32]);
    assert_eq!(request.action_digest(), &[3; 32]);
    assert_eq!(request.assessment_binding(), &[4; 32]);
    assert_eq!(format!("{request:?}"), "ManagementRequest(..)");
}

#[test]
fn request_bounds_reject_truncation_trailing_and_unknown_variants() {
    let good = frame(OperationTag::Start);
    for length in 0..160 {
        assert_eq!(
            ManagementRequest::decode(&good[..length]),
            Err(NativeFailure::InvalidRecord)
        );
    }
    let mut extra = good.to_vec();
    extra.push(0);
    assert_eq!(
        ManagementRequest::decode(&extra),
        Err(NativeFailure::InvalidRecord)
    );
    for offset in [5, 6] {
        let mut bad = good;
        bad[offset] = 255;
        assert_eq!(
            ManagementRequest::decode(&bad),
            Err(NativeFailure::InvalidRecord)
        );
    }
    for offset in [7, 48, 55, 152, 159] {
        let mut bad = good;
        bad[offset] = 1;
        assert_eq!(
            ManagementRequest::decode(&bad),
            Err(NativeFailure::InvalidRecord)
        );
    }
    let mut bad = good;
    bad[4] = 2;
    assert_eq!(
        ManagementRequest::decode(&bad),
        Err(NativeFailure::UnsupportedSchema)
    );
}

#[test]
fn right_is_bound_to_exact_operation_account_and_build() {
    for (build, namespace) in [
        (
            BuildIdentity::Release,
            "io.github.sadiksaifi.spaceterm.server",
        ),
        (
            BuildIdentity::Preflight,
            "io.github.sadiksaifi.spaceterm-preflight.server",
        ),
        (
            BuildIdentity::Development,
            "io.github.sadiksaifi.spaceterm-development.server",
        ),
    ] {
        for (operation, suffix) in [
            (OperationTag::Install, "install"),
            (OperationTag::Stage, "stage"),
            (OperationTag::Activate, "activate"),
            (OperationTag::Start, "start"),
            (OperationTag::Stop, "stop"),
            (OperationTag::Restart, "restart"),
            (OperationTag::Uninstall, "uninstall"),
            (OperationTag::Repair, "repair"),
            (OperationTag::Collect, "collect"),
            (OperationTag::RemoteAccess, "remote-access"),
            (OperationTag::OwnerEnrollment, "owner-enrollment"),
        ] {
            assert_eq!(
                authorization_right(build, operation),
                format!("{namespace}.manage.{suffix}")
            );
        }
    }
}

use native_management::controller::{
    AuthorizationRoute, IntentDisposition, JournalAdmission, ManagementBackend,
    ManagementController, ManagementPhase, ManagementProgress, ProgressState, Quiescence,
};
use spaceterm_host::startup::DesiredServerState;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct TestBackend(Arc<Mutex<TestState>>);
#[derive(Clone)]
struct TestTransaction {
    request: ManagementRequest,
    phase: ManagementPhase,
}
struct TestAuthority;
struct TestState {
    authorization: Option<NativeFailure>,
    revalidation: Option<NativeFailure>,
    intent: DesiredServerState,
    uncertain: bool,
    not_applied: Option<NativeFailure>,
    invalid_commit_phase: bool,
    assessment_age: std::time::Duration,
    authorization_delay: std::time::Duration,
    quiescence_gate: Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>,
    release_serial: Arc<Mutex<()>>,
    stop_commit_attempt: Option<std::sync::mpsc::Sender<()>>,
    drained: bool,
    cleanup_unresolved: bool,
    events: Vec<&'static str>,
    transaction: Option<TestTransaction>,
    retained: Option<ManagementProgress>,
    approved: u64,
    staged: u64,
    started: Vec<u64>,
}
impl TestBackend {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(TestState {
            authorization: None,
            revalidation: None,
            intent: DesiredServerState::Running,
            uncertain: false,
            not_applied: None,
            invalid_commit_phase: false,
            assessment_age: std::time::Duration::ZERO,
            authorization_delay: std::time::Duration::ZERO,
            quiescence_gate: None,
            release_serial: Arc::new(Mutex::new(())),
            stop_commit_attempt: None,
            drained: true,
            cleanup_unresolved: false,
            events: Vec::new(),
            transaction: None,
            retained: None,
            approved: 42,
            staged: 99,
            started: Vec::new(),
        })))
    }
    fn advance(
        &self,
        transaction: &mut TestTransaction,
        phase: ManagementPhase,
        event: &'static str,
    ) {
        transaction.phase = phase;
        let mut state = self.0.lock().unwrap();
        state.events.push(event);
        state.transaction = Some(transaction.clone());
    }
    fn progress(&self, transaction: &TestTransaction, state: ProgressState) -> ManagementProgress {
        ManagementProgress {
            request: *transaction.request.request_bytes(),
            phase: transaction.phase,
            state,
        }
    }
    fn quiescence(&self, transaction: &mut TestTransaction) -> Quiescence {
        let state = self.0.lock().unwrap();
        if state.cleanup_unresolved {
            return Quiescence::CleanupUnresolved;
        }
        if !state.drained {
            return Quiescence::Pending;
        }
        drop(state);
        self.advance(
            transaction,
            ManagementPhase::Quiesced,
            "writes-settled-sessions-reaped-endpoint-absent",
        );
        Quiescence::Complete
    }
    fn finish(&self, transaction: &mut TestTransaction, event: &'static str) -> ManagementProgress {
        self.advance(transaction, ManagementPhase::Completed, event);
        let progress = self.progress(transaction, ProgressState::Complete);
        self.0.lock().unwrap().retained = Some(progress);
        progress
    }
}
use native_management::helper_lifetime::HelperDeadline;

fn controller<B: ManagementBackend>(backend: B) -> ManagementController<B> {
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    ManagementController::new(backend, guard)
}

impl ManagementBackend for TestBackend {
    type Authorization = TestAuthority;
    type Transaction = TestTransaction;
    fn authorize(
        &self,
        request: &ManagementRequest,
        _: AuthorizationRoute,
        _deadline: HelperDeadline,
    ) -> Result<TestAuthority, NativeFailure> {
        let state = self.0.lock().unwrap();
        if let Some(failure) = state.authorization {
            return Err(failure);
        }
        if request.key().account_uid != 501
            || request.key().build_identity != BuildIdentity::Development
            || request.account_binding() != &[2; 32]
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        let delay = state.authorization_delay;
        drop(state);
        std::thread::sleep(delay);
        Ok(TestAuthority)
    }
    fn lookup_existing(
        &self,
        request: &ManagementRequest,
        _: &TestAuthority,
        _deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<TestTransaction>>, NativeFailure> {
        let state = self.0.lock().unwrap();
        let Some(transaction) = state.transaction.as_ref() else {
            return Ok(None);
        };
        if transaction.request.request_bytes() != request.request_bytes() {
            return Ok(None);
        }
        if transaction.request != *request {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(Some(match state.retained {
            Some(progress) => JournalAdmission::Retained(progress),
            None => JournalAdmission::Transaction(transaction.clone()),
        }))
    }
    fn revalidate(
        &self,
        request: &ManagementRequest,
        _: &TestAuthority,
        _deadline: HelperDeadline,
    ) -> Result<std::time::Duration, NativeFailure> {
        let state = self.0.lock().unwrap();
        if let Some(failure) = state.revalidation {
            return Err(failure);
        }
        if request.registration_generation() != 7 || request.lifecycle_generation() != 8 {
            return Err(NativeFailure::GenerationMismatch);
        }
        if request.operation() == OperationTag::Restart
            && state.intent == DesiredServerState::Stopped
        {
            return Err(NativeFailure::UnsafeNativeState);
        }
        Ok(state.assessment_age)
    }
    fn admit(
        &self,
        request: &ManagementRequest,
        _: TestAuthority,
        _deadline: HelperDeadline,
    ) -> Result<JournalAdmission<TestTransaction>, NativeFailure> {
        let mut state = self.0.lock().unwrap();
        if let Some(transaction) = &state.transaction {
            if transaction.request.request_bytes() == request.request_bytes() {
                if transaction.request != *request {
                    return Err(NativeFailure::IdentityMismatch);
                }
                if let Some(progress) = state.retained {
                    return Ok(JournalAdmission::Retained(progress));
                }
                return Ok(JournalAdmission::Transaction(transaction.clone()));
            }
            if state.retained.is_none() {
                return Err(NativeFailure::OutcomePending);
            }
        }
        let transaction = TestTransaction {
            request: request.clone(),
            phase: ManagementPhase::Admitted,
        };
        state.events.push("journal-admitted");
        state.transaction = Some(transaction.clone());
        state.retained = None;
        Ok(JournalAdmission::Transaction(transaction))
    }
    fn resolve(
        &self,
        request: [u8; 16],
        _deadline: HelperDeadline,
    ) -> Result<JournalAdmission<TestTransaction>, NativeFailure> {
        let state = self.0.lock().unwrap();
        let transaction = state
            .transaction
            .as_ref()
            .ok_or(NativeFailure::RegistrationMissing)?;
        if *transaction.request.request_bytes() != request {
            return Err(NativeFailure::IdentityMismatch);
        }
        match state.retained {
            Some(progress) => Ok(JournalAdmission::Retained(progress)),
            None => Ok(JournalAdmission::Transaction(transaction.clone())),
        }
    }
    fn request<'a>(&self, transaction: &'a TestTransaction) -> &'a ManagementRequest {
        &transaction.request
    }
    fn phase(&self, transaction: &TestTransaction) -> ManagementPhase {
        transaction.phase
    }
    fn desired_state(
        &self,
        _: &TestTransaction,
        _deadline: HelperDeadline,
    ) -> Result<DesiredServerState, NativeFailure> {
        Ok(self.0.lock().unwrap().intent)
    }
    fn commit_stopped(
        &self,
        transaction: &mut TestTransaction,
        _deadline: HelperDeadline,
    ) -> IntentDisposition {
        let serial = self.0.lock().unwrap().release_serial.clone();
        if let Some(attempt) = &self.0.lock().unwrap().stop_commit_attempt {
            attempt.send(()).unwrap();
        }
        let _release_fence = serial.lock().unwrap();
        if let Some(failure) = self.0.lock().unwrap().not_applied {
            return IntentDisposition::NotApplied(failure);
        }
        if self.0.lock().unwrap().uncertain {
            let progress = self.progress(transaction, ProgressState::Indeterminate);
            self.0.lock().unwrap().retained = Some(progress);
            return IntentDisposition::Indeterminate;
        }
        self.0.lock().unwrap().intent = DesiredServerState::Stopped;
        self.advance(
            transaction,
            ManagementPhase::IntentCommitted,
            "stopped-durable-generation-advanced-release-fenced",
        );
        if self.0.lock().unwrap().invalid_commit_phase {
            transaction.phase = ManagementPhase::Completed;
        }
        IntentDisposition::Durable(())
    }
    fn commit_restart(
        &self,
        transaction: &mut TestTransaction,
        _deadline: HelperDeadline,
    ) -> IntentDisposition {
        if self.0.lock().unwrap().uncertain {
            return IntentDisposition::Indeterminate;
        }
        self.advance(
            transaction,
            ManagementPhase::IntentCommitted,
            "restart-durable-generation-advanced-release-fenced",
        );
        IntentDisposition::Durable(())
    }
    fn suspend_automatic_start(
        &self,
        transaction: &mut TestTransaction,
        _deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.advance(
            transaction,
            ManagementPhase::StartupSuspended,
            "suspend-restart-without-termination",
        );
        Ok(())
    }
    fn begin_quiescence(
        &self,
        transaction: &mut TestTransaction,
        _deadline: HelperDeadline,
    ) -> Result<Quiescence, NativeFailure> {
        self.advance(
            transaction,
            ManagementPhase::Quiescing,
            "close-admissions-cancel-reads-drain-started-writes",
        );
        let gate = self.0.lock().unwrap().quiescence_gate.clone();
        if let Some((entered, release)) = gate {
            entered.wait();
            release.wait();
        }
        Ok(self.quiescence(transaction))
    }
    fn poll_quiescence(
        &self,
        transaction: &mut TestTransaction,
        _deadline: HelperDeadline,
    ) -> Result<Quiescence, NativeFailure> {
        Ok(self.quiescence(transaction))
    }
    fn retire_supervision(
        &self,
        transaction: &mut TestTransaction,
        _deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        assert_eq!(transaction.phase, ManagementPhase::Quiesced);
        self.advance(transaction, ManagementPhase::SupervisionRemoved, "bootout");
        Ok(())
    }
    fn finish_stop(
        &self,
        transaction: &mut TestTransaction,
        _deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.finish(transaction, "completed-durable");
        Ok(())
    }
    fn start_approved(
        &self,
        transaction: &mut TestTransaction,
        _deadline: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure> {
        let mut state = self.0.lock().unwrap();
        let installation = state.approved;
        assert_ne!(installation, state.staged);
        state.started.push(installation);
        state.intent = DesiredServerState::Running;
        drop(state);
        Ok(self.finish(transaction, "start-approved"))
    }
    fn restart_approved(
        &self,
        transaction: &mut TestTransaction,
        _deadline: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure> {
        assert!(matches!(
            transaction.phase,
            ManagementPhase::Quiesced | ManagementPhase::SupervisionRemoved
        ));
        self.start_approved(transaction, _deadline)
    }
    fn apply_owned_operation(
        &self,
        _: &mut TestTransaction,
        _deadline: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
}
fn request(operation: OperationTag) -> ManagementRequest {
    ManagementRequest::decode(&frame(operation)).unwrap()
}

#[test]
fn authorization_denied_cancelled_or_unavailable_has_no_effect() {
    for failure in [
        NativeFailure::AuthorizationDenied,
        NativeFailure::AuthorizationCancelled,
        NativeFailure::AuthorizationUnavailable,
    ] {
        let backend = TestBackend::new();
        backend.0.lock().unwrap().authorization = Some(failure);
        let controller = controller(backend.clone());
        assert_eq!(
            controller.execute(&request(OperationTag::Stop), AuthorizationRoute::Gui),
            Err(failure)
        );
        assert!(backend.0.lock().unwrap().events.is_empty());
    }
}
#[test]
fn stop_commits_stopped_before_supervision_or_runtime_effects() {
    let backend = TestBackend::new();
    let controller = controller(backend.clone());
    assert_eq!(
        controller
            .execute(
                &request(OperationTag::Stop),
                AuthorizationRoute::CliInteractive
            )
            .unwrap()
            .state,
        ProgressState::Complete
    );
    assert_eq!(
        backend.0.lock().unwrap().events,
        [
            "journal-admitted",
            "stopped-durable-generation-advanced-release-fenced",
            "suspend-restart-without-termination",
            "close-admissions-cancel-reads-drain-started-writes",
            "writes-settled-sessions-reaped-endpoint-absent",
            "bootout",
            "completed-durable"
        ]
    );
}
#[test]
fn stop_drains_started_git_write_before_bootout() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().drained = false;
    let controller = controller(backend.clone());
    let request = request(OperationTag::Stop);
    let pending = controller
        .execute(&request, AuthorizationRoute::CliNonInteractive)
        .unwrap();
    assert_eq!(pending.state, ProgressState::Pending);
    assert!(!backend.0.lock().unwrap().events.contains(&"bootout"));
    backend.0.lock().unwrap().drained = true;
    assert_eq!(
        controller.resolve(pending.request).unwrap().state,
        ProgressState::Complete
    );
    assert_eq!(
        backend
            .0
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|event| **event == "journal-admitted")
            .count(),
        1
    );
}
#[test]
fn uncertain_stop_is_not_completed() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().uncertain = true;
    let controller = controller(backend.clone());
    let progress = controller
        .execute(&request(OperationTag::Stop), AuthorizationRoute::Gui)
        .unwrap();
    assert_eq!(progress.state, ProgressState::Indeterminate);
    assert_eq!(
        controller.resolve(progress.request).unwrap().state,
        ProgressState::Indeterminate
    );
    assert_eq!(backend.0.lock().unwrap().events, ["journal-admitted"]);
}
#[test]
fn concurrent_start_stop_and_activate_use_one_native_journal() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().drained = false;
    let controller = controller(backend.clone());
    controller
        .execute(&request(OperationTag::Stop), AuthorizationRoute::Gui)
        .unwrap();
    for operation in [OperationTag::Start, OperationTag::Activate] {
        let mut bytes = frame(operation);
        bytes[16] = operation as u8;
        assert_eq!(
            controller.execute(
                &ManagementRequest::decode(&bytes).unwrap(),
                AuthorizationRoute::Gui
            ),
            Err(NativeFailure::OutcomePending)
        );
    }
    assert_eq!(
        backend
            .0
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|event| **event == "journal-admitted")
            .count(),
        1
    );
}
#[test]
fn restart_rejects_stopped_and_never_selects_staged() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().intent = DesiredServerState::Stopped;
    let controller = controller(backend.clone());
    assert_eq!(
        controller.execute(&request(OperationTag::Restart), AuthorizationRoute::Gui),
        Err(NativeFailure::UnsafeNativeState)
    );
    assert!(backend.0.lock().unwrap().events.is_empty());
    backend.0.lock().unwrap().intent = DesiredServerState::Running;
    controller
        .execute(&request(OperationTag::Restart), AuthorizationRoute::Gui)
        .unwrap();
    let state = backend.0.lock().unwrap();
    assert_eq!(state.intent, DesiredServerState::Running);
    assert_eq!(state.started, [42]);
    assert!(!state.events.contains(&"bootout"));
    assert!(
        !state
            .events
            .contains(&"stopped-durable-generation-advanced-release-fenced")
    );
}
#[test]
fn cancel_after_admission_retains_original_request_identity() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().drained = false;
    let controller = controller(backend.clone());
    let request = request(OperationTag::Stop);
    let pending = controller
        .execute(&request, AuthorizationRoute::Gui)
        .unwrap();
    backend.0.lock().unwrap().drained = true;
    assert_eq!(
        controller
            .resolve(*request.request_bytes())
            .unwrap()
            .request,
        pending.request
    );
    let effects = backend.0.lock().unwrap().events.len();
    assert_eq!(
        controller.resolve(pending.request).unwrap().state,
        ProgressState::Complete
    );
    assert_eq!(backend.0.lock().unwrap().events.len(), effects);
}
#[test]
fn authorization_delay_does_not_extend_assessment() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().revalidation = Some(NativeFailure::StaleAssessment);
    let controller = controller(backend.clone());
    assert_eq!(
        controller.execute(&request(OperationTag::Stop), AuthorizationRoute::Gui),
        Err(NativeFailure::StaleAssessment)
    );
    assert!(backend.0.lock().unwrap().events.is_empty());
}
#[test]
fn unresolved_old_instance_cleanup_prevents_bootout_and_success() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().cleanup_unresolved = true;
    let controller = controller(backend.clone());
    assert_eq!(
        controller
            .execute(&request(OperationTag::Stop), AuthorizationRoute::Gui)
            .unwrap()
            .state,
        ProgressState::CleanupUnresolved
    );
    assert!(!backend.0.lock().unwrap().events.contains(&"bootout"));
}

#[test]
fn stop_not_applied_has_no_supervisor_or_runtime_effects() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().not_applied = Some(NativeFailure::IoUnavailable);
    let controller = controller(backend.clone());
    assert_eq!(
        controller.execute(&request(OperationTag::Stop), AuthorizationRoute::Gui),
        Err(NativeFailure::IoUnavailable)
    );
    assert_eq!(backend.0.lock().unwrap().events, ["journal-admitted"]);
}
#[test]
fn assessment_expiry_is_measured_from_original_creation() {
    for age in [30, 31, 100] {
        let backend = TestBackend::new();
        backend.0.lock().unwrap().assessment_age = std::time::Duration::from_secs(age);
        let controller = controller(backend.clone());
        assert_eq!(
            controller.execute(&request(OperationTag::Stop), AuthorizationRoute::Gui),
            Err(NativeFailure::StaleAssessment)
        );
        assert!(backend.0.lock().unwrap().events.is_empty());
    }
}
#[test]
fn stalled_management_backend_is_retired_independently() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "stalled_management_backend_child", "--nocapture"])
        .env("SPACETERM_MANAGEMENT_TEST_CHILD", "1")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(NativeFailure::TimedOut as i32));
            assert!(started.elapsed() < Duration::from_secs(4));
            break;
        }
        if started.elapsed() >= Duration::from_secs(4) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("stalled helper did not retire within the independent lifetime bound");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn stalled_management_backend_child() {
    if std::env::var_os("SPACETERM_MANAGEMENT_TEST_CHILD").is_none() {
        return;
    }
    let backend = TestBackend::new();
    backend.0.lock().unwrap().authorization_delay = std::time::Duration::from_secs(60);
    let lifetime = native_management::helper_lifetime::HelperLifetimeGuard::install_for(
        std::time::Duration::from_secs(2),
    )
    .unwrap();
    let controller = ManagementController::new(backend, lifetime);
    let _ = controller.execute(&request(OperationTag::Stop), AuthorizationRoute::Gui);
    panic!("stalled native call returned rather than helper retirement");
}
#[test]
fn wrong_account_build_and_incarnation_fail_before_journal_admission() {
    for (offset, value) in [(15, 255), (6, 1), (56, 3)] {
        let mut bytes = frame(OperationTag::Stop);
        bytes[offset] = value;
        let backend = TestBackend::new();
        let controller = controller(backend.clone());
        assert_eq!(
            controller.execute(
                &ManagementRequest::decode(&bytes).unwrap(),
                AuthorizationRoute::Gui
            ),
            Err(NativeFailure::IdentityMismatch)
        );
        assert!(backend.0.lock().unwrap().events.is_empty());
    }
}
#[test]
fn request_rejects_zero_identity_generation_and_binding_fields() {
    for range in [12..16, 16..32, 32..40, 40..48, 56..88, 88..120, 120..152] {
        let mut bytes = frame(OperationTag::Start);
        bytes[range].fill(0);
        assert_eq!(
            ManagementRequest::decode(&bytes),
            Err(NativeFailure::InvalidRecord)
        );
    }
    let mut bytes = frame(OperationTag::Start);
    bytes[12..16].fill(255);
    assert_eq!(
        ManagementRequest::decode(&bytes),
        Err(NativeFailure::InvalidRecord)
    );
}

use native_management::SudoMode;
use native_management::cli_sudo::{
    SudoAdapter, SudoBoundary, SudoInvocation, UnavailableSudoBoundary,
};
#[derive(Clone)]
struct TestSudo(Arc<Mutex<SudoState>>);
#[derive(Default)]
struct SudoState {
    replaced: bool,
    invoked: usize,
    observed: Option<SudoInvocation>,
}
struct RetainedTestHelper;
impl SudoBoundary for TestSudo {
    type RetainedHelper = RetainedTestHelper;
    fn retain_fixed_helper(
        &self,
        _: &ManagementRequest,
    ) -> Result<RetainedTestHelper, NativeFailure> {
        Ok(RetainedTestHelper)
    }
    fn recheck_execution_binding(&self, _: &mut RetainedTestHelper) -> Result<(), NativeFailure> {
        if self.0.lock().unwrap().replaced {
            return Err(NativeFailure::ExecutionBindingUnproven);
        }
        Ok(())
    }
    fn invoke(
        &self,
        _: RetainedTestHelper,
        invocation: SudoInvocation,
        bytes: &[u8; 160],
    ) -> Result<ManagementProgress, NativeFailure> {
        let request = ManagementRequest::decode(bytes)?;
        let mut state = self.0.lock().unwrap();
        state.invoked += 1;
        state.observed = Some(invocation);
        Ok(ManagementProgress {
            request: *request.request_bytes(),
            phase: ManagementPhase::Admitted,
            state: ProgressState::Pending,
        })
    }
}
#[test]
fn sudo_executes_only_fixed_admitted_helper() {
    let boundary = TestSudo(Arc::default());
    let adapter = SudoAdapter::new(boundary.clone());
    adapter
        .execute(&request(OperationTag::Start), SudoMode::Interactive)
        .unwrap();
    let invocation = boundary.0.lock().unwrap().observed.unwrap();
    assert_eq!(invocation.executable(), "/usr/bin/sudo");
    assert_eq!(invocation.prefix(), ["-k", "--"]);
    assert_eq!(invocation.manager_arguments(), ["--request-stdin"]);
}
#[test]
fn sudo_noninteractive_uses_n_and_never_s_or_e() {
    let boundary = TestSudo(Arc::default());
    let adapter = SudoAdapter::new(boundary.clone());
    adapter
        .execute(&request(OperationTag::Start), SudoMode::NonInteractive)
        .unwrap();
    assert_eq!(
        boundary.0.lock().unwrap().observed.unwrap().prefix(),
        ["-k", "-n", "--"]
    );
}
#[test]
fn helper_replacement_before_exec_is_rejected() {
    let boundary = TestSudo(Arc::default());
    boundary.0.lock().unwrap().replaced = true;
    let adapter = SudoAdapter::new(boundary.clone());
    assert_eq!(
        adapter.execute(&request(OperationTag::Start), SudoMode::Interactive),
        Err(NativeFailure::ExecutionBindingUnproven)
    );
    assert_eq!(boundary.0.lock().unwrap().invoked, 0);
}
#[test]
fn sudo_uid_and_request_uid_are_not_native_caller_proof() {
    let adapter = SudoAdapter::new(UnavailableSudoBoundary);
    for mode in [SudoMode::Interactive, SudoMode::NonInteractive] {
        assert_eq!(
            adapter.execute(&request(OperationTag::Start), mode),
            Err(NativeFailure::AuthorizationUnavailable)
        );
    }
}
#[test]
fn background_items_suppression_is_not_missing_installation() {
    use native_management::macos::BackgroundItemsStatus;
    assert_eq!(
        BackgroundItemsStatus::from_native(2).supervision(),
        Err(NativeFailure::BackgroundItemsSuppressed)
    );
    assert_eq!(
        BackgroundItemsStatus::from_native(0).supervision(),
        Err(NativeFailure::SupervisorUnavailable)
    );
    assert_eq!(
        BackgroundItemsStatus::from_native(3).supervision(),
        Err(NativeFailure::SupervisorUnavailable)
    );
    assert_eq!(
        BackgroundItemsStatus::from_native(99).supervision(),
        Err(NativeFailure::NativeUnavailable)
    );
    assert_eq!(BackgroundItemsStatus::from_native(1).supervision(), Ok(()));
}
#[test]
fn pkexec_uses_exact_operation_action_without_gui_environment_fallback() {
    use native_management::linux::PkexecInvocation;
    let request = request(OperationTag::Uninstall);
    let invocation = PkexecInvocation::for_request(&request);
    assert_eq!(invocation.executable(), "/usr/bin/pkexec");
    assert_eq!(
        invocation.manager_arguments().as_slice(),
        ["uninstall", "--request-stdin"]
    );
    assert_eq!(
        invocation.action(BuildIdentity::Development),
        "io.github.sadiksaifi.spaceterm-development.server.manage.uninstall"
    );
}

#[test]
fn management_helper_exits_after_bounded_request() {
    let started = std::time::Instant::now();
    for (arguments, code) in [
        (vec!["--request-stdin"], 3),
        (vec!["--gui-demand"], 3),
        (vec!["stop", "--request-stdin"], 3),
        (vec!["--force"], 9),
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_spaceterm-server-manager"))
            .args(arguments)
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert_eq!(output.stdout, format!("classification={code}\n").as_bytes());
        assert!(output.stderr.is_empty());
    }
    assert!(started.elapsed() < native_management::controller::HELPER_LIFETIME);
}

#[test]
fn concurrent_callers_cannot_mutate_during_admitted_stop() {
    let backend = TestBackend::new();
    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    backend.0.lock().unwrap().quiescence_gate = Some((entered.clone(), release.clone()));
    let controller = Arc::new(controller(backend.clone()));
    let worker = {
        let controller = controller.clone();
        std::thread::spawn(move || {
            controller.execute(&request(OperationTag::Stop), AuthorizationRoute::Gui)
        })
    };
    entered.wait();
    let start = controller.execute(&request(OperationTag::Start), AuthorizationRoute::Gui);
    let activate = controller.execute(&request(OperationTag::Activate), AuthorizationRoute::Gui);
    release.wait();
    assert_eq!(start, Err(NativeFailure::OutcomePending));
    assert_eq!(activate, Err(NativeFailure::OutcomePending));
    assert_eq!(
        worker.join().unwrap().unwrap().state,
        ProgressState::Complete
    );
    assert_eq!(
        backend
            .0
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|event| **event == "journal-admitted")
            .count(),
        1
    );
}

#[test]
fn stop_cannot_skip_drain_and_bootout_from_an_invalid_phase_advance() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().invalid_commit_phase = true;
    let controller = controller(backend.clone());
    assert_eq!(
        controller.execute(&request(OperationTag::Stop), AuthorizationRoute::Gui),
        Err(NativeFailure::UnsafeNativeState)
    );
    assert!(!backend.0.lock().unwrap().events.contains(&"bootout"));
}

#[test]
fn repeated_admitted_request_returns_retained_outcome_after_assessment_expiry() {
    let backend = TestBackend::new();
    let controller = controller(backend.clone());
    let original = request(OperationTag::Stop);
    let outcome = controller
        .execute(&original, AuthorizationRoute::Gui)
        .unwrap();
    let effects = backend.0.lock().unwrap().events.len();
    backend.0.lock().unwrap().assessment_age = std::time::Duration::from_secs(31);
    assert_eq!(
        controller.execute(&original, AuthorizationRoute::Gui),
        Ok(outcome)
    );
    assert_eq!(backend.0.lock().unwrap().events.len(), effects);
    let mut reused = frame(OperationTag::Stop);
    reused[88] = 9;
    assert_eq!(
        controller.execute(
            &ManagementRequest::decode(&reused).unwrap(),
            AuthorizationRoute::Gui
        ),
        Err(NativeFailure::IdentityMismatch)
    );
}

#[test]
fn request_stdin_survives_broker_closefrom_and_is_cloexec() {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let good = frame(OperationTag::Stop);
    let mut oversized = good.to_vec();
    oversized.push(0);
    for (bytes, expected) in [
        (good.to_vec(), 0),
        (good[..159].to_vec(), 9),
        (oversized, 9),
    ] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "request_stdin_broker_child", "--nocapture"])
            .env("SPACETERM_MANAGEMENT_STDIN_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: the test-only broker simulation closes nonstandard descriptors
        // using only async-signal-safe native calls. No production broker is invoked.
        unsafe {
            command.pre_exec(|| {
                for fd in 3..1024 {
                    libc::close(fd);
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(&bytes).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.code() == Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if expected == 0 {
            assert!(
                output
                    .stdout
                    .windows(19)
                    .any(|value| value == b"owned-stdin-passed\n")
            );
        }
    }
}
#[test]
fn request_stdin_broker_child() {
    if std::env::var_os("SPACETERM_MANAGEMENT_STDIN_CHILD").is_none() {
        return;
    }
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    let pipe = native_management::stdin::RequestStdin::claim().unwrap();
    // SAFETY: this isolated test process owns its descriptors. Only inspection
    // occurs here; no caller identity is inferred from their numbers or types.
    assert_eq!(unsafe { libc::fcntl(0, libc::F_GETFD) }, -1);
    let mut pipes = 0;
    for fd in 3..256 {
        // SAFETY: fstat initializes the stack object only on successful inspection.
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, status.as_mut_ptr()) } != 0 {
            continue;
        }
        // SAFETY: the successful call initialized status.
        if unsafe { status.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFIFO {
            pipes += 1;
            assert_ne!(
                unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
        }
    }
    assert_eq!(pipes, 1);
    let request = match pipe.read(guard.deadline()) {
        Ok(request) => request,
        Err(failure) => std::process::exit(failure as i32),
    };
    assert_eq!(request.encode(), frame(OperationTag::Stop));
    println!("owned-stdin-passed");
}

#[test]
fn broker_socket_stdin_preserves_native_caller_transport_through_closefrom() {
    use std::{
        io::{Read, Write},
        net::Shutdown,
        os::unix::process::CommandExt,
        process::{Command, Stdio},
    };
    let (mut writer, reader) = native_management::stdin::broker_request_channel().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "broker_socket_stdin_child", "--nocapture"])
        .env("SPACETERM_MANAGEMENT_SOCKET_CHILD", "1")
        .stdin(Stdio::from(reader))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: this isolated test child performs only async-signal-safe descriptor closure.
    unsafe {
        command.pre_exec(|| {
            for fd in 3..1024 {
                libc::close(fd);
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let mut bytes = frame(OperationTag::Start);
    // SAFETY: getuid has no arguments or memory effects.
    bytes[12..16].copy_from_slice(&unsafe { libc::getuid() }.to_be_bytes());
    writer
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    writer.write_all(&bytes).unwrap();
    let mut challenge = [0; 32];
    writer.read_exact(&mut challenge).unwrap();
    writer.write_all(&challenge).unwrap();
    writer.shutdown(Shutdown::Write).unwrap();
    assert!(
        child.wait().unwrap().success(),
        "authenticated stdin route refused"
    );
}

#[test]
fn broker_socket_stdin_child() {
    if std::env::var_os("SPACETERM_MANAGEMENT_SOCKET_CHILD").is_none() {
        return;
    }
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    let request = native_management::stdin::RequestStdin::claim()
        .and_then(|stdin| stdin.read_authenticated(guard.deadline()));
    match request {
        Ok((request, caller)) => {
            use native_management::facade::caller::NativeCallerAuthentication;
            assert_eq!(request.operation(), OperationTag::Start);
            assert_eq!(
                caller.uid(caller.caller()).unwrap(),
                request.key().account_uid
            );
            caller.recheck(caller.caller(), guard.deadline()).unwrap();
            if std::env::var_os("SPACETERM_RECHECK_RETIRED_WRITER").is_some() {
                use std::{
                    io::{Read, Write},
                    os::fd::FromRawFd,
                    os::unix::net::UnixStream,
                };
                // SAFETY: only this isolated test process owns its fixed control stream at stderr.
                let mut control = unsafe { UnixStream::from_raw_fd(2) };
                control.write_all(&[1]).unwrap();
                control.read_exact(&mut [0; 1]).unwrap();
                assert_eq!(
                    caller.recheck(caller.caller(), guard.deadline()),
                    Err(NativeFailure::ProcessIdentityMismatch)
                );
            }
            if let Some(pid) = std::env::var_os("SPACETERM_EXPECTED_NATIVE_WRITER") {
                assert_eq!(
                    caller.observed_pid(),
                    pid.to_str().unwrap().parse::<u32>().unwrap()
                );
            }
        }
        Err(failure) => std::process::exit(failure as i32),
    }
}

#[test]
fn broker_principal_is_the_actual_writer_not_the_anonymous_socket_creator() {
    use std::{
        io::Write,
        os::fd::OwnedFd,
        os::unix::net::UnixStream,
        process::{Command, Stdio},
    };
    let (writer, reader) = native_management::stdin::broker_request_channel().unwrap();
    let (mut release, held) = UnixStream::pair().unwrap();
    let mut writer_child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "broker_actual_writer_child", "--nocapture"])
        .env("SPACETERM_MANAGEMENT_WRITER_CHILD", "1")
        .stdin(Stdio::from(OwnedFd::from(writer)))
        .stdout(Stdio::null())
        .stderr(Stdio::from(OwnedFd::from(held)))
        .spawn()
        .unwrap();
    let mut receiver = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "broker_socket_stdin_child", "--nocapture"])
        .env("SPACETERM_MANAGEMENT_SOCKET_CHILD", "1")
        .env(
            "SPACETERM_EXPECTED_NATIVE_WRITER",
            writer_child.id().to_string(),
        )
        .stdin(Stdio::from(reader))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let observed = receiver.wait().unwrap();
    release.write_all(&[1]).unwrap();
    assert!(writer_child.wait().unwrap().success());
    assert!(
        observed.success(),
        "native caller must bind the writer, not its socket creator"
    );
}
#[test]
fn broker_principal_rechecks_original_writer_after_exact_reaping() {
    use std::{
        io::{Read, Write},
        os::fd::OwnedFd,
        os::unix::net::UnixStream,
        process::{Command, Stdio},
    };
    let (writer, reader) = native_management::stdin::broker_request_channel().unwrap();
    let (mut release, held) = UnixStream::pair().unwrap();
    let (mut observation, observed) = UnixStream::pair().unwrap();
    let mut writer_child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "broker_actual_writer_child", "--nocapture"])
        .env("SPACETERM_MANAGEMENT_WRITER_CHILD", "1")
        .stdin(Stdio::from(OwnedFd::from(writer)))
        .stdout(Stdio::null())
        .stderr(Stdio::from(OwnedFd::from(held)))
        .spawn()
        .unwrap();
    let mut receiver = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "broker_socket_stdin_child", "--nocapture"])
        .env("SPACETERM_MANAGEMENT_SOCKET_CHILD", "1")
        .env("SPACETERM_RECHECK_RETIRED_WRITER", "1")
        .stdin(Stdio::from(reader))
        .stdout(Stdio::null())
        .stderr(Stdio::from(OwnedFd::from(observed)))
        .spawn()
        .unwrap();
    observation
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    observation.read_exact(&mut [0; 1]).unwrap();
    release.write_all(&[1]).unwrap();
    assert!(writer_child.wait().unwrap().success());
    observation.write_all(&[1]).unwrap();
    assert!(
        receiver.wait().unwrap().success(),
        "retired native caller remained valid"
    );
}
#[test]
fn broker_actual_writer_child() {
    if std::env::var_os("SPACETERM_MANAGEMENT_WRITER_CHILD").is_none() {
        return;
    }
    use std::{
        io::{Read, Write},
        net::Shutdown,
        os::fd::FromRawFd,
        os::unix::net::UnixStream,
    };
    // SAFETY: this isolated test process owns these two inherited local stream descriptors.
    let (mut channel, mut release) =
        unsafe { (UnixStream::from_raw_fd(0), UnixStream::from_raw_fd(2)) };
    channel
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    let mut bytes = frame(OperationTag::Start);
    // SAFETY: getuid reads a scalar native UID and takes no pointer.
    bytes[12..16].copy_from_slice(&unsafe { libc::getuid() }.to_be_bytes());
    channel.write_all(&bytes).unwrap();
    let mut challenge = [0; 32];
    channel.read_exact(&mut challenge).unwrap();
    channel.write_all(&challenge).unwrap();
    channel.shutdown(Shutdown::Write).unwrap();
    release.read_exact(&mut [0; 1]).unwrap();
}
#[test]
fn broker_principal_refuses_uid_substitution_wrong_challenge_and_trailing_data() {
    use std::{
        io::{Read, Write},
        net::Shutdown,
        process::{Command, Stdio},
    };
    for (case, expected) in [
        (1, NativeFailure::IdentityMismatch),
        (2, NativeFailure::AuthorizationDenied),
        (3, NativeFailure::InvalidRecord),
    ] {
        let (mut writer, reader) = native_management::stdin::broker_request_channel().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "broker_socket_stdin_child", "--nocapture"])
            .env("SPACETERM_MANAGEMENT_SOCKET_CHILD", "1")
            .stdin(Stdio::from(reader))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        writer
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut bytes = frame(OperationTag::Start);
        // SAFETY: the actual test account UID is observed by the OS, not supplied as production authority.
        let uid = unsafe { libc::getuid() };
        bytes[12..16].copy_from_slice(&(if case == 1 { uid + 1 } else { uid }).to_be_bytes());
        writer.write_all(&bytes).unwrap();
        if case != 1 {
            let mut challenge = [0; 32];
            writer.read_exact(&mut challenge).unwrap();
            if case == 2 {
                challenge[0] ^= 1;
            }
            let mut echo = challenge.to_vec();
            if case == 3 {
                echo.push(1);
            }
            writer.write_all(&echo).unwrap();
            writer.shutdown(Shutdown::Write).unwrap();
        }
        assert_eq!(child.wait().unwrap().code(), Some(expected as i32));
    }
}

#[path = "native_management_composition/authorization.rs"]
mod authorization_composition;

#[path = "native_management_composition/facade.rs"]
mod facade_composition;

#[test]
fn production_manager_refuses_before_parsing_request_or_operation_substitution() {
    use std::io::Write;
    use std::process::Stdio;
    for (arguments, expected) in [
        (vec!["--request-stdin"], 3),
        (vec!["start", "--request-stdin"], 3),
        (vec!["stop", "--request-stdin"], 3),
    ] {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_spaceterm-server-manager"))
            .args(arguments)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&frame(OperationTag::Start))
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(expected));
        assert_eq!(
            output.stdout,
            format!("classification={expected}\n").as_bytes()
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn production_native_broker_route_refuses_before_request_or_challenge_without_evidence() {
    use std::process::{Command, Stdio};
    let (_writer, reader) = native_management::stdin::broker_request_channel().unwrap();
    // Retain a silent sender. The closed source gate must not wait for a request or challenge.
    let child = Command::new(env!("CARGO_BIN_EXE_spaceterm-server-manager"))
        .arg("--request-stdin")
        .env_clear()
        .stdin(Stdio::from(reader))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(NativeFailure::CustodyUnavailable as i32)
    );
    assert_eq!(output.stdout, b"classification=3\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn manager_entry_source_gate_refuses_before_waiting_for_request_eof() {
    use std::process::Stdio;
    let started = std::time::Instant::now();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_spaceterm-server-manager"))
        .arg("--request-stdin")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let writer = child.stdin.take().unwrap();
    // Retain the writer without delivering EOF or a request. The closed source gate
    // must refuse without entering request reception or native authorization.
    let output = child.wait_with_output().unwrap();
    drop(writer);
    assert_eq!(
        output.status.code(),
        Some(NativeFailure::CustodyUnavailable as i32)
    );
    assert_eq!(output.stdout, b"classification=3\n");
    assert!(output.stderr.is_empty());
    assert!(started.elapsed() < std::time::Duration::from_secs(6));
}

#[path = "native_management_composition/execution.rs"]
mod native_execution_composition;

#[test]
fn manager_work_is_not_cut_off_at_two_seconds() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "manager_work_past_two_seconds_child",
            "--nocapture",
        ])
        .env("SPACETERM_MANAGEMENT_LONG_WORK_CHILD", "1")
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "valid manager work was retired after two seconds"
    );
}

#[test]
fn manager_work_past_two_seconds_child() {
    if std::env::var_os("SPACETERM_MANAGEMENT_LONG_WORK_CHILD").is_none() {
        return;
    }
    let backend = TestBackend::new();
    backend.0.lock().unwrap().authorization_delay = std::time::Duration::from_millis(2100);
    let result =
        controller(backend).execute(&request(OperationTag::Start), AuthorizationRoute::Gui);
    assert_eq!(result.unwrap().state, ProgressState::Complete);
}

#[path = "native_management_composition/ssh_authorization.rs"]
mod ssh_authorization_composition;

#[test]
fn restart_fences_release_and_automatic_start_before_draining_the_previous_instance() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().drained = false;
    let controller = controller(backend.clone());
    let progress = controller
        .execute(&request(OperationTag::Restart), AuthorizationRoute::Gui)
        .unwrap();
    assert_eq!(progress.state, ProgressState::Pending);
    assert_eq!(
        backend.0.lock().unwrap().intent,
        DesiredServerState::Running
    );
    assert_eq!(
        backend.0.lock().unwrap().events,
        [
            "journal-admitted",
            "restart-durable-generation-advanced-release-fenced",
            "suspend-restart-without-termination",
            "close-admissions-cancel-reads-drain-started-writes",
        ]
    );
    assert!(backend.0.lock().unwrap().started.is_empty());
    backend.0.lock().unwrap().drained = true;
    assert_eq!(
        controller.resolve(progress.request).unwrap().state,
        ProgressState::Complete
    );
    assert_eq!(backend.0.lock().unwrap().started, [42]);
    assert_eq!(
        backend
            .0
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|event| **event == "journal-admitted")
            .count(),
        1
    );
}

#[path = "native_management_composition/supervision.rs"]
mod supervision;

#[test]
fn stop_committed_before_native_release_has_zero_retrieval_and_delivery_effects() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().drained = false;
    let controller = controller(backend.clone());
    assert_eq!(
        controller
            .execute(&request(OperationTag::Stop), AuthorizationRoute::Gui)
            .unwrap()
            .state,
        ProgressState::Pending
    );
    // The injected native release port checks intent while holding the same owner lock.
    let serial = backend.0.lock().unwrap().release_serial.clone();
    let _release = serial.lock().unwrap();
    let mut state = backend.0.lock().unwrap();
    if state.intent == DesiredServerState::Running {
        state.events.extend(["native-retrieval", "native-delivery"]);
    }
    assert!(!state.events.contains(&"native-retrieval"));
    assert!(!state.events.contains(&"native-delivery"));
    assert!(!state.events.contains(&"bootout"));
}

#[test]
fn native_release_wins_stop_waits_for_delivery_then_exact_retriever_retirement() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().cleanup_unresolved = true;
    let serial = backend.0.lock().unwrap().release_serial.clone();
    let release_entered = Arc::new(std::sync::Barrier::new(2));
    let release_finish = Arc::new(std::sync::Barrier::new(2));
    let entered = release_entered.clone();
    let finish = release_finish.clone();
    let release_backend = backend.clone();
    let releasing = std::thread::spawn(move || {
        let _lease = serial.lock().unwrap();
        assert_eq!(
            release_backend.0.lock().unwrap().intent,
            DesiredServerState::Running
        );
        release_backend
            .0
            .lock()
            .unwrap()
            .events
            .push("native-retrieval");
        entered.wait();
        finish.wait();
        release_backend
            .0
            .lock()
            .unwrap()
            .events
            .push("native-delivery");
    });
    release_entered.wait();
    let (attempt, attempted) = std::sync::mpsc::channel();
    backend.0.lock().unwrap().stop_commit_attempt = Some(attempt);
    let controller = Arc::new(controller(backend.clone()));
    let stopping = controller.clone();
    let stopping = std::thread::spawn(move || {
        stopping.execute(&request(OperationTag::Stop), AuthorizationRoute::Gui)
    });
    attempted
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        backend.0.lock().unwrap().intent,
        DesiredServerState::Running
    );
    assert!(!backend.0.lock().unwrap().events.contains(&"bootout"));
    release_finish.wait();
    releasing.join().unwrap();
    let pending = stopping.join().unwrap().unwrap();
    assert_eq!(pending.state, ProgressState::CleanupUnresolved);
    let events = backend.0.lock().unwrap().events.clone();
    assert!(
        events
            .iter()
            .position(|event| *event == "native-delivery")
            .unwrap()
            < events
                .iter()
                .position(|event| *event == "stopped-durable-generation-advanced-release-fenced")
                .unwrap()
    );
    assert!(!events.contains(&"bootout"));
    backend.0.lock().unwrap().cleanup_unresolved = false;
    assert_eq!(
        controller.resolve(pending.request).unwrap().state,
        ProgressState::Complete
    );
}

#[test]
fn uncertain_restart_or_unresolved_prior_retirement_never_restarts_or_publishes_stopped() {
    let backend = TestBackend::new();
    backend.0.lock().unwrap().uncertain = true;
    let controller = controller(backend.clone());
    assert_eq!(
        controller
            .execute(&request(OperationTag::Restart), AuthorizationRoute::Gui)
            .unwrap()
            .state,
        ProgressState::Indeterminate
    );
    assert_eq!(backend.0.lock().unwrap().events, ["journal-admitted"]);
    assert_eq!(
        backend.0.lock().unwrap().intent,
        DesiredServerState::Running
    );
    backend.0.lock().unwrap().uncertain = false;
    backend.0.lock().unwrap().cleanup_unresolved = true;
    assert_eq!(
        controller.resolve([1; 16]).unwrap().state,
        ProgressState::CleanupUnresolved
    );
    assert!(backend.0.lock().unwrap().started.is_empty());
    assert!(!backend.0.lock().unwrap().events.contains(&"bootout"));
    backend.0.lock().unwrap().cleanup_unresolved = false;
    assert_eq!(
        controller.resolve([1; 16]).unwrap().state,
        ProgressState::Complete
    );
    assert_eq!(backend.0.lock().unwrap().started, [42]);
}

#[test]
fn restart_controller_resolves_replacement_startup_after_journaled_supervision_removal() {
    let backend = TestBackend::new();
    let request = request(OperationTag::Restart);
    backend.0.lock().unwrap().transaction = Some(TestTransaction {
        request: request.clone(),
        phase: ManagementPhase::SupervisionRemoved,
    });
    let result = controller(backend.clone())
        .resolve(*request.request_bytes())
        .unwrap();
    assert_eq!(result.state, ProgressState::Complete);
    let state = backend.0.lock().unwrap();
    assert_eq!(state.intent, DesiredServerState::Running);
    assert_eq!(state.started, [42]);
    assert_eq!(state.events, ["start-approved"]);
}
