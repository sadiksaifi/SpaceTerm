//! The Servers section driven through a scripted local server.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use super::testing::{ScriptedServer, connected, status_only};
use super::{
    CHECK_SELECTOR, CONTROL_SELECTOR, LocalServerAction, LocalServerControl,
    LocalServerPresentation, LocalServerStatus, OPEN_DEVELOPMENT_PANE_SELECTOR, OfferedLocalServer,
    SET_UP_SELECTOR, START_SELECTOR, present,
};
use crate::appearance::Appearance;
use crate::local_server::ServerError;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::settings_window::{SettingsSectionId, SettingsWindow};
use crate::ui::sidebar_window::form::CaptionTone;

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";
const ROW_SELECTOR: &str = "settings-row-local-server";

const EVERY_ERROR: [ServerError; 14] = [
    ServerError::NotPaired,
    ServerError::NotRunning,
    ServerError::CredentialUnavailable,
    ServerError::StorageUnavailable,
    ServerError::UnsafeStorage,
    ServerError::AuthenticationRefused,
    ServerError::IncompatibleProtocol,
    ServerError::Protocol,
    ServerError::StartupFailed,
    ServerError::Controlled,
    ServerError::SessionUnavailable,
    ServerError::StaleControl,
    ServerError::Unsupported,
    ServerError::CleanupUnresolved,
];

fn offered(server: &Arc<ScriptedServer>) -> OfferedLocalServer {
    Some(Ok(Arc::clone(server) as Arc<dyn LocalServerControl>))
}

fn open_settings(
    local_server: OfferedLocalServer,
    cx: &mut TestAppContext,
) -> (Entity<SettingsWindow>, &mut VisualTestContext) {
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
            None,
            local_server,
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
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

fn search(window: &Entity<SettingsWindow>, query: &str, cx: &mut VisualTestContext) {
    cx.update(|_, cx| {
        let search = window.read(cx).search.clone();
        search.update(cx, |search, cx| search.set_value(query.to_owned(), cx));
    });
    cx.run_until_parked();
}

fn presentation(
    window: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> LocalServerPresentation {
    window.read_with(cx, |settings, _| {
        settings
            .local_server_presentation()
            .expect("the local server should be offered")
    })
}

fn rendered(selector: &'static str, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(selector).is_some()
}

#[test]
fn each_failure_offers_the_one_step_that_can_recover_from_it() {
    for error in EVERY_ERROR {
        let presented = present(LocalServerStatus::Failed(error));
        let expected = match error {
            ServerError::NotPaired => LocalServerAction::SetUp,
            ServerError::NotRunning | ServerError::StartupFailed => LocalServerAction::Start,
            // Setting up again cannot repair storage or a refused, incompatible, or faulty server.
            _ => LocalServerAction::Check,
        };
        assert_eq!(presented.action, Some(expected), "{error:?}");
        assert!(!presented.working, "{error:?}");
    }
}

#[test]
fn a_failure_needing_attention_reads_as_an_error_in_its_bounded_wording() {
    for error in EVERY_ERROR {
        let presented = present(LocalServerStatus::Failed(error));
        match error {
            // These are steps in setup, not faults, so they read as guidance naming the step.
            ServerError::NotPaired | ServerError::NotRunning => {
                assert_eq!(presented.tone, CaptionTone::Guidance, "{error:?}");
            }
            _ => {
                assert_eq!(presented.tone, CaptionTone::Error, "{error:?}");
                assert_eq!(presented.explanation.as_ref(), error.to_string());
            }
        }
    }
}

#[test]
fn a_running_action_shows_progress_and_withholds_its_step() {
    for action in [
        LocalServerAction::Check,
        LocalServerAction::SetUp,
        LocalServerAction::Start,
    ] {
        let presented = present(LocalServerStatus::Working(action));
        assert!(presented.working, "{action:?}");
        assert_eq!(presented.action, Some(action));
    }
}

#[test]
fn a_connection_names_only_its_protocol_version() {
    let presented = present(LocalServerStatus::Connected(connected(3)));

    assert_eq!(presented.state, "Connected");
    assert_eq!(
        presented.explanation.as_ref(),
        "This device is paired and authenticated. Protocol version 3."
    );
    assert_eq!(presented.action, Some(LocalServerAction::Check));
}

#[gpui::test]
fn opening_settings_asks_the_local_server_nothing(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::NotPaired));
    let (window, cx) = open_settings(offered(&server), cx);

    assert_eq!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface
    );
    assert!(rendered(NAVIGATION_SELECTOR, cx));
    assert_eq!((server.refreshes(), server.setups()), (0, 0));
    assert!(matches!(
        window.read_with(cx, |settings, _| settings
            .local_server
            .as_ref()
            .map(|row| row.status())),
        Some(LocalServerStatus::NotChecked)
    ));
}

#[gpui::test]
fn presenting_servers_checks_the_pairing_without_setting_up(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::NotPaired));
    let (window, cx) = open_settings(offered(&server), cx);

    click(NAVIGATION_SELECTOR, cx);

    assert_eq!((server.refreshes(), server.setups()), (1, 0));
    for selector in [
        "settings-section-servers",
        ROW_SELECTOR,
        CONTROL_SELECTOR,
        "settings-local-server-state-not-paired",
        SET_UP_SELECTOR,
    ] {
        assert!(rendered(selector, cx), "{selector} should render");
    }
    let row = cx.debug_bounds(ROW_SELECTOR).expect("row bounds");
    let control = cx.debug_bounds(CONTROL_SELECTOR).expect("control bounds");
    assert!(row.contains(&control.origin) && control.right() <= row.right());
    assert_eq!(presentation(&window, cx).tone, CaptionTone::Guidance);

    // Choosing the presented section again reads nothing more.
    click(NAVIGATION_SELECTOR, cx);
    assert_eq!(server.refreshes(), 1);

    // Returning from another section checks again, still without setting up.
    click("settings-navigation-settings-section-git", cx);
    click(NAVIGATION_SELECTOR, cx);
    assert_eq!((server.refreshes(), server.setups()), (2, 0));
}

#[gpui::test]
fn set_up_runs_only_from_its_button_and_shows_the_authenticated_status(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::NotPaired));
    server.answer_setup(Ok(connected(2)));
    let (window, cx) = open_settings(offered(&server), cx);
    click(NAVIGATION_SELECTOR, cx);

    click(SET_UP_SELECTOR, cx);

    assert_eq!(server.setups(), 1);
    assert!(rendered("settings-local-server-state-connected", cx));
    assert!(rendered(CHECK_SELECTOR, cx));
    let presented = presentation(&window, cx);
    assert_eq!(presented.state, "Connected");
    assert!(presented.explanation.contains("Protocol version 2"));

    // Checking an authenticated server reads its status and sets up nothing.
    click(CHECK_SELECTOR, cx);
    assert_eq!((server.refreshes(), server.setups()), (2, 1));
}

#[gpui::test]
fn a_paired_server_that_is_not_running_offers_start(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::NotRunning));
    server.answer_setup(Err(ServerError::StartupFailed));
    let (window, cx) = open_settings(offered(&server), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert!(rendered("settings-local-server-state-not-running", cx));

    click(START_SELECTOR, cx);

    assert_eq!(server.setups(), 1);
    assert!(rendered("settings-local-server-state-startup-failed", cx));
    let presented = presentation(&window, cx);
    assert_eq!(presented.tone, CaptionTone::Error);
    assert_eq!(presented.action, Some(LocalServerAction::Start));
}

#[gpui::test]
fn a_refused_server_offers_only_another_check(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::AuthenticationRefused));
    let (window, cx) = open_settings(offered(&server), cx);

    click(NAVIGATION_SELECTOR, cx);

    assert!(rendered("settings-local-server-state-refused", cx));
    assert!(rendered(CHECK_SELECTOR, cx));
    // GPUI retains a selector once drawn, and neither step was ever offered here.
    assert!(!rendered(SET_UP_SELECTOR, cx));
    assert!(!rendered(START_SELECTOR, cx));
    assert_eq!(presentation(&window, cx).tone, CaptionTone::Error);
    assert_eq!(server.setups(), 0);
}

#[gpui::test]
fn one_action_runs_at_a_time(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::NotPaired));
    let (window, cx) = open_settings(offered(&server), cx);
    click(NAVIGATION_SELECTOR, cx);

    window.update(cx, |settings, cx| {
        settings.run_local_server(LocalServerAction::SetUp, cx);
        assert!(
            settings
                .local_server_presentation()
                .is_some_and(|presented| presented.working)
        );
        settings.run_local_server(LocalServerAction::SetUp, cx);
        settings.run_local_server(LocalServerAction::Check, cx);
    });
    cx.run_until_parked();

    assert_eq!((server.refreshes(), server.setups()), (1, 1));
    assert!(!presentation(&window, cx).working);
}

#[gpui::test]
fn the_progress_indicator_replaces_the_badge_while_an_action_runs(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::NotPaired));
    let (window, cx) = open_settings(offered(&server), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert!(!rendered("settings-local-server-progress-track", cx));

    window.update(cx, |settings, cx| {
        settings.run_local_server(LocalServerAction::SetUp, cx);
    });
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });

    assert!(rendered("settings-local-server-progress-track", cx));
}

#[gpui::test]
fn settings_search_reveals_the_local_server_and_checks_it_once(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Ok(connected(2)));
    let (window, cx) = open_settings(offered(&server), cx);

    for query in ["s", "se", "ser", "serv", "serve", "server"] {
        search(&window, query, cx);
    }

    assert_eq!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Servers
    );
    assert_eq!((server.refreshes(), server.setups()), (1, 0));
    assert!(rendered("settings-local-server-state-connected", cx));
}

#[gpui::test]
fn an_unresolved_server_explains_itself_and_offers_nothing(cx: &mut TestAppContext) {
    let (window, cx) = open_settings(Some(Err(ServerError::StorageUnavailable)), cx);

    click(NAVIGATION_SELECTOR, cx);

    assert!(rendered("settings-local-server-state-unavailable", cx));
    let presented = presentation(&window, cx);
    assert_eq!(presented.action, None);
    assert_eq!(
        presented.explanation.as_ref(),
        ServerError::StorageUnavailable.to_string()
    );
    for selector in [CHECK_SELECTOR, SET_UP_SELECTOR, START_SELECTOR] {
        assert!(!rendered(selector, cx), "{selector} should not be offered");
    }
}

#[gpui::test]
fn a_build_without_a_local_server_omits_the_servers_section(cx: &mut TestAppContext) {
    let (window, cx) = open_settings(None, cx);

    assert!(!rendered(NAVIGATION_SELECTOR, cx));
    search(&window, "pair", cx);
    assert_ne!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Servers
    );
    assert!(!rendered(ROW_SELECTOR, cx));
}

fn offer_development_pane(window: &gpui::Entity<SettingsWindow>, cx: &mut VisualTestContext) {
    window.update(cx, |settings, _| {
        settings.local_server = settings
            .local_server
            .take()
            .map(|row| row.offering_development_pane(true));
    });
}

#[gpui::test]
fn the_development_pane_waits_for_an_authenticated_server(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::NotRunning));
    let (window, cx) = open_settings(offered(&server), cx);
    offer_development_pane(&window, cx);
    click(NAVIGATION_SELECTOR, cx);

    // Opening never starts the server, so Start stays the step until a check authenticates.
    assert!(!presentation(&window, cx).development_pane);
    assert!(!rendered(OPEN_DEVELOPMENT_PANE_SELECTOR, cx));

    click(START_SELECTOR, cx);

    assert!(presentation(&window, cx).development_pane);
    assert!(rendered(OPEN_DEVELOPMENT_PANE_SELECTOR, cx));
}

#[gpui::test]
fn a_build_without_the_development_pane_never_offers_it(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Ok(connected(2)));
    let (window, cx) = open_settings(offered(&server), cx);
    window.update(cx, |settings, _| {
        settings.local_server = settings
            .local_server
            .take()
            .map(|row| row.offering_development_pane(false));
    });

    click(NAVIGATION_SELECTOR, cx);

    assert!(rendered("settings-local-server-state-connected", cx));
    assert!(!presentation(&window, cx).development_pane);
    assert!(!rendered(OPEN_DEVELOPMENT_PANE_SELECTOR, cx));
}

#[gpui::test]
fn a_server_without_the_development_pane_is_offered_no_terminal(cx: &mut TestAppContext) {
    // A server from before terminal operations authenticates but never says it runs the Pane.
    let server = ScriptedServer::new(Ok(status_only(2)));
    let (window, cx) = open_settings(offered(&server), cx);
    offer_development_pane(&window, cx);

    click(NAVIGATION_SELECTOR, cx);

    assert!(rendered("settings-local-server-state-connected", cx));
    assert!(!presentation(&window, cx).development_pane);
    assert!(!rendered(OPEN_DEVELOPMENT_PANE_SELECTOR, cx));
}
