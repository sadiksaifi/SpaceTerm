//! The Servers section driven through a scripted local server and service.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use super::testing::{
    Prompt, ScriptedServer, ScriptedService, ServiceCall, connected, not_installed, not_running,
    retained, running, status_only, stopped, uninstalled,
};
use super::{
    AUTHORIZE_CANCEL_SELECTOR, AUTHORIZE_CONFIRM_SELECTOR, AUTHORIZE_INSTALL_SELECTOR,
    AUTHORIZE_START_SELECTOR, AuthorizationPrompt, AuthorizedRequest, CHECK_SELECTOR,
    CONTROL_SELECTOR, INSTALL_CHOICE_SELECTOR, INSTALL_SELECTOR, InstallChoice, LocalServerAction,
    LocalServerControl, LocalServerPresentation, LocalServerStatus, OPEN_DEVELOPMENT_PANE_SELECTOR,
    OfferedLocalServer, OfferedServer, RESTART_CANCEL_SELECTOR, RESTART_CONFIRM_SELECTOR,
    RESTART_SELECTOR, SERVICE_CHECK_SELECTOR, SERVICE_CONTROL_SELECTOR, SET_UP_SELECTOR,
    START_SELECTOR, START_SERVICE_SELECTOR, STOP_CANCEL_SELECTOR, STOP_CONFIRM_SELECTOR,
    STOP_SELECTOR, ServerServiceControl, ServiceAction, ServiceConfirmation, ServicePresentation,
    ServiceStartup, ServiceState, UNINSTALL_CANCEL_SELECTOR, UNINSTALL_CONFIRM_SELECTOR,
    UNINSTALL_SELECTOR, present, present_service,
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
const SERVICE_ROW_SELECTOR: &str = "settings-row-server-service";

const EVERY_ERROR: [ServerError; 19] = [
    ServerError::NotPaired,
    ServerError::ServiceUnavailable,
    ServerError::NotInstalled,
    ServerError::Stopped,
    ServerError::RuntimeUnavailable,
    ServerError::AuthorizationRequired,
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

/// A server on a platform without a service layer, which keeps the connection row's workflow.
fn offered(server: &Arc<ScriptedServer>) -> OfferedLocalServer {
    Some(OfferedServer {
        connection: Ok(Arc::clone(server) as Arc<dyn LocalServerControl>),
        service: None,
    })
}

fn offered_with_service(
    server: &Arc<ScriptedServer>,
    service: &Arc<ScriptedService>,
) -> OfferedLocalServer {
    Some(OfferedServer {
        connection: Ok(Arc::clone(server) as Arc<dyn LocalServerControl>),
        service: Some(Ok(Arc::clone(service) as Arc<dyn ServerServiceControl>)),
    })
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

fn service_presentation(
    window: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> ServicePresentation {
    window.read_with(cx, |settings, _| {
        settings
            .server_service_presentation()
            .expect("the service should be offered")
    })
}

/// The selector the modal gives an alert action. GPUI looks selectors up by static name.
fn modal_action(selector: &str) -> &'static str {
    Box::leak(format!("modal-action-{selector}").into_boxed_str())
}

fn click_modal(selector: &str, cx: &mut VisualTestContext) {
    let selector = modal_action(selector);
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

fn alert_open(selector: &str, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(modal_action(selector)).is_some()
}

/// Opens Settings on the Servers section with a service in `status`, behind a paired server.
fn open_servers_with_service(
    status: Result<super::ServiceStatus, ServerError>,
    cx: &mut TestAppContext,
) -> (
    Entity<SettingsWindow>,
    Arc<ScriptedServer>,
    Arc<ScriptedService>,
    &mut VisualTestContext,
) {
    let server = ScriptedServer::new(Ok(connected(2)));
    let service = ScriptedService::new(status);
    let (window, cx) = open_settings(offered_with_service(&server, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    (window, server, service, cx)
}

#[test]
fn each_failure_offers_the_one_step_that_can_recover_from_it() {
    for error in EVERY_ERROR {
        let presented = present(LocalServerStatus::Failed(error));
        let expected = match error {
            ServerError::NotPaired => Some(LocalServerAction::SetUp),
            ServerError::NotRunning | ServerError::StartupFailed => Some(LocalServerAction::Start),
            // Only the service's explicit Start or Install reverses these, and only the service's
            // Start asks for startup authorization.
            ServerError::Stopped
            | ServerError::NotInstalled
            | ServerError::AuthorizationRequired => None,
            // Setting up again cannot repair storage or a refused, incompatible, or faulty server.
            _ => Some(LocalServerAction::Check),
        };
        assert_eq!(presented.action, expected, "{error:?}");
        assert!(!presented.working, "{error:?}");
    }
}

#[test]
fn a_failure_needing_attention_reads_as_an_error_in_its_bounded_wording() {
    for error in EVERY_ERROR {
        let presented = present(LocalServerStatus::Failed(error));
        match error {
            // These are steps in setup, not faults, so they read as guidance naming the step.
            ServerError::NotPaired
            | ServerError::NotRunning
            | ServerError::Stopped
            | ServerError::NotInstalled
            | ServerError::AuthorizationRequired => {
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
    let (window, cx) = open_settings(
        Some(OfferedServer {
            connection: Err(ServerError::StorageUnavailable),
            service: None,
        }),
        cx,
    );

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

#[gpui::test]
fn the_development_pane_waits_for_an_authenticated_server(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::NotRunning));
    let (window, cx) = open_settings(offered(&server), cx);
    click(NAVIGATION_SELECTOR, cx);

    // Opening never starts the server, so Start stays the step until a check authenticates.
    assert!(!presentation(&window, cx).development_pane);
    assert!(!rendered(OPEN_DEVELOPMENT_PANE_SELECTOR, cx));

    click(START_SELECTOR, cx);

    assert!(presentation(&window, cx).development_pane);
    assert!(rendered(OPEN_DEVELOPMENT_PANE_SELECTOR, cx));
}

#[gpui::test]
fn a_server_without_the_development_pane_is_offered_no_terminal(cx: &mut TestAppContext) {
    // A server from before terminal operations authenticates but never says it runs the Pane.
    let server = ScriptedServer::new(Ok(status_only(2)));
    let (window, cx) = open_settings(offered(&server), cx);

    click(NAVIGATION_SELECTOR, cx);

    assert!(rendered("settings-local-server-state-connected", cx));
    assert!(!presentation(&window, cx).development_pane);
    assert!(!rendered(OPEN_DEVELOPMENT_PANE_SELECTOR, cx));
}

#[test]
fn each_service_state_offers_the_steps_that_apply_to_it() {
    let presented = |status| {
        present_service(ServiceState {
            status: Some(status),
            installations: vec![retained("1.4.0", 'a')],
            ..ServiceState::default()
        })
    };
    for (status, state, actions, tone) in [
        (
            not_installed(),
            "Not Installed",
            vec![ServiceAction::Install],
            CaptionTone::Guidance,
        ),
        (
            running(),
            "Running",
            vec![
                ServiceAction::Stop,
                ServiceAction::Restart,
                ServiceAction::Uninstall,
            ],
            CaptionTone::Guidance,
        ),
        (
            stopped(),
            "Stopped",
            vec![ServiceAction::Start, ServiceAction::Uninstall],
            CaptionTone::Guidance,
        ),
        // Installed and not stopped by the person, yet not running, reads as needing attention.
        (
            not_running(),
            "Not Running",
            vec![ServiceAction::Start, ServiceAction::Uninstall],
            CaptionTone::Warning,
        ),
    ] {
        let presentation = presented(status);
        assert_eq!(presentation.state, state);
        assert_eq!(presentation.actions, actions, "{state}");
        assert_eq!(presentation.tone, tone, "{state}");
        assert_eq!(presentation.working, None);
    }
}

#[test]
fn installing_promises_to_start_the_server_only_when_the_person_has_not_stopped_it() {
    let presented = |status, installations, working| {
        present_service(ServiceState {
            status: Some(status),
            installations,
            working,
            ..ServiceState::default()
        })
    };
    let one = || vec![retained("1.4.0", 'a')];
    let several = || vec![retained("1.4.0", 'a'), retained("1.5.0", 'b')];

    // A first installation starts the server and keeps it running while the person is logged in.
    assert_eq!(
        presented(not_installed(), one(), None).explanation.as_ref(),
        "Version 1.4.0 (commit aaaaaaa) is verified and ready to install. The server then runs \
         for your account whenever you\u{2019}re logged in, even after SpaceTerm quits. No \
         administrator password is needed."
    );
    assert_eq!(
        presented(not_installed(), several(), None)
            .explanation
            .as_ref(),
        "Choose a verified server version from Install. It runs for your account whenever \
         you\u{2019}re logged in, even after SpaceTerm quits. No administrator password is \
         needed."
    );
    let installing = presented(not_installed(), one(), Some(ServiceAction::Install));
    assert_eq!(installing.state, "Installing");
    assert_eq!(
        installing.explanation.as_ref(),
        "Installing and starting the server\u{2026}"
    );

    // A Stop the person chose outlasts Uninstall, and installing again keeps it.
    for (installations, explanation) in [
        (
            one(),
            "Version 1.4.0 (commit aaaaaaa) is verified and ready to install. You stopped the \
             server. Installing keeps it stopped until you start it. No administrator password \
             is needed.",
        ),
        (
            several(),
            "Choose a verified server version from Install. You stopped the server. Installing \
             keeps it stopped until you start it. No administrator password is needed.",
        ),
    ] {
        let presentation = presented(uninstalled(), installations, None);
        assert_eq!(presentation.state, "Not Installed");
        assert_eq!(presentation.explanation.as_ref(), explanation);
        assert_eq!(presentation.tone, CaptionTone::Guidance);
        assert_eq!(presentation.actions, vec![ServiceAction::Install]);
    }
    let installing = presented(uninstalled(), one(), Some(ServiceAction::Install));
    assert_eq!(installing.state, "Installing");
    assert_eq!(
        installing.explanation.as_ref(),
        "Installing the server without starting it\u{2026}"
    );
}

#[test]
fn a_running_process_reads_as_running_even_while_a_stop_is_recorded() {
    let presentation = present_service(ServiceState {
        status: Some(super::ServiceStatus {
            stopped: true,
            ..running()
        }),
        ..ServiceState::default()
    });

    assert_eq!(presentation.state, "Running");
    assert_eq!(presentation.actions[0], ServiceAction::Stop);
}

#[test]
fn a_refused_operation_explains_itself_beside_the_status_read_after_it() {
    let presentation = present_service(ServiceState {
        status: Some(running()),
        failure: Some(ServerError::CleanupUnresolved),
        ..ServiceState::default()
    });

    assert_eq!(presentation.state, "Running");
    assert_eq!(presentation.tone, CaptionTone::Error);
    assert_eq!(
        presentation.explanation.as_ref(),
        ServerError::CleanupUnresolved.to_string()
    );
    assert_eq!(presentation.actions[0], ServiceAction::Stop);
}

#[test]
fn an_unreadable_service_offers_only_another_check() {
    let presentation = present_service(ServiceState {
        status: None,
        failure: Some(ServerError::ServiceUnavailable),
        ..ServiceState::default()
    });

    assert_eq!(presentation.state, "Unknown");
    assert_eq!(presentation.actions, vec![ServiceAction::Check]);
    assert_eq!(presentation.tone, CaptionTone::Error);
}

#[test]
fn only_actions_that_end_sessions_or_remove_the_service_ask_first() {
    let login = ServiceStartup::AtLogin;
    // The authorization steps ask through their own explanation instead.
    for action in [
        ServiceAction::Check,
        ServiceAction::Install,
        ServiceAction::Start,
        ServiceAction::AuthorizeInstall,
        ServiceAction::AuthorizeStart,
    ] {
        assert_eq!(ServiceConfirmation::for_action(action, true, login), None);
    }
    for action in [ServiceAction::Stop, ServiceAction::Restart] {
        let confirmation =
            ServiceConfirmation::for_action(action, true, login).expect("confirmation");
        assert!(confirmation.message.contains("end its Terminal Sessions"));
        assert_eq!(confirmation.confirm, action.label());
    }
    // Uninstalling a server that is not running ends no Terminal Session.
    let idle =
        ServiceConfirmation::for_action(ServiceAction::Uninstall, false, login).expect("idle");
    let live =
        ServiceConfirmation::for_action(ServiceAction::Uninstall, true, login).expect("live");
    assert!(!idle.message.contains("Terminal Sessions"));
    assert!(live.message.contains("end its Terminal Sessions"));
}

#[gpui::test]
fn presenting_servers_reads_the_service_without_changing_it(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::NotPaired));
    let service = ScriptedService::new(Ok(not_installed()));
    let (window, cx) = open_settings(offered_with_service(&server, &service), cx);
    assert!(service.calls().is_empty(), "opening Settings reads nothing");

    click(NAVIGATION_SELECTOR, cx);

    // Listing verified builds is a read: it installs and starts nothing.
    assert_eq!(
        service.calls(),
        vec![ServiceCall::Status, ServiceCall::RetainedInstallations]
    );
    assert!(service.changes().is_empty());
    for selector in [
        SERVICE_ROW_SELECTOR,
        SERVICE_CONTROL_SELECTOR,
        "settings-server-service-state-not-installed",
        INSTALL_SELECTOR,
    ] {
        assert!(rendered(selector, cx), "{selector} should render");
    }
    let row = cx.debug_bounds(SERVICE_ROW_SELECTOR).expect("row bounds");
    let control = cx
        .debug_bounds(SERVICE_CONTROL_SELECTOR)
        .expect("control bounds");
    assert!(row.contains(&control.origin) && control.right() <= row.right());
    // The connection row keeps its own Set Up beside the service.
    assert!(rendered(SET_UP_SELECTOR, cx));
    assert_eq!(
        service_presentation(&window, cx).actions,
        vec![ServiceAction::Install]
    );
}

#[gpui::test]
fn a_platform_without_a_service_omits_its_row(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Err(ServerError::NotRunning));
    let (window, cx) = open_settings(offered(&server), cx);

    click(NAVIGATION_SELECTOR, cx);

    assert!(!rendered(SERVICE_ROW_SELECTOR, cx));
    assert!(window.read_with(cx, |settings, _| {
        settings.server_service_presentation().is_none()
    }));
    // The existing workflow still starts the server from the connection row.
    click(START_SELECTOR, cx);
    assert_eq!(server.setups(), 1);
    search(&window, "launch agent", cx);
    assert!(!rendered(SERVICE_ROW_SELECTOR, cx));
}

#[test]
fn install_names_each_verified_build_by_version_and_commit() {
    let first = retained("1.4.0", 'a');
    let second = retained("1.5.0", 'b');
    let choices = InstallChoice::of(&[first.clone(), second]);
    let labels: Vec<_> = choices.iter().map(|choice| choice.label.as_ref()).collect();
    assert_eq!(
        labels,
        [
            "Version 1.4.0 (commit aaaaaaa)",
            "Version 1.5.0 (commit bbbbbbb)"
        ]
    );

    // Two packages of one version and commit also name their package, so they read apart.
    let mut rebuilt = first.clone();
    rebuilt.directory = rebuilt.directory.with_file_name("c".repeat(64));
    let choices = InstallChoice::of(&[first, rebuilt]);
    let labels: Vec<_> = choices.iter().map(|choice| choice.label.as_ref()).collect();
    assert_eq!(
        labels,
        [
            "Version 1.4.0 (commit aaaaaaa, package aaaaaaaa)",
            "Version 1.4.0 (commit aaaaaaa, package cccccccc)",
        ]
    );
}

#[test]
fn without_a_verified_build_the_row_explains_how_to_keep_one_and_offers_another_check() {
    let presentation = present_service(ServiceState {
        status: Some(not_installed()),
        ..ServiceState::default()
    });

    assert_eq!(presentation.state, "Not Installed");
    assert_eq!(presentation.actions, vec![ServiceAction::Check]);
    assert!(presentation.install_choices.is_empty());
    assert_eq!(presentation.tone, CaptionTone::Guidance);
    assert!(
        presentation
            .explanation
            .contains("Use the server installer command to keep a signed package")
    );
}

#[gpui::test]
fn one_verified_build_installs_from_install_and_checks_the_connection(cx: &mut TestAppContext) {
    let (window, server, service, cx) = open_servers_with_service(Ok(not_installed()), cx);
    let build = retained("1.4.0", 'a');
    let presented = service_presentation(&window, cx);
    assert!(
        presented
            .explanation
            .starts_with("Version 1.4.0 (commit aaaaaaa) is verified and ready to install.")
    );
    let checks = server.refreshes();

    click(INSTALL_SELECTOR, cx);

    assert!(!cx.did_prompt_for_paths(), "Install asks for no folder");
    assert_eq!(
        service.changes(),
        vec![ServiceCall::Install(build.directory)]
    );
    assert!(rendered("settings-server-service-state-running", cx));
    assert_eq!(service_presentation(&window, cx).state, "Running");
    assert_eq!(server.refreshes(), checks + 1);
}

#[gpui::test]
fn several_verified_builds_are_chosen_from_installs_menu(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Ok(connected(2)));
    let service = ScriptedService::new(Ok(not_installed()));
    let chosen = retained("1.5.0", 'b');
    service.keep(Ok(vec![retained("1.4.0", 'a'), chosen.clone()]));
    let (window, cx) = open_settings(offered_with_service(&server, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert!(
        service_presentation(&window, cx)
            .explanation
            .starts_with("Choose a verified server version from Install.")
    );

    click(INSTALL_SELECTOR, cx);
    assert!(
        service.changes().is_empty(),
        "opening the menu installs nothing"
    );
    assert!(!cx.did_prompt_for_paths(), "Install asks for no folder");
    let second = Box::leak(format!("{INSTALL_CHOICE_SELECTOR}-1").into_boxed_str());
    click(second, cx);

    assert_eq!(
        service.changes(),
        vec![ServiceCall::Install(chosen.directory)]
    );
    assert_eq!(service_presentation(&window, cx).state, "Running");
}

#[gpui::test]
fn a_build_kept_after_the_row_was_read_is_offered_after_another_check(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Ok(connected(2)));
    let service = ScriptedService::new(Ok(not_installed()));
    service.keep(Ok(Vec::new()));
    let (window, cx) = open_settings(offered_with_service(&server, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert!(!rendered(INSTALL_SELECTOR, cx));
    assert_eq!(
        service_presentation(&window, cx).actions,
        vec![ServiceAction::Check]
    );

    service.keep(Ok(vec![retained("1.4.0", 'a')]));
    click(SERVICE_CHECK_SELECTOR, cx);

    assert!(rendered(INSTALL_SELECTOR, cx));
    assert!(service.changes().is_empty());
}

#[gpui::test]
fn an_unreadable_build_store_explains_itself_and_offers_another_check(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Ok(connected(2)));
    let service = ScriptedService::new(Ok(not_installed()));
    service.keep(Err(ServerError::StorageUnavailable));
    let (window, cx) = open_settings(offered_with_service(&server, &service), cx);
    click(NAVIGATION_SELECTOR, cx);

    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Not Installed");
    assert_eq!(presented.tone, CaptionTone::Error);
    assert_eq!(
        presented.explanation.as_ref(),
        ServerError::StorageUnavailable.to_string()
    );
    assert_eq!(presented.actions, vec![ServiceAction::Check]);
}

#[gpui::test]
fn an_installed_service_lists_no_builds(cx: &mut TestAppContext) {
    let (_window, _server, service, _cx) = open_servers_with_service(Ok(running()), cx);

    assert_eq!(service.calls(), vec![ServiceCall::Status]);
}

#[gpui::test]
fn only_a_listed_build_is_installed(cx: &mut TestAppContext) {
    let (window, _server, service, cx) = open_servers_with_service(Ok(not_installed()), cx);

    window.update(cx, |settings, cx| {
        settings.install_server_service(PathBuf::from("/elsewhere"), cx);
    });
    cx.run_until_parked();

    assert!(service.changes().is_empty());
}

#[gpui::test]
fn a_refused_installation_explains_itself_and_offers_install_again(cx: &mut TestAppContext) {
    let (window, _server, service, cx) = open_servers_with_service(Ok(not_installed()), cx);
    service.refuse_next(ServerError::RuntimeUnavailable);

    click(INSTALL_SELECTOR, cx);

    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Not Installed");
    assert_eq!(presented.tone, CaptionTone::Error);
    assert_eq!(
        presented.explanation.as_ref(),
        ServerError::RuntimeUnavailable.to_string()
    );
    assert_eq!(presented.actions, vec![ServiceAction::Install]);
}

#[gpui::test]
fn stop_asks_first_and_cancelling_leaves_the_server_running(cx: &mut TestAppContext) {
    let (window, _server, service, cx) = open_servers_with_service(Ok(running()), cx);

    click(STOP_SELECTOR, cx);
    assert!(alert_open(STOP_CONFIRM_SELECTOR, cx));
    assert!(service.changes().is_empty(), "Stop waits for confirmation");

    click_modal(STOP_CANCEL_SELECTOR, cx);

    assert!(service.changes().is_empty());
    assert_eq!(service_presentation(&window, cx).state, "Running");
}

#[gpui::test]
fn a_confirmed_stop_stays_stopped_and_offers_start(cx: &mut TestAppContext) {
    let (window, server, service, cx) = open_servers_with_service(Ok(running()), cx);
    server.answer_refresh(Err(ServerError::Stopped));

    click(STOP_SELECTOR, cx);
    click_modal(STOP_CONFIRM_SELECTOR, cx);

    assert_eq!(service.changes(), vec![ServiceCall::Stop]);
    assert!(rendered("settings-server-service-state-stopped", cx));
    assert!(rendered(START_SERVICE_SELECTOR, cx));
    let presented = service_presentation(&window, cx);
    assert_eq!(
        presented.actions,
        vec![ServiceAction::Start, ServiceAction::Uninstall]
    );
    // The connection is checked afresh and offers no step that could reverse the Stop.
    assert!(rendered("settings-local-server-state-stopped", cx));
    let connection = presentation(&window, cx);
    assert_eq!(connection.action, None);
    assert!(!connection.development_pane && !connection.shared_workspaces);
}

#[gpui::test]
fn start_restores_a_stopped_server_without_asking(cx: &mut TestAppContext) {
    let (window, server, service, cx) = open_servers_with_service(Ok(stopped()), cx);
    let checks = server.refreshes();

    click(START_SERVICE_SELECTOR, cx);

    assert_eq!(service.changes(), vec![ServiceCall::Start]);
    assert_eq!(service_presentation(&window, cx).state, "Running");
    assert_eq!(server.refreshes(), checks + 1);
    assert!(rendered("settings-local-server-state-connected", cx));
}

#[gpui::test]
fn restart_asks_first_then_restarts(cx: &mut TestAppContext) {
    let (window, _server, service, cx) = open_servers_with_service(Ok(running()), cx);

    click(RESTART_SELECTOR, cx);
    assert!(alert_open(RESTART_CONFIRM_SELECTOR, cx));
    click_modal(RESTART_CANCEL_SELECTOR, cx);
    assert!(service.changes().is_empty());

    click(RESTART_SELECTOR, cx);
    click_modal(RESTART_CONFIRM_SELECTOR, cx);

    assert_eq!(service.changes(), vec![ServiceCall::Restart]);
    assert_eq!(service_presentation(&window, cx).state, "Running");
}

#[gpui::test]
fn uninstall_asks_first_then_offers_install_again(cx: &mut TestAppContext) {
    let (window, _server, service, cx) = open_servers_with_service(Ok(stopped()), cx);

    click(UNINSTALL_SELECTOR, cx);
    assert!(alert_open(UNINSTALL_CONFIRM_SELECTOR, cx));
    click_modal(UNINSTALL_CANCEL_SELECTOR, cx);
    assert!(service.changes().is_empty());

    click(UNINSTALL_SELECTOR, cx);
    click_modal(UNINSTALL_CONFIRM_SELECTOR, cx);

    assert_eq!(service.changes(), vec![ServiceCall::Uninstall]);
    let presented = service_presentation(&window, cx);
    assert_eq!(presented.actions, vec![ServiceAction::Install]);
    assert!(
        presented
            .explanation
            .contains("Installing keeps it stopped")
    );
    assert!(!presented.explanation.contains("logged in"));

    // Installing again keeps the Stop, and the progress says so while it runs.
    let build = retained("1.4.0", 'a');
    window.update(cx, |settings, cx| {
        settings.install_server_service(build.directory.clone(), cx);
    });
    assert_eq!(
        service_presentation(&window, cx).explanation.as_ref(),
        "Installing the server without starting it\u{2026}"
    );
    cx.run_until_parked();

    assert_eq!(
        service.changes(),
        vec![
            ServiceCall::Uninstall,
            ServiceCall::Install(build.directory)
        ]
    );
    assert!(rendered("settings-server-service-state-stopped", cx));
    assert_eq!(
        service_presentation(&window, cx).actions,
        vec![ServiceAction::Start, ServiceAction::Uninstall]
    );
}

#[gpui::test]
fn a_service_change_waits_for_the_connection_and_the_reverse(cx: &mut TestAppContext) {
    let (window, server, service, cx) = open_servers_with_service(Ok(stopped()), cx);
    let setups = server.setups();

    window.update(cx, |settings, cx| {
        settings.run_local_server(LocalServerAction::SetUp, cx);
        // The connection row is working, so the service waits.
        settings.run_server_service(ServiceAction::Start, cx);
    });
    cx.run_until_parked();
    assert!(service.changes().is_empty());
    assert_eq!(server.setups(), setups + 1);

    window.update(cx, |settings, cx| {
        settings.run_server_service(ServiceAction::Start, cx);
        // The service is changing, so the connection waits for the check that follows it.
        settings.run_local_server(LocalServerAction::SetUp, cx);
    });
    cx.run_until_parked();
    assert_eq!(service.changes(), vec![ServiceCall::Start]);
    assert_eq!(server.setups(), setups + 1);
}

#[gpui::test]
fn the_progress_indicator_replaces_the_service_badge_while_an_action_runs(cx: &mut TestAppContext) {
    let (window, _server, _service, cx) = open_servers_with_service(Ok(stopped()), cx);
    assert!(!rendered("settings-server-service-progress-track", cx));

    window.update(cx, |settings, cx| {
        settings.run_server_service(ServiceAction::Start, cx);
    });
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });

    assert!(rendered("settings-server-service-progress-track", cx));
    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Starting");
    assert_eq!(presented.working, Some(ServiceAction::Start));
}

#[gpui::test]
fn an_unresolved_service_explains_itself_and_offers_nothing(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Ok(connected(2)));
    let (window, cx) = open_settings(
        Some(OfferedServer {
            connection: Ok(Arc::clone(&server) as Arc<dyn LocalServerControl>),
            service: Some(Err(ServerError::StorageUnavailable)),
        }),
        cx,
    );

    click(NAVIGATION_SELECTOR, cx);

    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Unknown");
    assert!(presented.actions.is_empty());
    for selector in [SERVICE_CHECK_SELECTOR, INSTALL_SELECTOR, STOP_SELECTOR] {
        assert!(!rendered(selector, cx), "{selector} should not be offered");
    }
}

#[gpui::test]
fn settings_search_reaches_the_service(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Ok(connected(2)));
    let service = ScriptedService::new(Ok(running()));
    let (window, cx) = open_settings(offered_with_service(&server, &service), cx);

    search(&window, "launch agent", cx);

    assert_eq!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Servers
    );
    assert!(rendered("settings-server-service-state-running", cx));
}

#[gpui::test]
fn settings_search_reaches_the_service_by_its_linux_names(cx: &mut TestAppContext) {
    for query in ["systemd", "linger"] {
        let server = ScriptedServer::new(Ok(connected(2)));
        let service = ScriptedService::linux(Ok(running()));
        let (window, cx) = open_settings(offered_with_service(&server, &service), cx);

        search(&window, query, cx);

        assert_eq!(
            window.read_with(cx, |settings, _| settings.active_section),
            SettingsSectionId::Servers,
            "{query}"
        );
        assert!(rendered(SERVICE_ROW_SELECTOR, cx), "{query}");
        assert!(service.changes().is_empty(), "{query}");
    }
}

#[gpui::test]
fn assistive_technology_hears_the_service_state_and_what_each_step_does(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let (window, _server, service, cx) = open_servers_with_service(Ok(running()), cx);
    let tree = A11yTree::read(cx);
    assert!(
        tree.with_role("Status")
            .iter()
            .any(|status| status["aria"]["label"] == "Running")
    );
    let stop = tree.node("Stop Server");
    assert_eq!(stop["aria"]["role"], "Button");
    assert_eq!(
        stop["aria"]["description"],
        "Ends the server's Terminal Sessions. SpaceTerm asks first."
    );

    // Activating Stop from assistive technology asks first, with Cancel focused, so Escape keeps
    // the server running.
    perform(cx, stop, Action::Click);
    cx.run_until_parked();
    assert!(alert_open(STOP_CONFIRM_SELECTOR, cx));
    let tree = A11yTree::read(cx);
    assert_eq!(
        tree.focused().map(|node| node["aria"]["label"].clone()),
        Some("Cancel".into())
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(service.changes().is_empty());
    assert_eq!(service_presentation(&window, cx).state, "Running");
}

#[gpui::test]
fn assistive_technology_hears_which_build_install_installs(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (_window, _server, _service, cx) = open_servers_with_service(Ok(not_installed()), cx);
    let install = A11yTree::read(cx).node("Install").clone();

    assert_eq!(install["aria"]["role"], "Button");
    assert_eq!(
        install["aria"]["description"],
        "Installs Version 1.4.0 (commit aaaaaaa)."
    );
}

#[gpui::test]
fn keyboard_focus_stays_on_the_leading_step_when_stop_gives_way_to_start(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (_window, _server, service, cx) = open_servers_with_service(Ok(running()), cx);
    let tree = A11yTree::read(cx);
    let stop = tree.node("Stop Server");
    spaceterm_ui::a11y_testing::perform(cx, stop, gpui::accesskit::Action::Focus);
    cx.run_until_parked();
    assert_eq!(
        A11yTree::read(cx)
            .focused()
            .map(|node| node["aria"]["label"].clone()),
        Some("Stop Server".into())
    );
    // A button activates when Return is released.
    cx.simulate_keystrokes("enter");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").expect("keystroke"),
    });
    cx.run_until_parked();
    assert!(alert_open(STOP_CONFIRM_SELECTOR, cx));
    click_modal(STOP_CONFIRM_SELECTOR, cx);
    assert_eq!(service.changes(), vec![ServiceCall::Stop]);

    let tree = A11yTree::read(cx);
    assert_eq!(
        tree.focused().map(|node| node["aria"]["label"].clone()),
        Some("Start Server".into())
    );
}

#[test]
fn linux_wording_names_boot_and_logout_and_keeps_lingering_after_uninstall() {
    let presented = |startup, status| {
        present_service(ServiceState {
            startup,
            status: Some(status),
            installations: vec![retained("1.4.0", 'a')],
            ..ServiceState::default()
        })
        .explanation
    };
    for status in [not_installed(), running(), stopped()] {
        let linux = presented(ServiceStartup::AtBoot, status);
        let macos = presented(ServiceStartup::AtLogin, status);
        assert!(
            linux.contains("computer starts") || linux.contains("computer restarts"),
            "{linux}"
        );
        assert!(
            !linux.contains("log in") && !linux.contains("logged in"),
            "{linux}"
        );
        assert!(
            macos.contains("log in") || macos.contains("logged in"),
            "{macos}"
        );
    }
    // Lingering can be turned off outside SpaceTerm, and the status does not report it, so a
    // running server promises boot and logout only while lingering stays on. Surviving
    // SpaceTerm's quit does not depend on it.
    assert_eq!(
        presented(ServiceStartup::AtBoot, running()).as_ref(),
        "The server keeps running after SpaceTerm quits. While lingering stays on for your \
         account, it also starts when the computer starts and keeps running after you log out."
    );
    // Installing may need authorization on Linux, and SpaceTerm says it asks first.
    let install = presented(ServiceStartup::AtBoot, not_installed());
    assert!(install.contains("keeps running after you log out"));
    assert!(install.contains("SpaceTerm asks you first"));
    assert!(!install.contains("No administrator password is needed"));
    // Installing a stopped server starts nothing, so it needs no authorization.
    assert!(
        presented(ServiceStartup::AtBoot, uninstalled())
            .contains("Installing keeps it stopped until you start it. No administrator password")
    );

    let boot = ServiceStartup::AtBoot;
    let stop = ServiceConfirmation::for_action(ServiceAction::Stop, true, boot).expect("stop");
    assert!(stop.detail.contains("even after the computer restarts"));
    let uninstall =
        ServiceConfirmation::for_action(ServiceAction::Uninstall, true, boot).expect("uninstall");
    assert!(
        uninstall
            .detail
            .contains("no longer starts when the computer starts")
    );
    assert!(
        uninstall
            .detail
            .contains("Lingering stays on for your account")
    );
    assert!(uninstall.detail.contains("loginctl disable-linger"));
}

#[test]
fn the_authorization_prompt_names_its_consequences_and_what_follows() {
    let install = AuthorizationPrompt::for_request(
        &AuthorizedRequest::Install(retained("1.4.0", 'a').directory),
        Some("Version 1.4.0 (commit aaaaaaa)"),
    );
    assert_eq!(install.confirm, "Authorize and Install");
    assert_eq!(
        install.detail.as_ref(),
        "Linux asks for an administrator password to turn on lingering for your account. \
         Lingering applies to every service your account runs, and uninstalling the server \
         leaves it on. SpaceTerm then installs Version 1.4.0 (commit aaaaaaa) and starts the \
         server. If you cancel here or in the password prompt, nothing changes."
    );
    let start = AuthorizationPrompt::for_request(&AuthorizedRequest::Start, None);
    assert_eq!(start.confirm, "Authorize and Start");
    assert!(start.detail.contains("SpaceTerm then starts the server."));
    assert!(!start.detail.contains("installs"));
}

#[gpui::test]
fn a_linux_installation_waits_for_explained_authorization_then_installs_its_build(
    cx: &mut TestAppContext,
) {
    let server = ScriptedServer::new(Ok(connected(2)));
    let service = ScriptedService::linux(Ok(not_installed()));
    let (window, cx) = open_settings(offered_with_service(&server, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    let build = retained("1.4.0", 'a').directory;

    click(INSTALL_SELECTOR, cx);

    // The refusal asks the operating system nothing by itself.
    assert_eq!(service.changes(), vec![ServiceCall::Install(build.clone())]);
    assert!(rendered(
        "settings-server-service-state-authorization-required",
        cx
    ));
    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Authorization Required");
    assert_eq!(presented.tone, CaptionTone::Warning);
    assert_eq!(
        presented.explanation.as_ref(),
        "Linux needs an administrator password before the server can start at boot and keep \
         running after you log out. The server is not installed. Choose Authorize and Install to \
         continue."
    );
    assert_eq!(presented.actions, vec![ServiceAction::AuthorizeInstall]);

    click(AUTHORIZE_INSTALL_SELECTOR, cx);
    assert!(alert_open(AUTHORIZE_CONFIRM_SELECTOR, cx));
    assert_eq!(service.changes().len(), 1, "the explanation asks nothing");
    let checks = server.refreshes();

    click_modal(AUTHORIZE_CONFIRM_SELECTOR, cx);

    // Authorization continues with the build the person chose, and nothing more.
    assert_eq!(
        service.changes(),
        vec![
            ServiceCall::Install(build.clone()),
            ServiceCall::AuthorizeStartup,
            ServiceCall::Install(build),
        ]
    );
    assert!(rendered("settings-server-service-state-running", cx));
    let presented = service_presentation(&window, cx);
    assert_eq!(presented.authorization, None);
    assert!(
        presented
            .explanation
            .contains("keeps running after you log out")
    );
    assert_eq!(server.refreshes(), checks + 1);
}

#[gpui::test]
fn cancelling_the_operating_systems_prompt_changes_nothing_and_keeps_the_stop(
    cx: &mut TestAppContext,
) {
    let server = ScriptedServer::new(Err(ServerError::Stopped));
    let service = ScriptedService::linux(Ok(stopped()));
    let (window, cx) = open_settings(offered_with_service(&server, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    click(START_SERVICE_SELECTOR, cx);
    assert_eq!(service.changes(), vec![ServiceCall::Start]);
    assert_eq!(
        service_presentation(&window, cx).actions,
        vec![ServiceAction::AuthorizeStart, ServiceAction::Uninstall]
    );
    service.answer_prompt(Prompt::Cancel);

    click(AUTHORIZE_START_SELECTOR, cx);
    click_modal(AUTHORIZE_CONFIRM_SELECTOR, cx);

    // The cancelled prompt starts nothing, and the server stays stopped.
    assert_eq!(
        service.changes(),
        vec![ServiceCall::Start, ServiceCall::AuthorizeStartup]
    );
    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Authorization Required");
    assert_eq!(
        presented.explanation.as_ref(),
        "Authorization did not complete, so nothing changed. The server is stopped. Choose \
         Authorize and Start to try again."
    );
    assert_eq!(
        presented.actions,
        vec![ServiceAction::AuthorizeStart, ServiceAction::Uninstall]
    );
    assert_eq!(presentation(&window, cx).action, None);

    // Presenting the section again only reads. The Stop stands until an explicit Start.
    click("settings-navigation-settings-section-git", cx);
    click(NAVIGATION_SELECTOR, cx);
    assert_eq!(
        service.changes(),
        vec![ServiceCall::Start, ServiceCall::AuthorizeStartup]
    );
    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Stopped");
    assert_eq!(
        presented.actions,
        vec![ServiceAction::Start, ServiceAction::Uninstall]
    );
    assert!(
        presented
            .explanation
            .contains("even after the computer restarts")
    );
}

#[gpui::test]
fn a_stopped_linux_server_reinstalls_stopped_and_only_start_asks_for_authorization(
    cx: &mut TestAppContext,
) {
    let (window, _server, service, cx) = {
        let server = ScriptedServer::new(Err(ServerError::Stopped));
        let service = ScriptedService::linux(Ok(uninstalled()));
        let (window, cx) = open_settings(offered_with_service(&server, &service), cx);
        click(NAVIGATION_SELECTOR, cx);
        (window, server, service, cx)
    };
    let build = retained("1.4.0", 'a').directory;

    click(INSTALL_SELECTOR, cx);

    // Installing keeps the Stop, so it starts nothing and needs no authorization.
    assert_eq!(service.changes(), vec![ServiceCall::Install(build.clone())]);
    assert_eq!(service_presentation(&window, cx).state, "Stopped");

    click(START_SERVICE_SELECTOR, cx);
    click(AUTHORIZE_START_SELECTOR, cx);
    click_modal(AUTHORIZE_CONFIRM_SELECTOR, cx);

    assert_eq!(
        service.changes(),
        vec![
            ServiceCall::Install(build),
            ServiceCall::Start,
            ServiceCall::AuthorizeStartup,
            ServiceCall::Start,
        ]
    );
    assert_eq!(service_presentation(&window, cx).state, "Running");
}

#[gpui::test]
fn a_linux_restart_refused_after_stopping_offers_to_authorize_and_start(cx: &mut TestAppContext) {
    let server = ScriptedServer::new(Ok(connected(2)));
    let service = ScriptedService::linux(Ok(running()));
    let (window, cx) = open_settings(offered_with_service(&server, &service), cx);
    click(NAVIGATION_SELECTOR, cx);

    click(RESTART_SELECTOR, cx);
    click_modal(RESTART_CONFIRM_SELECTOR, cx);

    assert_eq!(service.changes(), vec![ServiceCall::Restart]);
    let presented = service_presentation(&window, cx);
    assert_eq!(
        presented.explanation.as_ref(),
        "Linux needs an administrator password before the server can start at boot and keep \
         running after you log out. The server is stopped. Choose Authorize and Start to \
         continue."
    );
    assert_eq!(
        presented.actions,
        vec![ServiceAction::AuthorizeStart, ServiceAction::Uninstall]
    );

    click(AUTHORIZE_START_SELECTOR, cx);
    click_modal(AUTHORIZE_CONFIRM_SELECTOR, cx);

    assert_eq!(
        service.changes(),
        vec![
            ServiceCall::Restart,
            ServiceCall::AuthorizeStartup,
            ServiceCall::Start,
        ]
    );
    assert_eq!(service_presentation(&window, cx).state, "Running");
}

#[gpui::test]
fn assistive_technology_hears_the_authorization_step_and_escape_leaves_it_unasked(
    cx: &mut TestAppContext,
) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let (window, _server, service, cx) = {
        let server = ScriptedServer::new(Ok(connected(2)));
        let service = ScriptedService::linux(Ok(not_running()));
        let (window, cx) = open_settings(offered_with_service(&server, &service), cx);
        click(NAVIGATION_SELECTOR, cx);
        (window, server, service, cx)
    };
    click(START_SERVICE_SELECTOR, cx);

    let tree = A11yTree::read(cx);
    // The status announces why the server did not start.
    assert!(
        tree.with_role("Status")
            .iter()
            .any(|status| status["aria"]["label"] == "Authorization Required")
    );
    let authorize = tree.node("Authorize and Start\u{2026}");
    assert_eq!(authorize["aria"]["role"], "Button");
    assert_eq!(
        authorize["aria"]["description"],
        "SpaceTerm explains the change first. Linux then asks for an administrator password, \
         and SpaceTerm starts the server."
    );

    // The explanation opens with Cancel focused, so Return and Escape ask nothing.
    perform(cx, authorize, Action::Click);
    cx.run_until_parked();
    assert!(alert_open(AUTHORIZE_CANCEL_SELECTOR, cx));
    let tree = A11yTree::read(cx);
    let prompt = tree.node("Run the Server at Startup");
    assert_eq!(prompt["aria"]["role"], "AlertDialog");
    assert_eq!(
        prompt["aria"]["description"],
        "Let your account\u{2019}s services start at boot and keep running after you log out?"
    );
    assert_eq!(
        tree.focused().map(|node| node["aria"]["label"].clone()),
        Some("Cancel".into())
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert_eq!(service.changes(), vec![ServiceCall::Start]);
    assert_eq!(
        service_presentation(&window, cx).actions,
        vec![ServiceAction::AuthorizeStart, ServiceAction::Uninstall]
    );
}
