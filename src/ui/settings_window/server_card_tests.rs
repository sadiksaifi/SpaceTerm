//! How each server state reads on the card, in the picker and in All Servers.

use super::super::servers::{LocalServerStatus, ServerConnection, ServiceAction, ServicePhase};
use super::*;
use crate::ssh::server_connection::{SshServerFacts, UnavailableKind};

const CONNECTED: LocalServerStatus =
    LocalServerStatus::Connected(ServerConnection { protocol_major: 1 });

fn facts() -> SshServerFacts {
    SshServerFacts {
        platform: ("linux".into(), "x86_64".into()),
        version: Some("1.4.0".into()),
        starts_automatically: true,
    }
}

#[test]
fn a_running_local_server_reads_as_running_for_this_account_and_live() {
    let state = local_state(
        CONNECTED,
        Some((Some(ServicePhase::Running), None, false)),
        false,
    );

    assert_eq!(state.state.as_ref(), "Running");
    assert_eq!(state.short.as_ref(), "Running \u{b7} this account");
    assert!(state.live);
}

#[test]
fn a_stopped_or_uninstalled_local_server_is_not_live() {
    for (phase, expected) in [
        (ServicePhase::Stopped, "Stopped"),
        (ServicePhase::NotInstalled, "Not installed"),
        (ServicePhase::NotRunning, "Not running"),
    ] {
        let state = local_state(
            LocalServerStatus::Failed(ServerError::NotRunning),
            Some((Some(phase), None, false)),
            false,
        );

        assert_eq!(state.state.as_ref(), expected);
        assert!(!state.live, "{expected} is not live");
    }
}

#[test]
fn a_local_server_waiting_for_startup_authorization_says_so() {
    let state = local_state(
        LocalServerStatus::Failed(ServerError::AuthorizationRequired),
        Some((Some(ServicePhase::Stopped), None, true)),
        false,
    );

    assert_eq!(state.state.as_ref(), "Needs authorization");
}

#[test]
fn a_local_service_step_underway_reads_as_its_verb() {
    for (action, verb) in [
        (ServiceAction::Start, "Starting\u{2026}"),
        (ServiceAction::Stop, "Stopping\u{2026}"),
        (ServiceAction::Restart, "Restarting\u{2026}"),
        (ServiceAction::Install, "Installing\u{2026}"),
    ] {
        let state = local_state(
            CONNECTED,
            Some((Some(ServicePhase::Running), Some(action), false)),
            false,
        );

        assert_eq!(state.state.as_ref(), verb);
        assert!(state.working);
    }
}

#[test]
fn an_update_still_running_reads_as_updating_when_settings_reopens() {
    let state = local_state(
        LocalServerStatus::Failed(ServerError::ActivationUnresolved),
        Some((Some(ServicePhase::Running), None, false)),
        true,
    );

    assert_eq!(state.state.as_ref(), "Updating\u{2026}");
}

#[test]
fn a_local_server_without_a_protocol_in_common_needs_an_update() {
    let state = local_state(
        LocalServerStatus::Failed(ServerError::IncompatibleProtocol),
        Some((Some(ServicePhase::Running), None, false)),
        false,
    );

    assert_eq!(state.state.as_ref(), "Needs update");
    assert_eq!(state.tone, Tone::Problem);
}

#[test]
fn an_ssh_server_reads_by_its_connection() {
    for (status, state, short, live) in [
        (
            RemoteConnectionStatus::Connected {
                protocol_major: 1,
                facts: facts(),
            },
            "Connected over SSH",
            "Connected \u{b7} SSH",
            true,
        ),
        (
            RemoteConnectionStatus::Inactive,
            "Not connected",
            "Not connected",
            false,
        ),
        (
            RemoteConnectionStatus::Incompatible {
                kind: IncompatibilityKind::Protocol,
            },
            "Needs update",
            "Needs update",
            false,
        ),
        (
            RemoteConnectionStatus::Unavailable {
                kind: UnavailableKind::HostUnreachable,
            },
            "Not reachable",
            "Not reachable",
            false,
        ),
        (RemoteConnectionStatus::Stopped, "Stopped", "Stopped", false),
    ] {
        let read = ssh_state(&status, None, None, None);

        assert_eq!(read.state.as_ref(), state);
        assert_eq!(read.short.as_ref(), short);
        assert_eq!(read.live, live, "{state}");
    }
}

#[test]
fn an_ssh_connection_or_maintenance_underway_reads_as_its_verb() {
    let installing = ssh_state(
        &RemoteConnectionStatus::Working {
            phase: ConnectPhase::Installing,
            cancellable: true,
        },
        None,
        None,
        None,
    );
    assert_eq!(installing.state.as_ref(), "Installing\u{2026}");

    let stopping = ssh_state(
        &RemoteConnectionStatus::Connected {
            protocol_major: 1,
            facts: facts(),
        },
        Some(Some(&SubmittedKind::Service(ServiceControlAction::Stop))),
        None,
        None,
    );
    assert_eq!(stopping.state.as_ref(), "Stopping\u{2026}");
    assert!(stopping.live, "the server stays connected while it stops");
}

#[test]
fn a_paired_server_reads_by_its_route() {
    assert_eq!(
        direct_state(DirectRouteStatus::Connected).state.as_ref(),
        "Connected over the network"
    );
    assert_eq!(
        direct_state(DirectRouteStatus::Connected).short.as_ref(),
        "Connected \u{b7} Paired"
    );
    assert_eq!(
        direct_state(DirectRouteStatus::Refused(RefusalReason::Revoked))
            .state
            .as_ref(),
        "Unpaired"
    );
    assert!(!direct_state(DirectRouteStatus::Disconnected).live);
}

#[test]
fn shown_values_name_the_time_today_and_the_day_otherwise() {
    let at = LocalTime {
        year: 126,
        month: 9,
        day: 21,
        hour: 8,
        minute: 5,
    };

    assert_eq!(clock_text(at, at), "08:05");
    assert_eq!(
        clock_text(
            at,
            LocalTime {
                day: 23,
                hour: 17,
                ..at
            }
        ),
        "Oct 21, 08:05"
    );
}

#[test]
fn reported_system_names_read_as_people_know_them() {
    assert_eq!(operating_system_name("linux").as_ref(), "Linux");
    assert_eq!(operating_system_name("macos").as_ref(), "macOS");
    assert_eq!(operating_system_name("FreeBSD").as_ref(), "FreeBSD");
}

#[test]
fn a_service_read_tells_a_stopped_ssh_server_from_a_disconnected_one() {
    let stopped = crate::local_server::service::ServiceStatus {
        installed: true,
        enabled: true,
        running: false,
        stopped: true,
    };
    let running = crate::local_server::service::ServiceStatus {
        running: true,
        stopped: false,
        ..stopped
    };

    assert_eq!(
        ssh_state(&RemoteConnectionStatus::Inactive, None, Some(stopped), None)
            .state
            .as_ref(),
        "Stopped"
    );
    assert_eq!(
        ssh_state(&RemoteConnectionStatus::Inactive, None, Some(running), None)
            .state
            .as_ref(),
        "Not connected"
    );
}

/// A saved connection SpaceTerm isn't connected to shows the state last observed, and only that:
/// it stays not live, so nothing a live server allows is offered.
#[test]
fn an_inactive_ssh_server_shows_its_last_observed_state_without_becoming_live() {
    let stopped = ssh_state(
        &RemoteConnectionStatus::Inactive,
        None,
        None,
        Some(ObservedServerStatus::Stopped),
    );
    assert_eq!(stopped.state.as_ref(), "Stopped");
    assert!(!stopped.live);

    let running = ssh_state(
        &RemoteConnectionStatus::Inactive,
        None,
        None,
        Some(ObservedServerStatus::Running),
    );
    assert_eq!(running.state.as_ref(), "Not connected");
    assert!(
        !running.live,
        "a remembered Running never makes a server live"
    );
}
