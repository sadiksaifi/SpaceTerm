//! Real native lifecycle and CLI dispatch against isolated storage and injected OS effects.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use spaceterm_client::management_cli::{
    AssessmentToken, ConfirmationPrompt, ConfirmationReply, ManagementFailure,
};
use spaceterm_client_native::{
    NativeClientProvider,
    cli::{
        arguments::ConfirmationMode,
        bindings::{
            NativeCliCommand, NativeCommandBindings, UnavailablePairingBindings,
            UnavailableProtocolManagement,
        },
        output::CliOutput,
        runner::{CliDependencies, CliIo, Dispatch, dispatch},
    },
    native::NativeActionPrompt,
};
use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    identity::ServerId,
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    retirement::SessionCleanupObservation,
};
use spaceterm_host::{
    account_service::{
        InstallationStatus, LingerState, RegisteredServer, RuntimeAssessment, ServiceAction,
        ServiceClock, ServiceFailure, ServiceFuture, ServiceManager, ServiceOutcome,
        ServiceProcesses, ServiceRequestState, ServiceTime, SupervisorState,
        test_support::AccountServiceFixture,
    },
    native_management::SudoMode,
    startup::{BuildIdentity, DesiredServerState},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

struct Clock(AtomicU64);
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: [1; 32],
            millis: self.0.load(Ordering::SeqCst),
        })
    }
}
struct Manager {
    state: Mutex<SupervisorState>,
    effects: Mutex<Vec<&'static str>>,
    block_start: AtomicBool,
    start_entered: tokio::sync::Notify,
    start_release: tokio::sync::Notify,
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
            *self.state.lock().unwrap() = SupervisorState::Running;
            if self.block_start.load(Ordering::SeqCst) {
                self.start_entered.notify_one();
                self.start_release.notified().await;
            }
            Ok(())
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.lock().unwrap().push("stop");
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
struct Processes(AtomicU64);
impl ServiceProcesses for Processes {
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            let mut token = [7; 32];
            token[..8].copy_from_slice(&self.0.fetch_add(1, Ordering::SeqCst).to_be_bytes());
            Ok(RuntimeAssessment {
                server: ServerId::new(registration.server_pin().identity),
                epoch: None,
                generation: 1,
                sessions: vec![],
                token,
            })
        })
    }
    fn revalidate<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
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
        _: NativeManagementRequestRef,
        _: NativeRetirementAction,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async { Ok(vec![]) })
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
    clock: Arc<Clock>,
}
impl Fixture {
    fn new(desired: Option<DesiredServerState>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        if let Some(desired) = desired {
            let installation: InstallationStatus = serde_json::from_value(serde_json::json!({
                "installation":1,"version":"1.2.3","artifact_sha256":vec![3;32]
            }))
            .unwrap();
            account
                .initialize_with_installation(desired, Some(installation))
                .unwrap();
        }
        let state = match desired {
            Some(DesiredServerState::Running) => SupervisorState::Running,
            Some(DesiredServerState::Stopped) => SupervisorState::Disabled,
            None => SupervisorState::NotInstalled,
        };
        Self {
            _root: root,
            account,
            manager: Arc::new(Manager {
                state: Mutex::new(state),
                effects: Mutex::new(vec![]),
                block_start: AtomicBool::new(false),
                start_entered: tokio::sync::Notify::new(),
                start_release: tokio::sync::Notify::new(),
            }),
            processes: Arc::new(Processes(AtomicU64::new(1))),
            clock: Arc::new(Clock(AtomicU64::new(100))),
        }
    }
    fn provider(&self) -> NativeClientProvider {
        NativeClientProvider::from_supervisor(self.account.with_providers(
            self.manager.clone(),
            self.processes.clone(),
            self.clock.clone(),
            None,
        ))
    }
}
#[derive(Default)]
struct Io {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    fail_stdout: bool,
    fail_stderr: bool,
    prompts: usize,
    confirm: bool,
}
impl CliIo for Io {
    fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), ManagementFailure> {
        if self.fail_stdout {
            return Err(ManagementFailure::IoUnavailable);
        }
        self.stdout.extend_from_slice(bytes);
        Ok(())
    }
    fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), ManagementFailure> {
        if self.fail_stderr {
            return Err(ManagementFailure::IoUnavailable);
        }
        self.stderr.extend_from_slice(bytes);
        Ok(())
    }
    fn read_confirmation(
        &mut self,
        _: &ConfirmationPrompt,
    ) -> Result<ConfirmationReply, ManagementFailure> {
        panic!("native lifecycle must not use a protocol assessment")
    }
    fn confirm_native_action(&mut self, _: &NativeActionPrompt) -> Result<(), ManagementFailure> {
        self.prompts += 1;
        if self.confirm {
            Ok(())
        } else {
            Err(ManagementFailure::ConfirmationRequired)
        }
    }
}
async fn run(
    provider: &NativeClientProvider,
    command: NativeCliCommand,
    confirmation: ConfirmationMode,
    io: &mut Io,
) -> Result<CliOutput, ManagementFailure> {
    provider
        .management()
        .run(
            provider.key(),
            command,
            confirmation,
            SudoMode::Interactive,
            io,
        )
        .await
}

struct DeniedArtifacts(AtomicU64);
impl spaceterm_client_native::native::artifacts::ArtifactTransport for DeniedArtifacts {
    fn get(&self, _: &str, _: usize) -> Result<Vec<u8>, ManagementFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ManagementFailure::AuthorizationDenied)
    }
}

#[tokio::test]
async fn cli_install_verifies_artifacts_before_allocating_request_or_service_effects() {
    let fixture = Fixture::new(None);
    let artifacts = Arc::new(DeniedArtifacts(AtomicU64::new(0)));
    let provider = fixture
        .provider()
        .with_artifact_transport(artifacts.clone());
    let mut io = Io::default();
    assert!(matches!(
        run(
            &provider,
            NativeCliCommand::Install {
                version: None,
                source: None
            },
            ConfirmationMode::Interactive,
            &mut io,
        )
        .await,
        Err(ManagementFailure::AuthorizationDenied)
    ));
    assert_eq!(artifacts.0.load(Ordering::SeqCst), 1);
    assert!(!fixture.account.anchor().exists());
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert!(io.stderr.is_empty());
    assert!(io.stdout.is_empty());

    let mut other = provider.key();
    other.build_identity = BuildIdentity::Release;
    assert!(matches!(
        provider
            .management()
            .run(
                other,
                NativeCliCommand::Install {
                    version: None,
                    source: None
                },
                ConfirmationMode::Interactive,
                SudoMode::NonInteractive,
                &mut io,
            )
            .await,
        Err(ManagementFailure::AuthorizationDenied)
    ));
    assert_eq!(artifacts.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_or_suppressed_service_is_not_stopped_and_discovery_creates_nothing() {
    let fixture = Fixture::new(None);
    let provider = fixture.provider();
    assert_eq!(
        provider.inspect(provider.key()).await,
        Err(ManagementFailure::NativeUnavailable)
    );
    assert!(!fixture.account.anchor().exists());
    assert!(matches!(
        provider.companion_status(),
        Err(ManagementFailure::PairingRequired)
    ));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());

    let fixture = Fixture::new(Some(DesiredServerState::Stopped));
    let provider = fixture.provider();
    assert_eq!(
        provider.inspect(provider.key()).await.unwrap().desired,
        Some(DesiredServerState::Stopped)
    );
    *fixture.manager.state.lock().unwrap() = SupervisorState::Suppressed;
    assert_eq!(
        provider.inspect(provider.key()).await,
        Err(ManagementFailure::NativeUnavailable)
    );
    let mut other = provider.key();
    other.build_identity = BuildIdentity::Release;
    assert_eq!(
        provider.inspect(other).await,
        Err(ManagementFailure::AuthorizationDenied)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn exact_token_restores_original_request_across_provider_reopen_and_cannot_change_action() {
    let fixture = Fixture::new(Some(DesiredServerState::Running));
    let provider = fixture.provider();
    let output = run(
        &provider,
        NativeCliCommand::Stop,
        ConfirmationMode::Assess,
        &mut Io::default(),
    )
    .await
    .unwrap();
    let CliOutput::ServiceAssessment(assessment) = output else {
        panic!("missing assessment")
    };
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let reopened = fixture.provider();
    assert!(matches!(
        run(
            &reopened,
            NativeCliCommand::Uninstall,
            ConfirmationMode::Token(assessment.token.clone()),
            &mut Io::default()
        )
        .await,
        Err(ManagementFailure::StaleAssessment)
    ));
    let output = run(
        &reopened,
        NativeCliCommand::Stop,
        ConfirmationMode::Token(assessment.token),
        &mut Io::default(),
    )
    .await
    .unwrap();
    let CliOutput::ServiceProgress(progress) = output else {
        panic!("missing progress")
    };
    assert_eq!(progress.request, assessment.request);
    assert_eq!(
        progress.state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    assert_eq!(
        reopened.management().status().await.unwrap().desired,
        Some(DesiredServerState::Stopped)
    );
    assert_eq!(
        reopened
            .management()
            .resolve(progress.request)
            .await
            .unwrap(),
        progress
    );
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
async fn expired_or_malformed_tokens_and_declined_confirmation_have_no_lifecycle_effect() {
    let fixture = Fixture::new(Some(DesiredServerState::Running));
    let provider = fixture.provider();
    let assessment = provider
        .management()
        .assess(ServiceAction::Restart)
        .await
        .unwrap();
    let prompt = NativeActionPrompt::from_assessment(&assessment).unwrap();
    fixture.clock.0.store(30_101, Ordering::SeqCst);
    assert!(matches!(
        run(
            &provider,
            NativeCliCommand::Restart,
            ConfirmationMode::Token(prompt.token),
            &mut Io::default()
        )
        .await,
        Err(ManagementFailure::StaleAssessment)
    ));
    assert!(matches!(
        run(
            &provider,
            NativeCliCommand::Stop,
            ConfirmationMode::Token(AssessmentToken::new("native_bad".into()).unwrap()),
            &mut Io::default()
        )
        .await,
        Err(ManagementFailure::StaleAssessment)
    ));
    let mut io = Io::default();
    assert!(matches!(
        run(
            &provider,
            NativeCliCommand::Stop,
            ConfirmationMode::Interactive,
            &mut io
        )
        .await,
        Err(ManagementFailure::ConfirmationRequired)
    ));
    assert_eq!(io.prompts, 1);
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn restart_cannot_start_stopped_service_but_explicit_start_can_and_uninstall_retains_data() {
    let fixture = Fixture::new(Some(DesiredServerState::Stopped));
    let provider = fixture.provider();
    assert!(matches!(
        run(
            &provider,
            NativeCliCommand::Restart,
            ConfirmationMode::Assess,
            &mut Io::default()
        )
        .await,
        Err(ManagementFailure::StaleAssessment)
    ));
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let CliOutput::ServiceProgress(started) = run(
        &provider,
        NativeCliCommand::Start,
        ConfirmationMode::Interactive,
        &mut Io::default(),
    )
    .await
    .unwrap() else {
        panic!("missing progress")
    };
    assert_eq!(
        started.state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    let original_pin = provider
        .management()
        .supervisor()
        .discover()
        .unwrap()
        .unwrap()
        .server_pin();
    let mut io = Io {
        confirm: true,
        ..Io::default()
    };
    let CliOutput::ServiceProgress(uninstalled) = run(
        &provider,
        NativeCliCommand::Uninstall,
        ConfirmationMode::Interactive,
        &mut io,
    )
    .await
    .unwrap() else {
        panic!("missing progress")
    };
    assert_eq!(
        uninstalled.state,
        ServiceRequestState::Completed(ServiceOutcome::Uninstalled)
    );
    assert_eq!(
        provider
            .management()
            .supervisor()
            .discover()
            .unwrap()
            .unwrap()
            .server_pin(),
        original_pin
    );
    assert!(fixture.account.data_directory().exists());
    assert_eq!(
        provider.inspect(provider.key()).await,
        Err(ManagementFailure::NativeUnavailable)
    );
}

#[tokio::test]
async fn cli_native_request_status_uses_the_same_supervisor_outcome_without_protocol_identity() {
    let fixture = Fixture::new(Some(DesiredServerState::Stopped));
    let provider = fixture.provider();
    let request = provider
        .management()
        .supervisor()
        .allocate_request()
        .unwrap();
    provider.management().start(request).await.unwrap();
    let deps = CliDependencies::new(
        Ok(provider.key()),
        Arc::new(UnavailableProtocolManagement),
        provider.management(),
        Arc::new(UnavailablePairingBindings),
    );
    let args = [
        "--json".to_owned(),
        "status".to_owned(),
        "--operation".to_owned(),
        format!(
            "native:{}",
            spaceterm_client_native::cli::output::hex(request.as_bytes())
        ),
    ];
    let invocation =
        spaceterm_client_native::cli::arguments::parse(args.into_iter().map(Into::into)).unwrap();
    let mut io = Io::default();
    assert!(
        matches!(dispatch(invocation, &deps, &mut io).await, Dispatch::Exit(exit) if exit.code() == 0)
    );
    let value: serde_json::Value = serde_json::from_slice(&io.stdout).unwrap();
    assert_eq!(
        value["result"]["native_request"],
        spaceterm_client_native::cli::output::hex(request.as_bytes())
    );
    assert_eq!(value["result"]["outcome"], "started");
}

fn dependencies(provider: &NativeClientProvider) -> CliDependencies {
    CliDependencies::new(
        Ok(provider.key()),
        Arc::new(UnavailableProtocolManagement),
        provider.management(),
        Arc::new(UnavailablePairingBindings),
    )
}
async fn dispatch_args(args: &[&str], deps: &CliDependencies, io: &mut Io) -> u8 {
    let invocation =
        spaceterm_client_native::cli::arguments::parse(args.iter().map(std::ffi::OsString::from))
            .unwrap();
    let Dispatch::Exit(exit) = dispatch(invocation, deps, io).await else {
        panic!("serve")
    };
    exit.code()
}

#[tokio::test]
async fn cli_lost_start_then_completed_stop_replays_original_start_without_restarting() {
    let fixture = Fixture::new(Some(DesiredServerState::Stopped));
    let deps = dependencies(&fixture.provider());
    let mut lost = Io {
        fail_stdout: true,
        ..Io::default()
    };
    assert_eq!(
        dispatch_args(&["--json", "start"], &deps, &mut lost).await,
        75
    );
    // The provider's durable reservation is announced before Start is polled.
    // A failed final response still leaves the original lookup reference available.
    let reservation: serde_json::Value =
        serde_json::from_slice(lost.stderr.split(|byte| *byte == b'\n').next().unwrap()).unwrap();
    let original = reservation["request"]["native_request"].as_str().unwrap();
    assert_eq!(original.len(), 32);
    assert!(lost.stdout.is_empty());
    let reopened = fixture.provider();
    assert_eq!(
        reopened.management().status().await.unwrap().desired,
        Some(DesiredServerState::Running)
    );
    let deps = dependencies(&reopened);
    let mut stopped = Io {
        confirm: true,
        ..Io::default()
    };
    assert_eq!(
        dispatch_args(&["--json", "stop"], &deps, &mut stopped).await,
        0
    );
    assert_eq!(
        reopened.management().status().await.unwrap().desired,
        Some(DesiredServerState::Stopped)
    );
    let effects = fixture.manager.effects.lock().unwrap().clone();
    let mut replay = Io::default();
    let selector = format!("native:{original}");
    assert_eq!(
        dispatch_args(
            &["--json", "status", "--operation", &selector],
            &dependencies(&fixture.provider()),
            &mut replay
        )
        .await,
        0
    );
    let response: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(response["result"]["native_request"], original);
    assert_eq!(response["result"]["outcome"], "started");
    assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
    assert_eq!(
        fixture
            .provider()
            .management()
            .status()
            .await
            .unwrap()
            .desired,
        Some(DesiredServerState::Stopped)
    );
}

#[tokio::test]
async fn cli_cannot_poll_start_or_destructive_assessment_without_publishing_retained_request_id() {
    for command in [vec!["start"], vec!["stop", "--assess"]] {
        let fixture = Fixture::new(Some(DesiredServerState::Running));
        let mut io = Io {
            fail_stderr: true,
            ..Io::default()
        };
        assert_eq!(
            dispatch_args(&command, &dependencies(&fixture.provider()), &mut io).await,
            75
        );
        assert!(fixture.manager.effects.lock().unwrap().is_empty());
        assert_eq!(fixture.processes.0.load(Ordering::SeqCst), 1);
        // Allocation reserved an ID but accepted no service action.
        assert_eq!(
            fixture
                .provider()
                .management()
                .status()
                .await
                .unwrap()
                .pending,
            None
        );
    }
}

struct InvalidFeedTransport(AtomicU64);
impl spaceterm_client_native::native::artifacts::ArtifactTransport for InvalidFeedTransport {
    fn get(&self, _: &str, _: usize) -> Result<Vec<u8>, ManagementFailure> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(if n == 0 {
            b"{\"version\":\"1.2.4\"}".to_vec()
        } else {
            vec![b'A'; 88]
        })
    }
}
#[tokio::test]
async fn cli_update_check_verifies_signed_metadata_without_artifact_or_lifecycle_effects() {
    let fixture = Fixture::new(None);
    let transport = Arc::new(InvalidFeedTransport(AtomicU64::new(0)));
    let provider = fixture
        .provider()
        .with_artifact_transport(transport.clone());
    let mut io = Io::default();
    let code = dispatch_args(
        &["--json", "update", "check"],
        &dependencies(&provider),
        &mut io,
    )
    .await;
    assert_eq!(transport.0.load(Ordering::SeqCst), 2);
    let response: serde_json::Value = serde_json::from_slice(&io.stderr).unwrap();
    assert_eq!(response["error"], "unsafe_native_state");
    assert_eq!(code, ManagementFailure::UnsafeNativeState.exit_code());
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert!(!fixture.account.anchor().exists());
    assert!(io.stdout.is_empty());
}

#[tokio::test]
async fn interrupted_cli_wait_preserves_and_resolves_the_original_admitted_start() {
    let fixture = Fixture::new(Some(DesiredServerState::Stopped));
    fixture.manager.block_start.store(true, Ordering::SeqCst);
    let provider = fixture.provider();
    let deps = dependencies(&provider);
    let mut io = Io::default();
    let invocation = spaceterm_client_native::cli::arguments::parse(
        ["--json", "start"].map(std::ffi::OsString::from),
    )
    .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        spaceterm_client_native::cli::runner::dispatch_until(
            invocation,
            &deps,
            &mut io,
            fixture.manager.start_entered.notified(),
        ),
    )
    .await;
    fixture.manager.start_release.notify_one();
    assert!(matches!(result, Ok(Dispatch::Exit(exit)) if exit.code() == 75));
    let mut records =
        serde_json::Deserializer::from_slice(&io.stderr).into_iter::<serde_json::Value>();
    let reservation = records.next().unwrap().unwrap();
    let failure = records.next().unwrap().unwrap();
    assert_eq!(failure["error"], "outcome_pending");
    assert!(records.next().is_none());
    let original = reservation["request"]["native_request"].as_str().unwrap();
    assert!(io.stdout.is_empty());
    let selector = format!("native:{original}");
    let parsed = spaceterm_client_native::cli::arguments::parse(
        ["status", "--operation", &selector].map(std::ffi::OsString::from),
    )
    .unwrap();
    let spaceterm_client_native::cli::arguments::CliCommand::Status {
        operation: Some(spaceterm_client_native::cli::arguments::RequestSelector::Native(request)),
    } = parsed.command
    else {
        panic!("native selector")
    };
    // Joining the service-owned worker is independent from the cancelled CLI wait.
    let progress = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match provider.management().resolve(request).await {
                Ok(progress) if matches!(progress.state, ServiceRequestState::Completed(_)) => {
                    break progress;
                }
                Ok(_) | Err(ServiceFailure::Pending) => tokio::task::yield_now().await,
                Err(error) => panic!("resolution failed: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(progress.request, request);
    assert_eq!(
        progress.state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    assert_eq!(
        fixture
            .manager
            .effects
            .lock()
            .unwrap()
            .iter()
            .filter(|effect| **effect == "start")
            .count(),
        1
    );
}

#[tokio::test]
async fn cli_activates_only_the_assessed_staged_installation_and_preserves_stopped() {
    let fixture = Fixture::new(Some(DesiredServerState::Stopped));
    let staged: InstallationStatus = serde_json::from_value(serde_json::json!({
        "installation":2,"version":"1.2.4","artifact_sha256":vec![4;32]
    }))
    .unwrap();
    fixture.account.stage_installation(staged.clone()).unwrap();
    let provider = fixture.provider();
    let deps = dependencies(&provider);
    let mut assessed = Io::default();
    assert_eq!(
        dispatch_args(
            &[
                "--json",
                "update",
                "activate",
                "--installation",
                "2",
                "--assess"
            ],
            &deps,
            &mut assessed
        )
        .await,
        0
    );
    let assessment: serde_json::Value = serde_json::from_slice(&assessed.stdout).unwrap();
    let token = assessment["result"]["token"].as_str().unwrap();
    let original = assessment["result"]["native_request"].as_str().unwrap();
    let reservation: serde_json::Value = serde_json::from_slice(&assessed.stderr).unwrap();
    assert_eq!(reservation["request"]["native_request"], original);
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    assert_eq!(
        dispatch_args(
            &[
                "--non-interactive",
                "update",
                "activate",
                "--installation",
                "3",
                "--confirm",
                token
            ],
            &deps,
            &mut Io::default()
        )
        .await,
        2
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let mut activated = Io::default();
    assert_eq!(
        dispatch_args(
            &[
                "--json",
                "--non-interactive",
                "update",
                "activate",
                "--installation",
                "2",
                "--confirm",
                token
            ],
            &dependencies(&fixture.provider()),
            &mut activated
        )
        .await,
        0
    );
    let result: serde_json::Value = serde_json::from_slice(&activated.stdout).unwrap();
    assert_eq!(result["result"]["native_request"], original);
    assert_eq!(result["result"]["outcome"], "activated");
    let status = provider.management().status().await.unwrap();
    assert_eq!(status.desired, Some(DesiredServerState::Stopped));
    assert_eq!(status.approved, Some(staged));
    assert_eq!(status.staged, None);
    assert!(!fixture.manager.effects.lock().unwrap().contains(&"start"));
    let effects = fixture.manager.effects.lock().unwrap().clone();
    let mut replay = Io::default();
    assert_eq!(
        dispatch_args(
            &[
                "--json",
                "status",
                "--operation",
                &format!("native:{original}")
            ],
            &dependencies(&fixture.provider()),
            &mut replay
        )
        .await,
        0
    );
    let result: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(result["result"]["outcome"], "activated");
    assert_eq!(fixture.manager.effects.lock().unwrap().as_slice(), effects);
}

#[tokio::test]
async fn cli_local_install_rejects_unsigned_inputs_before_request_or_service_effects() {
    let fixture = Fixture::new(None);
    let artifacts = Arc::new(DeniedArtifacts(AtomicU64::new(0)));
    let provider = fixture
        .provider()
        .with_artifact_transport(artifacts.clone());
    let local = tempfile::tempdir().unwrap();
    let feed = local.path().join("private-path-marker.json");
    let archive = local.path().join("private-path-marker.tar.gz");
    std::fs::write(&feed, b"private-content-marker").unwrap();
    std::fs::write(
        local.path().join("private-path-marker.json.sig"),
        b"unsigned",
    )
    .unwrap();
    std::fs::write(&archive, b"private-content-marker").unwrap();
    let invocation = spaceterm_client_native::cli::parse(
        [
            "--json",
            "--non-interactive",
            "install",
            "--feed",
            feed.to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
        ]
        .map(Into::into),
    )
    .unwrap();
    let dependencies = CliDependencies::new(
        Ok(provider.key()),
        Arc::new(UnavailableProtocolManagement),
        provider.management(),
        Arc::new(UnavailablePairingBindings),
    );
    let mut io = Io::default();
    let Dispatch::Exit(exit) = dispatch(invocation, &dependencies, &mut io).await else {
        panic!("local install cannot serve");
    };
    assert_eq!(
        exit.code(),
        ManagementFailure::UnsafeNativeState.exit_code()
    );
    assert_eq!(
        artifacts.0.load(Ordering::SeqCst),
        0,
        "local install must never download"
    );
    assert!(!fixture.account.anchor().exists());
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&io.stdout),
        String::from_utf8_lossy(&io.stderr)
    );
    assert!(!diagnostic.contains("private-path-marker"));
    assert!(!diagnostic.contains("private-content-marker"));
    assert!(!diagnostic.contains("native_request"));
}

#[tokio::test]
async fn cli_native_results_release_capacity_across_more_than_256_provider_restarts() {
    let fixture = Fixture::new(Some(DesiredServerState::Running));
    let mut first = None;
    for _ in 0..260 {
        let provider = fixture.provider();
        let CliOutput::ServiceProgress(progress) = run(
            &provider,
            NativeCliCommand::Start,
            ConfirmationMode::Interactive,
            &mut Io::default(),
        )
        .await
        .unwrap() else {
            panic!("missing progress")
        };
        assert_eq!(
            progress.state,
            ServiceRequestState::Completed(ServiceOutcome::Started)
        );
        first.get_or_insert(progress);
    }
    let provider = fixture.provider();
    let first = first.unwrap();
    let CliOutput::ServiceProgress(recovered) = run(
        &provider,
        NativeCliCommand::Resolve {
            request: first.request,
        },
        ConfirmationMode::Interactive,
        &mut Io::default(),
    )
    .await
    .unwrap() else {
        panic!("missing original result")
    };
    assert_eq!(recovered, first);
}
#[tokio::test]
async fn cli_update_stage_authenticates_artifacts_before_request_or_service_effects() {
    for desired in [DesiredServerState::Running, DesiredServerState::Stopped] {
        let fixture = Fixture::new(Some(desired));
        let artifacts = Arc::new(DeniedArtifacts(AtomicU64::new(0)));
        let provider = fixture
            .provider()
            .with_artifact_transport(artifacts.clone());
        assert!(matches!(
            run(
                &provider,
                NativeCliCommand::UpdateStage {
                    version: None,
                    source: None
                },
                ConfirmationMode::Interactive,
                &mut Io::default()
            )
            .await,
            Err(ManagementFailure::AuthorizationDenied)
        ));
        assert_eq!(artifacts.0.load(Ordering::SeqCst), 1);
        assert!(fixture.manager.effects.lock().unwrap().is_empty());
        assert_eq!(
            provider.management().status().await.unwrap().desired,
            Some(desired)
        );
    }
}

#[tokio::test]
async fn cli_native_completion_recovery_settles_crashes_before_and_after_host_acknowledgement() {
    for acknowledged in [false, true] {
        let fixture = Fixture::new(Some(DesiredServerState::Running));
        let provider = fixture.provider();
        let management = provider.management();
        let request = management.supervisor().allocate_request().unwrap();
        let original = management.start(request).await.unwrap();
        assert_eq!(management.retain_cli_completion(request).unwrap(), original);
        if acknowledged {
            management.acknowledge_request(request).unwrap();
        }
        drop(management);
        drop(provider);
        let reopened = fixture.provider();
        let CliOutput::ServiceProgress(recovered) = run(
            &reopened,
            NativeCliCommand::Resolve { request },
            ConfirmationMode::Interactive,
            &mut Io::default(),
        )
        .await
        .unwrap() else {
            panic!("missing original completion")
        };
        assert_eq!(recovered, original);
        assert_eq!(
            reopened.management().supervisor().request_progress(request),
            Err(ServiceFailure::RequestRetired)
        );
        assert_eq!(
            fixture
                .manager
                .effects
                .lock()
                .unwrap()
                .iter()
                .filter(|effect| **effect == "start")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn cli_local_update_stage_uses_release_verification_without_network_or_lifecycle_effects() {
    for desired in [DesiredServerState::Running, DesiredServerState::Stopped] {
        let fixture = Fixture::new(Some(desired));
        let artifacts = Arc::new(DeniedArtifacts(AtomicU64::new(0)));
        let provider = fixture
            .provider()
            .with_artifact_transport(artifacts.clone());
        let local = tempfile::tempdir().unwrap();
        let feed = local.path().join("private-path-marker.json");
        let archive = local.path().join("private-path-marker.tar.gz");
        std::fs::write(&feed, b"private-content-marker").unwrap();
        std::fs::write(
            local.path().join("private-path-marker.json.sig"),
            b"unsigned",
        )
        .unwrap();
        std::fs::write(&archive, b"private-content-marker").unwrap();
        let invocation = spaceterm_client_native::cli::parse(
            [
                "--json",
                "--non-interactive",
                "update",
                "stage",
                "--feed",
                feed.to_str().unwrap(),
                "--archive",
                archive.to_str().unwrap(),
            ]
            .map(Into::into),
        )
        .unwrap();
        let dependencies = CliDependencies::new(
            Ok(provider.key()),
            Arc::new(UnavailableProtocolManagement),
            provider.management(),
            Arc::new(UnavailablePairingBindings),
        );
        let mut io = Io::default();
        let Dispatch::Exit(exit) = dispatch(invocation, &dependencies, &mut io).await else {
            panic!("staging cannot serve")
        };
        assert_eq!(
            exit.code(),
            ManagementFailure::UnsafeNativeState.exit_code()
        );
        assert_eq!(artifacts.0.load(Ordering::SeqCst), 0);
        assert!(fixture.manager.effects.lock().unwrap().is_empty());
        assert_eq!(
            provider.management().status().await.unwrap().desired,
            Some(desired)
        );
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&io.stdout),
            String::from_utf8_lossy(&io.stderr)
        );
        assert!(!diagnostic.contains("private-path-marker"));
        assert!(!diagnostic.contains("private-content-marker"));
        assert!(!diagnostic.contains("native_request"));
    }
}
