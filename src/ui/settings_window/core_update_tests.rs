//! Settings uses the application Machine store's preview and current update state.

use super::super::machines::testing::{ScriptedCore, ScriptedService, connected, running};
use super::super::machines::{CoreServiceControl, LocalCoreControl, OfferedMachine, ServiceAction};
use super::super::{SettingsSectionId, SettingsWindow};
use super::{UPDATE_CANCEL_SELECTOR, UPDATE_CONFIRM_SELECTOR, UPDATE_SELECTOR};
use crate::ui::sidebar_window::SidebarOwner;
use crate::updates::machines::store::MachineUpdates;
use crate::updates::machines::testing::ScriptedMachines;
use crate::updates::machines::{MachineAction, MachineLink, MachineUpdate, MachineUpdatesEvent};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_core::protocol::capability::{Capabilities, Capability};
use spaceterm_core::update::facts::{
    PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
};
use spaceterm_ui::a11y_testing::A11yTree;
use std::{rc::Rc, sync::Arc};

const MACHINE: [u8; 32] = [1; 32];

fn machine(pending: Option<&str>, sessions: u32) -> MachineUpdate {
    MachineUpdate {
        machine: MACHINE,
        name: "This Mac".into(),
        local: true,
        link: MachineLink::Connected,
        capabilities: Capabilities::fake(&Capability::ALL),
        push: None,
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
    tree.text("Update SpaceTerm Core on This Mac?");
    tree.text("SpaceTerm Core restarts from 1.4.0 to 1.6.0. This ends 3 Terminal Sessions on This Mac. Your Workspaces, Tabs and Panes stay, and each Pane starts a new shell when you select it.");
    tree.node("Update Core");
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
    A11yTree::read(cx).text("Update SpaceTerm Core on This Mac?");
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
    tree.text("SpaceTerm Core 1.4.0 restarts without updating. This ends 3 Terminal Sessions on This Mac.");
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
    A11yTree::read(cx).text("SpaceTerm Core restarts from 1.4.0 to 1.7.0. This ends 4 Terminal Sessions on This Mac. Your Workspaces, Tabs and Panes stay, and each Pane starts a new shell when you select it.");
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
    let (settings, adapter, cx) = open(cx, None);
    for operation in [
        spaceterm_core::update::facts::Operation::Installing {
            version: Version::parse("1.6.0").unwrap(),
            received: 5,
            total: 10,
        },
        spaceterm_core::update::facts::Operation::Restarting {
            to: Version::parse("1.6.0").unwrap(),
        },
    ] {
        let mut update = machine(None, 3);
        update.facts.as_mut().unwrap().operation = Some(operation);
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(update)));
        cx.run_until_parked();
        let subtitle = settings.read_with(cx, |settings, cx| settings.core_subtitle(cx));
        assert!(subtitle.contains("Installing") || subtitle.contains("Updating"));
        assert!(cx.debug_bounds(UPDATE_SELECTOR).is_none());
    }
    assert!(adapter.performed().is_empty());
    assert!(adapter.executed().is_empty());
}

#[gpui::test]
fn an_update_request_never_turns_into_a_same_version_restart(cx: &mut TestAppContext) {
    let (settings, adapter, cx) = open(cx, None);
    settings.update(cx, |settings, cx| settings.request_core_update(cx));
    cx.run_until_parked();
    assert!(adapter.asked().is_empty());
    assert!(adapter.executed().is_empty());
}
