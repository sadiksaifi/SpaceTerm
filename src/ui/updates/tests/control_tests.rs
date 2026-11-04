use super::*;
use std::rc::Rc;

use gpui::prelude::*;
use spaceterm_core::protocol::capability::{Capabilities, Capability};
use spaceterm_core::update::facts::{
    PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
};
use spaceterm_ui::a11y_testing::A11yTree;

use crate::updates::UpdateEvent;
use crate::updates::machines::testing::ScriptedMachines;
use crate::updates::machines::{
    MachineAction, MachineId, MachineLink, MachineUpdate, MachineUpdatesEvent,
};

const PENGUIN: MachineId = [2; 32];
const BUILD_BOX: MachineId = [3; 32];

fn waiting(machine: MachineId, name: &'static str, sessions: u32) -> MachineUpdate {
    MachineUpdate {
        machine,
        name: name.into(),
        local: false,
        link: MachineLink::Connected,
        facts: Some(UpdateFacts {
            revision: 1,
            platform: Platform::LinuxX86_64,
            running: Version::parse("0.8.0").unwrap(),
            pending: Some(PendingVersion {
                version: Version::parse("0.9.0").unwrap(),
                protocols: Protocols::CURRENT,
                breaking: false,
                installed_at: crate::updates::now() - 60,
                overdue_since: None,
                stranded: Vec::new(),
            }),
            sessions,
            operation: None,
            check: UpdateCheck::Never,
            skipped: None,
            last_failure: None,
        }),
        capabilities: Capabilities::fake(&Capability::ALL),
        push: None,
    }
}

/// The update service with automatic downloads off, and Machine updates the test scripts.
fn services(
    cx: &mut gpui::TestAppContext,
) -> (
    Rc<crate::updates::testing::RecordingAdapter>,
    Entity<ApplicationUpdates>,
    Rc<ScriptedMachines>,
) {
    let machines = Rc::new(ScriptedMachines::default());
    cx.update(|cx| MachineUpdates::install(machines.clone(), cx));
    let (adapter, updates) = manual_updates(cx);
    (adapter, updates, machines)
}

#[gpui::test]
fn one_ready_desktop_update_is_a_plain_button_that_relaunches_without_asking(
    cx: &mut gpui::TestAppContext,
) {
    let (adapter, updates, _) = services(cx);
    let (_, visual) = cx.add_window_view(UpdateControl::new);
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("update-control").is_none(),
        "nothing needs doing yet"
    );

    updates.update(visual, |updates, cx| updates.check(false, cx));
    adapter.emit(UpdateEvent::Available(version()));
    visual.run_until_parked();
    updates.update(visual, |updates, cx| updates.download(cx));
    adapter.emit(UpdateEvent::Ready);
    visual.run_until_parked();

    let button = visual
        .debug_bounds("update-control")
        .expect("one item shows a plain button");
    assert!(visual.debug_bounds("update-control-menu").is_none());
    let tree = A11yTree::read(visual);
    tree.node("Relaunch");
    assert_eq!(tree.with_role("Button").len(), 1, "no chevron");

    visual.simulate_click(button.center(), gpui::Modifiers::none());
    visual.run_until_parked();
    assert_eq!(adapter.installations.get(), 1, "Relaunch never asks");
    assert!(
        visual
            .debug_bounds("modal-action-update-confirm-cancel")
            .is_none()
    );
}

#[gpui::test]
fn a_restart_with_no_sessions_performs_at_once(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    let (_, visual) = cx.add_window_view(UpdateControl::new);
    machines.emit(MachineUpdatesEvent::Machines(vec![waiting(
        PENGUIN, "penguin", 0,
    )]));
    visual.run_until_parked();
    A11yTree::read(visual).node("Restart");
    let button = visual.debug_bounds("update-control").unwrap();
    visual.simulate_click(button.center(), gpui::Modifiers::none());
    visual.run_until_parked();
    assert_eq!(
        machines.performed(),
        vec![(PENGUIN, MachineAction::Restart)]
    );
}

#[gpui::test]
fn two_items_open_a_compact_menu_with_one_line_per_machine_and_restart_all(
    cx: &mut gpui::TestAppContext,
) {
    let (_, _, machines) = services(cx);
    let (_, visual) = cx.add_window_view(UpdateControl::new);
    machines.emit(MachineUpdatesEvent::Machines(vec![
        waiting(PENGUIN, "penguin", 3),
        waiting(BUILD_BOX, "build-box", 1),
    ]));
    visual.run_until_parked();
    assert!(visual.debug_bounds("update-control").is_none());
    let trigger = visual
        .debug_bounds("update-control-menu")
        .expect("two items open a menu");
    A11yTree::read(visual).node("2 updates");

    visual.simulate_click(trigger.center(), gpui::Modifiers::none());
    visual.run_until_parked();
    assert!(visual.debug_bounds("update-row-1").is_some());
    assert!(visual.debug_bounds("update-row-2").is_some());
    assert!(visual.debug_bounds("update-row-3").is_none());
    assert!(visual.debug_bounds("update-restart-all").is_some());
    assert!(machines.performed().is_empty(), "opening performs nothing");
}

#[gpui::test]
fn the_agreed_confirmation_restarts_only_after_the_person_confirms(cx: &mut gpui::TestAppContext) {
    struct AlertHost;

    impl Render for AlertHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            spaceterm_ui::ModalLayer::new(gpui::div().size_full())
        }
    }

    let (_, _, machines) = services(cx);
    machines.emit(MachineUpdatesEvent::Machines(vec![waiting(
        PENGUIN, "penguin", 3,
    )]));
    cx.run_until_parked();
    let (host, visual) = cx.add_window_view(|_, _| AlertHost);
    let presentation = visual.update(|_, cx| crate::updates::presentation::current(cx));
    let Some(crate::updates::presentation::Command::Confirm(confirmation)) = presentation
        .machine(PENGUIN)
        .map(crate::updates::presentation::MachineView::restart_command)
    else {
        panic!("a restart that closes sessions asks first");
    };
    visual.update(|window, cx| {
        host.update(cx, |_, cx| commands::confirm(confirmation, window, cx));
    });
    visual.run_until_parked();

    let tree = A11yTree::read(visual);
    tree.text("Restart SpaceTerm Core on penguin?");
    tree.text("This will close your 3 running sessions.");
    assert!(machines.performed().is_empty());

    let restart = visual
        .debug_bounds("modal-action-update-confirm-proceed")
        .unwrap();
    visual.simulate_click(restart.center(), gpui::Modifiers::none());
    visual.run_until_parked();
    assert_eq!(
        machines.performed(),
        vec![(PENGUIN, MachineAction::Restart)]
    );
}
