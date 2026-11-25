//! The Command Line Tool row driven through scripted launcher files.
use std::rc::Rc;
use std::sync::Arc;

use gpui::accesskit::Action;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use spaceterm_ui::a11y_testing::{A11yTree, perform};

use super::{ACTION_SELECTOR, MENU_SELECTOR, ToolCheck};
use crate::appearance::Appearance;
use crate::command_line_tool::testing::{ScriptedFiles, tool};
use crate::command_line_tool::{Launcher, LinkOutcome, ToolAction, ToolFailure};
use crate::machine_connection::CoreError;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::settings_window::SettingsWindow;
use crate::ui::settings_window::machines::testing::ScriptedCore;
use crate::ui::settings_window::machines::{LocalCoreControl, OfferedMachine};
use crate::ui::sidebar_window::form::CaptionTone;

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";
const ROW_SELECTOR: &str = "settings-row-command-line-tool";

fn open_settings(
    launcher: Option<Launcher>,
    cx: &mut TestAppContext,
) -> (
    Arc<ScriptedFiles>,
    Entity<SettingsWindow>,
    &mut VisualTestContext,
) {
    let files = ScriptedFiles::new(Ok(launcher.unwrap_or(Launcher::Missing)));
    let command_line_tool = launcher.map(|_| Arc::new(tool(&files)));
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let core = ScriptedCore::new(Err(CoreError::NotSetUp));
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            crate::theme_registry::testing::offline(),
            Some(OfferedMachine {
                connection: Ok(core as Arc<dyn LocalCoreControl>),
                service: None,
                update: None,
                command_line_tool,
                removal: None,
            }),
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (files, window, cx)
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

fn presentation(
    window: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> super::ToolRowPresentation {
    window.read_with(cx, |settings, _| {
        settings
            .command_line_tool_presentation()
            .expect("the row is offered")
    })
}

/// Opens the row's menu and chooses `label`.
fn choose(label: &str, cx: &mut VisualTestContext) {
    click(MENU_SELECTOR, cx);
    let tree = A11yTree::read(cx);
    perform(cx, tree.node(label), Action::Click);
    cx.run_until_parked();
}

#[gpui::test]
fn opening_machines_checks_the_command_and_install_then_offers_its_menu(cx: &mut TestAppContext) {
    let (files, window, cx) = open_settings(Some(Launcher::Missing), cx);
    assert_eq!(
        window.read_with(cx, |settings, _| settings
            .command_line_tool
            .as_ref()
            .map(|row| row.check)),
        Some(ToolCheck::NotChecked)
    );
    click(NAVIGATION_SELECTOR, cx);
    let missing = presentation(&window, cx);
    assert_eq!(
        (missing.state, missing.action),
        ("Not installed", Some((ToolAction::Install, "Install")))
    );
    // The row names the directory PATH must include, without claiming whether it does.
    assert_eq!(
        missing.caption.map(|(_, tone)| tone),
        Some(CaptionTone::Guidance)
    );
    assert!(cx.debug_bounds(MENU_SELECTOR).is_none());

    click(ACTION_SELECTOR, cx);
    assert_eq!(files.links(), 1);
    let installed = presentation(&window, cx);
    assert_eq!(
        (installed.state, installed.action, installed.caption),
        ("Installed", None, None)
    );
    assert!(cx.debug_bounds(ACTION_SELECTOR).is_none());
    assert!(cx.debug_bounds(MENU_SELECTOR).is_some());
}

#[gpui::test]
fn uninstall_removes_the_command_and_offers_install_again(cx: &mut TestAppContext) {
    let (files, window, cx) = open_settings(Some(Launcher::Installed), cx);
    click(NAVIGATION_SELECTOR, cx);
    choose("Uninstall", cx);
    assert_eq!((files.unlinks(), files.links()), (1, 0));
    let removed = presentation(&window, cx);
    assert_eq!(
        (removed.state, removed.action, removed.menu),
        (
            "Not installed",
            Some((ToolAction::Install, "Install")),
            vec![]
        )
    );
    assert!(cx.debug_bounds(MENU_SELECTOR).is_none());
}

#[gpui::test]
fn repair_links_the_command_again(cx: &mut TestAppContext) {
    let (files, window, cx) = open_settings(Some(Launcher::Installed), cx);
    click(NAVIGATION_SELECTOR, cx);
    choose("Repair", cx);
    assert_eq!((files.links(), files.unlinks()), (1, 0));
    assert_eq!(presentation(&window, cx).state, "Installed");
}

#[gpui::test]
fn a_refused_uninstall_says_why_and_keeps_the_command(cx: &mut TestAppContext) {
    let (files, window, cx) = open_settings(Some(Launcher::Installed), cx);
    click(NAVIGATION_SELECTOR, cx);
    *files.unlink.lock().unwrap() = Err(ToolFailure::Busy);
    choose("Uninstall", cx);
    let refused = presentation(&window, cx);
    assert_eq!(
        (
            refused.state,
            refused.caption.map(|(text, tone)| (text.to_string(), tone))
        ),
        (
            "Installed",
            Some((
                "Another SpaceTerm Core operation is running. Try again when it finishes."
                    .to_owned(),
                CaptionTone::Error
            ))
        )
    );
}

#[gpui::test]
fn a_change_that_fails_after_it_landed_shows_the_launcher_as_it_is(cx: &mut TestAppContext) {
    let (files, window, cx) = open_settings(Some(Launcher::Installed), cx);
    click(NAVIGATION_SELECTOR, cx);
    // The link is removed, then saving the directory fails.
    *files.unlink.lock().unwrap() = Err(ToolFailure::StorageUnavailable);
    *files.launcher.lock().unwrap() = Ok(Launcher::Missing);
    choose("Uninstall", cx);
    let removed = presentation(&window, cx);
    assert_eq!(
        (
            removed.state,
            removed.action,
            removed.caption.map(|(_, tone)| tone)
        ),
        (
            "Not installed",
            Some((ToolAction::Install, "Install")),
            Some(CaptionTone::Error)
        )
    );
}

#[gpui::test]
fn another_programs_file_offers_nothing_and_is_never_replaced(cx: &mut TestAppContext) {
    let (files, window, cx) = open_settings(Some(Launcher::OtherFile), cx);
    click(NAVIGATION_SELECTOR, cx);
    let other = presentation(&window, cx);
    assert_eq!(
        (other.state, other.action, other.menu),
        ("Other file", None, vec![])
    );
    assert!(cx.debug_bounds(ACTION_SELECTOR).is_none());
    assert!(cx.debug_bounds(MENU_SELECTOR).is_none());
    assert_eq!((files.links(), files.unlinks()), (0, 0));
}

#[gpui::test]
fn a_file_that_appears_during_install_is_kept_and_read_again(cx: &mut TestAppContext) {
    let (files, window, cx) = open_settings(Some(Launcher::Missing), cx);
    click(NAVIGATION_SELECTOR, cx);
    // Another program links its own file after the row read the path.
    *files.link.lock().unwrap() = Ok(LinkOutcome::Kept);
    click(ACTION_SELECTOR, cx);
    let kept = presentation(&window, cx);
    assert_eq!((kept.state, kept.action), ("Other link", None));
    assert_eq!(kept.caption.map(|(_, tone)| tone), Some(CaptionTone::Error));
}

#[gpui::test]
fn a_refused_install_says_why_and_keeps_offering_install(cx: &mut TestAppContext) {
    let (files, window, cx) = open_settings(Some(Launcher::Missing), cx);
    click(NAVIGATION_SELECTOR, cx);
    *files.link.lock().unwrap() = Err(ToolFailure::NoCorePackage);
    click(ACTION_SELECTOR, cx);
    let refused = presentation(&window, cx);
    assert_eq!(
        (
            refused.state,
            refused.action,
            refused.caption.map(|(text, tone)| (text.to_string(), tone))
        ),
        (
            "Not installed",
            Some((ToolAction::Install, "Install")),
            Some((
                "This SpaceTerm build doesn\u{2019}t include a SpaceTerm Core package.".to_owned(),
                CaptionTone::Error
            ))
        )
    );
}

#[gpui::test]
fn a_build_without_the_tool_omits_the_row(cx: &mut TestAppContext) {
    let (_, window, cx) = open_settings(None, cx);
    click(NAVIGATION_SELECTOR, cx);
    assert!(cx.debug_bounds(ROW_SELECTOR).is_none());
    assert!(window.read_with(cx, |settings, _| {
        settings.command_line_tool_presentation().is_none()
    }));
}

#[gpui::test]
fn a_finished_service_operation_reads_the_command_again(cx: &mut TestAppContext) {
    let (files, window, cx) = open_settings(Some(Launcher::Installed), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert_eq!(presentation(&window, cx).state, "Installed");
    // An update couldn't repoint the launcher, which stays on the earlier server build.
    *files.launcher.lock().unwrap() = Ok(Launcher::Outdated);
    cx.update(|_, cx| {
        crate::ui::settings_window::local_operations::LocalCoreOperations::changed_elsewhere(
            cx,
            crate::machine_connection::service::ServiceStatus::default(),
        );
    });
    cx.run_until_parked();
    let outdated = presentation(&window, cx);
    assert_eq!(
        (outdated.state, outdated.action),
        ("Out of date", Some((ToolAction::Install, "Reinstall")))
    );
}

#[gpui::test]
fn a_finished_operation_during_a_read_reads_the_command_again(cx: &mut TestAppContext) {
    let (files, window, cx) = open_settings(Some(Launcher::Installed), cx);
    click(NAVIGATION_SELECTOR, cx);
    let reads = || files.reads.load(std::sync::atomic::Ordering::SeqCst);
    let before = reads();
    // The read is admitted, then the operation finishes before its answer lands.
    window.update(cx, |settings, cx| {
        settings.check_command_line_tool(cx);
        settings.reread_command_line_tool(cx);
    });
    cx.run_until_parked();
    assert_eq!(reads(), before + 2);
    assert_eq!(presentation(&window, cx).state, "Installed");
}
