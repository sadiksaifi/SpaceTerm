use super::*;
use native_management::facade::{
    ConfirmedNativeAction, FixedNativeManagement, GuiNativeManagement, NativeAction,
    NativeAssessment, NativeManagement, NativeManagementPort, NativeStatus,
};
use spaceterm_host::native_deployment::contracts::{
    AccountBuildKey, NativeManagementFuture, NativeRequestId,
};
use std::time::Duration;

struct Port {
    routed: Mutex<Vec<AuthorizationRoute>>,
    substitute_request: bool,
}
struct TestPort(Arc<Port>);
impl NativeManagementPort for TestPort {
    fn inspect(&self, _: AccountBuildKey) -> NativeManagementFuture<'_, NativeStatus> {
        Box::pin(async { Err(NativeFailure::NativeUnavailable) })
    }
    fn assess(
        &self,
        _: AccountBuildKey,
        action: NativeAction,
    ) -> NativeManagementFuture<'_, NativeAssessment> {
        Box::pin(async move {
            let mut bytes = frame(action.tag());
            bytes[88..120].copy_from_slice(&action.digest());
            NativeAssessment::from_native(
                ManagementRequest::decode(&bytes)?,
                action,
                Duration::from_secs(1),
            )
        })
    }
    fn execute(
        &self,
        confirmed: ConfirmedNativeAction,
        route: AuthorizationRoute,
    ) -> NativeManagementFuture<'_, ManagementProgress> {
        Box::pin(async move {
            self.0.routed.lock().unwrap().push(route);
            Ok(ManagementProgress {
                request: if self.0.substitute_request {
                    [9; 16]
                } else {
                    *confirmed.assessment().request().request_bytes()
                },
                phase: ManagementPhase::Completed,
                state: ProgressState::Complete,
            })
        })
    }
    fn resolve(&self, request: NativeRequestId) -> NativeManagementFuture<'_, ManagementProgress> {
        Box::pin(async move {
            Ok(ManagementProgress {
                request: *request.as_bytes(),
                phase: ManagementPhase::Quiescing,
                state: ProgressState::Pending,
            })
        })
    }
}
#[tokio::test]
async fn facade_preserves_exact_assessment_confirmation_and_fixed_os_routes() {
    let port = Arc::new(Port {
        routed: Mutex::new(Vec::new()),
        substitute_request: false,
    });
    let facade = FixedNativeManagement::new(TestPort(port.clone()));
    let key = request(OperationTag::Start).key();
    for route in [
        AuthorizationRoute::Gui,
        AuthorizationRoute::CliInteractive,
        AuthorizationRoute::CliNonInteractive,
    ] {
        let assessment = facade.assess(key, NativeAction::Start).await.unwrap();
        assert_eq!(assessment.issued(), Duration::from_secs(1));
        let confirmed = assessment.confirm();
        let progress = match route {
            AuthorizationRoute::Gui => facade.execute_gui(confirmed).await.unwrap(),
            AuthorizationRoute::CliInteractive => facade
                .execute(confirmed, SudoMode::Interactive)
                .await
                .unwrap(),
            AuthorizationRoute::CliNonInteractive => facade
                .execute(confirmed, SudoMode::NonInteractive)
                .await
                .unwrap(),
        };
        assert_eq!(progress.progress().request, [1; 16]);
    }
    assert_eq!(
        *port.routed.lock().unwrap(),
        [
            AuthorizationRoute::Gui,
            AuthorizationRoute::CliInteractive,
            AuthorizationRoute::CliNonInteractive
        ]
    );
}
#[tokio::test]
async fn facade_rejects_substituted_progress_and_resolves_original_request_only() {
    let facade = FixedNativeManagement::new(TestPort(Arc::new(Port {
        routed: Mutex::new(Vec::new()),
        substitute_request: true,
    })));
    let assessment = facade
        .assess(request(OperationTag::Start).key(), NativeAction::Start)
        .await
        .unwrap();
    assert_eq!(
        facade
            .execute(assessment.confirm(), SudoMode::Interactive)
            .await,
        Err(NativeFailure::IdentityMismatch)
    );
    let progress = facade
        .resolve(NativeRequestId::new([3; 16]).unwrap())
        .await
        .unwrap();
    assert_eq!(progress.progress().request, [3; 16]);
    assert_eq!(progress.progress().state, ProgressState::Pending);
}
#[test]
fn native_assessment_rejects_an_action_that_does_not_match_the_native_reply() {
    let original = request(OperationTag::Start);
    assert!(matches!(
        NativeAssessment::from_native(original, NativeAction::Start, Duration::from_secs(1)),
        Err(NativeFailure::IdentityMismatch)
    ));
}

use native_management::facade::transport::{
    NativeGuiManagementTransport, NativeManagementObservations, SystemManagementPort,
};
impl NativeManagementObservations for TestPort {
    fn inspect(&self, key: AccountBuildKey) -> NativeManagementFuture<'_, NativeStatus> {
        NativeManagementPort::inspect(self, key)
    }
    fn assess(
        &self,
        key: AccountBuildKey,
        action: NativeAction,
    ) -> NativeManagementFuture<'_, NativeAssessment> {
        NativeManagementPort::assess(self, key, action)
    }
    fn resolve(&self, request: NativeRequestId) -> NativeManagementFuture<'_, ManagementProgress> {
        NativeManagementPort::resolve(self, request)
    }
}
impl NativeGuiManagementTransport for TestPort {
    fn execute(
        &self,
        confirmed: ConfirmedNativeAction,
    ) -> NativeManagementFuture<'_, ManagementProgress> {
        NativeManagementPort::execute(self, confirmed, AuthorizationRoute::Gui)
    }
}
struct SlowSudo(Arc<std::sync::atomic::AtomicU32>);
impl SudoBoundary for SlowSudo {
    type RetainedHelper = ();
    fn retain_fixed_helper(&self, _: &ManagementRequest) -> Result<(), NativeFailure> {
        Ok(())
    }
    fn recheck_execution_binding(&self, _: &mut ()) -> Result<(), NativeFailure> {
        Ok(())
    }
    fn invoke(
        &self,
        _: (),
        _: SudoInvocation,
        bytes: &[u8; 160],
    ) -> Result<ManagementProgress, NativeFailure> {
        let request = ManagementRequest::decode(bytes)?;
        std::thread::sleep(Duration::from_millis(40));
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ManagementProgress {
            request: *request.request_bytes(),
            phase: ManagementPhase::Completed,
            state: ProgressState::Complete,
        })
    }
}
#[tokio::test(flavor = "current_thread")]
async fn concrete_facade_port_does_not_block_executor_or_cancel_admitted_work_with_waiter() {
    let make_port = || {
        TestPort(Arc::new(Port {
            routed: Mutex::new(Vec::new()),
            substitute_request: false,
        }))
    };
    let effects = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let facade = FixedNativeManagement::new(SystemManagementPort::new(
        make_port(),
        SlowSudo(effects.clone()),
        make_port(),
    ));
    let assessment = facade
        .assess(request(OperationTag::Start).key(), NativeAction::Start)
        .await
        .unwrap();
    let timed = tokio::time::timeout(
        Duration::from_millis(5),
        facade.execute(assessment.confirm(), SudoMode::Interactive),
    )
    .await;
    assert!(
        timed.is_err(),
        "the synchronous broker blocked the facade's executor"
    );
    let expires = std::time::Instant::now() + Duration::from_secs(2);
    while effects.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < expires,
            "dropping the waiter cancelled the native exchange"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 1);
}
