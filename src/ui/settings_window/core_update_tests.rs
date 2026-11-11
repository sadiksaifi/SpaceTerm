//! Settings uses the application Machine store's preview and current update state.

use super::super::machine_card::MENU_SELECTOR;
use super::super::machines::testing::{ScriptedCore, ScriptedService, connected, running};
use super::super::machines::{CoreServiceControl, LocalCoreControl, OfferedMachine, ServiceAction};
use super::super::{SettingsSectionId, SettingsWindow};
use super::{UPDATE_CANCEL_SELECTOR, UPDATE_CONFIRM_SELECTOR, UPDATE_SELECTOR};
use crate::ui::sidebar_window::SidebarOwner;
use crate::updates::machines::store::MachineUpdates;
use crate::updates::machines::testing::ScriptedMachines;
use crate::updates::machines::{MachineAction, MachineLink, MachineUpdate, MachineUpdatesEvent};
use gpui::accesskit::Action;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_core::protocol::capability::{Capabilities, Capability};
use spaceterm_core::update::facts::{
    PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
};
use spaceterm_ui::a11y_testing::{A11yTree, perform};
use std::{rc::Rc, sync::Arc};

const MACHINE: [u8; 32] = [1; 32];

fn machine(pending: Option<&str>, sessions: u32) -> MachineUpdate {
    MachineUpdate {
        detailed: None,
        machine: MACHINE,
        name: "This Mac".into(),
        local: true,
        link: MachineLink::Connected,
        capabilities: Capabilities::fake(&Capability::ALL),
        push: None,
        bundled: None,
        failed_push: None,
        asked: None,
        facts: Some(UpdateFacts {
            revision: 1,
            platform: Platform::MacosArm64,
            running: Version::parse("1.4.0").unwrap(),
            sessions,
            operation: None,
            pending: pending.map(|to| PendingVersion {
                version: Version::parse(to).unwrap(),
                protocols: Protocols::CURRENT,
                breaking: false,
                installed_at: 1,
                overdue_since: None,
                stranded: Vec::new(),
            }),
            check: UpdateCheck::Never,
            skipped: None,
            last_failure: None,
        }),
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    pending: Option<&str>,
) -> (
    Entity<SettingsWindow>,
    Rc<ScriptedMachines>,
    &'a mut VisualTestContext,
) {
    let adapter = Rc::new(ScriptedMachines::default());
    cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
    adapter.emit(MachineUpdatesEvent::Machines(vec![machine(pending, 3)]));
    cx.run_until_parked();
    let core = ScriptedCore::new(Ok(connected(1)));
    let service = ScriptedService::new(Ok(running()));
    let offered = OfferedMachine {
        connection: Ok(core as Arc<dyn LocalCoreControl>),
        service: Some(Ok(service as Arc<dyn CoreServiceControl>)),
        update: Some(super::testing::ScriptedUpdate::new(
            super::testing::available(),
        )),
        command_line_tool: None,
        removal: None,
    };
    let settings = crate::settings::Settings::load(
        crate::settings::storage::testing::MemoryStorage::with_document(&Default::default()),
    );
    cx.update(|cx| {
        crate::ui::appearance_runtime::install(
            settings,
            Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
            cx,
        )
        .unwrap();
        crate::ui::init(cx).unwrap();
    });
    let (settings, visual) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(), crate::theme_registry::testing::offline(), Some(offered), window, cx)
    });
    visual.update(|window, cx| {
        window.activate_window();
        settings.update(cx, |settings, cx| {
            settings.select_section(SettingsSectionId::Machines, cx)
        });
    });
    visual.run_until_parked();
    (settings, adapter, visual)
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is rendered"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}
fn answer(selector: &str, cx: &mut VisualTestContext) {
    click(
        Box::leak(format!("modal-action-{selector}").into_boxed_str()),
        cx,
    );
}

#[gpui::test]
fn local_update_uses_the_shared_pending_version_and_cancel_preserves_sessions(
    cx: &mut TestAppContext,
) {
    let (_, adapter, cx) = open(cx, Some("1.6.0"));
    assert!(
        adapter.performed().is_empty(),
        "opening Settings stages nothing"
    );
    click(UPDATE_SELECTOR, cx);
    let tree = A11yTree::read(cx);
    tree.text("Restart SpaceTerm Core on This Mac?");
    tree.text(
        "SpaceTerm Core restarts from 1.4.0 to 1.6.0. This ends 3 Terminal Sessions. \
         Workspaces, Tabs and Panes stay.",
    );
    tree.node("Restart Core");
    assert!(adapter.executed().is_empty());
    answer(UPDATE_CANCEL_SELECTOR, cx);
    assert!(adapter.executed().is_empty());
}

#[gpui::test]
fn settings_restart_targets_the_pending_version_through_the_shared_preview(
    cx: &mut TestAppContext,
) {
    let (settings, adapter, cx) = open(cx, Some("1.6.0"));
    cx.update(|window, cx| {
        settings.update(cx, |settings, cx| {
            settings.request_core_service(ServiceAction::Restart, window, cx)
        })
    });
    cx.run_until_parked();
    A11yTree::read(cx).text("Restart SpaceTerm Core on This Mac?");
    answer(UPDATE_CONFIRM_SELECTOR, cx);
    assert_eq!(
        adapter.executed_actions(),
        [(MACHINE, MachineAction::Restart)]
    );
    assert_eq!(
        adapter.executed()[0].target,
        Some(Version::parse("1.6.0").unwrap())
    );
}

#[gpui::test]
fn settings_same_version_restart_names_the_machine_version_and_sessions(cx: &mut TestAppContext) {
    let (settings, adapter, cx) = open(cx, None);
    cx.update(|window, cx| {
        settings.update(cx, |settings, cx| {
            settings.request_core_service(ServiceAction::Restart, window, cx)
        })
    });
    cx.run_until_parked();
    let tree = A11yTree::read(cx);
    tree.text("Restart SpaceTerm Core on This Mac?");
    tree.text(
        "SpaceTerm Core 1.4.0 restarts without updating. This ends 3 Terminal Sessions. \
         Workspaces, Tabs and Panes stay.",
    );
    tree.node("Restart Core");
    assert!(adapter.executed().is_empty());
    answer(UPDATE_CONFIRM_SELECTOR, cx);
    assert_eq!(adapter.executed()[0].target, None);
}

#[gpui::test]
fn settings_rebuilds_confirmation_when_the_pending_version_or_sessions_change(
    cx: &mut TestAppContext,
) {
    let (_, adapter, cx) = open(cx, Some("1.6.0"));
    click(UPDATE_SELECTOR, cx);
    adapter.emit(MachineUpdatesEvent::Changed(Box::new(machine(
        Some("1.7.0"),
        4,
    ))));
    cx.run_until_parked();
    answer(UPDATE_CONFIRM_SELECTOR, cx);
    assert!(
        adapter.executed().is_empty(),
        "the old approval ends no sessions"
    );
    A11yTree::read(cx).text(
        "SpaceTerm Core restarts from 1.4.0 to 1.7.0. This ends 4 Terminal Sessions. \
         Workspaces, Tabs and Panes stay.",
    );
    answer(UPDATE_CONFIRM_SELECTOR, cx);
    assert_eq!(adapter.executed().len(), 1);
    assert_eq!(
        adapter.executed()[0].target,
        Some(Version::parse("1.7.0").unwrap())
    );
    assert_eq!(adapter.executed()[0].sessions, 4);
}

#[gpui::test]
fn settings_tracks_the_shared_install_and_restart_without_starting_another(
    cx: &mut TestAppContext,
) {
    let (settings, adapter, cx) = open(cx, Some("1.6.0"));
    assert!(
        cx.debug_bounds(UPDATE_SELECTOR).is_some(),
        "the pending version offers Update until the work begins"
    );
    for (operation, status) in [
        (
            spaceterm_core::update::facts::Operation::Installing {
                version: Version::parse("1.6.0").unwrap(),
                received: 5,
                total: 10,
            },
            "Downloading 1.6.0\u{2026}",
        ),
        (
            spaceterm_core::update::facts::Operation::Restarting {
                to: Version::parse("1.6.0").unwrap(),
            },
            "Restarting into 1.6.0\u{2026}",
        ),
    ] {
        let mut update = machine(Some("1.6.0"), 3);
        update.facts.as_mut().unwrap().operation = Some(operation);
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(update)));
        cx.run_until_parked();
        let subtitle = settings.read_with(cx, |settings, cx| settings.core_subtitle(cx));
        assert!(subtitle.ends_with(status), "{subtitle}");
        assert!(cx.debug_bounds(UPDATE_SELECTOR).is_none());
    }
    assert!(adapter.performed().is_empty());
    assert!(adapter.executed().is_empty());
}

#[gpui::test]
fn a_failed_settings_preview_explains_the_failure(cx: &mut TestAppContext) {
    let (_, adapter, cx) = open(cx, Some("1.6.0"));
    adapter.hold_previews();
    click(UPDATE_SELECTOR, cx);
    adapter.answer_preview(|_, _| Err(crate::updates::machines::ActionFailure::Unreachable));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("modal-action-settings-update-explanation-ok")
            .is_some(),
        "the failure is explained"
    );
    assert!(adapter.executed().is_empty());
}

/// I7 for the Machine card: its subtitle is the row's version and status, and its one button is
/// the row's action, which asks this Machine's Core for the restart it names.
#[gpui::test]
fn the_card_shows_its_row_and_its_button_asks_for_that_restart(cx: &mut TestAppContext) {
    let (_, adapter, cx) = open(cx, Some("1.6.0"));
    let tree = A11yTree::read(cx);
    assert!(
        tree.find_text("1.4.0 \u{b7} Restarts into 1.6.0 when 3 Terminal Sessions end")
            .is_some(),
        "the card reads as the shared row"
    );
    tree.node("Restart Core\u{2026}");
    click(UPDATE_SELECTOR, cx);
    assert_eq!(adapter.asked(), [(MACHINE, MachineAction::Restart)]);
    assert!(adapter.executed().is_empty(), "it asks before it restarts");
}

/// The card's ⋯ menu offers a same-version restart only while no Pending Version waits; while
/// one waits, the card's button is the only restart.
#[gpui::test]
fn the_card_menu_offers_a_same_version_restart_only_without_a_pending_version(
    cx: &mut TestAppContext,
) {
    let (_, _, cx) = open(cx, Some("1.6.0"));
    click(MENU_SELECTOR, cx);
    let tree = A11yTree::read(cx);
    assert!(tree.find("Stop SpaceTerm Core\u{2026}").is_some());
    assert!(
        tree.find("Restart SpaceTerm Core\u{2026}").is_none(),
        "no second restart while 1.6.0 waits"
    );
}

#[gpui::test]
fn the_card_menu_offers_a_same_version_restart_with_nothing_waiting(cx: &mut TestAppContext) {
    let (_, adapter, cx) = open(cx, None);
    click(MENU_SELECTOR, cx);
    let tree = A11yTree::read(cx);
    perform(
        cx,
        tree.node("Restart SpaceTerm Core\u{2026}"),
        Action::Click,
    );
    cx.run_until_parked();
    assert_eq!(adapter.asked(), [(MACHINE, MachineAction::Restart)]);
    assert!(adapter.executed().is_empty(), "it asks before it restarts");
}

/// The card's Check Now asks this Machine's SpaceTerm Core to check.
#[gpui::test]
fn the_cards_check_now_asks_its_core_to_check(cx: &mut TestAppContext) {
    let (_, adapter, cx) = open(cx, None);
    click(MENU_SELECTOR, cx);
    let tree = A11yTree::read(cx);
    perform(cx, tree.node("Check Now"), Action::Click);
    cx.run_until_parked();
    assert_eq!(adapter.performed(), [(MACHINE, MachineAction::CheckAgain)]);
}
