//! Golden envelopes and text of `server network`, built by the functions the command uses.
use super::*;
use crate::contract::golden::assert_golden;
use spaceterm_server::service::lane::client::Outcome;
use spaceterm_server::service::lane::{LaneDevice, LaneOperationState, LaneReceipt};

const EXPIRES: &str = "2026-10-30T12:10:00Z";
const OPERATION: &str = "0123456789abcdef0123456789abcdef";
const BOOT: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TO: &str = "192.0.2.4:9231";
const STATUS: &str = "spaceterm --server id:4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e server status";

fn server() -> ServerRef {
    ServerRef {
        name: "penguin".into(),
        route: format!("id:{}", "4e".repeat(32)),
        id: "5b".repeat(32),
    }
}

fn on(bind: &str) -> LaneNetworkSetting {
    LaneNetworkSetting {
        enabled: true,
        bind: Some(bind.into()),
    }
}

fn effects(caller: LaneCallerEffect) -> LaneNetworkEffects {
    LaneNetworkEffects {
        from: on("0.0.0.0:9231"),
        to: on(TO),
        ends_connections: true,
        caller: Some(caller),
        disconnects: vec![LaneDevice {
            id: "6c".repeat(32),
            name: "Studio desktop".into(),
            fingerprint: "SHA256:studio".into(),
            last_connected_at: None,
        }],
    }
}

fn receipt(state: LaneOperationState) -> LaneReceipt {
    LaneReceipt {
        operation: OPERATION.into(),
        action: LaneAction::Network {
            access: NetworkAccess::Address(TO.parse().unwrap()),
        },
        state,
        code: None,
        boot: BOOT.into(),
    }
}

#[test]
fn a_preview_reports_the_callers_reconnect_and_the_devices_that_lose_their_route() {
    let effects = effects(LaneCallerEffect::Reconnects);
    let envelope = preview_envelope(
        server(),
        format!("c1-{}", "ab".repeat(32)),
        EXPIRES.into(),
        &effects,
        Vec::new(),
    );
    assert_eq!(envelope.exit(), 3);
    assert_golden("server_network_preview", &envelope);
    assert_eq!(
        summary("penguin", &effects),
        format!(
            "Change Network Access on penguin from on at 0.0.0.0:9231 to on at {TO}?\n\
             Every paired connection ends. A device reconnects only if the new setting serves the address it uses.\n\
             This device's connection ends, and it reconnects.\n\
             These connected devices lose their route:\n  id:{}  Studio desktop\n",
            "6c".repeat(32)
        )
    );
}

#[test]
fn a_preview_says_when_the_caller_cannot_reconnect_or_stays_connected() {
    let lost = summary("penguin", &effects(LaneCallerEffect::Disconnected));
    assert!(lost.contains("This device loses its connection and can't reconnect"));
    let widened = LaneNetworkEffects {
        from: on(TO),
        to: on("0.0.0.0:9231"),
        ends_connections: false,
        caller: Some(LaneCallerEffect::StaysConnected),
        disconnects: Vec::new(),
    };
    let text = summary("penguin", &widened);
    assert!(text.contains("This device stays connected."));
    assert!(!text.contains("Every paired connection ends"));
    assert!(!widened.unchanged());
    // The server's own machine has no paired connection to describe.
    let local = LaneNetworkEffects {
        caller: None,
        ..widened
    };
    assert!(!summary("local", &local).contains("This device"));
}

#[test]
fn a_change_that_disconnects_every_device_is_refused_with_its_own_code() {
    let failure = contract::from_lane_code("disconnects_every_device");
    assert_eq!(failure.code, Code::DisconnectsEveryDevice);
    let envelope = Envelope::error(COMMAND, Some(server()), failure, contract::Empty {});
    assert_eq!(envelope.exit(), 5);
    assert_golden("server_network_disconnects_every_device", &envelope);
}

#[test]
fn a_change_that_cut_and_restored_the_callers_connection_reports_done() {
    let settled = super::super::lifecycle::settle(
        ServiceAction::Network,
        Outcome::Settled {
            receipt: receipt(LaneOperationState::Done),
            boot: BOOT.into(),
        },
    );
    assert!(settled.acknowledged);
    let (envelope, text) = result(server(), settled, Some(on(TO)), STATUS);
    assert_eq!(envelope.exit(), 0);
    assert_eq!(text, format!("Network Access is now on at {TO}.\n"));
    assert_golden("server_network_done", &envelope);
}

#[test]
fn a_caller_that_lost_its_route_reports_an_unconfirmed_outcome() {
    let settled = super::super::lifecycle::settle(
        ServiceAction::Network,
        Outcome::Unconfirmed {
            operation: OPERATION.into(),
            boot: BOOT.into(),
            last: None,
        },
    );
    assert!(!settled.acknowledged);
    let (envelope, text) = result(server(), settled, None, STATUS);
    assert_eq!(envelope.exit(), 7);
    assert_eq!(
        text,
        format!(
            "Check Network Access with: {STATUS}\nIf this device can't reach penguin, run spaceterm server status on that machine.\n"
        )
    );
    assert_golden("server_network_unconfirmed", &envelope);
}

fn parsed(text: &str) -> Network {
    use clap::Parser;
    let cli = crate::args::Cli::try_parse_from(["spaceterm", "server", "network", text]).unwrap();
    match cli.command {
        crate::args::Command::Server(crate::args::ServerCommand::Network { access, .. }) => access,
        other => panic!("parsed {other:?}"),
    }
}

#[test]
fn the_access_argument_round_trips_into_the_confirmation_hint() {
    for text in ["off", "all", "192.0.2.4:9231", "[2001:db8::4]:9231"] {
        assert_eq!(argument(access(&parsed(text))), text);
    }
    assert_eq!(
        argument(access(&parsed("192.0.2.4"))),
        format!("192.0.2.4:{}", spaceterm_server::direct::DEFAULT_PORT)
    );
}

fn context_for(global: &crate::args::Global) -> Context<'_> {
    Context {
        identity: "spaceterm-development",
        global,
    }
}

#[test]
fn the_confirmation_names_the_server_and_actor_it_was_previewed_for() {
    use clap::Parser;
    let cli = crate::args::Cli::try_parse_from([
        "spaceterm",
        "--server",
        "penguin",
        "--as",
        "builder",
        "--no-input",
        "server",
        "network",
        "all",
    ])
    .unwrap();
    let line = confirm_line(
        &context_for(&cli.global),
        &server(),
        NetworkAccess::All,
        "c1-ab",
    );
    assert_eq!(
        line,
        format!(
            "spaceterm --server id:{} --as builder server network all --confirm c1-ab",
            "4e".repeat(32)
        )
    );
    // The copied line parses back to the same command, target and actor.
    let words: Vec<&str> = line.split(' ').collect();
    let copied = crate::args::Cli::try_parse_from(&words).unwrap();
    assert_eq!(
        copied.global.server.as_deref(),
        Some(server().route.as_str())
    );
    assert_eq!(
        copied.global.agent.as_ref().map(|v| v.as_str()),
        Some("builder")
    );
    assert!(matches!(
        copied.command,
        crate::args::Command::Server(crate::args::ServerCommand::Network {
            confirm: Some(_),
            ..
        })
    ));
}

/// The words a shell produces from `line`, with the leading `spaceterm` printing its arguments
/// instead of running. `None` when `shell` isn't installed.
fn shell_words(shell: &str, flags: &[&str], line: &str) -> Option<Vec<String>> {
    let script = line.replacen("spaceterm", "printf '%s\\n'", 1);
    let output = std::process::Command::new(shell)
        .args(flags)
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    assert!(
        output.status.success(),
        "{shell} refused the printed command: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect(),
    )
}

#[test]
fn an_ipv6_confirmation_pastes_into_sh_and_zsh_as_the_same_command() {
    use clap::Parser;
    let cli =
        crate::args::Cli::try_parse_from(["spaceterm", "server", "network", "[::1]:9231"]).unwrap();
    let access = NetworkAccess::Address("[::1]:9231".parse().unwrap());
    let line = confirm_line(&context_for(&cli.global), &server(), access, "c1-ab");
    assert!(line.contains(" '[::1]:9231' "), "{line}");
    // Zsh without its startup files, so its default `nomatch` applies, as in a new shell.
    let mut shells = 0;
    for (shell, flags) in [("/bin/sh", &[][..]), ("zsh", &["-f"][..])] {
        let Some(words) = shell_words(shell, flags, &line) else {
            continue;
        };
        shells += 1;
        let copied =
            crate::args::Cli::try_parse_from(std::iter::once("spaceterm".to_owned()).chain(words))
                .unwrap();
        assert_eq!(
            copied.global.server.as_deref(),
            Some(server().route.as_str())
        );
        let crate::args::Command::Server(crate::args::ServerCommand::Network {
            access: copied,
            confirm,
        }) = copied.command
        else {
            panic!("{shell} produced another command");
        };
        assert_eq!(argument(super::access(&copied)), "[::1]:9231");
        assert_eq!(confirm.as_deref(), Some("c1-ab"));
    }
    assert!(shells >= 1, "no shell ran");
}
