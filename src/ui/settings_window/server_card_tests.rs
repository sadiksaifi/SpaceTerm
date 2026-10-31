//! How each server state reads on the card, in the picker and in All Servers.

use super::super::servers::{ConnectedServer, LocalServerStatus, ServiceAction, ServicePhase};
use super::*;
use crate::direct_connections::UnavailableReason;

const CONNECTED: LocalServerStatus = LocalServerStatus::Connected(ConnectedServer {
    protocol_major: spaceterm_server::protocol::MAJOR,
});

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

/// A step underway names itself apart from the state, which keeps reading as last observed.
#[test]
fn a_local_service_step_underway_keeps_the_state_and_names_its_verb() {
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

        assert_eq!(state.state.as_ref(), "Running");
        assert_eq!(state.operation, Some(verb));
        assert!(!state.checking);
        assert!(state.live);
    }
}

/// A passive read keeps the state as last observed, so a running server stays live.
#[test]
fn a_local_service_read_keeps_a_running_server_live() {
    let state = local_state(
        CONNECTED,
        Some((
            Some(ServicePhase::Running),
            Some(ServiceAction::Check),
            false,
        )),
        false,
    );

    assert_eq!(state.state.as_ref(), "Running");
    assert!(state.checking);
    assert_eq!(state.operation, None);
    assert!(state.live);
}

#[test]
fn an_update_still_running_names_updating_when_settings_reopens() {
    let state = local_state(
        LocalServerStatus::Failed(ServerError::ActivationUnresolved),
        Some((Some(ServicePhase::Stopped), None, false)),
        true,
    );

    assert_eq!(state.state.as_ref(), "Stopped");
    assert_eq!(state.operation, Some("Updating\u{2026}"));
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
fn a_paired_server_reads_by_its_route() {
    for (status, state, short, live) in [
        (
            DirectRouteStatus::Connected,
            "Connected over the network",
            "Connected \u{b7} Paired",
            true,
        ),
        (
            DirectRouteStatus::Disconnected,
            "Not connected",
            "Not connected",
            false,
        ),
        (
            DirectRouteStatus::Refused(RefusalReason::Revoked),
            "Unpaired",
            "Unpaired",
            false,
        ),
        (
            DirectRouteStatus::Refused(RefusalReason::Incompatible),
            "Needs update",
            "Needs update",
            false,
        ),
        (
            DirectRouteStatus::Unavailable(UnavailableReason::Network),
            "Not reachable",
            "Not reachable",
            false,
        ),
    ] {
        let read = direct_state(status);

        assert_eq!(read.state.as_ref(), state);
        assert_eq!(read.short.as_ref(), short);
        assert_eq!(read.live, live, "{state}");
    }
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
