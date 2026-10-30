//! The Command Line Tool row driven through scripted launcher files, shell and file manager.
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use super::{ACTION_SELECTOR, ToolCheck};
use crate::appearance::Appearance;
use crate::command_line_tool::testing::{DIRECTORY, RecordingViewer, ScriptedFiles, tool};
use crate::command_line_tool::{Launcher, LinkOutcome, ToolAction, ToolFailure};
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::server_connection::ServerError;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::settings_window::SettingsWindow;
use crate::ui::settings_window::servers::testing::ScriptedServer;
use crate::ui::settings_window::servers::{LocalServerControl, OfferedServer};
use crate::ui::sidebar_window::form::CaptionTone;

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";
const ROW_SELECTOR: &str = "settings-row-command-line-tool";

struct Fixture {
    files: Arc<ScriptedFiles>,
    viewer: Arc<RecordingViewer>,
}

fn open_settings(
    launcher: Option<Launcher>,
    cx: &mut TestAppContext,
) -> (Fixture, Entity<SettingsWindow>, &mut VisualTestContext) {
    open_with_viewer(launcher, RecordingViewer::default(), cx)
}

fn open_with_viewer(
    launcher: Option<Launcher>,
    viewer: RecordingViewer,
    cx: &mut TestAppContext,
) -> (Fixture, Entity<SettingsWindow>, &mut VisualTestContext) {
    let files = ScriptedFiles::new(Ok(launcher.unwrap_or(Launcher::Missing)));
    let viewer = Arc::new(viewer);
    let command_line_tool = launcher.map(|_| Arc::new(tool(&files, &viewer)));
    let settings =
        crate::settings::Settings::load(MemoryStorage::with_document(&SettingsDocument::default()));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings, Rc::new(platform), cx)
            .expect("appearance runtime should install");
        crate::ui::init(cx).expect("UI initialization should succeed");
    });
    let server = ScriptedServer::new(Err(ServerError::NotSetUp));
    let (window, cx) = cx.add_window_view(|window, cx| {
        SettingsWindow::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            Default::default(),
            None,
            Some(OfferedServer {
                connection: Ok(server as Arc<dyn LocalServerControl>),
                service: None,
                update: None,
                command_line_tool,
            }),
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (Fixture { files, viewer }, window, cx)
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
    window.read_with(cx, |settings, cx| {
        settings
            .command_line_tool_presentation(cx)
            .expect("the row is offered")
    })
}

#[gpui::test]
fn opening_servers_checks_the_command_and_install_then_shows_it(cx: &mut TestAppContext) {
    let (fixture, window, cx) = open_settings(Some(Launcher::Missing), cx);
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

    click(ACTION_SELECTOR, cx);
    assert_eq!(fixture.files.links(), 1);
    let installed = presentation(&window, cx);
    assert_eq!(
        (installed.state, installed.action),
        ("Installed", Some((ToolAction::Reveal, "Show in Folder")))
    );

    click(ACTION_SELECTOR, cx);
    assert_eq!(
        *fixture.viewer.shown.lock().unwrap(),
        [std::path::Path::new(DIRECTORY).join("spaceterm-development")]
    );
    assert_eq!(fixture.files.links(), 1);
}

#[gpui::test]
fn another_programs_file_is_shown_and_never_replaced(cx: &mut TestAppContext) {
    let (fixture, window, cx) = open_settings(Some(Launcher::OtherFile), cx);
    click(NAVIGATION_SELECTOR, cx);
    let other = presentation(&window, cx);
    assert_eq!(
        (other.state, other.action),
        ("Other file", Some((ToolAction::Reveal, "Show in Folder")))
    );
    click(ACTION_SELECTOR, cx);
    assert_eq!(fixture.viewer.shown.lock().unwrap().len(), 1);
    assert_eq!(fixture.files.links(), 0);
}

#[gpui::test]
fn a_file_that_appears_during_install_is_kept_and_read_again(cx: &mut TestAppContext) {
    let (fixture, window, cx) = open_settings(Some(Launcher::Missing), cx);
    click(NAVIGATION_SELECTOR, cx);
    // Another program links its own file after the row read the path.
    *fixture.files.link.lock().unwrap() = Ok(LinkOutcome::Kept);
    click(ACTION_SELECTOR, cx);
    let kept = presentation(&window, cx);
    assert_eq!(
        (kept.state, kept.action.map(|(action, _)| action)),
        ("Other link", Some(ToolAction::Reveal))
    );
    assert_eq!(kept.caption.map(|(_, tone)| tone), Some(CaptionTone::Error));
}

#[gpui::test]
fn a_refused_install_says_why_and_keeps_offering_install(cx: &mut TestAppContext) {
    let (fixture, window, cx) = open_settings(Some(Launcher::Missing), cx);
    click(NAVIGATION_SELECTOR, cx);
    *fixture.files.link.lock().unwrap() = Err(ToolFailure::NoServerPackage);
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
                "This SpaceTerm build doesn\u{2019}t include a server package.".to_owned(),
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
    assert!(window.read_with(cx, |settings, cx| {
        settings.command_line_tool_presentation(cx).is_none()
    }));
}

#[gpui::test]
fn a_file_manager_that_cannot_show_the_command_is_reported(cx: &mut TestAppContext) {
    let refusing = RecordingViewer {
        refuse: true,
        ..RecordingViewer::default()
    };
    let (fixture, window, cx) = open_with_viewer(Some(Launcher::Installed), refusing, cx);
    click(NAVIGATION_SELECTOR, cx);
    click(ACTION_SELECTOR, cx);
    assert_eq!(fixture.viewer.shown.lock().unwrap().len(), 1);
    let refused = presentation(&window, cx);
    assert_eq!(
        (
            refused.state,
            refused.caption.map(|(text, tone)| (text.to_string(), tone))
        ),
        (
            "Installed",
            Some((
                "The file manager couldn\u{2019}t show the command.".to_owned(),
                CaptionTone::Error
            ))
        )
    );
}
