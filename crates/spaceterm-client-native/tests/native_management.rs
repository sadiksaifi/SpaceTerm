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
    atomic::{AtomicU64, Ordering},
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
    prompts: usize,
    confirm: bool,
}
impl CliIo for Io {
    fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), ManagementFailure> {
        self.stdout.extend_from_slice(bytes);
        Ok(())
    }
    fn write_stderr(&mut self, _: &[u8]) -> Result<(), ManagementFailure> {
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
