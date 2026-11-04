//! Settings > Advanced > Uninstall SpaceTerm, driven through a scripted removal. The installed
//! app's own removal is tested in `crate::installed_app`.
use std::sync::Arc;

use gpui::{Modifiers, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::A11yTree;

use super::testing::{ScriptedRemoval, Step};
use super::*;
use crate::ui::settings_window::machines::OfferedMachine;
use crate::ui::settings_window::machines::testing::{ScriptedCore, connected};
use crate::ui::settings_window::machines::tests::{alert_open, click_modal, open_settings};

const ADVANCED: &str = "settings-navigation-settings-section-advanced";
const SUPPRESSION: &str = "modal-alert-suppression";

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

/// Opens Settings on Advanced with `removal` offered.
fn open<'a>(
    removal: Option<&Arc<ScriptedRemoval>>,
    cx: &'a mut TestAppContext,
) -> &'a mut VisualTestContext {
    let offered = OfferedMachine {
        connection: Ok(ScriptedCore::new(Ok(connected(2)))),
        service: None,
        update: None,
        command_line_tool: None,
        removal: removal.map(|removal| Arc::clone(removal) as Arc<dyn SpaceTermRemoval>),
    };
    let (_, cx) = open_settings(Some(offered), cx);
    click(ADVANCED, cx);
    cx
}

fn shows(text: &str, cx: &mut VisualTestContext) -> bool {
    A11yTree::read(cx).find_text(text).is_some()
}

fn sessions(count: u8) -> Observed {
    Observed::new((0..count).map(|index| [index; 32]).collect())
}

#[gpui::test]
fn uninstall_names_the_sessions_that_close_and_keeps_data_unless_asked(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(3);
    let cx = open(Some(&removal), cx);
    assert!(shows(
        "Removes SpaceTerm and SpaceTerm Core from this computer.",
        cx
    ));

    click(UNINSTALL_SELECTOR, cx);
    assert!(shows("Uninstall SpaceTerm?", cx));
    assert!(shows(
        "This will close your 3 running sessions on this computer.",
        cx
    ));
    assert!(
        A11yTree::read(cx)
            .find("Also delete Workspaces, paired devices and settings")
            .is_some(),
        "the alert offers deleting the data too"
    );
    assert_eq!(
        removal.steps(),
        vec![Step::Observe],
        "asking removes nothing"
    );

    click_modal(CONFIRM_SELECTOR, cx);
    assert_eq!(
        removal.steps(),
        vec![
            Step::Observe,
            Step::RemoveCore(sessions(3), Data::Keep),
            Step::RemoveApp,
        ]
    );
    assert!(shows("SpaceTerm Was Uninstalled", cx));
    assert!(shows(
        "SpaceTerm and SpaceTerm Core were removed from this computer. Workspaces, paired \
         devices and settings were kept.",
        cx
    ));

    // SpaceTerm Core is gone, so Quit exits without the quit warning.
    let quits = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = std::rc::Rc::clone(&quits);
    cx.update(|_, cx| {
        cx.on_action(move |_: &crate::app::QuitUninstalledApplication, _| {
            counted.set(counted.get() + 1);
        });
    });
    click_modal(QUIT_SELECTOR, cx);
    assert_eq!(quits.get(), 1);
}

#[gpui::test]
fn opting_in_also_deletes_workspaces_paired_devices_and_settings(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(1);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    assert!(shows(
        "This will close your 1 running session on this computer.",
        cx
    ));
    click(SUPPRESSION, cx);
    click_modal(CONFIRM_SELECTOR, cx);

    assert_eq!(
        removal.steps(),
        vec![
            Step::Observe,
            Step::RemoveCore(sessions(1), Data::Delete),
            Step::RemoveApp,
            Step::DeleteDesktopData,
        ]
    );
    assert!(shows(
        "SpaceTerm and SpaceTerm Core were removed from this computer. Workspaces, paired \
         devices and settings were deleted.",
        cx
    ));
}

#[gpui::test]
fn cancel_removes_nothing(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(0);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    assert!(shows(
        "This will remove SpaceTerm and SpaceTerm Core from this computer.",
        cx
    ));
    click_modal(CANCEL_SELECTOR, cx);

    assert_eq!(removal.steps(), vec![Step::Observe]);
    assert!(!alert_open(CONFIRM_SELECTOR, cx));
    click(UNINSTALL_SELECTOR, cx);
    assert!(
        alert_open(CONFIRM_SELECTOR, cx),
        "Uninstall is offered again"
    );
}

#[gpui::test]
fn sessions_that_changed_since_asking_stop_the_uninstall_before_anything_is_removed(
    cx: &mut TestAppContext,
) {
    let removal = ScriptedRemoval::new(2);
    *removal.core.lock().unwrap() = Err(RemovalFailure::Changed);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    click_modal(CONFIRM_SELECTOR, cx);

    assert_eq!(
        removal.steps(),
        vec![Step::Observe, Step::RemoveCore(sessions(2), Data::Keep)],
        "the app stays when SpaceTerm Core does"
    );
    assert!(shows("Couldn’t Uninstall SpaceTerm", cx));
    assert!(shows(
        "The running sessions on this computer changed, so nothing was uninstalled. Try again.",
        cx
    ));
    click_modal(DISMISS_SELECTOR, cx);
    click(UNINSTALL_SELECTOR, cx);
    assert!(
        alert_open(CONFIRM_SELECTOR, cx),
        "Uninstall is offered again"
    );
}

#[gpui::test]
fn an_app_that_cannot_remove_itself_says_how_to_finish(cx: &mut TestAppContext) {
    let removal = ScriptedRemoval::new(0);
    *removal.app.lock().unwrap() = Err(RemovalFailure::App);
    let cx = open(Some(&removal), cx);

    click(UNINSTALL_SELECTOR, cx);
    click_modal(CONFIRM_SELECTOR, cx);

    assert!(shows(
        "SpaceTerm Core was removed, but SpaceTerm couldn’t remove itself, so run install.sh \
         --uninstall.",
        cx
    ));
    assert!(
        alert_open(QUIT_SELECTOR, cx),
        "without SpaceTerm Core the app can only quit"
    );
}

#[gpui::test]
fn a_build_that_is_not_an_installed_app_offers_no_uninstall(cx: &mut TestAppContext) {
    let cx = open(None, cx);
    assert!(cx.debug_bounds(UNINSTALL_SELECTOR).is_none());
    assert!(!shows("Uninstall SpaceTerm", cx));
}
