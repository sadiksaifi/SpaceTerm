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

/// Exercise the real title bar without a status item, with release notes from a previous launch.
#[cfg(spaceterm_release_notes)]
fn unread_notes_workspace(
    cx: &mut gpui::TestAppContext,
) -> (
    Entity<ApplicationUpdates>,
    Rc<ScriptedMachines>,
    &mut gpui::VisualTestContext,
) {
    let machines = Rc::new(ScriptedMachines::default());
    let mut adapter = crate::updates::testing::RecordingAdapter::available();
    adapter.history.launched("0.0.1", 0);
    cx.update(|cx| {
        MachineUpdates::install(machines.clone(), cx);
        ApplicationUpdates::install(Rc::new(adapter), cx);
    });
    cx.update(crate::ui::init).unwrap();
    let updates = cx.update(|cx| service(cx).unwrap());
    let (workspace, visual) = cx.add_window_view(|window, cx| {
        crate::ui::WorkspaceManager::new_with_adapters(
            std::env::temp_dir(),
            crate::ui::workspace_manager::tests::workspace_adapters(),
            crate::settings::sidebar::SidebarState::Expanded,
            window,
            cx,
        )
    });
    visual.update(|window, cx| {
        workspace.update(cx, |workspace, cx| workspace.focus(window, cx));
    });
    visual.run_until_parked();
    (updates, machines, visual)
}

#[cfg(spaceterm_release_notes)]
#[gpui::test]
fn the_title_bar_opens_unread_notes_with_and_without_an_application_menu(
    cx: &mut gpui::TestAppContext,
) {
    use crate::desktop_profile::{DesktopPresentation, HostFeature};
    use crate::updates::presentation::{Control, Glyph};

    for application_menu in [true, false] {
        let (updates, _, visual) = unread_notes_workspace(cx);
        visual.update(|_, cx| {
            let presentation =
                DesktopPresentation::get(cx)
                    .clone()
                    .without_features(if application_menu {
                        &[]
                    } else {
                        &[HostFeature::ApplicationMenuBar]
                    });
            cx.set_global(presentation);
            let Control::Single(item) = crate::updates::presentation::current(cx).control() else {
                panic!("unread notes offer a plain title-bar button");
            };
            assert_eq!(item.glyph, Glyph::WhatsNew, "the control uses sparkles");
        });
        A11yTree::read(visual).node("What's new");
        let button = visual.debug_bounds("update-control").unwrap();
        visual.simulate_click(button.center(), gpui::Modifiers::none());
        visual.run_until_parked();

        let tree = A11yTree::read(visual);
        tree.node(&crate::updates::release_notes::embedded().unwrap().title());
        tree.node("View on GitHub");
        assert!(!visual.update(|_, cx| updates.read(cx).release_notes_unread()));
        spaceterm_ui::a11y_testing::perform(
            visual,
            tree.node("Done"),
            gpui::accesskit::Action::Click,
        );
        visual.run_until_parked();
        assert!(visual.debug_bounds("update-control").is_none());
        visual.update(|window, _| window.remove_window());
    }
}

#[cfg(spaceterm_release_notes)]
#[gpui::test]
fn closing_the_update_menu_keeps_notes_unread_until_the_sheet_opens(cx: &mut gpui::TestAppContext) {
    let (updates, machines, visual) = unread_notes_workspace(cx);
    machines.emit(MachineUpdatesEvent::Machines(vec![waiting(
        PENGUIN, "penguin", 0,
    )]));
    visual.run_until_parked();
    let trigger = visual.debug_bounds("update-control-menu").unwrap();
    visual.simulate_click(trigger.center(), gpui::Modifiers::none());
    visual.run_until_parked();
    assert!(visual.debug_bounds("update-row-0").is_some());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(visual.update(|_, cx| updates.read(cx).release_notes_unread()));

    let trigger = visual.debug_bounds("update-control-menu").unwrap();
    visual.simulate_click(trigger.center(), gpui::Modifiers::none());
    visual.run_until_parked();
    let notes = visual
        .debug_bounds("update-row-0")
        .expect("unread notes stay available");
    visual.simulate_click(notes.center(), gpui::Modifiers::none());
    visual.run_until_parked();
    A11yTree::read(visual).node("View on GitHub");
    assert!(!visual.update(|_, cx| updates.read(cx).release_notes_unread()));
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
        machines.executed_actions(),
        vec![(PENGUIN, MachineAction::Restart)]
    );
}

/// When the Core refuses a restart's preview, the window previews again and asks; it never
/// restarts again by itself.
#[gpui::test]
fn a_refused_restart_never_runs_again_by_itself(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    // The application-wide owner of update prompts hears each window action's outcome.
    cx.update(crate::ui::updates::init);
    let visual = hosted(cx);
    machines.emit(MachineUpdatesEvent::Machines(vec![waiting(
        PENGUIN, "penguin", 0,
    )]));
    visual.run_until_parked();
    click("update-control", visual);
    assert_eq!(
        machines.executed_actions(),
        vec![(PENGUIN, MachineAction::Restart)]
    );

    machines.emit(MachineUpdatesEvent::Finished {
        machine: PENGUIN,
        action: MachineAction::Restart,
        outcome: Err(crate::updates::machines::ActionFailure::Changed),
    });
    visual.run_until_parked();
    assert_eq!(machines.asked().len(), 2, "it previews again");
    assert_eq!(
        machines.executed_actions().len(),
        1,
        "a refused restart waits for a fresh confirmation"
    );
    assert!(machines.performed().is_empty());
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
    assert!(
        machines.executed_actions().is_empty(),
        "opening performs nothing"
    );
}

/// A window whose root holds the title-bar control under a modal layer, as a Workspace Window
/// does, so the control's own alerts present.
struct Host {
    control: Entity<UpdateControl>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        spaceterm_ui::ModalLayer::new(gpui::div().size_full().child(self.control.clone()))
    }
}

fn hosted(cx: &mut gpui::TestAppContext) -> &mut gpui::VisualTestContext {
    let (_, visual) = cx.add_window_view(|window, cx| Host {
        control: cx.new(|cx| UpdateControl::new(window, cx)),
    });
    visual.update(|window, _| window.activate_window());
    visual
}

fn click(selector: &'static str, cx: &mut gpui::VisualTestContext) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} renders"));
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    cx.run_until_parked();
}

const CONFIRM: &str = "modal-action-update-confirm-proceed";
const CANCEL: &str = "modal-action-update-confirm-cancel";

#[gpui::test]
fn the_title_bar_restart_asks_and_only_the_approved_work_restarts(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    let visual = hosted(cx);
    machines.emit(MachineUpdatesEvent::Machines(vec![waiting(
        PENGUIN, "penguin", 3,
    )]));
    visual.run_until_parked();
    A11yTree::read(visual).node("Restart…");

    click("update-control", visual);
    assert!(
        visual.debug_bounds(CONFIRM).is_some(),
        "the agreed alert asks"
    );
    let tree = A11yTree::read(visual);
    tree.text("Restart SpaceTerm Core on penguin?");
    tree.text("This will close your 3 running sessions.");
    assert!(
        machines.executed_actions().is_empty(),
        "an open alert performs nothing"
    );

    click(CANCEL, visual);
    assert!(
        machines.executed_actions().is_empty(),
        "Cancel performs nothing"
    );

    // A fourth session opens while the alert says three: approving asks again.
    click("update-control", visual);
    machines.emit(MachineUpdatesEvent::Changed(Box::new(waiting(
        PENGUIN, "penguin", 4,
    ))));
    visual.run_until_parked();
    click(CONFIRM, visual);
    assert!(
        machines.executed_actions().is_empty(),
        "an approval for three sessions never closes four"
    );
    A11yTree::read(visual).text("This will close your 4 running sessions.");

    click(CONFIRM, visual);
    assert_eq!(
        machines.executed_actions(),
        vec![(PENGUIN, MachineAction::Restart)]
    );
}

struct RecordingMovement;

impl crate::platform::window_movement::WindowMovementFactory for RecordingMovement {
    fn create(
        &self,
    ) -> Rc<dyn crate::platform::window_movement::OperatingSystemWindowDragPlatform> {
        Rc::new(
            crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform::default(),
        )
    }
}

#[gpui::test]
fn installing_from_the_title_bar_performs_it_without_settings(cx: &mut gpui::TestAppContext) {
    use crate::settings::storage::testing::MemoryStorage;

    let machines = Rc::new(ScriptedMachines::default());
    let adapter = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| {
        ApplicationUpdates::install(adapter.clone(), cx);
        MachineUpdates::install(machines.clone(), cx);
        // Settings could open here, so a stray Settings request would show.
        crate::ui::appearance_runtime::install(
            crate::settings::Settings::load(MemoryStorage::with_document(
                &crate::settings::SettingsDocument::default(),
            )),
            Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
            cx,
        )
        .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
        crate::ui::settings_window::configure_window_chrome(
            Rc::new(RecordingMovement),
            Default::default(),
            crate::theme_registry::testing::offline(),
            cx,
        );
    });
    let visual = hosted(cx);
    let mut too_old = waiting(PENGUIN, "penguin", 3);
    too_old.link =
        MachineLink::Incompatible(spaceterm_core::protocol::version::Incompatible::ListenerTooOld);
    too_old.facts.as_mut().unwrap().pending = None;
    machines.emit(MachineUpdatesEvent::Machines(vec![too_old]));
    visual.run_until_parked();
    A11yTree::read(visual).node("Update needed");
    let windows = visual.update(|_, cx| cx.windows().len());

    click("update-control", visual);
    assert_eq!(
        machines.performed(),
        vec![(PENGUIN, MachineAction::Install)]
    );
    visual.update(|_, cx| {
        assert_eq!(
            cx.windows().len(),
            windows,
            "no window, Settings included, opens"
        );
    });
}

#[gpui::test]
fn an_overdue_restart_with_sessions_open_never_runs_by_itself(cx: &mut gpui::TestAppContext) {
    use crate::updates::reminders::UpdateReminders;
    use crate::updates::reminders::testing::RecordingReminders;

    let machines = Rc::new(ScriptedMachines::default());
    let reminders = Rc::new(RecordingReminders::default());
    cx.update(|cx| {
        MachineUpdates::install(machines.clone(), cx);
        UpdateReminders::install(reminders.clone(), cx);
    });
    let _ = manual_updates(cx);
    let mut overdue = waiting(PENGUIN, "penguin", 3);
    let pending = overdue.facts.as_mut().unwrap().pending.as_mut().unwrap();
    pending.installed_at = crate::updates::now() - 2 * crate::updates::policy::DAY;
    pending.overdue_since = Some(pending.installed_at + crate::updates::policy::DAY);
    machines.emit(MachineUpdatesEvent::Machines(vec![overdue]));
    cx.run_until_parked();

    let shown = reminders.shown.borrow().clone();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].1, "Restart overdue on penguin");
    assert!(
        machines.executed_actions().is_empty(),
        "the reminder restarts nothing"
    );

    // Clicking it does what the control does, which asks first with sessions open.
    reminders.click(&shown[0].0);
    cx.run_until_parked();
    assert!(
        machines.executed_actions().is_empty(),
        "the click restarts nothing unasked"
    );
}

#[gpui::test]
fn work_in_progress_is_a_focusable_status(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    let visual = hosted(cx);
    let mut installing = waiting(PENGUIN, "penguin", 0);
    let facts = installing.facts.as_mut().unwrap();
    facts.pending = None;
    facts.operation = Some(spaceterm_core::update::facts::Operation::Installing {
        version: Version::parse("0.9.0").unwrap(),
        received: 1,
        total: 2,
    });
    machines.emit(MachineUpdatesEvent::Machines(vec![installing]));
    visual.run_until_parked();

    let tree = A11yTree::read(visual);
    let status = tree.node("Installing…");
    assert_eq!(status["aria"]["role"], "Status");
    assert_ne!(status["aria"]["disabled"], true);
    assert_eq!(
        status["aria"]["description"],
        "Installing SpaceTerm Core 0.9.0 on penguin."
    );
    assert!(tree.with_role("Button").is_empty(), "nothing to press");
}

#[gpui::test]
fn the_menu_reads_in_full_and_its_information_stays_reachable(cx: &mut gpui::TestAppContext) {
    let (_, _, machines) = services(cx);
    let visual = hosted(cx);
    let restarting = |machine, name| {
        let mut update = waiting(machine, name, 0);
        update.facts.as_mut().unwrap().operation =
            Some(spaceterm_core::update::facts::Operation::Restarting {
                to: Version::parse("0.9.0").unwrap(),
            });
        update
    };
    machines.emit(MachineUpdatesEvent::Machines(vec![
        restarting(PENGUIN, "penguin"),
        restarting(BUILD_BOX, "build-box"),
    ]));
    visual.update(|_, cx| crate::updates::record_recent("penguin updated to 0.9.0".into(), cx));
    visual.run_until_parked();

    let tree = A11yTree::read(visual);
    let trigger = tree.node("2 updates");
    assert_ne!(
        trigger["aria"]["disabled"], true,
        "two working items still open the menu"
    );
    assert_eq!(
        trigger["aria"]["description"],
        "2 updates need you. Open the menu to see each machine."
    );

    click("update-control-menu", visual);
    let tree = A11yTree::read(visual);
    let row = tree.node("penguin");
    assert_eq!(row["aria"]["role"], "Label", "a working row is information");
    assert_ne!(row["aria"]["disabled"], true);
    assert_eq!(
        row["aria"]["description"],
        "Restarting SpaceTerm Core on penguin."
    );
    assert!(row["aria"]["keyboard_shortcut"].is_null());
    let recent = tree.node("Recent");
    assert_ne!(recent["aria"]["disabled"], true, "Recent opens");
    assert!(machines.executed_actions().is_empty());
}
