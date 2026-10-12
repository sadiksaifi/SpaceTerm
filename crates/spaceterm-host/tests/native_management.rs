// The dispatch did not include ND520/1's common seed. This test-only fixture
// supplies its failure ABI, without adding a production authority or enum.
#[allow(dead_code)]
mod native_deployment {
    pub mod contracts {
        pub enum PublicationDisposition<T> {
            Durable(T),
            NotApplied(NativeFailure),
            Indeterminate,
        }
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(u32)]
        pub enum NativeFailure {
            UnsupportedHost = 1,
            NativeUnavailable = 2,
            CustodyUnavailable = 3,
            AccountIdentityUnavailable = 4,
            RegistrationMissing = 5,
            StorageLocked = 6,
            StorageUnavailable = 7,
            UnsafeNativeState = 8,
            InvalidRecord = 9,
            UnsupportedSchema = 10,
            LegacyRegistrationRequiresMigration = 11,
            IdentityMismatch = 12,
            GenerationMismatch = 13,
            StaleAssessment = 14,
            ConfirmationRequired = 15,
            AuthorizationDenied = 16,
            AuthorizationCancelled = 17,
            AuthorizationUnavailable = 18,
            InstallationUnavailable = 19,
            ArtifactRejected = 20,
            CredentialUnavailable = 21,
            CredentialPolicyRejected = 22,
            InteractionRequired = 23,
            SupervisorUnavailable = 24,
            BackgroundItemsSuppressed = 25,
            ProcessIdentityMismatch = 26,
            PrivilegeDropUnproven = 27,
            ExecutionBindingUnproven = 28,
            TimedOut = 29,
            LimitExceeded = 30,
            OutcomePending = 31,
            Indeterminate = 32,
            CleanupUnresolved = 33,
            IoUnavailable = 34,
        }
    }
}
pub use spaceterm_host::startup;
#[path = "../src/native_management.rs"]
mod native_management;
use native_deployment::contracts::NativeFailure;
use native_management::request::{ManagementRequest, OperationTag, authorization_right};
use spaceterm_host::startup::BuildIdentity;

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
        assert_eq!(transaction.phase, ManagementPhase::Quiesced);
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
    let controller = controller(backend);
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
use native_management::cli_sudo::{NativeSudoBoundary, SudoAdapter, SudoBoundary, SudoInvocation};
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
    assert_eq!(invocation.prefix(), ["--"]);
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
        ["-n", "--"]
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
    let adapter = SudoAdapter::new(NativeSudoBoundary);
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
    use native_management::linux::{PkexecInvocation, authorize_native_request};
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
    assert_eq!(
        authorize_native_request(&request),
        Err(NativeFailure::AuthorizationUnavailable)
    );
}

#[test]
fn management_helper_exits_after_bounded_request() {
    let started = std::time::Instant::now();
    for arguments in [
        vec!["--request-stdin"],
        vec!["--gui-demand"],
        vec!["stop", "--request-stdin"],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_spaceterm-server-manager"))
            .args(arguments)
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(18));
        assert_eq!(output.stdout, b"classification=authorization-unavailable\n");
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
