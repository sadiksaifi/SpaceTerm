//! Golden envelopes for every `server.*` command, built by the same functions the commands use.
use super::{lifecycle, status, unpair, update};
use crate::contract::golden::assert_golden;
use crate::contract::{
    Code, DeviceRef, Envelope, Failure, HeldOutcomes, Launcher, Receipt, ReceiptState,
    RecoveredIdentity, RecoveredKind, RecoveredOperation, RecoveredOutcome, RecoveredState,
    ServerRef, ServerSetup, ServerStart, ServerUnpair, ServiceAction, SetupNetwork, SetupStep,
    SetupStepName, StepResult, UpdateEffects, UpdateKind, Version,
};
use spaceterm_server::service::lane::client::Outcome;
use spaceterm_server::service::lane::{
    LaneAction, LaneActivation, LaneCheck, LaneDevice, LaneEffects, LaneNetwork,
    LaneOperationState, LaneReceipt, LaneStaged, LaneStatus, LaneUpdateKind, LaneVersion,
};

const EXPIRES: &str = "2026-10-30T12:10:00Z";
const OPERATION: &str = "0123456789abcdef0123456789abcdef";
const BOOT: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const NEW_BOOT: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn server() -> ServerRef {
    ServerRef {
        name: "penguin".into(),
        route: format!("id:{}", "4e".repeat(32)),
        id: "5b".repeat(32),
    }
}

fn local() -> ServerRef {
    ServerRef {
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
fn server_status_reports_service_network_devices_and_held_outcomes() {
    let lane = LaneStatus {
        running: true,
        stopped: false,
        installed: true,
        automatic_start: true,
        linger: Some(true),
        runtime: lane_version("0.7.0", 'a'),
        staged: vec![LaneStaged {
            version: "0.7.1".into(),
            sha256: "b".repeat(64),
        }],
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
    };
    let held = vec![HeldOutcomes {
        identity: RecoveredIdentity::Agent("agent-1".parse().unwrap()),
        count: 3,
    }];
    let (envelope, _) = status::envelope(server(), &lane, held, recovered());
    assert_exit(&envelope, 0);
    assert_golden("server_status", &envelope);
}

#[test]
fn server_status_of_an_unreachable_server_exits_5() {
    let envelope = status::unreachable_envelope(
        server(),
        Failure::new(Code::Unreachable),
        Some("2026-10-30T11:00:00Z".into()),
    );
    assert_exit(&envelope, 5);
    assert_golden("server_status_unreachable", &envelope);
}

fn stop_effects() -> LaneEffects {
    LaneEffects {
        ends_sessions: 1,
        sessions: vec!["0e".repeat(32)],
        start_requires_machine: true,
        from: None,
        to: None,
        kind: None,
        to_major: None,
    }
}

#[test]
fn a_stop_preview_names_the_sessions_it_ends_and_exits_3() {
    let envelope = lifecycle::preview_envelope(
        "server.stop",
        server(),
        format!("c1-{}", "ab".repeat(32)),
        EXPIRES.into(),
        &stop_effects(),
        Vec::new(),
    );
    assert_exit(&envelope, 3);
    assert_golden("server_stop_preview", &envelope);
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
    let envelope = lifecycle::operation_envelope("server.stop", server(), settled, Vec::new());
    assert_exit(&envelope, 7);
    assert_golden("server_stop_shutdown", &envelope);
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
    let envelope = lifecycle::operation_envelope("server.restart", server(), settled, recovered());
    assert_exit(&envelope, 0);
    assert_golden("server_restart_done", &envelope);
}

#[test]
fn failed_not_performed_and_unconfirmed_restarts_map_to_their_codes() {
    for (name, outcome, exit) in [
        (
            "server_restart_failed",
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
            "server_restart_not_performed",
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
            "server_restart_unconfirmed",
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
        let envelope =
            lifecycle::operation_envelope("server.restart", server(), settled, Vec::new());
        assert_exit(&envelope, exit);
        assert_golden(name, &envelope);
    }
}

#[test]
fn update_check_lists_approved_latest_and_staged() {
    let data = update::lane_check(LaneCheck {
        approved: lane_version("0.7.0", 'a'),
        latest: Some(lane_version("0.7.1", 'b')),
        staged: vec![lane_version("0.7.1", 'b')],
    });
    let envelope = update::check_envelope(server(), data, Vec::new());
    assert_exit(&envelope, 0);
    assert_golden("server_update_check", &envelope);
}

#[test]
fn an_update_preview_warns_when_the_candidate_has_another_protocol_major() {
    let effects = UpdateEffects {
        from: version("0.7.0", 'a'),
        to: version("0.8.0", 'b'),
        kind: UpdateKind::Update,
        ends_sessions: 1,
        sessions: vec!["0e".repeat(32)],
    };
    let major = spaceterm_server::protocol::MAJOR + 1;
    let envelope = update::preview_envelope(
        server(),
        format!("c1-{}", "cd".repeat(32)),
        EXPIRES.into(),
        effects,
        Some(major),
        Vec::new(),
    );
    assert_exit(&envelope, 3);
    assert_golden("server_update_preview", &envelope);
    // The same major adds no warning.
    let lane = LaneEffects {
        kind: Some(LaneUpdateKind::Update),
        to_major: Some(spaceterm_server::protocol::MAJOR),
        ..stop_effects()
    };
    let same = update::preview_envelope(
        server(),
        "c1-00".into(),
        EXPIRES.into(),
        UpdateEffects {
            from: version("0.7.0", 'a'),
            to: version("0.7.1", 'b'),
            kind: UpdateKind::Update,
            ends_sessions: 0,
            sessions: Vec::new(),
        },
        lane.to_major,
        Vec::new(),
    );
    assert!(same.warnings.is_empty());
}

#[test]
fn a_return_preview_on_the_servers_machine_ends_no_sessions() {
    let envelope = update::preview_envelope(
        local(),
        format!("c1-{}", "ef".repeat(32)),
        EXPIRES.into(),
        UpdateEffects {
            from: version("0.8.0", 'b'),
            to: version("0.7.0", 'a'),
            kind: UpdateKind::Return,
            ends_sessions: 0,
            sessions: Vec::new(),
        },
        None,
        Vec::new(),
    );
    assert_exit(&envelope, 3);
    assert_golden("server_update_return_preview", &envelope);
}

#[test]
fn a_settled_update_reports_its_receipt_and_version() {
    let settled = lifecycle::settle(
        ServiceAction::Update,
        Outcome::Settled {
            receipt: receipt(
                LaneAction::Update {
                    installation: "b".repeat(64),
                },
                LaneOperationState::Done,
                None,
                NEW_BOOT,
            ),
            boot: BOOT.into(),
        },
    );
    let envelope = update::operation_envelope(
        server(),
        settled.receipt,
        settled.failure,
        Some(version("0.8.0", 'b')),
        Vec::new(),
    );
    assert_exit(&envelope, 0);
    assert_golden("server_update_done", &envelope);
    let unresolved = lifecycle::settle(
        ServiceAction::Update,
        Outcome::Settled {
            receipt: receipt(
                LaneAction::Update {
                    installation: "b".repeat(64),
                },
                LaneOperationState::Unresolved,
                None,
                NEW_BOOT,
            ),
            boot: BOOT.into(),
        },
    );
    let envelope = update::operation_envelope(
        server(),
        unresolved.receipt,
        unresolved.failure,
        Some(version("0.8.0", 'b')),
        Vec::new(),
    );
    assert_exit(&envelope, 7);
    assert_golden("server_update_unresolved", &envelope);
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
    assert_golden("server_unpair_preview", &preview);
    let revoked = Envelope::ok(
        "server.unpair",
        Some(local()),
        ServerUnpair { revoked: device },
    );
    assert_exit(&revoked, 0);
    assert_golden("server_unpair_revoked", &revoked);
}

#[test]
fn unpair_lists_no_label_for_ambiguous_names() {
    let device = |id: u8| spaceterm_server::direct::DeviceInfo {
        pairing: [id; 32],
        client: [id; 32],
        name: "Laptop".into(),
        public_key: [id; 32],
        last_connected_at_unix_seconds: None,
    };
    let devices = [device(1), device(2)];
    let failure = unpair::select(&devices, "Laptop").unwrap_err();
    let envelope = Envelope::error(
        "server.unpair",
        Some(local()),
        failure,
        crate::contract::Empty {},
    );
    assert_exit(&envelope, 4);
    assert_golden("server_unpair_ambiguous", &envelope);
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
        "server.setup",
        Some(local()),
        ServerSetup {
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
        },
    );
    assert_exit(&envelope, 0);
    assert_golden("server_setup", &envelope);
}

#[test]
fn start_succeeds_locally_and_is_local_only_remotely() {
    let started = Envelope::ok("server.start", Some(local()), ServerStart { running: true });
    assert_exit(&started, 0);
    assert_golden("server_start", &started);
    let remote = Envelope::error(
        "server.start",
        None,
        Failure::new(Code::LocalOnly),
        crate::contract::Empty {},
    );
    assert_exit(&remote, 5);
    assert_golden("server_start_local_only", &remote);
}

#[test]
fn pair_without_a_terminal_needs_interaction() {
    let envelope = Envelope::error(
        "server.pair",
        None,
        Failure::new(Code::InteractionRequired),
        crate::contract::Empty {},
    );
    assert_exit(&envelope, 3);
    assert_golden("server_pair_interaction_required", &envelope);
}

#[test]
fn receipts_keep_the_lane_state_and_code() {
    let receipt = lifecycle::receipt_of(&receipt(
        LaneAction::Return,
        LaneOperationState::Failed,
        Some("return_unavailable"),
        BOOT,
    ));
    assert_eq!(
        receipt,
        Receipt {
            operation: OPERATION.into(),
            action: ServiceAction::Return,
            state: ReceiptState::Failed,
            code: Some(Code::ReturnUnavailable),
            boot: BOOT.into(),
        }
    );
}
