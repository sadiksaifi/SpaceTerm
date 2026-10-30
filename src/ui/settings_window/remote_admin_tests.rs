//! Paired server Status, Stop, Restart and Update from the Servers card.
use gpui::{TestAppContext, VisualTestContext};
use spaceterm_server::service::lane::{
    LaneActivation, LaneEffects, LaneNetwork, LaneOperationState, LaneReceipt,
};
use spaceterm_ui::a11y_testing::A11yTree;

use super::super::server_list::tests::{
    Servers, click, connected_paired, disconnected, open_with, settle,
};
use super::testing::ScriptedRemote;
use super::*;

fn version(version: &str, commit: &str) -> LaneVersion {
    LaneVersion {
        version: version.into(),
        commit: commit.into(),
        sha256: format!("{version}-package"),
    }
}

fn effects(ends_sessions: u32) -> LaneEffects {
    LaneEffects {
        ends_sessions,
        sessions: Vec::new(),
        start_requires_machine: true,
        from: None,
        to: None,
        kind: None,
        to_major: None,
    }
}

fn preview(action: LaneAction, effects: LaneEffects) -> Preview {
    Preview {
        action,
        token: "c1-previewed".into(),
        expires_at: "2026-10-30T12:10:00Z".into(),
        effects,
    }
}

fn update_preview(to_major: Option<u16>) -> Preview {
    preview(
        LaneAction::Update {
            installation: "1.3.0-package".into(),
        },
        LaneEffects {
            from: Some(version("1.2.0", "aaaaaaaaaa")),
            to: Some(version("1.3.0", "bbbbbbbbbb")),
            kind: Some(LaneUpdateKind::Update),
            to_major,
            ..effects(2)
        },
    )
}

fn receipt(action: LaneAction, state: LaneOperationState, boot: &str) -> LaneReceipt {
    LaneReceipt {
        operation: "0123456789abcdef0123456789abcdef".into(),
        action,
        state,
        code: None,
        boot: boot.into(),
    }
}

fn status(runtime: LaneVersion) -> LaneStatus {
    LaneStatus {
        running: true,
        stopped: false,
        installed: true,
        automatic_start: true,
        linger: None,
        runtime,
        staged: Vec::new(),
        activation: LaneActivation::Settled,
        sessions: 2,
        network: LaneNetwork {
            enabled: true,
            bind: None,
            listening: true,
            addresses: Vec::new(),
        },
        devices: Vec::new(),
        lane_major: 1,
    }
}

#[test]
fn a_stop_confirmation_says_every_client_loses_access_and_where_start_happens() {
    let confirmation = confirmation("penguin", &preview(LaneAction::Stop, effects(2)));

    assert_eq!(confirmation.title, "Stop Server");
    assert_eq!(confirmation.message, "Stop the penguin server?");
    assert_eq!(
        confirmation.detail,
        "Its 2 Terminal Sessions end for every client. Every client connected to it loses \
         access, and Workspaces are kept. It stays stopped until someone starts it on its own \
         computer."
    );
    assert_eq!(confirmation.confirm, "Stop Server");
}

#[test]
fn a_restart_confirmation_names_the_exact_sessions_that_end() {
    let confirmation = confirmation("penguin", &preview(LaneAction::Restart, effects(1)));

    assert_eq!(confirmation.message, "Restart the penguin server?");
    assert_eq!(
        confirmation.detail,
        "Its 1 Terminal Session ends for every client. Every client connected to it disconnects \
         until it starts again, and Workspaces are kept."
    );
    assert_eq!(confirmation.confirm, "Restart Server");
}

#[test]
fn an_update_confirmation_says_it_replaces_the_running_server() {
    let confirmation = confirmation("penguin", &update_preview(None));

    assert_eq!(confirmation.title, "Update Server");
    assert_eq!(
        confirmation.message,
        "Update the penguin server to 1.3.0 (commit bbbbbbb)?"
    );
    assert_eq!(
        confirmation.detail,
        "It changes from 1.2.0 (commit aaaaaaa) to 1.3.0 (commit bbbbbbb), replacing the running \
         server. Its 2 Terminal Sessions end for every client. Every client connected to it \
         disconnects until it starts again, and Workspaces are kept."
    );
    assert_eq!(confirmation.confirm, "Update Server");
}

#[test]
fn an_update_to_another_protocol_says_this_client_needs_its_own_update() {
    let other = spaceterm_server::protocol::MAJOR + 1;
    let confirmation = confirmation("penguin", &update_preview(Some(other)));

    assert!(
        confirmation.detail.ends_with(
            "Afterward, SpaceTerm here needs its own update to open penguin\u{2019}s Workspaces."
        ),
        "got {:?}",
        confirmation.detail
    );
    let same = confirmation_detail(Some(spaceterm_server::protocol::MAJOR));
    assert!(!same.contains("Afterward"), "got {same:?}");
}

fn confirmation_detail(to_major: Option<u16>) -> String {
    confirmation("penguin", &update_preview(to_major)).detail
}

#[test]
fn a_return_confirmation_names_the_version_it_returns_to() {
    let returning = preview(
        LaneAction::Return,
        LaneEffects {
            from: Some(version("1.3.0", "bbbbbbbbbb")),
            to: Some(version("1.2.0", "aaaaaaaaaa")),
            kind: Some(LaneUpdateKind::Return),
            ..effects(0)
        },
    );
    let confirmation = confirmation("penguin", &returning);

    assert_eq!(confirmation.title, "Return to Previous Version");
    assert_eq!(
        confirmation.message,
        "Return the penguin server to 1.2.0 (commit aaaaaaa)?"
    );
    assert_eq!(confirmation.confirm, "Return to 1.2.0 (commit aaaaaaa)");
    assert!(
        confirmation
            .detail
            .contains("No Terminal Sessions are running on it."),
        "got {:?}",
        confirmation.detail
    );
}

/// The CLI contract's uncertain outcome reads as uncertain, never as success or failure.
#[test]
fn an_outcome_the_client_cannot_confirm_says_so() {
    let (stop, tone) = ended_message("penguin", &Ended::Changed(Act::Stop, Settlement::Shutdown));
    assert_eq!(
        stop,
        "penguin closed its connection while stopping, so SpaceTerm can\u{2019}t confirm the \
         stop finished. Check on its own computer with \u{201C}spaceterm server status\u{201D}, \
         and start it again there."
    );
    assert_eq!(tone, CaptionTone::Warning);

    // A Stop that ran out of time with the lane open proves no shutdown, so it reads as any
    // other uncertain change.
    let (stop, tone) = ended_message("penguin", &Ended::Changed(Act::Stop, Settlement::Uncertain));
    assert_eq!(
        stop,
        "SpaceTerm couldn\u{2019}t confirm whether penguin finished its stop. Check Now asks \
         penguin again."
    );
    assert_eq!(tone, CaptionTone::Warning);

    let (restart, tone) = ended_message(
        "penguin",
        &Ended::Changed(Act::Restart, Settlement::Uncertain),
    );
    assert_eq!(
        restart,
        "SpaceTerm couldn\u{2019}t confirm whether penguin finished its restart. Check Now asks \
         penguin again."
    );
    assert_eq!(tone, CaptionTone::Warning);
}

#[test]
fn each_settled_change_reads_its_own_way() {
    let message = |ended| ended_message("penguin", &ended).0;
    assert_eq!(
        message(Ended::Changed(Act::Stop, Settlement::Done)),
        "penguin stopped, and its Terminal Sessions ended for every client. Start it again on \
         its own computer."
    );
    assert_eq!(
        message(Ended::Changed(Act::Restart, Settlement::Done)),
        "penguin restarted."
    );
    assert_eq!(
        message(Ended::Changed(
            Act::Update(Some("1.3.0".into())),
            Settlement::Done
        )),
        "penguin updated to 1.3.0."
    );
    assert_eq!(
        message(Ended::Changed(Act::Restart, Settlement::NotPerformed)),
        "penguin didn\u{2019}t restart, so nothing changed."
    );
    assert_eq!(
        message(Ended::Changed(
            Act::Update(None),
            Settlement::Failed(RemoteFailure::StartupFailed)
        )),
        "penguin couldn\u{2019}t update. penguin didn\u{2019}t start after the change."
    );
    assert_eq!(
        message(Ended::Refused(RemoteFailure::Changed)),
        "penguin changed after SpaceTerm asked, so nothing changed. Try again."
    );
}

#[test]
fn lane_codes_classify_without_carrying_their_text() {
    for (code, failure) in [
        ("changed", RemoteFailure::Changed),
        ("service_busy", RemoteFailure::Busy),
        ("no_candidate", RemoteFailure::NoUpdate),
        ("unreachable", RemoteFailure::Unreachable),
        ("revoked", RemoteFailure::Refused),
        ("activation_unresolved", RemoteFailure::ActivationUnresolved),
        ("something_new", RemoteFailure::Failed),
    ] {
        assert_eq!(RemoteFailure::of_code(code), failure, "{code}");
    }
    assert_eq!(
        RemoteFailure::of_lane(LaneError::Server(ServerError::Direct(
            spaceterm_server::direct::DirectError::Transport
        ))),
        RemoteFailure::Unreachable
    );
}

/// Opens Servers on one connected paired server named penguin, with `remote` as its lane steps.
fn open_penguin<'a>(remote: &Arc<ScriptedRemote>, cx: &'a mut TestAppContext) -> Servers<'a> {
    let paired = connected_paired(&["penguin"], cx);
    cx.update(|cx| remote.install(cx));
    let servers = open_with(false, paired, cx);
    servers.cx.run_until_parked();
    servers
}

/// Whether the Server row's ⋯ menu offers `selector`. It closes the menu again without
/// choosing anything.
fn menu_offers(selector: &str, cx: &mut VisualTestContext) -> bool {
    click(super::super::server_card::MENU_SELECTOR, cx);
    let offered = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    offered
}

fn alert_open(cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(Box::leak(
        format!("modal-action-{CONFIRM_SELECTOR}").into_boxed_str(),
    ))
    .is_some()
}

fn answer(selector: &str, cx: &mut VisualTestContext) {
    click(&format!("modal-action-{selector}"), cx);
}

fn footnote(servers: &Servers<'_>) -> String {
    servers.window.read_with(servers.cx, |settings, cx| {
        settings
            .server_card_footnote(cx)
            .map(|footnote| footnote.to_string())
            .unwrap_or_default()
    })
}

fn state(servers: &Servers<'_>) -> super::super::server_card::ServerState {
    servers.window.read_with(servers.cx, |settings, _| {
        settings.server_state(&super::super::server_list::ServerChoice::Direct(
            servers.routes[0],
        ))
    })
}

#[gpui::test]
fn a_connected_paired_server_offers_restart_and_stop_but_never_start(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    let servers = open_penguin(&remote, cx);

    assert!(menu_offers(RESTART_SELECTOR, servers.cx));
    assert!(menu_offers(STOP_SELECTOR, servers.cx));
    assert!(
        !menu_offers(
            super::super::servers::ServiceAction::Start.selector(),
            servers.cx
        ),
        "Start stays on the server's own computer"
    );
    assert!(
        remote.prepared.lock().unwrap().is_empty(),
        "showing the card asks the server nothing"
    );
}

#[gpui::test]
fn a_disconnected_paired_server_takes_no_lane_step(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    let paired = disconnected(&["penguin"]);
    cx.update(|cx| remote.install(cx));
    let servers = open_with(false, paired, cx);
    servers.cx.run_until_parked();

    let ready = servers.window.read_with(servers.cx, |settings, cx| {
        settings.remote_ready(&servers.routes[0], cx)
    });
    assert!(!ready);
    click(super::super::server_card::MENU_SELECTOR, servers.cx);
    click(STOP_SELECTOR, servers.cx);
    assert!(remote.prepared.lock().unwrap().is_empty());
    assert!(
        footnote(&servers).contains("Connect to penguin to change these settings."),
        "got {:?}",
        footnote(&servers)
    );
}

/// Stop previews on the server, the alert names it and what ends, and confirming executes that
/// exact preview once. A Stop whose outcome only the server's computer can read says so.
#[gpui::test]
fn stop_confirms_the_servers_preview_and_reports_an_unconfirmed_stop(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.prepare.lock().unwrap() = Ok(Prepared {
        preview: preview(LaneAction::Stop, effects(2)),
        settled: Vec::new(),
    });
    *remote.perform.lock().unwrap() = Ok(Performed {
        outcome: Outcome::Shutdown {
            operation: "0123456789abcdef0123456789abcdef".into(),
            boot: "a".into(),
        },
        settled: Vec::new(),
    });
    let servers = open_penguin(&remote, cx);

    click(STOP_SELECTOR, servers.cx);
    assert_eq!(*remote.prepared.lock().unwrap(), vec![Change::Stop]);
    assert!(alert_open(servers.cx), "Stop asks first");
    let tree = A11yTree::read(servers.cx);
    assert!(tree.find_text("Stop the penguin server?").is_some());
    assert!(remote.performed.lock().unwrap().is_empty());

    answer(CONFIRM_SELECTOR, servers.cx);
    servers.cx.run_until_parked();

    let performed = remote.performed.lock().unwrap().clone();
    assert_eq!(performed.len(), 1, "one confirmation executes once");
    assert_eq!(performed[0].token, "c1-previewed");
    assert!(
        footnote(&servers).contains("can\u{2019}t confirm the stop finished"),
        "got {:?}",
        footnote(&servers)
    );
    assert!(footnote(&servers).contains("start it again there"));

    // The connection ends as the server stops, and the card reads Stopped rather than a
    // connection that keeps retrying.
    servers.direct.disconnect(&servers.routes[0]);
    settle(servers.cx, |_| true);
    assert_eq!(state(&servers).state.as_ref(), "Stopped");
    // Someone started it on its own computer, so connecting again leaves nothing to explain.
    servers.direct.connect(&servers.routes[0]);
    let route = servers.routes[0];
    let direct = servers.direct.clone();
    settle(servers.cx, |_| {
        direct.status(&route) == Some(crate::direct_connections::DirectRouteStatus::Connected)
    });
    assert!(
        !footnote(&servers).contains("stop"),
        "got {:?}",
        footnote(&servers)
    );
}

#[gpui::test]
fn cancelling_a_confirmation_executes_nothing(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.prepare.lock().unwrap() = Ok(Prepared {
        preview: preview(LaneAction::Restart, effects(0)),
        settled: Vec::new(),
    });
    let servers = open_penguin(&remote, cx);

    click(RESTART_SELECTOR, servers.cx);
    answer(CANCEL_SELECTOR, servers.cx);
    servers.cx.run_until_parked();

    assert!(remote.performed.lock().unwrap().is_empty());
    let ready = servers.window.read_with(servers.cx, |settings, cx| {
        settings.remote_ready(&servers.routes[0], cx)
    });
    assert!(ready, "the card is free again");
}

/// A refused preview explains itself and asks nothing.
#[gpui::test]
fn a_refused_preview_explains_why_and_asks_nothing(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.prepare.lock().unwrap() = Err(RemoteFailure::Busy);
    let servers = open_penguin(&remote, cx);

    click(RESTART_SELECTOR, servers.cx);

    assert!(!alert_open(servers.cx));
    assert!(
        footnote(&servers).contains("penguin is finishing another change."),
        "got {:?}",
        footnote(&servers)
    );
}

/// Check Now reads the server's lane. A newer release puts Update on the row, which confirms the
/// server's preview and follows the update until the server returns.
#[gpui::test]
fn check_now_offers_the_servers_update_and_update_follows_it_to_the_end(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.read.lock().unwrap() = Ok(Reading {
        status: status(version("1.2.0", "aaaaaaaaaa")),
        check: LaneCheck {
            approved: version("1.2.0", "aaaaaaaaaa"),
            latest: Some(version("1.3.0", "bbbbbbbbbb")),
            staged: Vec::new(),
        },
        settled: vec![receipt(LaneAction::Restart, LaneOperationState::Done, "a")],
    });
    *remote.prepare.lock().unwrap() = Ok(Prepared {
        preview: update_preview(None),
        settled: Vec::new(),
    });
    let update = LaneAction::Update {
        installation: "1.3.0-package".into(),
    };
    *remote.perform.lock().unwrap() = Ok(Performed {
        outcome: Outcome::Settled {
            receipt: receipt(update, LaneOperationState::Done, "b"),
            boot: "a".into(),
        },
        settled: Vec::new(),
    });
    let servers = open_penguin(&remote, cx);

    click(
        super::super::server_card::VERSION_CHECK_SELECTOR,
        servers.cx,
    );
    servers.cx.run_until_parked();
    let subtitle = servers
        .window
        .read_with(servers.cx, |settings, cx| settings.server_subtitle(cx));
    assert_eq!(subtitle.as_ref(), "Version 1.2.0 \u{b7} 1.3.0 is available");
    assert!(
        footnote(&servers)
            .contains("SpaceTerm confirmed that an earlier restart of penguin finished."),
        "got {:?}",
        footnote(&servers)
    );

    click(UPDATE_SELECTOR, servers.cx);
    assert_eq!(*remote.prepared.lock().unwrap(), vec![Change::Update]);
    let tree = A11yTree::read(servers.cx);
    let message = tree
        .find_text("Update the penguin server to 1.3.0 (commit bbbbbbb)?")
        .is_some();
    assert!(message, "the alert names the server and the version");
    answer(CONFIRM_SELECTOR, servers.cx);
    servers.cx.run_until_parked();

    assert_eq!(remote.performed.lock().unwrap().len(), 1);
    assert!(
        footnote(&servers).contains("penguin updated to 1.3.0."),
        "got {:?}",
        footnote(&servers)
    );
    assert!(state(&servers).operation.is_none(), "nothing is underway");
}

/// A Stop that runs out of time while the server keeps its connection proves no shutdown: the
/// card says the outcome is unknown, and a later disconnect doesn't read as Stopped.
#[gpui::test]
fn a_stop_without_an_observed_shutdown_reads_as_uncertain(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.prepare.lock().unwrap() = Ok(Prepared {
        preview: preview(LaneAction::Stop, effects(1)),
        settled: Vec::new(),
    });
    *remote.perform.lock().unwrap() = Ok(Performed {
        outcome: Outcome::Unconfirmed {
            operation: "0123456789abcdef0123456789abcdef".into(),
            boot: "a".into(),
            last: Some(receipt(LaneAction::Stop, LaneOperationState::Running, "a")),
        },
        settled: Vec::new(),
    });
    let servers = open_penguin(&remote, cx);

    click(STOP_SELECTOR, servers.cx);
    answer(CONFIRM_SELECTOR, servers.cx);
    servers.cx.run_until_parked();

    let note = footnote(&servers);
    assert!(
        note.contains("SpaceTerm couldn\u{2019}t confirm whether penguin finished its stop."),
        "got {note:?}"
    );
    assert!(!note.contains("closed its connection"), "got {note:?}");
    servers.direct.disconnect(&servers.routes[0]);
    settle(servers.cx, |_| true);
    assert_eq!(state(&servers).state.as_ref(), "Not connected");
}

/// A reading belongs to the link it was read over. Once the connection is replaced, as after an
/// update made from the CLI, the facts the new connection observes take precedence, and the old
/// update offer goes.
#[gpui::test]
fn a_new_connection_retires_the_last_lane_reading(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.read.lock().unwrap() = Ok(Reading {
        status: status(version("1.2.0", "aaaaaaaaaa")),
        check: LaneCheck {
            approved: version("1.2.0", "aaaaaaaaaa"),
            latest: Some(version("1.3.0", "bbbbbbbbbb")),
            staged: Vec::new(),
        },
        settled: Vec::new(),
    });
    let servers = open_penguin(&remote, cx);
    let route = servers.routes[0];
    let subtitle = |servers: &Servers<'_>| {
        servers
            .window
            .read_with(servers.cx, |settings, cx| settings.server_subtitle(cx))
    };

    click(
        super::super::server_card::VERSION_CHECK_SELECTOR,
        servers.cx,
    );
    servers.cx.run_until_parked();
    assert_eq!(
        subtitle(&servers).as_ref(),
        "Version 1.2.0 \u{b7} 1.3.0 is available"
    );

    // Someone updated penguin from its own command line, and SpaceTerm's connection to it is
    // replaced.
    let before = servers.direct.server(&route).expect("a connection");
    servers.direct.connect(&route);
    let direct = servers.direct.clone();
    settle(servers.cx, |_| {
        direct
            .server(&route)
            .is_some_and(|server| !server.same_handle(&before))
            && direct.status(&route)
                == Some(crate::direct_connections::DirectRouteStatus::Connected)
    });
    servers.cx.run_until_parked();
    let key = super::super::server_list::ServerChoice::Direct(route).key();
    let observed = servers.cx.update(|_, cx| {
        super::super::server_selection::facts(&key, cx).and_then(|facts| facts.version)
    });

    // The card shows what the new connection observed, never the old link's version.
    let expected = observed.map_or_else(
        || "Version unknown".to_owned(),
        |version| format!("Version {version}"),
    );
    assert_eq!(subtitle(&servers).as_ref(), expected);
    assert!(!subtitle(&servers).contains("1.2.0"));
    assert!(
        servers.cx.debug_bounds(UPDATE_SELECTOR).is_none(),
        "the update the old reading offered is gone"
    );
}

/// A Restart whose receipt never arrives says it can't be confirmed. The next Check Now settles
/// the pending operation and says what became of it.
#[gpui::test]
fn an_uncertain_restart_is_reported_and_the_next_check_settles_it(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.prepare.lock().unwrap() = Ok(Prepared {
        preview: preview(LaneAction::Restart, effects(1)),
        settled: Vec::new(),
    });
    *remote.perform.lock().unwrap() = Ok(Performed {
        outcome: Outcome::Unconfirmed {
            operation: "0123456789abcdef0123456789abcdef".into(),
            boot: "a".into(),
            last: None,
        },
        settled: Vec::new(),
    });
    let servers = open_penguin(&remote, cx);

    click(RESTART_SELECTOR, servers.cx);
    answer(CONFIRM_SELECTOR, servers.cx);
    servers.cx.run_until_parked();
    assert!(
        footnote(&servers).contains(
            "SpaceTerm couldn\u{2019}t confirm whether penguin finished its restart. Check Now \
             asks penguin again."
        ),
        "got {:?}",
        footnote(&servers)
    );

    // The server's start-up couldn't prove the Restart either, so its receipt settles as
    // unresolved, and this connection acknowledges it.
    *remote.read.lock().unwrap() = Ok(Reading {
        status: status(version("1.2.0", "aaaaaaaaaa")),
        check: LaneCheck {
            approved: version("1.2.0", "aaaaaaaaaa"),
            latest: None,
            staged: Vec::new(),
        },
        settled: vec![receipt(
            LaneAction::Restart,
            LaneOperationState::Unresolved,
            "b",
        )],
    });
    click(
        super::super::server_card::VERSION_CHECK_SELECTOR,
        servers.cx,
    );
    servers.cx.run_until_parked();
    assert!(
        footnote(&servers)
            .contains("penguin couldn\u{2019}t confirm whether an earlier restart finished."),
        "got {:?}",
        footnote(&servers)
    );
}
