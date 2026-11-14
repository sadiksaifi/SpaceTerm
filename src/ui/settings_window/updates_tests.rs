//! The Updates section driven through its rendered controls.

use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use crate::appearance::Appearance;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::ui::appearance_runtime;
use crate::updates::policy::{CheckInterval, UpdatePreferences};

use spaceterm_core::protocol::capability::{Capabilities, Capability};
use spaceterm_core::update::facts::{
    PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
};

use super::updates::{APP_MENU_SELECTOR, CHECK_NOW_SELECTOR, RESTART_SELECTOR};
use super::{SettingsSectionId, SettingsWindow};
use crate::settings::storage::testing::MemoryStorage;
use crate::updates::copy::settings as copy;
use crate::updates::machines::store::MachineUpdates;
use crate::updates::machines::testing::ScriptedMachines;
use crate::updates::machines::{
    MachineAction, MachineId, MachineLink, MachineUpdate, MachineUpdatesAdapter,
    MachineUpdatesEvent,
};
use crate::updates::state;
use spaceterm_ui::a11y_testing::A11yTree;

fn open_updates(cx: &mut TestAppContext) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            crate::theme_registry::testing::offline(),
            None,
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    click("settings-navigation-settings-section-updates", cx);
    (window, cx)
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

fn preferences(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> UpdatePreferences {
    window.read_with(cx, |settings, _| settings.editor.document().updates)
}

#[gpui::test]
fn updates_section_should_default_to_quiet_automatic_downloads(cx: &mut TestAppContext) {
    let (window, cx) = open_updates(cx);

    assert_eq!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Updates
    );
    for selector in [
        "settings-row-update-status",
        "settings-row-automatic-update-downloads",
        "settings-row-update-check-interval",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} should render"
        );
    }
    assert!(
        cx.debug_bounds("settings-row-update-reminder-interval")
            .is_none(),
        "reminders repeat daily and have no setting"
    );
    assert_eq!(preferences(&window, cx), UpdatePreferences::default());
    assert!(preferences(&window, cx).automatic_downloads);
    // Defaults offer nothing to reset.
    assert!(
        cx.debug_bounds("settings-row-automatic-update-downloads-reset")
            .is_none()
    );
}

#[gpui::test]
fn update_preferences_should_edit_the_draft_and_reset_one_row_at_a_time(cx: &mut TestAppContext) {
    let (window, cx) = open_updates(cx);

    click("settings-automatic-update-downloads", cx);
    click("settings-update-check-interval-hourly", cx);
    assert_eq!(
        preferences(&window, cx),
        UpdatePreferences {
            automatic_downloads: false,
            check_interval: CheckInterval::Hourly,
        }
    );

    click("settings-row-update-check-interval-reset", cx);
    assert_eq!(
        preferences(&window, cx),
        UpdatePreferences {
            automatic_downloads: false,
            check_interval: CheckInterval::Daily,
        }
    );
    click("settings-row-automatic-update-downloads-reset", cx);
    assert_eq!(preferences(&window, cx), UpdatePreferences::default());
}

/// Without a desktop update service the page still shows the shared SpaceTerm row, and Check
/// Now still asks each connected Machine's SpaceTerm Core to check.
#[gpui::test]
fn without_a_desktop_update_service_the_shared_row_and_check_now_remain(cx: &mut TestAppContext) {
    let adapter = with_machines(vec![machine(PENGUIN, "penguin", 0)], cx);
    let (window, cx) = open_updates(cx);

    let row = cx.update(|_, cx| state::current(cx).app_row());
    let status = window.read_with(cx, |settings, cx| settings.update_status(cx));
    assert_eq!(status.summary.as_ref(), copy::caption(&row));
    assert!(
        A11yTree::read(cx).find_text(&status.summary).is_some(),
        "the SpaceTerm row shows its version: {}",
        status.summary
    );
    click(CHECK_NOW_SELECTOR, cx);
    assert_eq!(adapter.performed(), [(PENGUIN, MachineAction::CheckAgain)]);
}

/// Check Now sits beside the page title, and SpaceTerm's own preferences, which name SpaceTerm,
/// share SpaceTerm's card above every Machine.
#[gpui::test]
fn check_now_sits_beside_the_title_and_spaceterms_preferences_share_its_card(
    cx: &mut TestAppContext,
) {
    with_machines(vec![machine(PENGUIN, "penguin", 0)], cx);
    let (_, cx) = open_updates(cx);
    let mut bounds = |selector: &'static str| {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} renders"))
    };
    let check_now = bounds(CHECK_NOW_SELECTOR);
    let spaceterm = bounds("settings-row-update-status");
    let downloads = bounds("settings-row-automatic-update-downloads");
    let interval = bounds("settings-row-update-check-interval");
    let core = bounds("settings-row-core-updates");
    assert!(
        check_now.bottom() <= spaceterm.top(),
        "Check Now heads the page"
    );
    assert!(spaceterm.bottom() <= downloads.top() && downloads.bottom() <= interval.top());
    assert!(
        interval.bottom() < core.top(),
        "SpaceTerm's card comes first"
    );
    assert!(
        cx.debug_bounds("settings-row-update-check-now").is_none(),
        "Check Now has no row of its own"
    );
    let tree = A11yTree::read(cx);
    for label in [copy::AUTOMATIC_DOWNLOADS, copy::CHECK_INTERVAL] {
        assert!(label.contains("SpaceTerm"), "{label}");
        assert!(tree.find_text(label).is_some(), "{label} renders");
    }
}

/// Opens `menu` and chooses its entry `entry`.
fn choose(menu: &str, entry: &str, cx: &mut VisualTestContext) {
    click_owned(menu, cx);
    click_owned(&format!("{menu}-{entry}"), cx);
}

fn click_owned(selector: &str, cx: &mut VisualTestContext) {
    // Debug selectors are looked up by static name.
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

/// Whether some node reads `text` as its label or value.
fn shows(text: &str, cx: &mut VisualTestContext) -> bool {
    A11yTree::read(cx).in_order().iter().any(|node| {
        ["label", "value"]
            .into_iter()
            .any(|field| node["aria"][field].as_str() == Some(text))
    })
}

/// Each item's menu checks that item alone: SpaceTerm checks without asking any Core, a
/// Machine's Core checks without SpaceTerm, and an offline Machine offers Connect and Check.
#[gpui::test]
fn each_items_menu_checks_only_that_item(cx: &mut TestAppContext) {
    use crate::updates::ApplicationUpdates;
    let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(updater.clone(), cx));
    let mut offline = machine(OFFLINE, "build-box", 3);
    offline.link = MachineLink::Offline { since: 1 };
    let adapter = with_machines(
        vec![
            machine(LOCAL, "This Mac", 2),
            machine(PENGUIN, "penguin", 0),
            offline,
        ],
        cx,
    );
    let (_, cx) = open_updates(cx);
    updater.emit(crate::updates::UpdateEvent::UpToDate);
    updater.emit(crate::updates::UpdateEvent::Finished);
    cx.run_until_parked();
    let checks = updater.checks.get();

    choose("settings-row-core-update-1-menu", "check", cx);
    assert_eq!(adapter.performed(), [(PENGUIN, MachineAction::CheckAgain)]);
    assert_eq!(updater.checks.get(), checks, "SpaceTerm did not check");

    choose(APP_MENU_SELECTOR, "check", cx);
    assert_eq!(updater.checks.get(), checks + 1);
    assert_eq!(adapter.performed().len(), 1, "no Core checked");

    click_owned("settings-row-core-update-2-menu", cx);
    assert!(
        shows("Connect and Check", cx),
        "an offline Machine connects first"
    );
    click_owned("settings-row-core-update-2-menu-check", cx);
    assert_eq!(
        adapter.performed().last(),
        Some(&(OFFLINE, MachineAction::ConnectAndCheck))
    );
    assert!(
        shows("0.8.0 \u{b7} Connecting\u{2026}", cx),
        "the offline row shows the connection the person asked for"
    );
}

/// A Core whose update waits shows a check the person asked for, while it keeps its waiting
/// version and Restart Core: Checking, then a failure, then its usual line after a success.
/// The title bar stays as it was.
#[gpui::test]
fn a_check_shows_on_a_core_whose_update_waits(cx: &mut TestAppContext) {
    let adapter = with_machines(vec![waiting(machine(LOCAL, "This Mac", 2))], cx);
    let (_, cx) = open_updates(cx);
    let before = cx.update(|_, cx| state::current(cx));
    let usual = before.machine_row(LOCAL).unwrap();
    let restart = usual
        .action
        .clone()
        .expect("the waiting Core offers Restart Core");

    choose("settings-row-core-updates-menu", "check", cx);
    assert_eq!(adapter.performed(), [(LOCAL, MachineAction::CheckAgain)]);
    let checking = cx
        .update(|_, cx| state::current(cx))
        .machine_row(LOCAL)
        .unwrap();
    assert_eq!(checking.status.as_deref(), Some("Checking\u{2026}"));
    assert_eq!(
        checking.target.as_deref(),
        Some("0.9.0"),
        "the waiting version stays"
    );
    assert_eq!(checking.action, Some(restart.clone()), "Restart Core stays");
    assert!(shows("0.8.0 \u{2192} 0.9.0 \u{b7} Checking\u{2026}", cx));

    adapter.emit(MachineUpdatesEvent::Finished {
        machine: LOCAL,
        action: MachineAction::CheckAgain,
        outcome: Err(crate::updates::machines::ActionFailure::Unreachable),
    });
    cx.run_until_parked();
    let view = cx.update(|_, cx| state::current(cx));
    let failed = view.machine_row(LOCAL).unwrap();
    assert_eq!(failed.status.as_deref(), Some("Check failed"));
    assert_eq!(
        failed.target.as_deref(),
        Some("0.9.0"),
        "the waiting version stays"
    );
    assert!(shows("0.8.0 \u{2192} 0.9.0 \u{b7} Check failed", cx));
    assert_eq!(failed.tone, state::RowTone::Failure);
    assert_eq!(failed.action, Some(restart.clone()), "Restart Core stays");
    assert_eq!(view.step, before.step, "the title bar shows nothing new");

    let mut checked = waiting(machine(LOCAL, "This Mac", 2));
    let facts = checked.facts.as_mut().unwrap();
    facts.revision = 2;
    facts.check = UpdateCheck::Succeeded {
        at: crate::updates::now() + 1,
    };
    adapter.emit(MachineUpdatesEvent::Changed(Box::new(checked)));
    cx.run_until_parked();
    let after = cx
        .update(|_, cx| state::current(cx))
        .machine_row(LOCAL)
        .unwrap();
    assert_eq!(
        after.status, usual.status,
        "a newer successful check restores the usual line"
    );
    assert_eq!(after.action, Some(restart));
}

/// A check the person started reports at once in each row it reached: Checking while it runs,
/// then a failure with Check Again, for SpaceTerm and for a Core. The title bar stays quiet.
#[gpui::test]
fn a_check_reports_progress_and_failure_in_each_row_at_once(cx: &mut TestAppContext) {
    use crate::updates::{ApplicationUpdates, UpdateError, UpdateEvent};
    let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(updater.clone(), cx));
    let adapter = with_machines(vec![machine(PENGUIN, "penguin", 0)], cx);
    let (_, cx) = open_updates(cx);
    updater.emit(UpdateEvent::UpToDate);
    updater.emit(UpdateEvent::Finished);
    cx.run_until_parked();
    let running = env!("SPACETERM_VERSION");

    click(CHECK_NOW_SELECTOR, cx);
    assert!(shows(&format!("{running} \u{b7} Checking\u{2026}"), cx));
    assert!(shows("0.8.0 \u{b7} Checking\u{2026}", cx));
    assert!(
        cx.debug_bounds(CHECK_NOW_SELECTOR).is_some(),
        "Check Now stays while it waits"
    );

    let mut failing = machine(PENGUIN, "penguin", 0);
    let facts = failing.facts.as_mut().unwrap();
    facts.revision = 2;
    facts.check = UpdateCheck::Failing {
        since: crate::updates::now(),
        last_success: None,
        reason: spaceterm_core::update::facts::CheckFailure::Network,
    };
    adapter.emit(MachineUpdatesEvent::Finished {
        machine: PENGUIN,
        action: MachineAction::CheckAgain,
        outcome: Ok(()),
    });
    adapter.emit(MachineUpdatesEvent::Changed(Box::new(failing)));
    updater.emit(UpdateEvent::Failed(UpdateError::Check));
    updater.emit(UpdateEvent::Finished);
    cx.run_until_parked();

    let failed = crate::updates::copy::rows::CHECK_FAILED;
    assert!(
        shows(&format!("{running} \u{b7} {failed}"), cx),
        "SpaceTerm's check failed"
    );
    assert!(
        shows(&format!("0.8.0 \u{b7} {failed}"), cx),
        "penguin's check failed"
    );
    let view = cx.update(|_, cx| state::current(cx));
    assert_eq!(
        view.step, None,
        "a failed check the person sees in Settings stays out of the title bar"
    );
    assert_eq!(
        view.app_row().action.map(|action| action.command),
        Some(state::Command::Desktop(state::DesktopAction::Check))
    );
    click("settings-row-core-updates-action", cx);
    assert_eq!(
        adapter.performed().last(),
        Some(&(PENGUIN, MachineAction::CheckAgain)),
        "Check Again checks penguin again"
    );
}

/// SpaceTerm and two Cores waiting at once keep their own versions, statuses and actions.
#[gpui::test]
fn simultaneous_updates_keep_separate_versions_statuses_and_actions(cx: &mut TestAppContext) {
    use crate::updates::{ApplicationUpdates, UpdateEvent, UpdateService};
    let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(updater.clone(), cx));
    let adapter = with_machines(
        vec![
            waiting(machine(LOCAL, "This Mac", 2)),
            waiting(machine(PENGUIN, "penguin", 0)),
        ],
        cx,
    );
    let (_, cx) = open_updates(cx);
    let updates = cx.update(|_, cx| cx.global::<UpdateService>().0.clone());
    updates.update(cx, |updates, cx| updates.check(false, cx));
    updater.emit(UpdateEvent::Available("0.11.1".into()));
    updater.emit(UpdateEvent::Ready);
    cx.run_until_parked();

    let view = cx.update(|_, cx| state::current(cx));
    let app = view.app_row();
    let rows = [
        (app.clone(), RESTART_SELECTOR),
        (
            view.machine_row(LOCAL).unwrap(),
            "settings-row-core-updates-action",
        ),
    ];
    for (row, selector) in rows {
        let line = copy::caption(&row);
        assert!(shows(&line, cx), "{} shows {line}", row.name);
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    assert_eq!(app.target.as_deref(), Some("0.11.1"));
    let penguin = view.machine_row(PENGUIN).unwrap();
    assert_eq!(penguin.version.as_deref(), Some("0.8.0"), "{penguin:?}");
    assert_eq!(penguin.target.as_deref(), Some("0.9.0"), "{penguin:?}");
    assert_eq!(
        app.action.map(|action| action.label.to_string()).as_deref(),
        Some("Relaunch to Update")
    );
    click("settings-row-core-updates-action", cx);
    assert_eq!(
        adapter.asked(),
        [(LOCAL, MachineAction::Restart)],
        "only This Mac asks"
    );
    assert_eq!(updater.installations.get(), 0, "SpaceTerm did not relaunch");
}

/// Each menu names the version its notes describe: another version than the one this build
/// carries notes for opens its release page, and nothing updates.
#[gpui::test]
fn release_notes_name_their_version_and_never_update(cx: &mut TestAppContext) {
    use crate::updates::{ApplicationUpdates, UpdateEvent, UpdateService};
    let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(updater.clone(), cx));
    let adapter = with_machines(vec![waiting(machine(PENGUIN, "penguin", 0))], cx);
    let (_, cx) = open_updates(cx);
    let updates = cx.update(|_, cx| cx.global::<UpdateService>().0.clone());
    updates.update(cx, |updates, cx| updates.check(false, cx));
    updater.emit(UpdateEvent::Available("0.11.1".into()));
    cx.run_until_parked();

    let running = env!("SPACETERM_VERSION");
    let (downloads, installations) = (updater.downloads.get(), updater.installations.get());
    click_owned(APP_MENU_SELECTOR, cx);
    // A build that carries its notes opens them in Settings, a release names its release page,
    // and a Development build names no release.
    match (
        crate::updates::release_notes::embedded(),
        crate::updates::stable_version(running),
    ) {
        (Some(_), _) => assert!(shows(&format!("What\u{2019}s New in {running}"), cx)),
        (None, Some(_)) => assert!(shows(&format!("Release Notes for {running}"), cx)),
        (None, None) => assert!(!shows(&format!("Release Notes for {running}"), cx)),
    }
    assert!(shows("Release Notes for 0.11.1", cx));
    click_owned(&format!("{APP_MENU_SELECTOR}-next-notes"), cx);
    assert_eq!(
        cx.opened_url(),
        Some(crate::updates::release_notes::release_url("0.11.1"))
    );

    choose("settings-row-core-updates-menu", "next-notes", cx);
    assert_eq!(
        cx.opened_url(),
        Some(crate::updates::release_notes::release_url("0.9.0"))
    );
    assert_eq!(updater.installations.get(), installations);
    assert_eq!(updater.downloads.get(), downloads);
    assert!(adapter.performed().is_empty() && adapter.asked().is_empty());
}

/// Notes asked for from Settings open on Settings even with a Workspace Window in front, and
/// activate it, so Done answers the keyboard and Escape dismisses the sheet. With no window
/// named, the active window holds them.
#[gpui::test]
fn notes_open_on_the_window_that_asked_and_dismiss_by_keyboard(cx: &mut TestAppContext) {
    use crate::ui::updates::whats_new::{SheetWindow, sheet_window, show};
    use crate::updates::release_notes::ReleaseNotes;
    use gpui::AppContext as _;
    let (_, cx) = open_updates(cx);
    let asked = cx.update(|window, _| window.window_handle());
    let workspace = cx.update(|_, cx| {
        cx.open_window(Default::default(), |window, cx| {
            cx.new(|cx| {
                crate::ui::WorkspaceManager::new_with_adapters(
                    std::env::temp_dir(),
                    crate::ui::workspace_manager::tests::workspace_adapters(),
                    crate::settings::sidebar::SidebarState::Expanded,
                    window,
                    cx,
                )
            })
        })
        .expect("a Workspace Window opens")
    });
    let _ = cx.update(|_, cx| workspace.update(cx, |_, window, _| window.activate_window()));
    cx.run_until_parked();

    let target = cx.update(|_, cx| sheet_window(Some(asked), cx));
    assert!(
        matches!(target, Some(SheetWindow::Settings(_))),
        "Settings asked"
    );
    let front = cx.update(|_, cx| sheet_window(None, cx));
    assert!(
        matches!(front, Some(SheetWindow::Workspace(_))),
        "with no window named, the active Workspace Window"
    );

    let notes = ReleaseNotes::parse(
        "0.9.0",
        "2026-11-03",
        "### Fixes\n\n- updates: Read faster\n",
    )
    .unwrap();
    assert!(!cx.update(|window, _| window.is_window_active()));
    // As every surface does, the sheet opens after the event that asked for it.
    let target = target.unwrap();
    cx.update(|_, cx| cx.defer(move |cx| show(&notes, target, 2026, cx)));
    cx.run_until_parked();
    assert!(
        cx.update(|window, _| window.is_window_active()),
        "Settings is active"
    );
    let title = "What\u{2019}s New in SpaceTerm 0.9.0";
    assert!(shows(title, cx), "the sheet opens on Settings");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shows(title, cx), "Escape dismisses the sheet");
}

/// Opens a Workspace Window beside Settings and makes it the active window.
fn open_workspace(cx: &mut VisualTestContext) -> gpui::WindowHandle<crate::ui::WorkspaceManager> {
    use gpui::AppContext as _;
    let workspace = cx.update(|_, cx| {
        cx.open_window(Default::default(), |window, cx| {
            cx.new(|cx| {
                crate::ui::WorkspaceManager::new_with_adapters(
                    std::env::temp_dir(),
                    crate::ui::workspace_manager::tests::workspace_adapters(),
                    crate::settings::sidebar::SidebarState::Expanded,
                    window,
                    cx,
                )
            })
        })
        .expect("a Workspace Window opens")
    });
    let _ = cx.update(|_, cx| workspace.update(cx, |_, window, _| window.activate_window()));
    cx.run_until_parked();
    workspace
}

/// A second notes request, from another window, activates the window that already holds the
/// sheet and opens no second one. Once that window closes, a request opens the sheet again.
#[gpui::test]
fn a_second_notes_request_activates_the_window_holding_the_sheet(cx: &mut TestAppContext) {
    use crate::ui::updates::whats_new::{sheet_window, show};
    use crate::updates::release_notes::ReleaseNotes;
    let (_, cx) = open_updates(cx);
    let settings = cx.update(|window, _| window.window_handle());
    let workspace = open_workspace(cx);
    let notes = ReleaseNotes::parse(
        "0.9.0",
        "2026-11-03",
        "### Fixes\n\n- updates: Read faster\n",
    )
    .unwrap();
    let request = |source: gpui::AnyWindowHandle, cx: &mut VisualTestContext| {
        let notes = notes.clone();
        cx.update(|_, cx| {
            cx.defer(move |cx| {
                if let Some(target) = sheet_window(Some(source), cx) {
                    show(&notes, target, 2026, cx);
                }
            })
        });
        cx.run_until_parked();
    };
    let modal_open = |window: gpui::AnyWindowHandle, cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            window
                .update(cx, |_, window, cx| {
                    spaceterm_ui::window_modal_is_open(window, cx)
                })
                .unwrap_or(false)
        })
    };

    let title = "What\u{2019}s New in SpaceTerm 0.9.0";
    request(workspace.into(), cx);
    assert!(
        modal_open(workspace.into(), cx),
        "the Workspace Window holds the sheet"
    );
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();

    request(settings, cx);
    assert_eq!(
        cx.update(|_, cx| cx.active_window()),
        Some(workspace.into()),
        "the window holding the sheet comes forward"
    );
    assert!(!shows(title, cx), "no second sheet opens on Settings");

    let _ = cx.update(|_, cx| workspace.update(cx, |_, window, _| window.remove_window()));
    cx.run_until_parked();
    request(settings, cx);
    assert!(
        shows(title, cx),
        "with its window gone, the sheet opens again"
    );
}

/// Check Now and SpaceTerm's own Check for Updates join a scheduled check that is already
/// running: no second check starts, and the row reports that check as the person's.
#[gpui::test]
fn a_manual_check_joins_a_scheduled_check(cx: &mut TestAppContext) {
    use crate::updates::{ApplicationUpdates, UpdateError, UpdateEvent, UpdateService};
    let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(updater.clone(), cx));
    let (_, cx) = open_updates(cx);
    let running = env!("SPACETERM_VERSION");
    let failed = crate::updates::copy::rows::CHECK_FAILED;
    let updates = cx.update(|_, cx| cx.global::<UpdateService>().0.clone());
    // A scheduled check runs.
    updates.update(cx, |updates, cx| updates.check(false, cx));
    cx.run_until_parked();
    let checks = updater.checks.get();

    click(CHECK_NOW_SELECTOR, cx);
    assert_eq!(updater.checks.get(), checks, "no second check starts");
    assert!(shows(&format!("{running} \u{b7} Checking\u{2026}"), cx));
    updater.emit(UpdateEvent::Failed(UpdateError::Check));
    updater.emit(UpdateEvent::Finished);
    cx.run_until_parked();
    assert!(shows(&format!("{running} \u{b7} {failed}"), cx));

    updates.update(cx, |updates, cx| updates.check(false, cx));
    cx.run_until_parked();
    let checks = updater.checks.get();
    choose(APP_MENU_SELECTOR, "check", cx);
    assert_eq!(updater.checks.get(), checks, "no second check starts");
    assert!(shows(&format!("{running} \u{b7} Checking\u{2026}"), cx));
}

/// A SpaceTerm release that waits for its download shows a check the person asked for: its
/// progress and failure beside the waiting version, which keeps Download. A successful check
/// restores the usual line.
#[gpui::test]
fn a_check_shows_beside_a_waiting_spaceterm_release(cx: &mut TestAppContext) {
    use crate::updates::{ApplicationUpdates, UpdateError, UpdateEvent, UpdateService};
    let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(updater.clone(), cx));
    let (_, cx) = open_updates(cx);
    let running = env!("SPACETERM_VERSION");
    let updates = cx.update(|_, cx| cx.global::<UpdateService>().0.clone());
    let mut document = SettingsDocument::default();
    document.updates.automatic_downloads = false;
    updates.update(cx, |updates, _| {
        updates.attach_settings(crate::settings::Settings::load(
            MemoryStorage::with_document(&document),
        ));
    });
    updates.update(cx, |updates, cx| updates.check(false, cx));
    // A scheduled check finds 0.11.1, which waits for its download.
    updater.emit(UpdateEvent::Available("0.11.1".into()));
    updater.emit(UpdateEvent::Finished);
    cx.run_until_parked();
    let usual = format!("{running} \u{2192} 0.11.1");
    assert!(shows(&usual, cx), "{usual}");

    click(CHECK_NOW_SELECTOR, cx);
    assert!(shows(
        &format!("{running} \u{2192} 0.11.1 \u{b7} Checking\u{2026}"),
        cx
    ));
    updater.emit(UpdateEvent::Failed(UpdateError::Check));
    updater.emit(UpdateEvent::Finished);
    cx.run_until_parked();
    assert!(shows(
        &format!(
            "{running} \u{2192} 0.11.1 \u{b7} {}",
            crate::updates::copy::rows::CHECK_FAILED
        ),
        cx
    ));
    let row = cx.update(|_, cx| state::current(cx).app_row());
    assert_eq!(row.tone, state::RowTone::Failure);
    assert_eq!(
        row.action.map(|action| action.command),
        Some(state::Command::Desktop(state::DesktopAction::Download)),
        "Download stays"
    );

    click(CHECK_NOW_SELECTOR, cx);
    updater.emit(UpdateEvent::Available("0.11.1".into()));
    cx.run_until_parked();
    assert!(
        shows(&usual, cx),
        "a successful check restores the usual line"
    );

    // The found release's cycle still waits for Download. The person's check ends it first,
    // then checks as theirs.
    let checks = updater.checks.get();
    choose(APP_MENU_SELECTOR, "check", cx);
    assert_eq!(updater.cancellations.get(), 1, "the waiting cycle ends");
    updater.emit(UpdateEvent::Finished);
    cx.run_until_parked();
    assert_eq!(
        updater.checks.get(),
        checks + 1,
        "the person's check follows"
    );
    assert!(shows(
        &format!("{running} \u{2192} 0.11.1 \u{b7} Checking\u{2026}"),
        cx
    ));
}

#[gpui::test]
fn a_downloaded_update_offers_the_spaceterm_rows_action_and_relaunches_at_once(
    cx: &mut TestAppContext,
) {
    use crate::updates::{ApplicationUpdates, UpdateEvent, UpdateService};
    let adapter = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(adapter.clone(), cx));
    let (_, cx) = open_updates(cx);
    let updates = cx.update(|_, cx| cx.global::<UpdateService>().0.clone());
    updates.update(cx, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::Available("0.11.1".into()));
    adapter.emit(UpdateEvent::Ready);
    cx.run_until_parked();
    let tree = A11yTree::read(cx);
    assert!(
        tree.find("Relaunch to Update").is_some(),
        "the SpaceTerm row offers its step"
    );
    assert!(
        tree.in_order().iter().any(|node| {
            ["label", "value"].into_iter().any(|field| {
                node["aria"][field]
                    .as_str()
                    .is_some_and(|text| text.ends_with("\u{2192} 0.11.1"))
            })
        }),
        "the SpaceTerm row names the downloaded version apart from the running one"
    );
    click(RESTART_SELECTOR, cx);
    assert_eq!(
        adapter.installations.get(),
        1,
        "one click relaunches without asking"
    );
}

const LOCAL: MachineId = [1; 32];
const PENGUIN: MachineId = [2; 32];
const OFFLINE: MachineId = [3; 32];

fn machine(id: MachineId, name: &'static str, sessions: u32) -> MachineUpdate {
    MachineUpdate {
        detailed: None,
        machine: id,
        name: name.into(),
        local: id == LOCAL,
        link: MachineLink::Connected,
        facts: Some(UpdateFacts {
            revision: 1,
            platform: Platform::LinuxX86_64,
            running: Version::parse("0.8.0").unwrap(),
            pending: None,
            sessions,
            operation: None,
            check: UpdateCheck::Succeeded {
                at: crate::updates::now(),
            },
            skipped: None,
            last_failure: None,
        }),
        capabilities: Capabilities::fake(&Capability::ALL),
        push: None,
        bundled: None,
        failed_push: None,
        asked: None,
    }
}

/// `update` with 0.9.0 downloaded just now and waiting on a restart.
fn waiting(mut update: MachineUpdate) -> MachineUpdate {
    update.facts.as_mut().unwrap().pending = Some(PendingVersion {
        version: Version::parse("0.9.0").unwrap(),
        protocols: Protocols::CURRENT,
        breaking: false,
        installed_at: crate::updates::now(),
        overdue_since: None,
        stranded: Vec::new(),
    });
    update
}

/// Installs the shared Machine updates with `machines` before Settings opens.
fn with_machines(machines: Vec<MachineUpdate>, cx: &mut TestAppContext) -> Rc<ScriptedMachines> {
    let adapter = Rc::new(ScriptedMachines::default());
    cx.update(|cx| {
        MachineUpdates::install(Rc::clone(&adapter) as Rc<dyn MachineUpdatesAdapter>, cx);
    });
    adapter.emit(MachineUpdatesEvent::Machines(machines));
    cx.run_until_parked();
    adapter
}

/// Settings > Updates lists every Machine under SpaceTerm Core, local first, then store order,
/// and listing them asks no Core anything.
#[gpui::test]
fn every_machine_is_listed_under_spaceterm_core_without_asking_any_core(cx: &mut TestAppContext) {
    let mut offline = machine(OFFLINE, "build-box", 3);
    offline.link = MachineLink::Offline { since: 1 };
    let adapter = with_machines(
        vec![
            machine(PENGUIN, "penguin", 0),
            waiting(machine(LOCAL, "This Mac", 2)),
            offline,
        ],
        cx,
    );
    let (window, cx) = open_updates(cx);
    let rows = window.read_with(cx, |settings, cx| settings.update_machine_rows(cx));
    let view = cx.update(|_, cx| state::current(cx));
    let expected: Vec<_> = [LOCAL, PENGUIN, OFFLINE]
        .into_iter()
        .map(|id| view.machine_row(id).expect("every Machine has a row"))
        .collect();
    assert_eq!(rows, expected);
    for selector in [
        "settings-row-core-updates",
        "settings-row-core-update-1",
        "settings-row-core-update-2",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(adapter.performed().is_empty(), "listing asks no Core");
}

/// Check Now checks for a newer SpaceTerm and asks each connected Machine's SpaceTerm Core to
/// check. An offline Machine is left alone, so no connection opens.
#[gpui::test]
fn check_now_asks_each_connected_machine_to_check(cx: &mut TestAppContext) {
    use crate::updates::ApplicationUpdates;
    let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(updater.clone(), cx));
    let mut offline = machine(OFFLINE, "build-box", 3);
    offline.link = MachineLink::Offline { since: 1 };
    let adapter = with_machines(
        vec![
            machine(LOCAL, "This Mac", 2),
            machine(PENGUIN, "penguin", 0),
            offline,
        ],
        cx,
    );
    let (_, cx) = open_updates(cx);
    // The startup check ends first, since Check Now waits for a check underway.
    updater.emit(crate::updates::UpdateEvent::UpToDate);
    updater.emit(crate::updates::UpdateEvent::Finished);
    cx.run_until_parked();
    let checks = updater.checks.get();
    click(CHECK_NOW_SELECTOR, cx);
    assert_eq!(updater.checks.get(), checks + 1);
    assert_eq!(
        adapter.performed(),
        [
            (LOCAL, MachineAction::CheckAgain),
            (PENGUIN, MachineAction::CheckAgain)
        ]
    );
}

/// I7 for Settings > Updates: each Machine row shows the shared row's name, version and status,
/// and its button carries the shared action's label and asks that Machine's Core for the
/// restart it names. Nothing restarts before the person confirms.
#[gpui::test]
fn each_machine_row_shows_and_performs_its_shared_row(cx: &mut TestAppContext) {
    let adapter = with_machines(
        vec![
            waiting(machine(LOCAL, "This Mac", 2)),
            waiting(machine(PENGUIN, "penguin", 3)),
        ],
        cx,
    );
    let (_, cx) = open_updates(cx);
    let view = cx.update(|_, cx| state::current(cx));
    let tree = A11yTree::read(cx);
    for (id, name) in [(LOCAL, "This Mac"), (PENGUIN, "penguin")] {
        let row = view.machine_row(id).unwrap();
        assert!(tree.find_text(name).is_some(), "{name} is listed");
        let line = copy::caption(&row);
        assert_eq!(
            line, "0.8.0 \u{2192} 0.9.0",
            "{name} keeps its versions apart"
        );
        assert!(tree.find_text(&line).is_some(), "{name} shows {line}");
        let action = row
            .action
            .expect("a waiting Machine with sessions has an action");
        assert!(
            tree.find(&action.label).is_some(),
            "{name}\u{2019}s button reads {}",
            action.label
        );
    }
    click("settings-row-core-update-1-action", cx);
    assert!(
        adapter.asked().contains(&(PENGUIN, MachineAction::Restart)),
        "penguin\u{2019}s button asks penguin\u{2019}s Core for a restart preview: {:?}",
        adapter.asked()
    );
    click("modal-action-settings-server-update-cancel", cx);
    click("settings-row-core-updates-action", cx);
    assert!(
        adapter.asked().contains(&(LOCAL, MachineAction::Restart)),
        "This Mac\u{2019}s button asks This Mac\u{2019}s Core: {:?}",
        adapter.asked()
    );
    assert!(
        adapter.executed().is_empty(),
        "nothing restarts unconfirmed"
    );
}
