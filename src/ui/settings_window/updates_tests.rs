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

use super::updates::{CHECK_NOW_SELECTOR, RESTART_SELECTOR};
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
    assert_eq!(
        status.summary.as_ref(),
        copy::line(row.version.as_deref(), row.status.as_deref())
    );
    assert!(
        A11yTree::read(cx).find_text(&status.summary).is_some(),
        "the SpaceTerm row shows its version: {}",
        status.summary
    );
    click(CHECK_NOW_SELECTOR, cx);
    assert_eq!(adapter.performed(), [(PENGUIN, MachineAction::CheckAgain)]);
}

/// Check Now follows the check interval and says what it does in visible text.
#[gpui::test]
fn check_now_follows_the_interval_with_its_explanation(cx: &mut TestAppContext) {
    let (_, cx) = open_updates(cx);
    let interval = cx
        .debug_bounds("settings-row-update-check-interval")
        .expect("the interval renders");
    let check_now = cx
        .debug_bounds("settings-row-update-check-now")
        .expect("Check Now has its own row");
    assert!(
        check_now.top() >= interval.bottom(),
        "Check Now comes after the interval"
    );
    assert!(
        A11yTree::read(cx)
            .find_text("Checks for a newer SpaceTerm and asks each connected Machine to check.")
            .is_some(),
        "the explanation is visible"
    );
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
                    .is_some_and(|text| text.ends_with("0.11.1 downloaded"))
            })
        }),
        "the SpaceTerm row says the update is downloaded"
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
        let status = row.status.clone().expect("a waiting Machine has a status");
        let line = copy::line(row.version.as_deref(), Some(&status));
        assert!(
            line.contains("0.8.0") && line.contains(status.as_ref()),
            "{line}"
        );
        assert!(tree.find_text(&line).is_some(), "{name} shows {line}");
        let action = row
            .action
            .expect("a waiting Machine with sessions has an action");
        assert!(
            tree.find(&action.label).is_some(),
            "{name}'s button reads {}",
            action.label
        );
    }
    click("settings-row-core-update-1-action", cx);
    assert!(
        adapter.asked().contains(&(PENGUIN, MachineAction::Restart)),
        "penguin's button asks penguin's Core for a restart preview: {:?}",
        adapter.asked()
    );
    click("modal-action-settings-server-update-cancel", cx);
    click("settings-row-core-updates-action", cx);
    assert!(
        adapter.asked().contains(&(LOCAL, MachineAction::Restart)),
        "This Mac's button asks This Mac's Core: {:?}",
        adapter.asked()
    );
    assert!(
        adapter.executed().is_empty(),
        "nothing restarts unconfirmed"
    );
}
