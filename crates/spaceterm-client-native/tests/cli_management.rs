use std::sync::{Arc, Mutex};

use spaceterm_client::management_cli::*;
use spaceterm_client_native::cli::{bindings::*, output::*, runner::*, *};
use spaceterm_core::operations::OperationId;
use spaceterm_host::{native_management::SudoMode, startup::*};

#[derive(Default)]
struct Io {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    confirmations: usize,
}
impl CliIo for Io {
    fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
        self.stdout.extend_from_slice(bytes);
        Ok(())
    }
    fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
        self.stderr.extend_from_slice(bytes);
        Ok(())
    }
    fn read_confirmation(
        &mut self,
        prompt: &ConfirmationPrompt,
    ) -> Result<ConfirmationReply, CliFailure> {
        self.confirmations += 1;
        prompt.reply(b"confirm")
    }
}
#[derive(Default)]
struct Calls(Mutex<Vec<&'static str>>);
struct Native {
    calls: Arc<Calls>,
    status: Result<NativeObservation, CliFailure>,
}
impl NativeCommandBindings for Native {
    fn inspect(&self, _: AccountBuildKey) -> ManagementFuture<'_, NativeObservation> {
        self.calls.0.lock().unwrap().push("inspect");
        let status = self.status.clone();
        Box::pin(async move { status })
    }
    fn run<'a>(
        &'a self,
        _: AccountBuildKey,
        command: NativeCliCommand,
        _: ConfirmationMode,
        _: SudoMode,
        _: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput> {
        self.calls.0.lock().unwrap().push(match command {
            NativeCliCommand::Resolve { .. } => "resolve",
            NativeCliCommand::Start => "start",
            NativeCliCommand::Stop => "stop",
            NativeCliCommand::RemoteAccess { .. } => "remote_access",
            NativeCliCommand::UpdateCheck => "update_check",
            NativeCliCommand::UpdateStage { .. } => "update_stage",
            NativeCliCommand::UpdateActivate { .. } => "update_activate",
        });
        Box::pin(async { Err(CliFailure::AuthorizationUnavailable) })
    }
}
struct Protocol {
    calls: Arc<Calls>,
    hang_status: bool,
    resolution: Option<ManagementProgress>,
}
impl ProtocolManagement for Protocol {
    fn negotiated_major(&self) -> Result<u32, CliFailure> {
        Err(CliFailure::PairingRequired)
    }
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        self.calls.0.lock().unwrap().push("protocol_status");
        Box::pin(async {
            if self.hang_status {
                std::future::pending::<()>().await;
            }
            Err(CliFailure::PairingRequired)
        })
    }
    fn assess(&self, _: ManagementAction) -> ManagementFuture<'_, ManagementAssessment> {
        panic!("unexpected mutation")
    }
    fn restore_assessment(
        &self,
        _: ManagementAction,
        _: AssessmentToken,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        panic!("unexpected mutation")
    }
    fn execute(&self, _: ConfirmedManagementAction) -> ManagementFuture<'_, ManagementProgress> {
        panic!("unexpected mutation")
    }
    fn resolve(&self, _: OperationId) -> ManagementFuture<'_, ManagementProgress> {
        self.calls.0.lock().unwrap().push("resolve");
        Box::pin(async { Ok(self.resolution.expect("unexpected resolution")) })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        panic!("unexpected subscription")
    }
}
fn dependencies(
    status: Result<NativeObservation, CliFailure>,
    uid: u32,
) -> (CliDependencies, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    (
        CliDependencies::new(
            Ok(AccountBuildKey {
                account_uid: uid,
                build_identity: BuildIdentity::Development,
            }),
            Arc::new(Protocol {
                calls: calls.clone(),
                hang_status: false,
                resolution: None,
            }),
            Arc::new(Native {
                calls: calls.clone(),
                status,
            }),
            Arc::new(UnavailablePairingBindings),
        ),
        calls,
    )
}
async fn run(args: &[&str], deps: &CliDependencies) -> (u8, Io) {
    let mut io = Io::default();
    let Dispatch::Exit(exit) = dispatch(
        parse(args.iter().map(std::ffi::OsString::from)).unwrap(),
        deps,
        &mut io,
    )
    .await
    else {
        panic!("CLI selected daemon")
    };
    (exit.code(), io)
}
#[tokio::test]
async fn help_and_version_require_no_credentials_or_display() {
    let (deps, calls) = dependencies(Err(CliFailure::NativeUnavailable), 0);
    for args in [&["--help"][..], &["--version"][..], &[][..]] {
        let (code, io) = run(args, &deps).await;
        assert_eq!(code, 0);
        assert!(!io.stdout.is_empty());
        assert!(calls.0.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn root_cli_execution_is_rejected() {
    let (deps, calls) = dependencies(Err(CliFailure::NativeUnavailable), 0);
    let (code, io) = run(&["--json", "start"], &deps).await;
    assert_eq!(code, 77);
    assert!(calls.0.lock().unwrap().is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&io.stderr).unwrap()["error"],
        "authorization_denied"
    );
}
#[tokio::test]
async fn stopped_status_does_not_load_server_credentials() {
    let (deps, calls) = dependencies(
        Ok(NativeObservation {
            availability: RegistrationAvailability::Registered,
            desired: Some(DesiredServerState::Stopped),
            process: ProcessObservation::Absent,
            verified: true,
            approved: Some(InstallationId(std::num::NonZeroU64::new(1).unwrap())),
            staged: None,
        }),
        501,
    );
    let (code, io) = run(&["--json", "status"], &deps).await;
    assert_eq!(code, 0);
    assert_eq!(*calls.0.lock().unwrap(), ["inspect"]);
    let value: serde_json::Value = serde_json::from_slice(&io.stdout).unwrap();
    assert_eq!(value["result"]["native"]["desired_state"], "stopped");
    assert!(value["result"]["authenticated"].is_null());
}
#[tokio::test]
async fn status_never_installs_pairs_starts_repairs_or_enables_remote_access() {
    let (deps, calls) = dependencies(Err(CliFailure::NativeUnavailable), 501);
    let (code, io) = run(&["--json", "status"], &deps).await;
    assert_eq!(code, 0);
    assert_eq!(*calls.0.lock().unwrap(), ["inspect"]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&io.stdout).unwrap()["result"]["native_failure"],
        "native_unavailable"
    );
}
#[tokio::test]
async fn locked_storage_is_not_a_missing_installation() {
    let (deps, calls) = dependencies(Err(CliFailure::StorageLocked), 501);
    let (code, io) = run(&["--json", "status"], &deps).await;
    assert_eq!(code, 0);
    assert_eq!(*calls.0.lock().unwrap(), ["inspect"]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&io.stdout).unwrap()["result"]["native_failure"],
        "storage_locked"
    );
}
#[tokio::test]
async fn production_bindings_fail_closed_without_native_or_pairing_fallbacks() {
    let deps = CliDependencies::new(
        Ok(AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        }),
        Arc::new(UnavailableProtocolManagement),
        Arc::new(UnavailableNativeBindings::default()),
        Arc::new(UnavailablePairingBindings),
    );
    assert_eq!(run(&["start"], &deps).await.0, 69);
    assert_eq!(run(&["end-all-sessions", "--assess"], &deps).await.0, 77);
    assert_eq!(run(&["unpair", "--retry-pending"], &deps).await.0, 75);
}

#[test]
fn successful_output_rejects_control_sequences_and_oversize_before_emitting_bytes() {
    for (output, error) in [
        (
            CliOutput::Update {
                available_version: Some("\u{1b}[2J".to_owned()),
            },
            CliFailure::UnsafeNativeState,
        ),
        (
            CliOutput::Update {
                available_version: Some("x".repeat(129)),
            },
            CliFailure::LimitExceeded,
        ),
    ] {
        for json in [false, true] {
            let mut io = Io::default();
            assert_eq!(emit(&output, json, &mut io), Err(error));
            assert!(io.stdout.is_empty());
        }
    }
}
#[test]
fn controlling_terminal_line_is_bounded_and_eof_cannot_confirm() {
    use spaceterm_client_native::cli::tty::read_confirmation_line;
    assert_eq!(
        read_confirmation_line(&mut &b"confirm\n"[..]).unwrap(),
        b"confirm"
    );
    assert_eq!(
        read_confirmation_line(&mut &b"confirm"[..]),
        Err(CliFailure::ConfirmationRequired)
    );
    let mut at_limit = vec![b'x'; 256];
    at_limit.push(b'\n');
    assert_eq!(
        read_confirmation_line(&mut &at_limit[..]).unwrap().len(),
        256
    );
    let over = vec![b'x'; 257];
    assert_eq!(
        read_confirmation_line(&mut &over[..]),
        Err(CliFailure::LimitExceeded)
    );
}
#[test]
fn native_progress_preserves_all_request_bytes_and_uncertainty() {
    use spaceterm_host::native_management::controller::{
        ManagementPhase, ManagementProgress as NativeProgress, ProgressState,
    };
    let output = CliOutput::NativeProgress(NativeProgress {
        request: [0xab; 16],
        phase: ManagementPhase::IntentCommitted,
        state: ProgressState::Indeterminate,
    });
    let mut io = Io::default();
    emit(&output, true, &mut io).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&io.stdout).unwrap();
    assert_eq!(
        value["result"]["native_request"],
        "abababababababababababababababab"
    );
    assert_eq!(output.exit_code(), 75);
}

struct Pairing {
    calls: Arc<Calls>,
    report: UnpairReport,
}
fn self_target() -> spaceterm_core::pairing::PairingTarget {
    use spaceterm_core::{identity::*, pairing::*};
    let server = ServerId::new([1; 16]);
    PairingTarget::new(PairingTargetParts {
        server,
        device: DeviceId::new([2; 16]),
        pairing: PairingId::new(server, 3).unwrap(),
    })
    .unwrap()
}
fn pending_report() -> UnpairReport {
    let target = self_target();
    let parts = target.parts();
    UnpairReport {
        server: parts.server,
        device: parts.device,
        pairing: parts.pairing,
        local_connection_removed: true,
        revocation: RevocationState::Pending,
    }
}
impl PairingCommandBindings for Pairing {
    fn enroll<'a>(
        &'a self,
        _: Option<spaceterm_client_native::cli::arguments::PublicFingerprint>,
        _: bool,
        _: SudoMode,
        _: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput> {
        panic!("retry must not enroll")
    }
    fn authorize(
        &self,
        _: spaceterm_client_native::cli::arguments::PublicRequestId,
        _: spaceterm_client_native::cli::arguments::PublicFingerprint,
    ) -> ManagementFuture<'_, ManagementProgress> {
        panic!("unexpected authorization")
    }
    fn list_page(
        &self,
        cursor: Option<InventoryCursor>,
    ) -> ManagementFuture<'_, PairingInventoryPage> {
        self.calls.0.lock().unwrap().push("list_page");
        let target = self_target();
        let parts = target.parts();
        let pairings = if cursor.is_none() { 1..=128 } else { 129..=256 };
        let next = cursor
            .is_none()
            .then(|| InventoryCursor::new("page2".to_owned()).unwrap());
        let devices = pairings
            .map(|local| PublicPairedDevice {
                server: parts.server,
                device: parts.device,
                pairing: spaceterm_core::identity::PairingId::new(parts.server, local).unwrap(),
            })
            .collect();
        Box::pin(async move { Ok(PairingInventoryPage { devices, next }) })
    }
    fn assess_unpair(
        &self,
        device: spaceterm_core::identity::DeviceId,
        token: Option<AssessmentToken>,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        use spaceterm_core::{
            authority::*, identity::*, observations::MonotonicTick, operations::*, pairing::*,
        };
        self.calls.0.lock().unwrap().push("assess_unpair");
        let target = self_target();
        assert_eq!(device, target.parts().device);
        let parts = target.parts();
        let client = ClientId::new(parts.server, 4).unwrap();
        let stream = OperationStreamId::new(
            OperationStreamKey::new(parts.server, 5).unwrap(),
            parts.pairing,
            client,
        )
        .unwrap();
        let reference = AssessmentRef {
            operation: OperationId::new(stream, OperationSequence::new(1).unwrap()),
            digest: RequestDigest::from_bytes([1; 32]),
        };
        let evidence = UnpairAssessment::new(UnpairAssessmentParts {
            reference,
            target: target.clone(),
            authorization_version: FieldVersion::new(1),
            clients: vec![ClientRecord {
                id: client,
                pairing: parts.pairing,
            }],
        })
        .unwrap();
        let assessment = ManagementAssessment::new(
            token.unwrap_or_else(|| AssessmentToken::new("unpair_1".to_owned()).unwrap()),
            ManagementAction::Unpair { target },
            AssessmentEvidence::Unpair(evidence),
            MonotonicTick::new(0),
            MonotonicTick::new(30_000),
        )
        .unwrap();
        Box::pin(async move { Ok(assessment) })
    }
    fn unpair(&self, _: ConfirmedManagementAction) -> ManagementFuture<'_, UnpairReport> {
        self.calls.0.lock().unwrap().push("unpair");
        Box::pin(async { Ok(self.report.clone()) })
    }
    fn retry_pending(&self) -> ManagementFuture<'_, Vec<UnpairReport>> {
        self.calls.0.lock().unwrap().push("retry_pending");
        Box::pin(async { Ok(vec![pending_report()]) })
    }
}
fn pairing_dependencies() -> (CliDependencies, Arc<Calls>) {
    pairing_dependencies_with_report(pending_report())
}
fn pairing_dependencies_with_report(report: UnpairReport) -> (CliDependencies, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    let deps = CliDependencies::new(
        Ok(AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        }),
        Arc::new(Protocol {
            calls: calls.clone(),
            hang_status: false,
            resolution: None,
        }),
        Arc::new(Native {
            calls: calls.clone(),
            status: Err(CliFailure::NativeUnavailable),
        }),
        Arc::new(Pairing {
            calls: calls.clone(),
            report,
        }),
    );
    (deps, calls)
}
#[tokio::test]
async fn self_unpair_reports_local_removal_separately_from_revocation() {
    let (deps, calls) = pairing_dependencies();
    let device = hex(&[2; 16]);
    let (code, io) = run(
        &[
            "--json",
            "--non-interactive",
            "unpair",
            "--device",
            &device,
            "--confirm",
            "unpair_1",
        ],
        &deps,
    )
    .await;
    assert_eq!(code, 75);
    assert_eq!(*calls.0.lock().unwrap(), ["assess_unpair", "unpair"]);
    let result: serde_json::Value = serde_json::from_slice(&io.stdout).unwrap();
    assert_eq!(result["result"]["local_connection_removed"], true);
    assert_eq!(result["result"]["revocation"], "pending");
}
#[tokio::test]
async fn pending_revocation_retry_cannot_start_enroll_or_update() {
    let (deps, calls) = pairing_dependencies();
    let (code, io) = run(&["--json", "unpair", "--retry-pending"], &deps).await;
    assert_eq!(code, 75);
    assert_eq!(*calls.0.lock().unwrap(), ["retry_pending"]);
    assert!(io.stderr.is_empty());
}
#[tokio::test]
async fn paired_devices_are_printed_as_bounded_pages() {
    let (deps, calls) = pairing_dependencies();
    let (code, io) = run(&["--json", "pair", "list"], &deps).await;
    assert_eq!(code, 0);
    assert_eq!(*calls.0.lock().unwrap(), ["list_page", "list_page"]);
    let pages = io
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(pages.len(), 2);
    assert!(
        pages
            .iter()
            .all(|p| p["result"]["devices"].as_array().unwrap().len() == 128)
    );
}

struct WrongNativeResolution;
impl NativeCommandBindings for WrongNativeResolution {
    fn inspect(&self, _: AccountBuildKey) -> ManagementFuture<'_, NativeObservation> {
        Box::pin(async { Err(CliFailure::NativeUnavailable) })
    }
    fn run<'a>(
        &'a self,
        _: AccountBuildKey,
        command: NativeCliCommand,
        _: ConfirmationMode,
        _: SudoMode,
        _: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput> {
        assert!(matches!(command, NativeCliCommand::Resolve { .. }));
        use spaceterm_host::native_management::controller::{
            ManagementPhase, ManagementProgress as NativeProgress, ProgressState,
        };
        Box::pin(async {
            Ok(CliOutput::NativeProgress(NativeProgress {
                request: [2; 16],
                phase: ManagementPhase::Completed,
                state: ProgressState::Complete,
            }))
        })
    }
}
#[tokio::test]
async fn native_resolution_cannot_report_another_request_as_completed() {
    let deps = CliDependencies::new(
        Ok(AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        }),
        Arc::new(UnavailableProtocolManagement),
        Arc::new(WrongNativeResolution),
        Arc::new(UnavailablePairingBindings),
    );
    let request = format!("native:{}", hex(&[1; 16]));
    let (code, io) = run(&["--json", "status", "--operation", &request], &deps).await;
    assert_eq!(code, 70);
    assert!(io.stdout.is_empty());
}
#[test]
fn native_completion_requires_the_completed_journal_phase() {
    use spaceterm_host::native_management::controller::{
        ManagementPhase, ManagementProgress as NativeProgress, ProgressState,
    };
    let mut io = Io::default();
    let output = CliOutput::NativeProgress(NativeProgress {
        request: [1; 16],
        phase: ManagementPhase::IntentCommitted,
        state: ProgressState::Complete,
    });
    assert_eq!(
        emit(&output, true, &mut io),
        Err(CliFailure::UnsafeNativeState)
    );
    assert!(io.stdout.is_empty());
}

#[test]
fn status_separates_committed_remote_policy_from_listener_failure() {
    use spaceterm_core::{
        authority::*, identity::ServerId, management::*, organization::Versioned,
        remote_access::RemoteAccessPolicy,
    };
    let server = ServerId::new([1; 16]);
    let endpoint = "127.0.0.1:4000".parse().unwrap();
    let status = ServerStatus::new(ServerStatusParts {
        server,
        epoch: ServerEpoch::new(1),
        revision: std::num::NonZeroU64::new(1).unwrap(),
        running: None,
        approved: None,
        staged: None,
        native_verification: NativeVerification::Unverified,
        capabilities: vec![],
        remote_access: Versioned {
            version: FieldVersion::new(1),
            value: RemoteAccessPolicy::enabled(server, vec![endpoint]).unwrap(),
        },
        listeners: vec![ListenerStatus {
            generation: std::num::NonZeroU64::new(1).unwrap(),
            endpoint,
            state: ListenerState::Unavailable,
        }],
        progress: vec![],
    })
    .unwrap();
    let output = CliOutput::Status(LocalStatus {
        native: None,
        native_failure: Some(CliFailure::NativeUnavailable),
        authenticated: Some(status),
        negotiated_protocol_major: Some(2),
        authentication_failure: None,
    });
    let mut io = Io::default();
    emit(&output, true, &mut io).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&io.stdout).unwrap();
    assert_eq!(
        value["result"]["authenticated"]["remote_access_enabled"],
        true
    );
    assert_eq!(
        value["result"]["authenticated"]["listeners"][0]["state"],
        "Unavailable"
    );
}

#[tokio::test]
async fn unpair_completion_cannot_be_substituted_for_another_assessed_pairing() {
    use spaceterm_core::identity::{DeviceId, PairingId, ServerId};
    for mismatch in 0..3 {
        let mut report = pending_report();
        report.revocation = RevocationState::Confirmed;
        match mismatch {
            0 => report.device = DeviceId::new([3; 16]),
            1 => report.pairing = PairingId::new(report.server, 4).unwrap(),
            _ => {
                report.server = ServerId::new([3; 16]);
                report.pairing = PairingId::new(report.server, 3).unwrap();
            }
        }
        let (deps, calls) = pairing_dependencies_with_report(report);
        let device = hex(&[2; 16]);
        let (code, io) = run(
            &[
                "--json",
                "unpair",
                "--device",
                &device,
                "--confirm",
                "unpair_1",
            ],
            &deps,
        )
        .await;
        assert_eq!(code, 70);
        assert!(io.stdout.is_empty());
        assert_eq!(*calls.0.lock().unwrap(), ["assess_unpair", "unpair"]);
    }
}

#[tokio::test]
async fn protocol_resolution_cannot_report_another_operation_as_completed() {
    use spaceterm_core::{management::*, operations::*};
    let original = "11111111111111111111111111111111:2:3:4:5";
    let CliCommand::Status {
        operation: Some(spaceterm_client_native::cli::arguments::RequestSelector::Protocol(id)),
    } = parse(["status", "--operation", original].map(std::ffi::OsString::from))
        .unwrap()
        .command
    else {
        panic!("operation")
    };
    let calls = Arc::new(Calls::default());
    let deps = CliDependencies::new(
        Ok(AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        }),
        Arc::new(Protocol {
            calls: calls.clone(),
            hang_status: false,
            resolution: Some(ManagementProgress {
                request: ManagementRequestReference::Operation(OperationBinding {
                    id: OperationId::new(id.stream(), OperationSequence::new(6).unwrap()),
                    digest: RequestDigest::from_bytes([1; 32]),
                }),
                state: ManagementProgressState::Completed,
                completed_units: 1,
                total_units: 1,
            }),
        }),
        Arc::new(UnavailableNativeBindings::default()),
        Arc::new(UnavailablePairingBindings),
    );
    let (code, io) = run(&["--json", "status", "--operation", original], &deps).await;
    assert_eq!(code, 70);
    assert!(io.stdout.is_empty());
    assert_eq!(*calls.0.lock().unwrap(), ["resolve"]);
}

#[tokio::test]
async fn controller_bounds_protocol_status_reads_without_starting_or_submitting_work() {
    let calls = Arc::new(Calls::default());
    let deps = CliDependencies::new(
        Ok(AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        }),
        Arc::new(Protocol {
            calls: calls.clone(),
            hang_status: true,
            resolution: None,
        }),
        Arc::new(Native {
            calls: calls.clone(),
            status: Ok(NativeObservation {
                availability: RegistrationAvailability::Registered,
                desired: Some(DesiredServerState::Running),
                process: ProcessObservation::Observed,
                verified: false,
                approved: None,
                staged: None,
            }),
        }),
        Arc::new(UnavailablePairingBindings),
    );
    let (code, io) = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        run(&["--json", "status"], &deps),
    )
    .await
    .expect("the status read must have a five-second deadline");
    assert_eq!(code, 0);
    let status: serde_json::Value = serde_json::from_slice(&io.stdout).unwrap();
    assert_eq!(status["result"]["authentication_failure"], "offline");
    assert_eq!(*calls.0.lock().unwrap(), ["inspect", "protocol_status"]);
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NativeCall {
    key: AccountBuildKey,
    command: NativeCliCommand,
    confirmation: ConfirmationMode,
    mode: SudoMode,
}
struct RoutingNative(Arc<Mutex<Vec<NativeCall>>>);
impl NativeCommandBindings for RoutingNative {
    fn inspect(&self, _: AccountBuildKey) -> ManagementFuture<'_, NativeObservation> {
        panic!("management dispatch must not perform implicit discovery")
    }
    fn run<'a>(
        &'a self,
        key: AccountBuildKey,
        command: NativeCliCommand,
        confirmation: ConfirmationMode,
        mode: SudoMode,
        _: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput> {
        self.0.lock().unwrap().push(NativeCall {
            key,
            command,
            confirmation,
            mode,
        });
        Box::pin(async { Err(CliFailure::AuthorizationUnavailable) })
    }
}

#[tokio::test]
async fn native_dispatch_preserves_exact_action_scope_confirmation_and_sudo_mode() {
    let key = AccountBuildKey {
        account_uid: 501,
        build_identity: BuildIdentity::Development,
    };
    let calls = Arc::new(Mutex::new(vec![]));
    let deps = CliDependencies::new(
        Ok(key),
        Arc::new(UnavailableProtocolManagement),
        Arc::new(RoutingNative(calls.clone())),
        Arc::new(UnavailablePairingBindings),
    );
    let CliCommand::UpdateStage {
        version: Some(version),
    } = parse(["update", "stage", "--version", "1.2.3"].map(std::ffi::OsString::from))
        .unwrap()
        .command
    else {
        panic!("version")
    };
    let installation = InstallationId(std::num::NonZeroU64::new(7).unwrap());
    let token = AssessmentToken::new("original_1".to_owned()).unwrap();
    for (args, command, confirmation, mode) in [
        (
            vec!["start"],
            NativeCliCommand::Start,
            ConfirmationMode::Interactive,
            SudoMode::Interactive,
        ),
        (
            vec!["--non-interactive", "start"],
            NativeCliCommand::Start,
            ConfirmationMode::Interactive,
            SudoMode::NonInteractive,
        ),
        (
            vec!["stop", "--assess"],
            NativeCliCommand::Stop,
            ConfirmationMode::Assess,
            SudoMode::Interactive,
        ),
        (
            vec!["--non-interactive", "stop", "--confirm", "original_1"],
            NativeCliCommand::Stop,
            ConfirmationMode::Token(token.clone()),
            SudoMode::NonInteractive,
        ),
        (
            vec![
                "remote-access",
                "enable",
                "--bind",
                "0.0.0.0:0",
                "--bind",
                "[::]:4000",
                "--confirm",
                "original_1",
            ],
            NativeCliCommand::RemoteAccess {
                endpoints: vec!["0.0.0.0:0".parse().unwrap(), "[::]:4000".parse().unwrap()],
            },
            ConfirmationMode::Token(token.clone()),
            SudoMode::Interactive,
        ),
        (
            vec!["remote-access", "disable", "--assess"],
            NativeCliCommand::RemoteAccess { endpoints: vec![] },
            ConfirmationMode::Assess,
            SudoMode::Interactive,
        ),
        (
            vec!["update", "check"],
            NativeCliCommand::UpdateCheck,
            ConfirmationMode::Assess,
            SudoMode::Interactive,
        ),
        (
            vec!["--non-interactive", "update", "stage", "--version", "1.2.3"],
            NativeCliCommand::UpdateStage {
                version: Some(version),
            },
            ConfirmationMode::Interactive,
            SudoMode::NonInteractive,
        ),
        (
            vec![
                "update",
                "activate",
                "--installation",
                "7",
                "--confirm",
                "original_1",
            ],
            NativeCliCommand::UpdateActivate { installation },
            ConfirmationMode::Token(token),
            SudoMode::Interactive,
        ),
    ] {
        let (code, io) = run(&args, &deps).await;
        assert_eq!(code, 69);
        assert_eq!(io.confirmations, 0);
        assert_eq!(
            calls.lock().unwrap().pop().unwrap(),
            NativeCall {
                key,
                command,
                confirmation,
                mode
            }
        );
        assert!(calls.lock().unwrap().is_empty());
    }
}
