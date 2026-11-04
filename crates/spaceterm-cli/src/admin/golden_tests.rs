//! Golden envelopes for every machine verb, built by the same functions the commands use.
use super::{lifecycle, status, unpair, update};
use crate::contract::golden::assert_golden;
use crate::contract::{
    Code, CoreSetup, CoreStart, CoreUnpair, DeviceRef, Envelope, Failure, HeldOutcomes, Launcher,
    MachineRef, Receipt, ReceiptState, RecoveredIdentity, RecoveredKind, RecoveredOperation,
    RecoveredOutcome, RecoveredState, ServiceAction, SetupNetwork, SetupStep, SetupStepName,
    StepResult, Version,
};
use spaceterm_core::diagnostics::{PaneWorkerWaits, TerminalCounterSnapshot};
use spaceterm_core::service::lane::client::Outcome;
use spaceterm_core::service::lane::{
    LaneAction, LaneActivation, LaneDevice, LaneEffects, LaneNetwork, LaneOperationState,
    LaneReceipt, LaneStatus, LaneVersion,
};
use spaceterm_core::update::facts::{
    CheckFailure, Operation, PendingVersion, Platform, Protocols, StrandedDevice, UpdateCheck,
    UpdateFacts, Version as CoreVersion,
};

const EXPIRES: &str = "2026-10-30T12:10:00Z";
const OPERATION: &str = "0123456789abcdef0123456789abcdef";
const BOOT: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const NEW_BOOT: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn core() -> MachineRef {
    MachineRef {
        name: "penguin".into(),
        route: format!("id:{}", "4e".repeat(32)),
        id: "5b".repeat(32),
    }
}

fn local() -> MachineRef {
    MachineRef {
        name: "local".into(),
        route: "local".into(),
        id: "5b".repeat(32),
    }
}

fn lane_version(version: &str, digit: char) -> LaneVersion {
    LaneVersion {
        version: version.into(),
        commit: digit.to_string().repeat(40),
        sha256: digit.to_string().repeat(64),
    }
}

fn version(version: &str, digit: char) -> Version {
    status::version(&lane_version(version, digit))
}

fn recovered() -> Vec<RecoveredOperation> {
    vec![RecoveredOperation {
        operation: "fedcba9876543210fedcba9876543210".into(),
        identity: RecoveredIdentity::Base,
        kind: RecoveredKind::Service,
        outcome: RecoveredOutcome {
            state: RecoveredState::Applied,
            code: None,
            target: None,
        },
    }]
}

fn assert_exit<D: serde::Serialize>(envelope: &Envelope<D>, exit: u8) {
    assert_eq!(envelope.exit(), exit);
}

#[test]
fn status_reports_service_network_devices_and_held_outcomes() {
    let lane = LaneStatus {
        running: true,
        stopped: false,
        installed: true,
        automatic_start: true,
        linger: Some(true),
        runtime: lane_version("0.7.0", 'a'),
        update: Some(Box::new(facts(
            Some(pending(true, true)),
            UpdateCheck::Failing {
                since: FAILING,
                last_success: Some(SUCCEEDED),
                reason: CheckFailure::Proxy,
            },
        ))),
        activation: LaneActivation::Settled,
        sessions: 2,
        network: LaneNetwork {
            enabled: true,
            bind: Some("0.0.0.0:9231".into()),
            listening: true,
            addresses: vec!["192.0.2.10".parse().unwrap()],
        },
        devices: vec![LaneDevice {
            id: "9c".repeat(32),
            name: "Laptop".into(),
            fingerprint: "ABCD-EFGH".into(),
            last_connected_at: Some("2026-10-30T11:00:00Z".into()),
        }],
        lane_major: 1,
        terminal_counters: Some(TerminalCounterSnapshot {
            pty_read_attempts: 6,
            pty_data_reads: 4,
            pty_read_bytes: 10_000,
            pane_worker_waits: PaneWorkerWaits {
                output: 4,
                deadline: 2,
                ..PaneWorkerWaits::default()
            },
            ..TerminalCounterSnapshot::default()
        }),
    };
    let held = vec![HeldOutcomes {
        identity: RecoveredIdentity::Agent("agent-1".parse().unwrap()),
        count: 3,
    }];
    let (envelope, text) = status::envelope(core(), &lane, held, recovered(), RESTART);
    assert_exit(&envelope, 0);
    assert_golden("status", &envelope);
    assert!(
        text.contains(
            "Pending: 0.8.0, installed 2026-11-03T09:12:00Z, restart overdue. Restart to use it: \
             spaceterm --machine penguin restart\n  It changes the protocol. These devices stop \
             working until they update: old-mini\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "Update check: failing since 2026-11-02T08:00:00Z, the proxy is unreachable\n"
        ),
        "{text}"
    );
}

const INSTALLED: u64 = 1_793_697_120;
const SUCCEEDED: u64 = 1_793_520_000;
const FAILING: u64 = 1_793_606_400;
const RESTART: &str = "spaceterm --machine penguin restart";

fn pending(breaking: bool, overdue: bool) -> PendingVersion {
    PendingVersion {
        version: CoreVersion::parse("0.8.0").unwrap(),
        protocols: Protocols::CURRENT,
        breaking,
        installed_at: INSTALLED,
        overdue_since: overdue.then_some(INSTALLED + 86_400),
        stranded: if breaking {
            vec![StrandedDevice {
                pairing: [0x9c; 32],
                name: "old-mini".into(),
            }]
        } else {
            Vec::new()
        },
    }
}

fn facts(pending: Option<PendingVersion>, check: UpdateCheck) -> UpdateFacts {
    UpdateFacts {
        revision: 4,
        platform: Platform::LinuxX86_64,
        running: CoreVersion::parse("0.7.0").unwrap(),
        pending,
        sessions: 2,
        operation: None,
        check,
        skipped: None,
        last_failure: None,
    }
}

#[test]
fn status_reports_each_update_check_state() {
    use crate::contract::{UpdateCheckReason, UpdateCheckState};
    let check = |check, operation| {
        let mut facts = facts(None, check);
        facts.operation = operation;
        status::update_check(Some(&facts))
    };
    let never = check(UpdateCheck::Never, None);
    assert_eq!(never.state, UpdateCheckState::Never);
    assert_eq!(
        (never.last_success_at, never.failing_since, never.reason),
        (None, None, None)
    );
    assert_eq!(
        check(UpdateCheck::Disabled, None).state,
        UpdateCheckState::Disabled
    );
    let succeeded = check(UpdateCheck::Succeeded { at: SUCCEEDED }, None);
    assert_eq!(succeeded.state, UpdateCheckState::Succeeded);
    assert_eq!(
        succeeded.last_success_at.as_deref(),
        Some("2026-11-01T08:00:00Z")
    );
    let failing = check(
        UpdateCheck::Failing {
            since: FAILING,
            last_success: None,
            reason: CheckFailure::SystemTooOld,
        },
        None,
    );
    assert_eq!(failing.state, UpdateCheckState::Failing);
    assert_eq!(failing.last_success_at, None);
    assert_eq!(
        failing.failing_since.as_deref(),
        Some("2026-11-02T08:00:00Z")
    );
    assert_eq!(failing.reason, Some(UpdateCheckReason::SystemTooOld));
    let checking = check(
        UpdateCheck::Failing {
            since: FAILING,
            last_success: Some(SUCCEEDED),
            reason: CheckFailure::Network,
        },
        Some(Operation::Checking),
    );
    assert_eq!(checking.state, UpdateCheckState::Checking);
    assert_eq!(
        (checking.failing_since, checking.reason),
        (None, None),
        "only a failing state names its failure"
    );
    assert_eq!(
        checking.last_success_at.as_deref(),
        Some("2026-11-01T08:00:00Z")
    );
    assert_eq!(status::update_check(None).state, UpdateCheckState::Never);
}

#[test]
fn status_of_an_unreachable_machine_exits_5() {
    let envelope = status::unreachable_envelope(
        core(),
        Failure::new(Code::Unreachable),
        Some("2026-10-30T11:00:00Z".into()),
    );
    assert_exit(&envelope, 5);
    assert_golden("status_unreachable", &envelope);
}

fn stop_effects() -> LaneEffects {
    LaneEffects {
        ends_sessions: 1,
        sessions: vec!["0e".repeat(32)],
        start_requires_machine: true,
        from: None,
        to: None,
        to_major: None,
    }
}

#[test]
fn a_stop_preview_names_the_sessions_it_ends_and_exits_3() {
    let envelope = lifecycle::preview_envelope(
        "stop",
        core(),
        format!("c1-{}", "ab".repeat(32)),
        EXPIRES.into(),
        lifecycle::service_effects(&stop_effects(), None),
        Vec::new(),
    );
    assert_exit(&envelope, 3);
    assert_golden("stop_preview", &envelope);
}

/// The lane's preview of a restart into the Pending Version 0.8.0 on work major `to_major`.
fn restart_effects(to_major: u16) -> LaneEffects {
    LaneEffects {
        ends_sessions: 1,
        sessions: vec!["0e".repeat(32)],
        start_requires_machine: false,
        from: Some(lane_version("0.7.0", 'a')),
        to: Some(lane_version("0.8.0", 'b')),
        to_major: Some(to_major),
    }
}

#[test]
fn a_breaking_restart_preview_names_its_versions_and_stranded_devices_and_warns() {
    let next = spaceterm_core::protocol::MAJOR + 1;
    let facts = facts(Some(pending(true, false)), UpdateCheck::Never);
    let previewed = lifecycle::service_effects(&restart_effects(next), Some(&facts));
    assert_eq!(
        lifecycle::summary(lifecycle::Kind::Restart, "penguin", &previewed),
        "Restart SpaceTerm Core on penguin from 0.7.0 into 0.8.0?\nAll Terminal Sessions on it \
         end for every client. Workspaces are kept, and each Pane starts a new shell when you use \
         it.\n0.8.0 changes the protocol. These devices can\u{2019}t connect until they update: \
         old-mini\n"
    );
    let envelope = lifecycle::preview_envelope(
        "restart",
        core(),
        format!("c1-{}", "cd".repeat(32)),
        EXPIRES.into(),
        previewed,
        Vec::new(),
    );
    assert_exit(&envelope, 3);
    assert_golden("restart_preview", &envelope);
}

#[test]
fn a_restart_into_a_compatible_pending_version_names_it_without_a_warning() {
    let compatible = facts(Some(pending(false, false)), UpdateCheck::Never);
    let previewed = lifecycle::service_effects(
        &restart_effects(spaceterm_core::protocol::MAJOR),
        Some(&compatible),
    );
    let target = previewed.effects.pending.clone().unwrap();
    assert_eq!(
        (target.from.as_str(), target.to.as_str()),
        ("0.7.0", "0.8.0")
    );
    assert!(!target.breaking && target.stranded.is_empty());
    assert_eq!(
        lifecycle::summary(lifecycle::Kind::Restart, "penguin", &previewed),
        "Restart SpaceTerm Core on penguin from 0.7.0 into 0.8.0?\nAll Terminal Sessions on it \
         end for every client. Workspaces are kept, and each Pane starts a new shell when you use \
         it.\n"
    );
    let envelope = lifecycle::preview_envelope(
        "restart",
        core(),
        "c1-00".into(),
        EXPIRES.into(),
        previewed,
        Vec::new(),
    );
    assert!(envelope.warnings.is_empty());
    // Facts of another Pending Version say nothing about this one.
    let other = facts(Some(pending(true, false)), UpdateCheck::Never);
    let mut moved = restart_effects(spaceterm_core::protocol::MAJOR);
    moved.to = Some(lane_version("0.9.0", 'c'));
    let target = lifecycle::service_effects(&moved, Some(&other))
        .effects
        .pending
        .unwrap();
    assert!(!target.breaking && target.stranded.is_empty());
    // A restart into the running version names no versions.
    assert_eq!(
        lifecycle::service_effects(&stop_effects(), None)
            .effects
            .pending,
        None
    );
}

fn receipt(
    action: LaneAction,
    state: LaneOperationState,
    code: Option<&str>,
    boot: &str,
) -> LaneReceipt {
    LaneReceipt {
        operation: OPERATION.into(),
        action,
        state,
        code: code.map(Into::into),
        boot: boot.into(),
    }
}

#[test]
fn a_remote_stop_is_unconfirmed_with_an_observed_shutdown() {
    let settled = lifecycle::settle(
        ServiceAction::Stop,
        Outcome::Shutdown {
            operation: OPERATION.into(),
            boot: BOOT.into(),
        },
    );
    assert!(!settled.acknowledged);
    let envelope = lifecycle::operation_envelope("stop", core(), settled, Vec::new());
    assert_exit(&envelope, 7);
    assert_golden("stop_shutdown", &envelope);
}

#[test]
fn a_restart_that_returned_with_a_new_boot_is_done() {
    let settled = lifecycle::settle(
        ServiceAction::Restart,
        Outcome::Settled {
            receipt: receipt(
                LaneAction::Restart,
                LaneOperationState::Done,
                None,
                NEW_BOOT,
            ),
            boot: BOOT.into(),
        },
    );
    assert!(settled.acknowledged);
    let envelope = lifecycle::operation_envelope("restart", core(), settled, recovered());
    assert_exit(&envelope, 0);
    assert_golden("restart_done", &envelope);
}

#[test]
fn failed_not_performed_and_unconfirmed_restarts_map_to_their_codes() {
    for (name, outcome, exit) in [
        (
            "restart_failed",
            Outcome::Settled {
                receipt: receipt(
                    LaneAction::Restart,
                    LaneOperationState::Failed,
                    Some("startup_failed"),
                    NEW_BOOT,
                ),
                boot: BOOT.into(),
            },
            1,
        ),
        (
            "restart_not_performed",
            Outcome::Settled {
                receipt: receipt(
                    LaneAction::Restart,
                    LaneOperationState::NotPerformed,
                    Some("operation_failed"),
                    NEW_BOOT,
                ),
                boot: BOOT.into(),
            },
            1,
        ),
        (
            "restart_unconfirmed",
            Outcome::Unconfirmed {
                operation: OPERATION.into(),
                boot: BOOT.into(),
                last: Some(receipt(
                    LaneAction::Restart,
                    LaneOperationState::Running,
                    None,
                    BOOT,
                )),
            },
            7,
        ),
    ] {
        let settled = lifecycle::settle(ServiceAction::Restart, outcome);
        let envelope = lifecycle::operation_envelope("restart", core(), settled, Vec::new());
        assert_exit(&envelope, exit);
        assert_golden(name, &envelope);
    }
}

#[test]
fn update_reports_the_version_it_installed_and_how_to_use_it() {
    let installed = facts(
        Some(pending(false, false)),
        UpdateCheck::Succeeded { at: SUCCEEDED },
    );
    let envelope = update::envelope(core(), true, &installed, Vec::new());
    assert_exit(&envelope, 0);
    assert_golden("update_installed", &envelope);
    assert_eq!(
        update::text(true, &installed, RESTART),
        "Installed SpaceTerm Core 0.8.0 beside 0.7.0. Restart to use it; restarting ends 2 \
         Terminal Sessions: spaceterm --machine penguin restart\n"
    );
    assert_eq!(
        update::text(false, &installed, RESTART),
        "SpaceTerm Core 0.8.0 is already installed beside 0.7.0. Restart to use it; restarting \
         ends 2 Terminal Sessions: spaceterm --machine penguin restart\n"
    );
    let mut idle = installed.clone();
    idle.sessions = 0;
    assert_eq!(
        update::text(true, &idle, RESTART),
        "Installed SpaceTerm Core 0.8.0 beside 0.7.0. SpaceTerm Core restarts into it by itself \
         while no Terminal Session is open.\n"
    );
    let mut breaking = idle.clone();
    breaking.pending = Some(pending(true, false));
    assert_eq!(
        update::text(true, &breaking, RESTART),
        "Installed SpaceTerm Core 0.8.0 beside 0.7.0. Restart to use it: spaceterm --machine \
         penguin restart\n",
        "a breaking version never restarts by itself"
    );
    let current = facts(None, UpdateCheck::Succeeded { at: SUCCEEDED });
    let envelope = update::envelope(local(), false, &current, Vec::new());
    assert_exit(&envelope, 0);
    assert_golden("update_current", &envelope);
    assert_eq!(
        update::text(false, &current, RESTART),
        "SpaceTerm Core 0.7.0 is up to date.\n"
    );
}

#[test]
fn update_installs_directly_only_while_the_local_core_is_not_running() {
    for (code, offline) in [
        (Code::NotRunning, true),
        (Code::Unreachable, true),
        (Code::ServiceBusy, false),
        (Code::IncompatibleProtocol, false),
        (Code::AuthenticationRefused, false),
        (Code::ProtocolError, false),
    ] {
        assert_eq!(update::offline(&Failure::new(code)), offline, "{code:?}");
    }
}

#[test]
fn update_refusals_have_their_own_codes() {
    for (code, exit) in [
        (Code::NotNewer, 4),
        (Code::VersionSkipped, 4),
        (Code::NoUpdateFeed, 5),
        (Code::FeedUnreachable, 5),
        (Code::ProxyUnreachable, 5),
        (Code::SystemTooOld, 5),
        (Code::DiskFull, 1),
        (Code::DownloadFailed, 1),
    ] {
        assert_eq!(crate::contract::from_lane_code(code.as_str()).code, code);
        assert_eq!(code.exit(), exit, "{code:?}");
    }
    let envelope = Envelope::error(
        "update",
        Some(core()),
        Failure::new(Code::NoUpdateFeed),
        crate::contract::Empty {},
    );
    assert_golden("update_no_feed", &envelope);
}

#[test]
fn unpair_previews_then_revokes_one_exact_device() {
    let device = DeviceRef {
        id: "9c".repeat(32),
        name: "Laptop".into(),
    };
    let preview = unpair::preview_envelope(
        local(),
        format!("c1-{}", "12".repeat(32)),
        EXPIRES.into(),
        device.clone(),
    );
    assert_exit(&preview, 3);
    assert_golden("unpair_preview", &preview);
    let revoked = Envelope::ok("unpair", Some(local()), CoreUnpair { revoked: device });
    assert_exit(&revoked, 0);
    assert_golden("unpair_revoked", &revoked);
}

#[test]
fn unpair_lists_no_label_for_ambiguous_names() {
    let device = |id: u8| spaceterm_core::direct::DeviceInfo {
        pairing: [id; 32],
        client: [id; 32],
        name: "Laptop".into(),
        public_key: [id; 32],
        last_connected_at_unix_seconds: None,
    };
    let devices = [device(1), device(2)];
    let failure = unpair::select(&devices, "Laptop").unwrap_err();
    let envelope = Envelope::error("unpair", Some(local()), failure, crate::contract::Empty {});
    assert_exit(&envelope, 4);
    assert_golden("unpair_ambiguous", &envelope);
    assert_eq!(
        unpair::select(&devices, "id:010101").unwrap().pairing,
        [1; 32]
    );
    assert_eq!(
        unpair::select(&devices, "id:0101").unwrap_err().code,
        Code::InvalidArguments
    );
}

#[test]
fn setup_reports_each_step() {
    let envelope = Envelope::ok(
        "setup",
        Some(local()),
        CoreSetup {
            steps: vec![
                SetupStep {
                    step: SetupStepName::InstallService,
                    result: StepResult::Done,
                },
                SetupStep {
                    step: SetupStepName::Linger,
                    result: StepResult::AlreadyDone,
                },
                SetupStep {
                    step: SetupStepName::Start,
                    result: StepResult::Done,
                },
                SetupStep {
                    step: SetupStepName::Network,
                    result: StepResult::Done,
                },
                SetupStep {
                    step: SetupStepName::AutomaticStart,
                    result: StepResult::Skipped,
                },
                SetupStep {
                    step: SetupStepName::UpdateProxy,
                    result: StepResult::Done,
                },
            ],
            runtime: version("0.7.0", 'a'),
            network: SetupNetwork {
                enabled: true,
                bind: Some("0.0.0.0:9231".into()),
                addresses: vec!["192.0.2.10".parse().unwrap()],
            },
            automatic_start: true,
            launcher: Launcher::Linked,
            path_ok: true,
            update_proxy: Some("http://proxy.example:3128".into()),
        },
    );
    assert_exit(&envelope, 0);
    assert_golden("setup", &envelope);
}

#[test]
fn start_succeeds_locally_and_is_local_only_remotely() {
    let started = Envelope::ok("start", Some(local()), CoreStart { running: true });
    assert_exit(&started, 0);
    assert_golden("start", &started);
    let remote = Envelope::error(
        "start",
        None,
        Failure::new(Code::LocalOnly),
        crate::contract::Empty {},
    );
    assert_exit(&remote, 5);
    assert_golden("start_local_only", &remote);
}

#[test]
fn pair_without_a_terminal_needs_interaction() {
    let envelope = Envelope::error(
        "pair",
        None,
        Failure::new(Code::InteractionRequired),
        crate::contract::Empty {},
    );
    assert_exit(&envelope, 3);
    assert_golden("pair_interaction_required", &envelope);
}

#[test]
fn receipts_keep_the_lane_state_and_code() {
    let receipt = lifecycle::receipt_of(&receipt(
        LaneAction::Restart,
        LaneOperationState::Failed,
        Some("startup_failed"),
        BOOT,
    ));
    assert_eq!(
        receipt,
        Receipt {
            operation: OPERATION.into(),
            action: ServiceAction::Restart,
            state: ReceiptState::Failed,
            code: Some(Code::StartupFailed),
            boot: BOOT.into(),
        }
    );
}
