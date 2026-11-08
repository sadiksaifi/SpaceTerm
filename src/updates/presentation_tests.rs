use super::*;
use spaceterm_core::protocol::capability::{Capabilities, Capability};
use spaceterm_core::update::facts::{
    PendingVersion, Platform, Protocols, StrandedDevice, UpdateFacts, UpdateFailure,
};

const NOW: u64 = 1_800_000_000;
const PENGUIN: MachineId = [2; 32];
const BUILD_BOX: MachineId = [3; 32];
const LOCAL: MachineId = [1; 32];

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn machine(id: MachineId, name: &'static str, sessions: u32) -> MachineUpdate {
    MachineUpdate {
        machine: id,
        name: name.into(),
        local: id == LOCAL,
        link: MachineLink::Connected,
        facts: Some(UpdateFacts {
            revision: 1,
            platform: Platform::LinuxX86_64,
            running: version("0.8.0"),
            pending: None,
            sessions,
            operation: None,
            check: UpdateCheck::Succeeded { at: NOW - 60 },
            skipped: None,
            last_failure: None,
        }),
        capabilities: Capabilities::fake(&Capability::ALL),
        push: None,
    }
}

fn waiting(mut update: MachineUpdate, installed_at: u64) -> MachineUpdate {
    let facts = update.facts.as_mut().unwrap();
    facts.pending = Some(PendingVersion {
        version: version("0.9.0"),
        protocols: Protocols::CURRENT,
        breaking: false,
        installed_at,
        overdue_since: (NOW >= installed_at + DAY).then_some(installed_at + DAY),
        stranded: Vec::new(),
    });
    update
}

fn desktop(state: &UpdateState, stage: UpdateStage) -> DesktopFacts<'_> {
    DesktopFacts {
        state,
        pending: None,
        stage,
        notes_unread: false,
        running: "0.8.0",
    }
}

fn present(machines: &[MachineUpdate]) -> Presentation {
    Presentation::new(
        desktop(&UpdateState::UpToDate, UpdateStage::Optional),
        machines,
        &Dismissed::new(),
        NOW,
    )
}

fn single(presentation: &Presentation) -> Item {
    match presentation.control() {
        Control::Single(item) => item,
        other => panic!("expected one item, got {other:?}"),
    }
}

#[test]
fn the_control_is_hidden_until_something_needs_doing_or_reading() {
    let presentation = present(&[
        machine(LOCAL, "This Mac", 3),
        machine(PENGUIN, "penguin", 0),
    ]);
    assert_eq!(presentation.control(), Control::Hidden);

    let mut offline = machine(BUILD_BOX, "build-box", 0);
    offline.link = MachineLink::Offline { since: NOW - 60 };
    offline.facts = None;
    assert_eq!(present(&[offline]).control(), Control::Hidden);
}

#[test]
fn restarting_a_core_with_sessions_asks_with_the_agreed_alert() {
    let presentation = present(&[waiting(machine(PENGUIN, "penguin", 3), NOW - 60)]);
    let item = single(&presentation);
    assert_eq!(
        item.label.as_ref(),
        "Update penguin…",
        "it names the Machine and asks first"
    );
    assert_eq!(item.tone, Tone::Action);
    assert_eq!(item.glyph, Glyph::UpdateCore);
    let Some(Command::Confirm(confirmation)) = item.command else {
        panic!("a restart that closes sessions asks first");
    };
    assert_eq!(
        confirmation.title.as_ref(),
        "Update SpaceTerm Core on penguin?"
    );
    assert_eq!(
        confirmation.message.as_ref(),
        "SpaceTerm Core restarts from 0.8.0 to 0.9.0. This ends 3 Terminal Sessions on penguin. Your Workspaces, Tabs and Panes stay, and each Pane starts a new shell when you select it."
    );
    assert_eq!(confirmation.button.as_ref(), "Update Core");
    assert_eq!(
        confirmation.actions,
        vec![(PENGUIN, MachineAction::Restart)]
    );
}

#[test]
fn a_restart_with_no_sessions_acts_unless_it_breaks_other_devices() {
    let presentation = present(&[waiting(machine(PENGUIN, "penguin", 0), NOW - 60)]);
    assert_eq!(
        presentation.machine(PENGUIN).unwrap().restart_command(),
        Command::Machine(PENGUIN, MachineAction::Restart)
    );

    assert_eq!(
        presentation.control(),
        Control::Hidden,
        "a non-breaking update with no sessions restarts automatically"
    );
    let mut breaking = waiting(machine(PENGUIN, "penguin", 0), NOW - 60);
    let pending = breaking.facts.as_mut().unwrap().pending.as_mut().unwrap();
    pending.breaking = true;
    pending.stranded = ["Sadik\u{2019}s iPhone", "old-mini"]
        .iter()
        .enumerate()
        .map(|(n, name)| StrandedDevice {
            pairing: [n as u8; 32],
            name: (*name).into(),
        })
        .collect();
    let presentation = present(&[breaking]);
    let Some(Command::Confirm(confirmation)) = single(&presentation).command else {
        panic!("a breaking restart always asks");
    };
    assert_eq!(
        confirmation.message.as_ref(),
        "SpaceTerm Core restarts from 0.8.0 to 0.9.0. This ends 0 Terminal Sessions on penguin. Your Workspaces, Tabs and Panes stay, and each Pane starts a new shell when you select it. SpaceTerm on Sadik\u{2019}s iPhone and old-mini will stop working with penguin until they update."
    );
}

#[test]
fn the_desktop_relaunch_never_asks() {
    let ready = UpdateState::Ready {
        version: "0.9.0".into(),
    };
    let presentation = Presentation::new(
        desktop(&ready, UpdateStage::Optional),
        &[],
        &Dismissed::new(),
        NOW,
    );
    let item = single(&presentation);
    assert_eq!(item.label.as_ref(), "Relaunch");
    assert_eq!(item.tone, Tone::Action);
    assert_eq!(item.command, Some(Command::Desktop(DesktopAction::Update)));
}

#[test]
fn after_one_day_the_desktop_update_is_overdue_and_reminds() {
    let ready = UpdateState::Ready {
        version: "0.9.0".into(),
    };
    for (stage, body) in [
        (
            UpdateStage::Warning,
            "Relaunch to install SpaceTerm 0.9.0. Your Terminal Sessions keep running.",
        ),
        (
            UpdateStage::Overdue,
            "SpaceTerm 0.9.0 has waited more than two days. Relaunch to install it. Your Terminal Sessions keep running.",
        ),
    ] {
        let presentation = Presentation::new(desktop(&ready, stage), &[], &Dismissed::new(), NOW);
        let item = single(&presentation);
        assert_eq!(item.label.as_ref(), "Relaunch");
        assert_eq!(item.tone, Tone::Problem);
        assert_eq!(item.glyph, Glyph::Restart);
        assert_eq!(item.command, Some(Command::Desktop(DesktopAction::Update)));
        let reminders = presentation.reminders();
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].title.as_ref(), "SpaceTerm update overdue");
        assert_eq!(reminders[0].body.as_ref(), body);
    }
}

#[test]
fn an_overdue_core_restart_names_the_machine_and_its_sessions() {
    let presentation = present(&[waiting(machine(PENGUIN, "penguin", 3), NOW - DAY - 60)]);
    let item = single(&presentation);
    assert_eq!(item.label.as_ref(), "Update penguin…");
    assert_eq!(item.tone, Tone::Problem);
    assert!(matches!(item.command, Some(Command::Confirm(_))));
    assert_eq!(
        presentation.reminders(),
        vec![Reminder {
            key: ItemKey::Machine(PENGUIN),
            title: "Restart overdue on penguin".into(),
            body: "SpaceTerm Core 0.9.0 has waited 1 day. Restarting closes 3 running sessions."
                .into(),
        }]
    );
    let rows = presentation.machine_rows();
    assert_eq!(rows[0].status.as_ref(), "Overdue · 3 sessions");
    assert_eq!(
        rows[0].button.as_ref().unwrap().label.as_ref(),
        "Update penguin…"
    );
}

#[test]
fn two_items_open_a_menu_with_restart_all() {
    let presentation = present(&[
        waiting(machine(LOCAL, "This Mac", 3), NOW - 60),
        waiting(machine(PENGUIN, "penguin", 4), NOW - 60),
    ]);
    let Control::Menu {
        label, tone, glyph, ..
    } = presentation.control()
    else {
        panic!("two items open a menu");
    };
    assert_eq!(label.as_ref(), "2 updates");
    assert_eq!(tone, Tone::Action);
    assert_eq!(glyph, Glyph::UpdateCore);
    let all = presentation.restart_all().unwrap();
    assert_eq!(all.label.as_ref(), "Update All…");
    assert_eq!(all.trailing.as_ref(), "7 sessions");
    assert_eq!(
        all.subtitle.as_ref(),
        "Updates 2 machines, closes 7 sessions"
    );
    let Command::Confirm(confirmation) = all.command else {
        panic!("Restart All asks");
    };
    assert_eq!(
        confirmation.title.as_ref(),
        "Update SpaceTerm Core on This Mac and penguin?"
    );
    assert_eq!(
        confirmation.message.as_ref(),
        "SpaceTerm Core restarts from 0.8.0 to 0.9.0. This ends 3 Terminal Sessions on This Mac. Your Workspaces, Tabs and Panes stay, and each Pane starts a new shell when you select it. SpaceTerm Core restarts from 0.8.0 to 0.9.0. This ends 4 Terminal Sessions on penguin. Your Workspaces, Tabs and Panes stay, and each Pane starts a new shell when you select it."
    );
    assert_eq!(confirmation.actions.len(), 2);
}

#[test]
fn a_problem_turns_the_menu_orange_and_leads_its_glyph() {
    let mut failed = machine(PENGUIN, "penguin", 0);
    failed.facts.as_mut().unwrap().last_failure = Some(UpdateFailure {
        id: 7,
        at: NOW - 60,
        version: version("0.9.0"),
        kind: FailureKind::Start(StartFailure::NotReady),
    });
    let machines = [waiting(machine(LOCAL, "This Mac", 3), NOW - 60), failed];
    let presentation = present(&machines);
    let Control::Menu { tone, glyph, .. } = presentation.control() else {
        panic!("two items open a menu");
    };
    assert_eq!(tone, Tone::Problem);
    assert_eq!(glyph, Glyph::UpdateFailed);
    assert_eq!(presentation.restart_all(), None);

    let rows = presentation.machine_rows();
    assert_eq!(rows[1].status.as_ref(), "Back on 0.8.0");

    let dismissed = Dismissed::from([(PENGUIN, 7)]);
    let presentation = Presentation::new(
        desktop(&UpdateState::UpToDate, UpdateStage::Optional),
        &machines,
        &dismissed,
        NOW,
    );
    assert!(matches!(presentation.control(), Control::Single(_)));
}

#[test]
fn a_failed_update_explains_with_the_agreed_sentence_and_ok_dismisses_it() {
    let mut failed = machine(PENGUIN, "penguin", 0);
    failed.facts.as_mut().unwrap().last_failure = Some(UpdateFailure {
        id: 7,
        at: NOW - 60,
        version: version("0.9.0"),
        kind: FailureKind::Start(StartFailure::NotReady),
    });
    let presentation = present(&[failed]);
    let item = single(&presentation);
    assert_eq!(item.label.as_ref(), "penguin update failed");
    let Some(Command::Explain(explanation)) = item.command else {
        panic!("a failure explains");
    };
    assert_eq!(
        explanation.title.as_ref(),
        "SpaceTerm Core 0.9.0 couldn't start on penguin"
    );
    assert_eq!(
        explanation.message.as_ref(),
        "SpaceTerm Core 0.9.0 couldn't start on penguin. penguin went back to SpaceTerm Core 0.8.0. Its Workspaces are kept. The Terminal Sessions that ended don't come back."
    );
    assert_eq!(
        explanation.then.as_deref(),
        Some(&Command::Machine(PENGUIN, MachineAction::DismissFailure(7)))
    );
}

#[test]
fn the_named_states_match_the_mockup() {
    let mut too_old = machine(PENGUIN, "penguin", 3);
    too_old.link = MachineLink::Incompatible(Incompatible::ListenerTooOld);
    too_old.push = Some(version("0.9.0"));
    let item = single(&present(&[too_old]));
    assert_eq!(
        (item.label.as_ref(), item.tone, item.command),
        (
            "Update penguin",
            Tone::Problem,
            Some(Command::Machine(PENGUIN, MachineAction::Install))
        )
    );

    let mut desktop_too_old = machine(PENGUIN, "penguin", 3);
    desktop_too_old.link = MachineLink::Incompatible(Incompatible::CallerTooOld);
    let item = single(&present(&[desktop_too_old]));
    assert_eq!(
        (item.key, item.label.as_ref(), item.tone),
        (ItemKey::Desktop, "Update SpaceTerm", Tone::Problem)
    );

    let mut cant_check = machine(PENGUIN, "penguin", 0);
    cant_check.facts.as_mut().unwrap().check = UpdateCheck::Failing {
        since: NOW - 2 * DAY,
        last_success: None,
        reason: CheckFailure::Network,
    };
    let item = single(&present(std::slice::from_ref(&cant_check)));
    assert_eq!(item.label.as_ref(), "Can't check penguin");
    assert!(matches!(item.command, Some(Command::Explain(_))));
    cant_check.push = Some(version("0.9.0"));
    assert_eq!(
        single(&present(&[cant_check])).command,
        Some(Command::Machine(PENGUIN, MachineAction::Install))
    );

    let mut install_failed = machine(PENGUIN, "penguin", 0);
    install_failed.facts.as_mut().unwrap().last_failure = Some(UpdateFailure {
        id: 3,
        at: NOW - 60,
        version: version("0.9.0"),
        kind: FailureKind::Install(InstallFailure::Download),
    });
    let item = single(&present(&[install_failed]));
    assert_eq!(item.label.as_ref(), "penguin install failed");
    let Some(Command::Explain(explanation)) = item.command else {
        panic!("Install failed explains");
    };
    assert_eq!(
        explanation.message.as_ref(),
        "SpaceTerm Core 0.9.0 didn't finish downloading on penguin. SpaceTerm tries the \
         install again later."
    );

    let mut unconfirmed = machine(BUILD_BOX, "build-box", 0);
    unconfirmed.link = MachineLink::Unconfirmed {
        expected: version("0.9.0"),
        since: NOW - 60,
    };
    let item = single(&present(&[unconfirmed]));
    assert_eq!(
        (item.label.as_ref(), item.command),
        (
            "Can't confirm build-box",
            Some(Command::Machine(BUILD_BOX, MachineAction::CheckAgain))
        )
    );
}

#[test]
fn work_in_progress_is_a_status_with_a_spinner() {
    let mut installing = machine(PENGUIN, "penguin", 0);
    installing.facts.as_mut().unwrap().operation = Some(Operation::Installing {
        version: version("0.9.0"),
        received: 5,
        total: 10,
    });
    let shown = present(&[installing]);
    assert_eq!(shown.control(), Control::Hidden);
    let rows = shown.machine_rows();
    assert_eq!(rows[0].status.as_ref(), "Installing 0.9.0…");
    assert_eq!(rows[0].glyph, Some(Glyph::Working));
    assert_eq!(rows[0].fraction, Some(0.5));
    assert_eq!(rows[0].button, None);
}

#[test]
fn every_core_label_names_its_subject() {
    let states = [
        MachineState::Current,
        MachineState::Available {
            to: version("0.9.0"),
        },
        MachineState::Connecting,
        MachineState::Ready {
            to: version("0.9.0"),
            breaking: true,
            stranded: Vec::new(),
        },
        MachineState::Overdue {
            to: version("0.9.0"),
            days: 1,
            breaking: false,
            stranded: Vec::new(),
        },
        MachineState::CoreTooOld { push: None },
        MachineState::DesktopTooOld,
        MachineState::Installing {
            to: version("0.9.0"),
            fraction: None,
        },
        MachineState::Restarting {
            to: version("0.9.0"),
        },
        MachineState::Checking,
        MachineState::Stopping,
        MachineState::Uninstalling,
        MachineState::UpdateFailed {
            id: 1,
            version: version("0.9.0"),
            reason: StartFailure::NotReady,
        },
        MachineState::InstallFailed {
            id: 1,
            version: version("0.9.0"),
            reason: InstallFailure::DiskFull,
        },
        MachineState::CantConfirm {
            expected: version("0.9.0"),
        },
        MachineState::CantCheck {
            reason: CheckProblem::Failing(CheckFailure::Proxy),
            push: None,
        },
        MachineState::Stopped,
        MachineState::Offline { offered: None },
    ];
    for state in states {
        let view = MachineView {
            machine: PENGUIN,
            name: "penguin".into(),
            local: false,
            running: Some(version("0.8.0")),
            sessions: 3,
            pending: None,
            state,
        };
        assert!(
            view.control_label()
                .contains(if view.state == MachineState::DesktopTooOld {
                    "SpaceTerm"
                } else {
                    "penguin"
                }),
            "{view:?}"
        );
    }
}

#[test]
fn the_menu_bar_extra_uses_one_line_pattern_and_offers_stop_only_on_the_local_machine() {
    let presentation = present(&[
        waiting(machine(LOCAL, "This Mac", 3), NOW - 60),
        machine(PENGUIN, "penguin", 0),
    ]);
    let rows = presentation.extra_rows();
    assert_eq!(rows[0].line.as_ref(), "Version 0.8.0 · Update ready");
    assert_eq!(rows[1].line.as_ref(), "Version 0.8.0 · No sessions");
    let local = rows[0].items.as_ref().unwrap();
    let ExtraItem::Action {
        title, subtitle, ..
    } = &local[0]
    else {
        panic!("the update action leads");
    };
    assert_eq!(title.as_ref(), "Update This Mac…");
    assert_eq!(subtitle.as_ref().unwrap().as_ref(), "Closes 3 sessions");
    let Some(ExtraItem::Action { title, .. }) = local.last() else {
        panic!("Stop ends the local submenu");
    };
    assert_eq!(title.as_ref(), "Stop SpaceTerm Core…");
    let remote = rows[1].items.as_ref().unwrap();
    assert!(
        remote.is_empty(),
        "same-version restart appears only in Settings"
    );
}

#[test]
fn a_breaking_restart_asks_even_with_no_sessions_and_no_stranded_devices() {
    let mut breaking = waiting(machine(PENGUIN, "penguin", 0), NOW - 60);
    let pending = breaking.facts.as_mut().unwrap().pending.as_mut().unwrap();
    pending.breaking = true;
    let presentation = present(&[breaking]);
    let item = single(&presentation);
    assert_eq!(item.label.as_ref(), "Update penguin…");
    let Some(Command::Confirm(confirmation)) = item.command else {
        panic!("a breaking restart always asks");
    };
    assert_eq!(
        confirmation.message.as_ref(),
        "SpaceTerm Core restarts from 0.8.0 to 0.9.0. This ends 0 Terminal Sessions on penguin. Your Workspaces, Tabs and Panes stay, and each Pane starts a new shell when you select it."
    );
}

#[test]
fn an_approval_holds_only_while_the_described_work_is_unchanged() {
    let described = waiting(machine(PENGUIN, "penguin", 3), NOW - 60);
    let presentation = present(std::slice::from_ref(&described));
    let Some(Command::Confirm(confirmation)) = single(&presentation).command else {
        panic!("a restart that closes sessions asks first");
    };
    assert!(
        confirmation
            .approval
            .holds(std::slice::from_ref(&described))
    );

    // Unrelated changes keep it: a later check.
    let mut checked = described.clone();
    checked.facts.as_mut().unwrap().check = UpdateCheck::Succeeded { at: NOW };
    assert!(confirmation.approval.holds(&[checked]));

    let mut more = described.clone();
    more.facts.as_mut().unwrap().sessions = 4;
    assert!(!confirmation.approval.holds(std::slice::from_ref(&more)));
    let mut retargeted = described.clone();
    retargeted
        .facts
        .as_mut()
        .unwrap()
        .pending
        .as_mut()
        .unwrap()
        .version = version("0.9.1");
    assert!(!confirmation.approval.holds(&[retargeted]));
    assert!(
        !confirmation.approval.holds(&[]),
        "a removed Machine holds nothing"
    );
}

#[test]
fn a_confirmation_names_the_sessions_each_core_previews() {
    use crate::updates::machines::testing::preview;

    let described = waiting(machine(PENGUIN, "penguin", 3), NOW - 60);
    let presentation = present(std::slice::from_ref(&described));
    let restart = |sessions| MachinePreview {
        target: Some(version("0.9.0")),
        ..preview(PENGUIN, MachineAction::Restart, sessions)
    };
    let asked = presentation
        .previewed(&[restart(4)], false)
        .expect("four sessions ask");
    assert_eq!(
        asked.message.as_ref(),
        "SpaceTerm Core restarts from 0.8.0 to 0.9.0. This ends 4 Terminal Sessions on penguin. Your Workspaces, Tabs and Panes stay, and each Pane starts a new shell when you select it."
    );
    assert_eq!(
        presentation.previewed(&[restart(0)], false),
        None,
        "with no sessions to close, the restart needs no question"
    );
    let again = presentation
        .previewed(&[restart(0)], true)
        .expect("a refused preview always asks");
    assert_eq!(again.button.as_ref(), "Update Core");

    // A Core without facts is described by its preview alone, and its approval holds on the
    // Core's token.
    let mut unreported = machine(PENGUIN, "penguin", 0);
    unreported.facts = None;
    unreported.link = MachineLink::Unreported;
    let presentation = present(std::slice::from_ref(&unreported));
    let stop = presentation
        .previewed(&[preview(PENGUIN, MachineAction::Stop, 2)], false)
        .expect("two sessions ask");
    assert!(
        stop.message
            .starts_with("This will close your 2 running sessions.")
    );
    assert!(stop.approval.holds(&[unreported]));
}

#[test]
fn start_failures_remain_visible_with_the_machine_and_a_remedy() {
    for (reason, remedy) in [
        (
            StartFailure::DiskFull,
            "Free some space, then choose Update penguin again.",
        ),
        (
            StartFailure::PortInUse,
            "Close that app or change its port, then choose Update penguin again.",
        ),
    ] {
        let mut failed = waiting(machine(PENGUIN, "penguin", 0), NOW - 120);
        failed.facts.as_mut().unwrap().last_failure = Some(UpdateFailure {
            id: 7,
            at: NOW - 60,
            version: version("0.9.0"),
            kind: FailureKind::Start(reason),
        });
        let shown = present(&[failed]);
        assert!(
            matches!(shown.machine(PENGUIN).unwrap().state, MachineState::UpdateFailed { reason: actual, .. } if reason == actual)
        );
        let item = single(&shown);
        assert!(
            item.sentence
                .contains("penguin went back to SpaceTerm Core 0.8.0.")
        );
        assert!(item.sentence.contains(remedy));
    }
}

#[test]
fn a_core_older_than_the_bundled_core_is_never_called_up_to_date() {
    let mut old = machine(PENGUIN, "penguin", 3);
    old.push = Some(version("0.9.0"));
    let shown = present(&[old]);
    assert!(
        !shown
            .machine(PENGUIN)
            .unwrap()
            .sentence()
            .contains("up to date")
    );
    assert!(matches!(
        shown.machine(PENGUIN).unwrap().state,
        MachineState::Available { .. }
    ));
}

#[test]
fn offline_and_unconfirmed_updates_name_the_machine_and_target() {
    let mut offline = machine(PENGUIN, "penguin", 0);
    offline.push = Some(version("0.9.0"));
    offline.link = MachineLink::Offline { since: NOW - 60 };
    assert_eq!(
        present(&[offline])
            .machine(PENGUIN)
            .unwrap()
            .sentence()
            .as_ref(),
        "penguin is offline. It last ran SpaceTerm Core 0.8.0. SpaceTerm installs 0.9.0 when it reconnects."
    );
    let mut unconfirmed = machine(PENGUIN, "penguin", 0);
    unconfirmed.link = MachineLink::Unconfirmed {
        expected: version("0.9.0"),
        since: NOW - 180,
    };
    let item = single(&present(&[unconfirmed]));
    assert_eq!(
        item.sentence.as_ref(),
        "penguin stopped answering during its update, so SpaceTerm can't confirm that SpaceTerm Core 0.9.0 started."
    );
    assert_eq!(
        item.command,
        Some(Command::Machine(PENGUIN, MachineAction::CheckAgain))
    );
}

#[test]
fn desktop_actions_share_relaunch_copy_and_never_claim_a_core_restart() {
    let ready = UpdateState::Ready {
        version: "0.11.1".into(),
    };
    let shown = Presentation::new(
        desktop(&ready, UpdateStage::Optional),
        &[],
        &Dismissed::new(),
        NOW,
    );
    let item = single(&shown);
    assert_eq!(item.glyph, Glyph::Restart);
    assert_eq!(
        item.sentence.as_ref(),
        "SpaceTerm 0.11.1 is ready. Relaunching keeps every Terminal Session running."
    );
    let ExtraItem::Action {
        title, subtitle, ..
    } = &shown.extra_actions()[0]
    else {
        panic!("ready offers Relaunch")
    };
    assert_eq!(title.as_ref(), "Relaunch to Update SpaceTerm");
    assert_eq!(
        subtitle.as_ref().unwrap().as_ref(),
        "Terminal Sessions keep running"
    );
    let available = UpdateState::Available {
        version: "0.11.1".into(),
    };
    let shown = Presentation::new(
        desktop(&available, UpdateStage::Optional),
        &[],
        &Dismissed::new(),
        NOW,
    );
    let ExtraItem::Action { title, .. } = &shown.extra_actions()[0] else {
        panic!("available offers Download")
    };
    assert_eq!(title.as_ref(), "Download SpaceTerm 0.11.1");
}

#[test]
fn unread_notes_and_a_running_installer_do_not_claim_title_bar_space() {
    for state in [
        DesktopState::WhatsNew {
            version: "0.11.1".into(),
        },
        DesktopState::Relaunching {
            version: "0.11.1".into(),
        },
    ] {
        let shown = Presentation {
            desktop: state,
            machines: Vec::new(),
        };
        assert!(shown.items().is_empty());
        assert_eq!(shown.control(), Control::Hidden);
    }
    let failed = UpdateState::Failed {
        error: UpdateError::Relaunch,
    };
    let shown = Presentation::new(
        DesktopFacts {
            pending: Some("0.11.1"),
            ..desktop(&failed, UpdateStage::Optional)
        },
        &[],
        &Dismissed::new(),
        NOW,
    );
    assert_eq!(single(&shown).label.as_ref(), "Relaunch failed");
}

#[test]
fn a_fresh_preview_replaces_cached_versions_before_confirmation() {
    let shown = present(&[waiting(machine(PENGUIN, "penguin", 3), NOW - 60)]);
    let preview = MachinePreview {
        machine: PENGUIN,
        action: MachineAction::Restart,
        id: 1,
        sessions: 4,
        running: Some(version("0.8.1")),
        target: Some(version("0.9.1")),
    };
    let confirmation = shown.previewed(&[preview], false).unwrap();
    assert_eq!(
        confirmation.message.as_ref(),
        "SpaceTerm Core restarts from 0.8.1 to 0.9.1. This ends 4 Terminal Sessions on penguin. Your Workspaces, Tabs and Panes stay, and each Pane starts a new shell when you select it."
    );
}

#[test]
fn an_overdue_or_required_desktop_download_keeps_its_download_name() {
    let available = UpdateState::Available {
        version: "0.11.1".into(),
    };
    let facts = DesktopFacts {
        pending: Some("0.11.1"),
        ..desktop(&available, UpdateStage::Overdue)
    };
    for machines in [
        Vec::new(),
        vec![{
            let mut newer = machine(PENGUIN, "penguin", 3);
            newer.link = MachineLink::Incompatible(Incompatible::CallerTooOld);
            newer
        }],
    ] {
        let shown = Presentation::new(facts, &machines, &Dismissed::new(), NOW);
        assert_eq!(single(&shown).label.as_ref(), "Download SpaceTerm 0.11.1");
        let ExtraItem::Action { title, .. } = &shown.extra_actions()[0] else {
            panic!("the update has an action")
        };
        assert_eq!(title.as_ref(), "Download SpaceTerm 0.11.1");
        assert_eq!(
            shown.desktop_row().unwrap().button.unwrap().label.as_ref(),
            "Download SpaceTerm 0.11.1"
        );
    }
}

#[test]
fn a_failure_notice_does_not_hide_a_breaking_restarts_consequences() {
    let mut failed = waiting(machine(PENGUIN, "penguin", 0), NOW - 120);
    let facts = failed.facts.as_mut().unwrap();
    facts.pending.as_mut().unwrap().breaking = true;
    facts.pending.as_mut().unwrap().stranded = vec![StrandedDevice {
        pairing: [4; 32],
        name: "old-mini".into(),
    }];
    facts.last_failure = Some(UpdateFailure {
        id: 9,
        at: NOW - 60,
        version: version("0.9.0"),
        kind: FailureKind::Start(StartFailure::DiskFull),
    });
    let shown = present(&[failed]);
    let preview = MachinePreview {
        target: Some(version("0.9.0")),
        ..crate::updates::machines::testing::preview(PENGUIN, MachineAction::Restart, 0)
    };
    let confirmation = shown
        .previewed(&[preview], false)
        .expect("a breaking restart always asks, including after a retryable failure");
    assert!(
        confirmation
            .message
            .contains("SpaceTerm on old-mini will stop working with penguin until it updates.")
    );
}
