//! Paired machine Status, Restart and Update from the Machines card.
use gpui::{TestAppContext, VisualTestContext};
use spaceterm_core::service::lane::{
    LaneActivation, LaneEffects, LaneNetwork, LaneOperationState, LaneReceipt,
};
use spaceterm_ui::a11y_testing::A11yTree;

use super::super::machine_list::tests::{
    Machines, click, connected_paired, disconnected, open_with, settle,
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
        LaneAction::Restart,
        LaneEffects {
            from: Some(version("1.2.0", "aaaaaaaaaa")),
            to: Some(version("1.3.0", "bbbbbbbbbb")),
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

/// A Status whose update facts name `pending` as the version that waits for a restart.
fn pending_status(runtime: LaneVersion, pending: &str) -> LaneStatus {
    use spaceterm_core::update::facts::{
        PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
    };
    let mut status = status(runtime.clone());
    status.update = Some(Box::new(UpdateFacts {
        revision: 1,
        platform: Platform::LinuxX86_64,
        running: Version::parse(&runtime.version).unwrap(),
        pending: Some(PendingVersion {
            version: Version::parse(pending).unwrap(),
            protocols: Protocols::CURRENT,
            breaking: false,
            installed_at: 1,
            overdue_since: None,
            stranded: Vec::new(),
        }),
        sessions: 2,
        operation: None,
        check: UpdateCheck::Never,
        skipped: None,
        last_failure: None,
    }));
    status
}

fn status(runtime: LaneVersion) -> LaneStatus {
    LaneStatus {
        running: true,
        stopped: false,
        installed: true,
        automatic_start: true,
        linger: None,
        runtime,
        update: None,
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
        terminal_counters: None,
    }
}

/// Remove Machine's confirmation follows the mockup: it names the machine, the sessions that
/// close there, and that it unpairs.
#[test]
fn a_remove_confirmation_names_the_machine_and_the_sessions_that_close() {
    let detail = |sessions| {
        confirmation(
            "build-box",
            &preview(LaneAction::Uninstall, effects(sessions)),
        )
        .detail
    };
    let removal = confirmation("build-box", &preview(LaneAction::Uninstall, effects(1)));
    assert_eq!(removal.message, "Remove build-box?");
    assert_eq!(removal.confirm, "Remove");
    assert_eq!(
        detail(1),
        "This will uninstall SpaceTerm Core on build-box, close your 1 running session there, \
         and unpair it."
    );
    assert_eq!(
        detail(3),
        "This will uninstall SpaceTerm Core on build-box, close your 3 running sessions there, \
         and unpair it."
    );
    assert_eq!(
        detail(0),
        "This will uninstall SpaceTerm Core on build-box and unpair it."
    );
}

#[test]
fn a_restart_confirmation_names_the_exact_sessions_that_end() {
    let confirmation = confirmation("penguin", &preview(LaneAction::Restart, effects(1)));

    assert_eq!(confirmation.message, "Restart SpaceTerm Core on penguin?");
    assert_eq!(
        confirmation.detail,
        "Its 1 Terminal Session ends for every client. Every client connected to it disconnects \
         until it starts again, and Workspaces are kept."
    );
    assert_eq!(confirmation.confirm, "Restart SpaceTerm Core");
}

#[test]
fn an_update_confirmation_says_it_replaces_the_running_core() {
    let confirmation = confirmation("penguin", &update_preview(None));

    assert_eq!(confirmation.title, "Update SpaceTerm Core");
    assert_eq!(
        confirmation.message,
        "Update SpaceTerm Core on penguin to 1.3.0 (commit bbbbbbb)?"
    );
    assert_eq!(
        confirmation.detail,
        "It changes from 1.2.0 (commit aaaaaaa) to 1.3.0 (commit bbbbbbb), replacing the running \
         SpaceTerm Core. Its 2 Terminal Sessions end for every client. Every client connected to it \
         disconnects until it starts again, and Workspaces are kept."
    );
    assert_eq!(confirmation.confirm, "Update SpaceTerm Core");
}

#[test]
fn an_update_to_another_protocol_says_this_client_needs_its_own_update() {
    let other = spaceterm_core::protocol::MAJOR + 1;
    let confirmation = confirmation("penguin", &update_preview(Some(other)));

    assert!(
        confirmation.detail.ends_with(
            "Afterward, SpaceTerm here needs its own update to open penguin\u{2019}s Workspaces."
        ),
        "got {:?}",
        confirmation.detail
    );
    let same = confirmation_detail(Some(spaceterm_core::protocol::MAJOR));
    assert!(!same.contains("Afterward"), "got {same:?}");
}

fn confirmation_detail(to_major: Option<u16>) -> String {
    confirmation("penguin", &update_preview(to_major)).detail
}

#[test]
fn an_outcome_the_client_cannot_confirm_says_so() {
    let (closed, tone) = ended_message(
        "penguin",
        &Ended::Changed(Act::Restart, Settlement::Shutdown),
    );
    assert_eq!(
        closed,
        "penguin closed its connection before it confirmed its restart, so SpaceTerm \
         can\u{2019}t confirm it finished."
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
        RemoteFailure::of_lane(LaneError::Core(CoreError::Direct(
            spaceterm_core::direct::DirectError::Transport
        ))),
        RemoteFailure::Unreachable
    );
}

/// Opens Servers on one connected paired server named penguin, with `remote` as its lane steps.
fn open_penguin<'a>(remote: &Arc<ScriptedRemote>, cx: &'a mut TestAppContext) -> Machines<'a> {
    let paired = connected_paired(&["penguin"], cx);
    cx.update(|cx| remote.install(cx));
    let machines = open_with(false, paired, cx);
    machines.cx.run_until_parked();
    machines
}

/// Whether the Server row's ⋯ menu offers `selector`. It closes the menu again without
/// choosing anything.
fn menu_offers(selector: &str, cx: &mut VisualTestContext) -> bool {
    click(super::super::machine_card::MENU_SELECTOR, cx);
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

fn footnote(machines: &Machines<'_>) -> String {
    machines.window.read_with(machines.cx, |settings, cx| {
        settings
            .machine_card_footnote(cx)
            .map(|footnote| footnote.to_string())
            .unwrap_or_default()
    })
}

fn state(machines: &Machines<'_>) -> super::super::machine_card::CoreState {
    machines.window.read_with(machines.cx, |settings, _| {
        settings.core_state(&super::super::machine_list::MachineChoice::Direct(
            machines.routes[0],
        ))
    })
}

#[gpui::test]
fn a_connected_paired_machine_offers_restart_but_never_start_or_stop(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    let machines = open_penguin(&remote, cx);

    assert!(menu_offers(RESTART_SELECTOR, machines.cx));
    // A paired Machine has no Stop change at all; the local Stop is never offered for it either.
    assert!(
        !menu_offers(
            super::super::machines::ServiceAction::Stop.selector(),
            machines.cx
        ),
        "Stop runs only on the Machine itself"
    );
    assert!(
        !menu_offers(
            super::super::machines::ServiceAction::Start.selector(),
            machines.cx
        ),
        "Start stays on the server's own computer"
    );
    assert!(
        remote.prepared.lock().unwrap().is_empty(),
        "showing the card asks the server nothing"
    );
}

#[gpui::test]
fn a_disconnected_paired_machine_takes_no_lane_step(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    let paired = disconnected(&["penguin"]);
    cx.update(|cx| remote.install(cx));
    let machines = open_with(false, paired, cx);
    machines.cx.run_until_parked();

    let ready = machines.window.read_with(machines.cx, |settings, cx| {
        settings.remote_ready(&machines.routes[0], cx)
    });
    assert!(!ready);
    click(super::super::machine_card::MENU_SELECTOR, machines.cx);
    click(RESTART_SELECTOR, machines.cx);
    assert!(remote.prepared.lock().unwrap().is_empty());
    assert!(
        footnote(&machines).contains("Connect to penguin to change these settings."),
        "got {:?}",
        footnote(&machines)
    );
}

#[gpui::test]
fn cancelling_a_confirmation_executes_nothing(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.prepare.lock().unwrap() = Ok(Prepared {
        preview: preview(LaneAction::Restart, effects(0)),
        settled: Vec::new(),
    });
    let machines = open_penguin(&remote, cx);

    click(RESTART_SELECTOR, machines.cx);
    answer(CANCEL_SELECTOR, machines.cx);
    machines.cx.run_until_parked();

    assert!(remote.performed.lock().unwrap().is_empty());
    let ready = machines.window.read_with(machines.cx, |settings, cx| {
        settings.remote_ready(&machines.routes[0], cx)
    });
    assert!(ready, "the card is free again");
}

/// A refused preview explains itself and asks nothing.
#[gpui::test]
fn a_refused_preview_explains_why_and_asks_nothing(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.prepare.lock().unwrap() = Err(RemoteFailure::Busy);
    let machines = open_penguin(&remote, cx);

    click(RESTART_SELECTOR, machines.cx);

    assert!(!alert_open(machines.cx));
    assert!(
        footnote(&machines).contains("penguin is finishing another change."),
        "got {:?}",
        footnote(&machines)
    );
}

/// Check Now reads the server's lane. A newer release puts Update on the row, which confirms the
/// server's preview and follows the update until the server returns.
#[gpui::test]
fn check_now_offers_the_machines_update_and_update_follows_it_to_the_end(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.read.lock().unwrap() = Ok(Reading {
        status: pending_status(version("1.2.0", "aaaaaaaaaa"), "1.3.0"),
        settled: vec![receipt(LaneAction::Restart, LaneOperationState::Done, "a")],
    });
    *remote.prepare.lock().unwrap() = Ok(Prepared {
        preview: update_preview(None),
        settled: Vec::new(),
    });
    let update = LaneAction::Restart;
    *remote.perform.lock().unwrap() = Ok(Performed {
        outcome: Outcome::Settled {
            receipt: receipt(update, LaneOperationState::Done, "b"),
            boot: "a".into(),
        },
        settled: Vec::new(),
    });
    let machines = open_penguin(&remote, cx);

    click(
        super::super::machine_card::VERSION_CHECK_SELECTOR,
        machines.cx,
    );
    machines.cx.run_until_parked();
    let subtitle = machines
        .window
        .read_with(machines.cx, |settings, cx| settings.core_subtitle(cx));
    assert_eq!(subtitle.as_ref(), "Version 1.2.0 \u{b7} 1.3.0 is available");
    assert!(
        footnote(&machines)
            .contains("SpaceTerm confirmed that an earlier restart of penguin finished."),
        "got {:?}",
        footnote(&machines)
    );

    click(UPDATE_SELECTOR, machines.cx);
    assert_eq!(*remote.prepared.lock().unwrap(), vec![Change::Update]);
    let tree = A11yTree::read(machines.cx);
    let message = tree
        .find_text("Update SpaceTerm Core on penguin to 1.3.0 (commit bbbbbbb)?")
        .is_some();
    assert!(message, "the alert names the server and the version");
    answer(CONFIRM_SELECTOR, machines.cx);
    machines.cx.run_until_parked();

    assert_eq!(remote.performed.lock().unwrap().len(), 1);
    assert!(
        footnote(&machines).contains("penguin updated to 1.3.0."),
        "got {:?}",
        footnote(&machines)
    );
    assert!(state(&machines).operation.is_none(), "nothing is underway");
}

/// A reading belongs to the link it was read over. Once the connection is replaced, as after an
/// update made from the CLI, the facts the new connection observes take precedence, and the old
/// update offer goes.
#[gpui::test]
fn a_new_connection_retires_the_last_lane_reading(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.read.lock().unwrap() = Ok(Reading {
        status: pending_status(version("1.2.0", "aaaaaaaaaa"), "1.3.0"),
        settled: Vec::new(),
    });
    let machines = open_penguin(&remote, cx);
    let route = machines.routes[0];
    let subtitle = |machines: &Machines<'_>| {
        machines
            .window
            .read_with(machines.cx, |settings, cx| settings.core_subtitle(cx))
    };

    click(
        super::super::machine_card::VERSION_CHECK_SELECTOR,
        machines.cx,
    );
    machines.cx.run_until_parked();
    assert_eq!(
        subtitle(&machines).as_ref(),
        "Version 1.2.0 \u{b7} 1.3.0 is available"
    );

    let late = remote.hold_read();
    click(
        super::super::machine_card::VERSION_CHECK_SELECTOR,
        machines.cx,
    );
    machines.cx.run_until_parked();
    late.wait_started(&machines.cx.cx);
    // Someone updated penguin from its own command line, and SpaceTerm's connection to it is
    // replaced.
    let before = machines.direct.core(&route).expect("a connection");
    machines.direct.connect(&route);
    let direct = machines.direct.clone();
    settle(machines.cx, |_| {
        direct
            .core(&route)
            .is_some_and(|core| !core.same_handle(&before))
            && direct.status(&route)
                == Some(crate::direct_connections::DirectRouteStatus::Connected)
    });
    let replacement_lane = ScriptedRemote::new();
    *replacement_lane.read.lock().unwrap() = Ok(Reading {
        status: status(version("2.4.0", "cccccccccc")),
        settled: Vec::new(),
    });
    machines.cx.update(|_, cx| replacement_lane.install(cx));
    late.release(Ok(Reading {
        status: pending_status(version("1.1.0", "dddddddddd"), "1.9.0"),
        settled: Vec::new(),
    }));
    machines.cx.run_until_parked();
    let key = super::super::machine_list::MachineChoice::Direct(route).key();
    let observed = machines.cx.update(|_, cx| {
        super::super::machine_selection::facts(&key, cx).and_then(|facts| facts.version)
    });

    // The card shows what the new connection observed, never the old link's version.
    let expected = observed.map_or_else(
        || "Version unknown".to_owned(),
        |version| format!("Version {version}"),
    );
    assert_eq!(subtitle(&machines).as_ref(), expected);
    assert!(!subtitle(&machines).contains("1.2.0"));
    assert!(!subtitle(&machines).contains("1.1.0"));
    assert!(!subtitle(&machines).contains("1.9.0"));
    click(
        super::super::machine_card::VERSION_CHECK_SELECTOR,
        machines.cx,
    );
    machines.cx.run_until_parked();
    machines
        .window
        .update(machines.cx, |settings, cx| settings.read_remote(route, cx));
    machines.cx.run_until_parked();
    assert_eq!(subtitle(&machines).as_ref(), "Version 2.4.0");
    assert!(
        machines.cx.debug_bounds(UPDATE_SELECTOR).is_none(),
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
    let machines = open_penguin(&remote, cx);

    click(RESTART_SELECTOR, machines.cx);
    answer(CONFIRM_SELECTOR, machines.cx);
    machines.cx.run_until_parked();
    assert!(
        footnote(&machines).contains(
            "SpaceTerm couldn\u{2019}t confirm whether penguin finished its restart. Check Now \
             asks penguin again."
        ),
        "got {:?}",
        footnote(&machines)
    );

    // The server's start-up couldn't prove the Restart either, so its receipt settles as
    // unresolved, and this connection acknowledges it.
    *remote.read.lock().unwrap() = Ok(Reading {
        status: status(version("1.2.0", "aaaaaaaaaa")),
        settled: vec![receipt(
            LaneAction::Restart,
            LaneOperationState::Unresolved,
            "b",
        )],
    });
    click(
        super::super::machine_card::VERSION_CHECK_SELECTOR,
        machines.cx,
    );
    machines.cx.run_until_parked();
    assert!(
        footnote(&machines)
            .contains("penguin couldn\u{2019}t confirm whether an earlier restart finished."),
        "got {:?}",
        footnote(&machines)
    );
}

/// The card makes no Network Access change, so it names one only when a connection settles an
/// earlier change the CLI made.
#[test]
fn an_earlier_network_access_change_reads_as_an_earlier_change() {
    let network = LaneAction::Network {
        access: spaceterm_core::service::lane::network::NetworkAccess::All,
    };
    let (act, settlement) = earlier(&[receipt(network, LaneOperationState::Done, "a")]).unwrap();
    assert_eq!(act, Act::NetworkAccess);
    assert_eq!(
        earlier_message("penguin", &act, &settlement).0,
        "SpaceTerm confirmed that an earlier Network Access change of penguin finished."
    );
    assert_eq!(
        ended_message("penguin", &Ended::Changed(act, Settlement::Done)).0,
        "penguin changed its Network Access."
    );
}

/// Remove Machine previews the uninstall, names the machine and the sessions that close, and
/// once the Core admits it, the machine leaves this computer with nothing left to unpair.
#[gpui::test]
fn remove_machine_uninstalls_core_there_and_unpairs_in_one_step(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.prepare.lock().unwrap() = Ok(Prepared {
        preview: preview(LaneAction::Uninstall, effects(1)),
        settled: Vec::new(),
    });
    // The Core answers nothing once its uninstall starts, so the lane returns at admission.
    *remote.perform.lock().unwrap() = Ok(Performed {
        outcome: Outcome::Shutdown {
            operation: "0123456789abcdef0123456789abcdef".into(),
            boot: "a".into(),
        },
        settled: Vec::new(),
    });
    let machines = open_penguin(&remote, cx);
    let route = machines.routes[0];

    click(REMOVE_SELECTOR, machines.cx);
    assert_eq!(*remote.prepared.lock().unwrap(), vec![Change::Remove]);
    let tree = A11yTree::read(machines.cx);
    assert!(tree.find_text("Remove penguin?").is_some());
    assert!(
        tree.find_text(
            "This will uninstall SpaceTerm Core on penguin, close your 1 running session there, \
             and unpair it."
        )
        .is_some()
    );
    assert!(
        remote.performed.lock().unwrap().is_empty(),
        "Remove asks first"
    );

    answer(CONFIRM_SELECTOR, machines.cx);
    let direct = machines.direct.clone();
    settle(machines.cx, |_| direct.route(&route).is_none());

    assert_eq!(remote.performed.lock().unwrap().len(), 1);
    assert!(
        machines.direct.pending_unpairs().is_empty(),
        "the uninstalled Core already revoked every pairing"
    );
    let selected = machines
        .window
        .read_with(machines.cx, |settings, _| settings.selected_machine());
    assert_ne!(
        selected,
        Some(super::super::machine_list::MachineChoice::Direct(route)),
        "the selection leaves the removed machine"
    );
}

/// Confirms Remove Machine with the lane answering `performed`, and checks that the Machine, its
/// key and its pairing stay, so a later check can settle what happened.
fn removal_keeps_the_machine(
    performed: Result<Performed, RemoteFailure>,
    says: &str,
    cx: &mut TestAppContext,
) {
    let remote = ScriptedRemote::new();
    *remote.prepare.lock().unwrap() = Ok(Prepared {
        preview: preview(LaneAction::Uninstall, effects(0)),
        settled: Vec::new(),
    });
    *remote.perform.lock().unwrap() = performed;
    let machines = open_penguin(&remote, cx);
    let route = machines.routes[0];

    click(REMOVE_SELECTOR, machines.cx);
    answer(CONFIRM_SELECTOR, machines.cx);
    let window = machines.window.clone();
    settle(machines.cx, |cx| {
        window.read_with(cx, |settings, cx| {
            settings
                .machine_card_footnote(cx)
                .is_some_and(|footnote| footnote.contains(says))
        })
    });

    assert_eq!(remote.performed.lock().unwrap().len(), 1);
    assert!(
        machines.direct.route(&route).is_some(),
        "the Machine stays until its Core admits the uninstall"
    );
    assert!(
        machines.direct.pending_unpairs().is_empty(),
        "nothing was unpaired"
    );
}

/// No Core run accepted the request, which may never have arrived.
#[gpui::test]
fn an_uninstall_no_core_admitted_keeps_the_machine_and_its_pairing(cx: &mut TestAppContext) {
    removal_keeps_the_machine(
        Ok(Performed {
            outcome: Outcome::Unconfirmed {
                operation: "0123456789abcdef0123456789abcdef".into(),
                boot: String::new(),
                last: None,
            },
            settled: Vec::new(),
        }),
        "SpaceTerm couldn\u{2019}t confirm whether penguin finished its uninstall.",
        cx,
    );
}

#[gpui::test]
fn an_uninstall_that_failed_keeps_the_machine_and_its_pairing(cx: &mut TestAppContext) {
    removal_keeps_the_machine(
        Err(RemoteFailure::Unreachable),
        &RemoteFailure::Unreachable.explanation("penguin"),
        cx,
    );
}

/// A Core too old to uninstall itself refuses, and Remove Machine then says why it's unavailable.
#[gpui::test]
fn remove_machine_says_why_a_core_too_old_to_uninstall_cant_be_removed(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    *remote.prepare.lock().unwrap() = Err(RemoteFailure::of_code("unsupported"));
    let machines = open_penguin(&remote, cx);

    click(REMOVE_SELECTOR, machines.cx);
    assert!(!alert_open(machines.cx));
    let reason = "Remove Machine needs a newer SpaceTerm Core on penguin. Update it first.";
    assert!(
        footnote(&machines).contains(reason),
        "got {:?}",
        footnote(&machines)
    );

    click(super::super::machine_card::MENU_SELECTOR, machines.cx);
    let tree = A11yTree::read(machines.cx);
    assert_eq!(
        tree.node("Remove Machine\u{2026}")["aria"]["disabled"],
        true
    );
    assert!(
        machines.direct.route(&machines.routes[0]).is_some(),
        "nothing was removed"
    );
}

/// A reading that shows a Core whose service lane predates uninstall makes Remove Machine
/// unavailable and says why before the person asks.
#[gpui::test]
fn a_reading_of_a_core_too_old_to_uninstall_disables_remove_machine(cx: &mut TestAppContext) {
    let remote = ScriptedRemote::new();
    let mut old = status(version("0.9.0", "aaaaaaaaaa"));
    old.lane_major = 0;
    *remote.read.lock().unwrap() = Ok(Reading {
        status: old,
        settled: Vec::new(),
    });
    let machines = open_penguin(&remote, cx);

    click(
        super::super::machine_card::VERSION_CHECK_SELECTOR,
        machines.cx,
    );
    machines.cx.run_until_parked();

    assert!(
        footnote(&machines)
            .contains("Remove Machine needs a newer SpaceTerm Core on penguin. Update it first."),
        "got {:?}",
        footnote(&machines)
    );
    click(super::super::machine_card::MENU_SELECTOR, machines.cx);
    let tree = A11yTree::read(machines.cx);
    assert_eq!(
        tree.node("Remove Machine\u{2026}")["aria"]["disabled"],
        true
    );
    assert!(remote.prepared.lock().unwrap().is_empty());
}
