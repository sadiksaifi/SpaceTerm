//! The Servers section driven through a scripted local server and service.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use super::testing::{
    Prompt, ScriptedCore, ScriptedService, ServiceCall, connected, not_installed, not_running,
    retained, running, stopped, uninstalled,
};
use super::{
    AUTHORIZE_CANCEL_SELECTOR, AUTHORIZE_CONFIRM_SELECTOR, AUTHORIZE_INSTALL_SELECTOR,
    AUTHORIZE_START_SELECTOR, AuthorizationPrompt, AuthorizedRequest, CoreServiceControl,
    INSTALL_CHOICE_SELECTOR, INSTALL_SELECTOR, InstallChoice, LocalCoreControl,
    LocalCorePresentation, LocalCoreStatus, OfferedLocalMachine, OfferedMachine,
    RESTART_CANCEL_SELECTOR, RESTART_CONFIRM_SELECTOR, RESTART_SELECTOR, SERVICE_CHECK_SELECTOR,
    START_SERVICE_SELECTOR, STOP_CANCEL_SELECTOR, STOP_CONFIRM_SELECTOR, STOP_SELECTOR,
    ServiceAction, ServiceConfirmation, ServicePresentation, ServiceStartup, ServiceState,
    UNINSTALL_CANCEL_SELECTOR, UNINSTALL_CONFIRM_SELECTOR, UNINSTALL_SELECTOR, present,
    present_service,
};
use crate::appearance::Appearance;
use crate::machine_connection::CoreError;
use crate::platform::appearance::testing::RecordingAppearancePlatform;
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::settings::SettingsDocument;
use crate::settings::storage::testing::MemoryStorage;
use crate::ui::appearance_runtime;
use crate::ui::settings_window::core_update::CoreUpdateControl;
use crate::ui::settings_window::core_update::testing::{ScriptedUpdate, available};
use crate::ui::settings_window::machine_card::{
    AUTO_START_SELECTOR, MENU_SELECTOR, STATUS_SELECTOR,
};
use crate::ui::settings_window::machine_list::MachineChoice;
use crate::ui::settings_window::{SettingsSectionId, SettingsWindow};
use crate::ui::sidebar_window::SidebarOwner;
use crate::ui::sidebar_window::form::CaptionTone;

const NAVIGATION_SELECTOR: &str = "settings-navigation-settings-section-servers";
const ROW_SELECTOR: &str = "settings-row-server-status";
/// Only a server a service runs starts on its own, so only it offers this row.
const SERVICE_ROW_SELECTOR: &str = "settings-row-server-auto-start";
/// The spinner leading the Server row's subtitle while it names work underway.
const SUBTITLE_PROGRESS: &str = "settings-server-progress-track";
/// The confirmation Install asks before it installs a verified build.
const INSTALL_CONFIRM_SELECTOR: &str = "settings-server-install-confirm";

macro_rules! core_error_cases {
    ($($variant:ident => ($retry:literal, $tone:ident)),+ $(,)?) => {
        fn every_error() -> impl Iterator<Item = (CoreError, bool, CaptionTone)> {
            fn expectation(error: CoreError) -> (bool, CaptionTone) {
                match error {
                    $(CoreError::$variant => ($retry, CaptionTone::$tone),)+
                    CoreError::Direct(_) => (true, CaptionTone::Error),
                }
            }
            [$(CoreError::$variant,)+ CoreError::Direct(
                crate::machine_connection::direct::DirectError::Transport
            )].into_iter().map(|error| {
                let (retry, tone) = expectation(error);
                (error, retry, tone)
            })
        }
    };
}

core_error_cases! {
    NotSetUp => (true, Guidance),
    ServiceUnavailable => (true, Error),
    ServiceBusy => (true, Guidance),
    NotInstalled => (false, Guidance),
    Stopped => (false, Guidance),
    Changed => (true, Error),
    ActivationUnresolved => (true, Guidance),
    ReturnUnavailable => (true, Error),
    RuntimeUnavailable => (true, Error),
    CandidateMissing => (false, Error),
    AuthorizationRequired => (false, Guidance),
    NotRunning => (true, Guidance),
    StorageUnavailable => (true, Error),
    IncompatibleState => (true, Error),
    UnsafeStorage => (true, Error),
    AuthenticationRefused => (true, Error),
    IncompatibleProtocol => (true, Error),
    CandidateChanged => (false, Error),
    Protocol => (true, Error),
    StartupFailed => (true, Error),
    Controlled => (true, Error),
    SessionUnavailable => (true, Error),
    StaleControl => (true, Error),
    Unsupported => (true, Error),
    DirectoryUnavailable => (true, Error),
    CleanupUnresolved => (true, Error),
    Capacity => (true, Error),
}

/// A server on a platform without a service layer, which shows only the connection row.
fn offered(core: &Arc<ScriptedCore>) -> OfferedLocalMachine {
    Some(OfferedMachine {
        connection: Ok(Arc::clone(core) as Arc<dyn LocalCoreControl>),
        service: None,
        update: None,
        command_line_tool: None,
    })
}

fn offered_with_service(
    core: &Arc<ScriptedCore>,
    service: &Arc<ScriptedService>,
) -> OfferedLocalMachine {
    Some(OfferedMachine {
        connection: Ok(Arc::clone(core) as Arc<dyn LocalCoreControl>),
        service: Some(Ok(Arc::clone(service) as Arc<dyn CoreServiceControl>)),
        update: None,
        command_line_tool: None,
    })
}

fn open_settings(
    local_core: OfferedLocalMachine,
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
            crate::theme_registry::testing::offline(),
            local_core,
            window,
            cx,
        )
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (window, cx)
}

/// Clicks `selector` on the page, or in the Server row's ⋯ menu, where every step but the row's
/// one button lives.
fn click(selector: &'static str, cx: &mut VisualTestContext) {
    if cx.debug_bounds(selector).is_none() && cx.debug_bounds(MENU_SELECTOR).is_some() {
        click_at(MENU_SELECTOR, cx);
    }
    click_at(selector, cx);
}

fn click_at(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

/// Whether the Server row offers `selector`, as its button or in its ⋯ menu. It opens the menu
/// to look and closes it again without choosing anything.
fn row_offers(selector: &'static str, cx: &mut VisualTestContext) -> bool {
    if rendered(selector, cx) {
        return true;
    }
    if !rendered(MENU_SELECTOR, cx) {
        return false;
    }
    click_at(MENU_SELECTOR, cx);
    let offered = rendered(selector, cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    offered
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
) -> LocalCorePresentation {
    window.read_with(cx, |settings, _| {
        settings
            .local_core_presentation()
            .expect("the local server should be offered")
    })
}

fn rendered(selector: &'static str, cx: &mut VisualTestContext) -> bool {
    cx.debug_bounds(selector).is_some()
}

/// What the card's Status row says about this computer's server.
fn status(window: &Entity<SettingsWindow>, cx: &mut VisualTestContext) -> String {
    window.read_with(cx, |settings, _| {
        settings.core_state(&MachineChoice::Local).state.to_string()
    })
}

/// Chooses `selector` from the Status row's ⋯ menu.
fn choose(selector: &'static str, cx: &mut VisualTestContext) {
    click(MENU_SELECTOR, cx);
    click(selector, cx);
}

/// Installs the build Install offers, confirming what installing does.
fn install(cx: &mut VisualTestContext) {
    click(INSTALL_SELECTOR, cx);
    click_modal(INSTALL_CONFIRM_SELECTOR, cx);
}

fn service_presentation(
    window: &Entity<SettingsWindow>,
    cx: &mut VisualTestContext,
) -> ServicePresentation {
    window.read_with(cx, |settings, _| {
        settings
            .core_service_presentation()
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

/// Opens Settings on the Servers section with a service in `status`, behind an account-local server.
fn open_machines_with_service(
    status: Result<super::ServiceStatus, CoreError>,
    cx: &mut TestAppContext,
) -> (
    Entity<SettingsWindow>,
    Arc<ScriptedCore>,
    Arc<ScriptedService>,
    &mut VisualTestContext,
) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let service = ScriptedService::new(status);
    let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    (window, core, service, cx)
}

/// Opens Servers for a running server with its service and update journey, every read settled.
fn open_machines_with_update(
    cx: &mut TestAppContext,
) -> (
    Entity<SettingsWindow>,
    Arc<ScriptedCore>,
    Arc<ScriptedUpdate>,
    &mut VisualTestContext,
) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let service = ScriptedService::new(Ok(running()));
    let update = ScriptedUpdate::new(available());
    let offered = Some(OfferedMachine {
        connection: Ok(Arc::clone(&core) as Arc<dyn LocalCoreControl>),
        service: Some(Ok(Arc::clone(&service) as Arc<dyn CoreServiceControl>)),
        update: Some(Arc::clone(&update) as Arc<dyn CoreUpdateControl>),
        command_line_tool: None,
    });
    let (window, cx) = open_settings(offered, cx);
    click(NAVIGATION_SELECTOR, cx);
    (window, core, update, cx)
}

/// Where the latest frame drew each row Servers shows for this computer's server, and the Server
/// row's subtitle.
fn core_page_layout(
    cx: &mut VisualTestContext,
) -> Vec<(&'static str, Option<gpui::Bounds<gpui::Pixels>>)> {
    [
        ROW_SELECTOR,
        "settings-row-server-status-description",
        SERVICE_ROW_SELECTOR,
        "settings-row-end-all-sessions",
        "settings-row-network-access",
        "settings-row-all-servers",
        "settings-row-add-server",
    ]
    .into_iter()
    .map(|selector| (selector, cx.debug_bounds(selector)))
    .collect()
}

/// Returning to Servers checks the connection, the service and the version again. While those
/// reads are held open and then finish one at a time, in whichever order the scheduler picks,
/// nothing on the page moves, dims or gains a footnote.
/// `a_check_says_checking_only_once_it_runs_longer_than_a_second` covers a check that outlasts
/// the progress delay.
#[gpui::test(iterations = 20)]
fn returning_to_machines_moves_nothing_while_its_checks_finish_in_any_order(
    cx: &mut TestAppContext,
) {
    let (window, core, update, cx) = open_machines_with_update(cx);
    cx.run_until_parked();
    let settled = core_page_layout(cx);
    for (selector, bounds) in settled.iter().take(3) {
        assert!(bounds.is_some(), "{selector} should render");
    }
    let refreshes = core.refreshes();
    let reads = update.reads();
    let footnote = window.read_with(cx, |settings, cx| settings.machine_card_footnote(cx));

    // Leaving and returning starts the three checks. The frame drawn at the end of this update
    // shows them all held, since no background work has run yet.
    window.update(cx, |settings, cx| {
        settings.select_section(SettingsSectionId::Interface, cx);
        settings.select_section(SettingsSectionId::Machines, cx);
    });
    let unchanged = |moment: &str, cx: &mut VisualTestContext| {
        assert_eq!(core_page_layout(cx), settled, "{moment}: the page moved");
        window.read_with(cx, |settings, cx| {
            assert!(
                settings.selected_machine_live(),
                "{moment}: the running server stays live"
            );
            assert_eq!(
                settings.machine_card_footnote(cx),
                footnote,
                "{moment}: the footnote stays as it was"
            );
        });
    };
    unchanged("every check held", cx);

    let mut step = 0;
    while cx.executor().tick() {
        step += 1;
        unchanged(&format!("after step {step}"), cx);
    }
    cx.run_until_parked();
    unchanged("every check finished", cx);
    assert_eq!(core.refreshes(), refreshes + 1);
    assert!(update.reads() > reads, "the version was read again");
}

#[test]
fn checking_again_is_offered_unless_only_the_service_can_change_the_answer() {
    for (error, expected, _) in every_error() {
        let presented = present(LocalCoreStatus::Failed(error));
        assert_eq!(presented.check, expected, "{error:?}");
        assert!(!presented.working, "{error:?}");
    }
}

#[test]
fn a_failure_needing_attention_reads_as_an_error_in_its_bounded_wording() {
    for (error, _, tone) in every_error() {
        let presented = present(LocalCoreStatus::Failed(error));
        assert_eq!(presented.tone, tone, "{error:?}");
        if tone == CaptionTone::Error {
            assert_eq!(
                presented.explanation.as_ref(),
                error.to_string(),
                "{error:?}"
            );
        } else {
            assert!(!presented.explanation.is_empty(), "{error:?}");
        }
    }
}

#[test]
fn a_connection_names_only_its_protocol_version() {
    let presented = present(LocalCoreStatus::Connected(connected(3)));

    assert_eq!(presented.state, "Connected");
    assert_eq!(
        presented.explanation.as_ref(),
        "Connected to SpaceTerm Core for your account. Protocol version 3."
    );
    assert!(presented.check);

    let checking = present(LocalCoreStatus::Checking);
    assert!(checking.working);
    assert!(checking.check);
}

/// A Development run's server ends with the run, so Servers never claims it keeps running after
/// SpaceTerm quits, while an installed server keeps that description.
#[gpui::test]
fn a_development_run_core_is_described_as_ending_with_the_run(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let (window, cx) = open_settings(
        Some(OfferedMachine {
            connection: Ok(Arc::new(super::DevelopmentRunCore(
                Arc::clone(&core) as Arc<dyn LocalCoreControl>
            )) as Arc<dyn LocalCoreControl>),
            service: None,
            update: None,
            command_line_tool: None,
        }),
        cx,
    );
    click(NAVIGATION_SELECTOR, cx);
    let (description, presentation) = window.read_with(cx, |settings, cx| {
        (
            settings.section_description(SettingsSectionId::Machines, cx),
            settings.local_core_presentation(),
        )
    });
    assert!(
        description.contains("SpaceTerm Core ends when the run ends"),
        "got {description:?}"
    );
    assert!(!description.contains("keep running after SpaceTerm quits"));
    let presentation = presentation.expect("the local server is presented");
    assert_eq!(presentation.state, "Connected");
    assert_eq!(
        presentation.explanation.as_ref(),
        "Connected to the SpaceTerm Core this Development run owns. It ends with its Terminal Sessions \
         when the run ends. Protocol version 2."
    );
    assert!(!rendered(SERVICE_ROW_SELECTOR, cx), "no service runs it");

    let (window, cx) = open_settings(offered(&core), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert_eq!(
        window.read_with(cx, |settings, cx| settings
            .section_description(SettingsSectionId::Machines, cx)),
        "Machines run Workspaces and Terminal Sessions."
    );
}

#[gpui::test]
fn opening_settings_asks_the_local_machine_nothing(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Err(CoreError::NotSetUp));
    let (window, cx) = open_settings(offered(&core), cx);

    assert_eq!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Interface
    );
    assert!(rendered(NAVIGATION_SELECTOR, cx));
    assert_eq!(core.refreshes(), 0);
    assert!(matches!(
        window.read_with(cx, |settings, _| settings
            .local_core
            .as_ref()
            .map(|row| row.current_status())),
        Some(LocalCoreStatus::NotChecked)
    ));
}

#[gpui::test]
fn presenting_machines_checks_the_connection_without_setting_up(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Err(CoreError::NotSetUp));
    let (window, cx) = open_settings(offered(&core), cx);

    click(NAVIGATION_SELECTOR, cx);

    assert_eq!(core.refreshes(), 1);
    for selector in ["settings-section-servers", ROW_SELECTOR, STATUS_SELECTOR] {
        assert!(rendered(selector, cx), "{selector} should render");
    }
    assert_eq!(status(&window, cx), "Not installed");
    let row = cx.debug_bounds(ROW_SELECTOR).expect("row bounds");
    let control = cx.debug_bounds(STATUS_SELECTOR).expect("control bounds");
    assert!(row.contains(&control.origin) && control.right() <= row.right());
    assert_eq!(presentation(&window, cx).tone, CaptionTone::Guidance);

    // Choosing the presented section again reads nothing more.
    click(NAVIGATION_SELECTOR, cx);
    assert_eq!(core.refreshes(), 1);

    // Git shows the same server picker, so moving between them reads nothing more.
    click("settings-navigation-settings-section-git", cx);
    click(NAVIGATION_SELECTOR, cx);
    assert_eq!(core.refreshes(), 1);

    // Returning from a section without the picker checks again.
    click("settings-navigation-settings-section-interface", cx);
    click(NAVIGATION_SELECTOR, cx);
    assert_eq!(core.refreshes(), 2);
}

#[gpui::test]
fn check_now_shows_the_authenticated_status(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Err(CoreError::NotRunning));
    let (window, cx) = open_settings(offered(&core), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert_eq!(status(&window, cx), "Not running");
    core.answer_refresh(Ok(connected(2)));

    choose(SERVICE_CHECK_SELECTOR, cx);

    assert_eq!(core.refreshes(), 2);
    assert_eq!(status(&window, cx), "Running");
    let presented = presentation(&window, cx);
    assert_eq!(presented.state, "Connected");
    assert!(presented.explanation.contains("Protocol version 2"));
}

#[gpui::test]
fn one_check_runs_at_a_time(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Err(CoreError::NotSetUp));
    let (window, cx) = open_settings(offered(&core), cx);
    click(NAVIGATION_SELECTOR, cx);
    cx.run_until_parked();
    let checks = core.refreshes();

    window.update(cx, |settings, cx| {
        settings.check_local_core(cx);
        assert!(
            settings
                .local_core_presentation()
                .is_some_and(|presented| presented.working)
        );
        settings.check_local_core(cx);
    });
    cx.run_until_parked();

    assert_eq!(core.refreshes(), checks + 1);
    assert!(!presentation(&window, cx).working);
}

/// Draws the window without running any background work, so work underway stays underway.
fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

/// A check keeps the state in place, and says Checking in the state's place only once it runs
/// longer than a second, without moving the row. Check Now waits on SpaceTerm's own update
/// check, which the test holds open across the delay.
#[gpui::test]
fn a_check_says_checking_only_once_it_runs_longer_than_a_second(cx: &mut TestAppContext) {
    let adapter = Rc::new(crate::updates::testing::RecordingAdapter::available());
    cx.update(|cx| crate::updates::ApplicationUpdates::install(adapter.clone(), cx));
    let core = ScriptedCore::new(Err(CoreError::NotSetUp));
    let (window, cx) = open_settings(offered(&core), cx);
    click(NAVIGATION_SELECTOR, cx);
    let settled = core_page_layout(cx);
    assert!(!rendered("settings-server-status-progress-track", cx));

    choose(super::super::machine_card::VERSION_CHECK_SELECTOR, cx);
    assert!(!rendered("settings-server-status-progress-track", cx));
    assert_eq!(status(&window, cx), "Not installed");

    cx.executor()
        .advance_clock(std::time::Duration::from_millis(1_100));
    draw(cx);
    assert!(rendered("settings-server-status-progress-track", cx));
    assert_eq!(core_page_layout(cx), settled, "Checking moves nothing");

    adapter.emit(crate::updates::UpdateEvent::UpToDate);
    adapter.emit(crate::updates::UpdateEvent::Finished);
    for _ in 0..5 {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(250));
        cx.run_until_parked();
    }
    assert!(!rendered("settings-server-status-progress-track", cx));
    assert_eq!(core_page_layout(cx), settled);
}

#[gpui::test]
fn settings_search_reveals_the_local_machine_and_checks_it_once(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let (window, cx) = open_settings(offered(&core), cx);

    for query in ["m", "ma", "mac", "mach", "machi", "machine"] {
        search(&window, query, cx);
    }

    assert_eq!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Machines
    );
    assert_eq!(core.refreshes(), 1);
    assert_eq!(status(&window, cx), "Running");
}

#[gpui::test]
fn an_unresolved_core_explains_itself_and_offers_nothing(cx: &mut TestAppContext) {
    let (window, cx) = open_settings(
        Some(OfferedMachine {
            connection: Err(CoreError::StorageUnavailable),
            service: None,
            update: None,
            command_line_tool: None,
        }),
        cx,
    );

    click(NAVIGATION_SELECTOR, cx);

    assert_eq!(status(&window, cx), "Unavailable");
    let presented = presentation(&window, cx);
    assert!(!presented.check);
    assert_eq!(
        presented.explanation.as_ref(),
        CoreError::StorageUnavailable.to_string()
    );
    // Only Check Now remains, which checks SpaceTerm itself and connects to nothing.
    click(MENU_SELECTOR, cx);
    assert!(
        !rendered(SERVICE_CHECK_SELECTOR, cx),
        "checking resolves nothing"
    );
}

#[gpui::test]
fn a_build_without_a_local_machine_omits_the_machines_section(cx: &mut TestAppContext) {
    let (window, cx) = open_settings(None, cx);

    assert!(!rendered(NAVIGATION_SELECTOR, cx));
    search(&window, "account", cx);
    assert_ne!(
        window.read_with(cx, |settings, _| settings.active_section),
        SettingsSectionId::Machines
    );
    assert!(!rendered(ROW_SELECTOR, cx));
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
            "Not installed",
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
            "Not running",
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
fn installing_promises_to_start_the_core_only_when_the_person_has_not_stopped_it() {
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
        "Version 1.4.0 (commit aaaaaaa) is verified and ready to install. SpaceTerm Core then runs \
         for your account whenever you\u{2019}re logged in, even after SpaceTerm quits. No \
         administrator password is needed."
    );
    assert_eq!(
        presented(not_installed(), several(), None)
            .explanation
            .as_ref(),
        "Choose a verified SpaceTerm Core version from Install. It runs for your account whenever \
         you\u{2019}re logged in, even after SpaceTerm quits. No administrator password is \
         needed."
    );
    let installing = presented(not_installed(), one(), Some(ServiceAction::Install));
    assert_eq!(installing.state, "Installing");
    assert_eq!(
        installing.explanation.as_ref(),
        "Installing and starting SpaceTerm Core\u{2026}"
    );

    // A Stop the person chose outlasts Uninstall, and installing again keeps it.
    for (installations, explanation) in [
        (
            one(),
            "Version 1.4.0 (commit aaaaaaa) is verified and ready to install. You stopped SpaceTerm \
             Core. Installing keeps it stopped until you start it. No administrator password \
             is needed.",
        ),
        (
            several(),
            "Choose a verified SpaceTerm Core version from Install. You stopped SpaceTerm Core. Installing \
             keeps it stopped until you start it. No administrator password is needed.",
        ),
    ] {
        let presentation = presented(uninstalled(), installations, None);
        assert_eq!(presentation.state, "Not installed");
        assert_eq!(presentation.explanation.as_ref(), explanation);
        assert_eq!(presentation.tone, CaptionTone::Guidance);
        assert_eq!(presentation.actions, vec![ServiceAction::Install]);
    }
    let installing = presented(uninstalled(), one(), Some(ServiceAction::Install));
    assert_eq!(installing.state, "Installing");
    assert_eq!(
        installing.explanation.as_ref(),
        "Installing SpaceTerm Core without starting it\u{2026}"
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
        failure: Some(CoreError::CleanupUnresolved),
        ..ServiceState::default()
    });

    assert_eq!(presentation.state, "Running");
    assert_eq!(presentation.tone, CaptionTone::Error);
    assert_eq!(
        presentation.explanation.as_ref(),
        CoreError::CleanupUnresolved.to_string()
    );
    assert_eq!(presentation.actions[0], ServiceAction::Stop);
}

#[test]
fn a_stop_whose_cleanup_was_not_confirmed_offers_only_another_stop() {
    let presentation = present_service(ServiceState {
        status: None,
        failure: Some(CoreError::CleanupUnresolved),
        ..ServiceState::default()
    });

    assert_eq!(presentation.state, "Cleanup unresolved");
    // Retrying Stop never restarts the server, so Start and Restart stay unavailable.
    assert_eq!(
        presentation.actions,
        vec![ServiceAction::Stop, ServiceAction::Check]
    );
}

#[test]
fn an_unreadable_service_offers_only_another_check() {
    let presentation = present_service(ServiceState {
        status: None,
        failure: Some(CoreError::ServiceUnavailable),
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
        let core = crate::machine_connection::service::control::this_computer();
        let question = match action {
            ServiceAction::Stop => format!("Stop SpaceTerm Core on {core}?"),
            ServiceAction::Restart => format!("Restart SpaceTerm Core on {core}?"),
            _ => unreachable!(),
        };
        assert_eq!(confirmation.message.as_ref(), question);
        let kept = match action {
            ServiceAction::Stop => {
                "Workspaces are kept, and SpaceTerm Core stays stopped until you start it again."
            }
            ServiceAction::Restart => {
                "Workspaces are kept, and each Pane starts a new shell when you use it."
            }
            _ => unreachable!(),
        };
        assert_eq!(
            confirmation.detail.as_ref(),
            format!("All Terminal Sessions on it end for every client. {kept}")
        );
        // The menu entry ends in an ellipsis because it asks first; the confirmation does not.
        assert_eq!(
            confirmation.confirm,
            action.label().trim_end_matches('\u{2026}')
        );
    }
    // Uninstalling a server that is not running ends no Terminal Session.
    let idle =
        ServiceConfirmation::for_action(ServiceAction::Uninstall, false, login).expect("idle");
    let live =
        ServiceConfirmation::for_action(ServiceAction::Uninstall, true, login).expect("live");
    assert!(!idle.message.contains("Terminal Sessions"));
    assert_eq!(
        idle.detail.as_ref(),
        "It no longer starts when you log in. Workspaces are kept, so you can install it again \
         later."
    );
    assert_eq!(
        live.detail.as_ref(),
        "All Terminal Sessions on it end for every client. Workspaces are kept, so you can \
         install it again later."
    );
}

#[gpui::test]
fn presenting_machines_reads_the_service_without_changing_it(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Err(CoreError::NotSetUp));
    let service = ScriptedService::new(Ok(not_installed()));
    let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
    assert!(service.calls().is_empty(), "opening Settings reads nothing");

    click(NAVIGATION_SELECTOR, cx);

    // Listing verified builds is a read: it installs and starts nothing.
    assert_eq!(
        service.calls(),
        vec![ServiceCall::Status, ServiceCall::RetainedInstallations]
    );
    assert!(service.changes().is_empty());
    for selector in [ROW_SELECTOR, STATUS_SELECTOR] {
        assert!(rendered(selector, cx), "{selector} should render");
    }
    assert!(row_offers(INSTALL_SELECTOR, cx), "Install is offered");
    assert_eq!(status(&window, cx), "Not installed");
    let row = cx.debug_bounds(ROW_SELECTOR).expect("row bounds");
    let control = cx.debug_bounds(STATUS_SELECTOR).expect("control bounds");
    assert!(row.contains(&control.origin) && control.right() <= row.right());
    assert_eq!(
        service_presentation(&window, cx).actions,
        vec![ServiceAction::Install]
    );
}

#[gpui::test]
fn a_platform_without_a_service_omits_its_row(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Err(CoreError::NotRunning));
    let (window, cx) = open_settings(offered(&core), cx);

    click(NAVIGATION_SELECTOR, cx);

    assert!(!rendered(SERVICE_ROW_SELECTOR, cx));
    assert!(window.read_with(cx, |settings, _| {
        settings.core_service_presentation().is_none()
    }));
    search(&window, "launch agent", cx);
    assert!(!rendered(SERVICE_ROW_SELECTOR, cx));
}

#[test]
fn install_names_each_verified_build_by_version_and_commit() {
    let first = retained("1.4.0", 'a');
    let second = retained("1.5.0", 'b');
    let choices = InstallChoice::of(&[first.clone(), second]);
    let labels: Vec<_> = choices.iter().map(|choice| choice.label.as_ref()).collect();
    assert_eq!(labels, ["1.4.0 (aaaaaaa)", "1.5.0 (bbbbbbb)"]);

    // Two packages of one version and commit also name their package, so they read apart.
    let mut rebuilt = first.clone();
    rebuilt.directory = rebuilt.directory.with_file_name("c".repeat(64));
    let choices = InstallChoice::of(&[first, rebuilt]);
    let labels: Vec<_> = choices.iter().map(|choice| choice.label.as_ref()).collect();
    assert_eq!(
        labels,
        ["1.4.0 (aaaaaaa, aaaaaaaa)", "1.4.0 (aaaaaaa, cccccccc)",]
    );
}

#[test]
fn without_a_verified_build_the_row_explains_how_to_keep_one_and_offers_another_check() {
    let presentation = present_service(ServiceState {
        status: Some(not_installed()),
        ..ServiceState::default()
    });

    assert_eq!(presentation.state, "Not installed");
    assert_eq!(presentation.actions, vec![ServiceAction::Check]);
    assert!(presentation.install_choices.is_empty());
    assert_eq!(presentation.tone, CaptionTone::Guidance);
    assert!(
        presentation
            .explanation
            .contains("Use the SpaceTerm Core installer command to keep a signed package")
    );
}

#[gpui::test]
fn one_verified_build_installs_from_install_and_checks_the_connection(cx: &mut TestAppContext) {
    let (window, core, service, cx) = open_machines_with_service(Ok(not_installed()), cx);
    let build = retained("1.4.0", 'a');
    let presented = service_presentation(&window, cx);
    assert!(
        presented
            .explanation
            .starts_with("Version 1.4.0 (commit aaaaaaa) is verified and ready to install.")
    );
    let checks = core.refreshes();

    click(INSTALL_SELECTOR, cx);
    assert!(
        service.changes().is_empty(),
        "Install says what installing does first"
    );
    click_modal(INSTALL_CONFIRM_SELECTOR, cx);

    assert!(!cx.did_prompt_for_paths(), "Install asks for no folder");
    assert_eq!(
        service.changes(),
        vec![ServiceCall::Install(build.directory)]
    );
    assert_eq!(status(&window, cx), "Running");
    assert_eq!(service_presentation(&window, cx).state, "Running");
    assert_eq!(core.refreshes(), checks + 1);
}

#[gpui::test]
fn several_verified_builds_are_chosen_from_installs_menu(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let service = ScriptedService::new(Ok(not_installed()));
    let chosen = retained("1.5.0", 'b');
    service.keep(Ok(vec![retained("1.4.0", 'a'), chosen.clone()]));
    let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert!(
        service_presentation(&window, cx)
            .explanation
            .starts_with("Choose a verified SpaceTerm Core version from Install.")
    );

    click(MENU_SELECTOR, cx);
    assert!(
        service.changes().is_empty(),
        "opening the menu installs nothing"
    );
    assert!(!cx.did_prompt_for_paths(), "Install asks for no folder");
    let second = Box::leak(format!("{INSTALL_CHOICE_SELECTOR}-1").into_boxed_str());
    click(second, cx);
    click_modal(INSTALL_CONFIRM_SELECTOR, cx);

    assert_eq!(
        service.changes(),
        vec![ServiceCall::Install(chosen.directory)]
    );
    assert_eq!(service_presentation(&window, cx).state, "Running");
}

#[gpui::test]
fn a_build_kept_after_the_row_was_read_is_offered_after_another_check(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let service = ScriptedService::new(Ok(not_installed()));
    service.keep(Ok(Vec::new()));
    let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert!(!row_offers(INSTALL_SELECTOR, cx));
    assert_eq!(
        service_presentation(&window, cx).actions,
        vec![ServiceAction::Check]
    );

    service.keep(Ok(vec![retained("1.4.0", 'a')]));
    choose(SERVICE_CHECK_SELECTOR, cx);

    assert!(row_offers(INSTALL_SELECTOR, cx));
    assert!(service.changes().is_empty());
}

#[gpui::test]
fn an_unreadable_build_store_explains_itself_and_offers_another_check(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let service = ScriptedService::new(Ok(not_installed()));
    service.keep(Err(CoreError::StorageUnavailable));
    let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
    click(NAVIGATION_SELECTOR, cx);

    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Not installed");
    assert_eq!(presented.tone, CaptionTone::Error);
    assert_eq!(
        presented.explanation.as_ref(),
        CoreError::StorageUnavailable.to_string()
    );
    assert_eq!(presented.actions, vec![ServiceAction::Check]);
}

#[gpui::test]
fn an_installed_service_lists_no_builds(cx: &mut TestAppContext) {
    let (_window, _core, service, _cx) = open_machines_with_service(Ok(running()), cx);

    assert_eq!(service.calls(), vec![ServiceCall::Status]);
}

#[gpui::test]
fn only_a_listed_build_is_installed(cx: &mut TestAppContext) {
    let (window, _core, service, cx) = open_machines_with_service(Ok(not_installed()), cx);

    window.update(cx, |settings, cx| {
        settings.install_core_service(PathBuf::from("/elsewhere"), cx);
    });
    cx.run_until_parked();

    assert!(service.changes().is_empty());
}

#[gpui::test]
fn a_refused_installation_explains_itself_and_offers_install_again(cx: &mut TestAppContext) {
    let (window, _core, service, cx) = open_machines_with_service(Ok(not_installed()), cx);
    service.refuse_next(CoreError::RuntimeUnavailable);

    install(cx);

    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Not installed");
    assert_eq!(presented.tone, CaptionTone::Error);
    assert_eq!(
        presented.explanation.as_ref(),
        CoreError::RuntimeUnavailable.to_string()
    );
    assert_eq!(presented.actions, vec![ServiceAction::Install]);
}

/// A Start from the status item reaches a Settings window already open, which reads the service
/// again rather than keep showing the server stopped.
#[gpui::test]
fn a_start_from_the_status_item_shows_the_machine_running_in_open_settings(
    cx: &mut TestAppContext,
) {
    let (window, core, service, cx) = open_machines_with_service(Ok(stopped()), cx);
    core.answer_refresh(Err(CoreError::Stopped));
    assert_eq!(status(&window, cx), "Stopped");

    // The status item started the server, outside Settings.
    service.report(Ok(running()));
    core.answer_refresh(Ok(connected(1)));
    cx.update(|_, cx| crate::status_menu::operation_succeeded(cx, running()));
    cx.run_until_parked();

    assert_eq!(status(&window, cx), "Running");
}

/// A read Settings began before the status item started the server answers what it saw then.
/// Settings sets that answer aside and reads again, so it shows the server running rather than
/// stopped.
#[gpui::test]
fn a_read_begun_before_a_status_item_start_doesnt_leave_settings_stopped(cx: &mut TestAppContext) {
    let (window, _core, service, cx) = open_machines_with_service(Ok(stopped()), cx);
    assert_eq!(status(&window, cx), "Stopped");

    // Settings begins a read, which observes the stopped server before the start.
    service.read_before_change();
    window.update(cx, |settings, cx| settings.check_core_service(cx));
    // The status item starts the server while that read is still pending.
    service.report(Ok(running()));
    cx.update(|_, cx| crate::status_menu::operation_succeeded(cx, running()));
    cx.run_until_parked();

    assert_eq!(status(&window, cx), "Running");
}

/// Check Now reads the service again, so a start made where Settings didn't hear of it shows.
#[gpui::test]
fn check_now_reads_the_service_again(cx: &mut TestAppContext) {
    use crate::ui::settings_window::machine_card::VERSION_CHECK_SELECTOR;

    let (window, _core, service, cx) = open_machines_with_service(Ok(stopped()), cx);
    assert_eq!(status(&window, cx), "Stopped");

    service.report(Ok(running()));
    click(VERSION_CHECK_SELECTOR, cx);
    cx.run_until_parked();

    assert_eq!(status(&window, cx), "Running");
}

#[gpui::test]
fn a_confirmed_stop_stays_stopped_and_offers_start(cx: &mut TestAppContext) {
    let (window, core, service, cx) = open_machines_with_service(Ok(running()), cx);
    core.answer_refresh(Err(CoreError::Stopped));

    choose(STOP_SELECTOR, cx);
    assert!(alert_open(STOP_CONFIRM_SELECTOR, cx));
    assert!(service.changes().is_empty(), "Stop waits for confirmation");
    click_modal(STOP_CANCEL_SELECTOR, cx);
    assert!(service.changes().is_empty());
    assert_eq!(service_presentation(&window, cx).state, "Running");
    choose(STOP_SELECTOR, cx);
    click_modal(STOP_CONFIRM_SELECTOR, cx);

    assert_eq!(service.changes(), vec![ServiceCall::Stop]);
    assert_eq!(status(&window, cx), "Stopped");
    assert!(rendered(START_SERVICE_SELECTOR, cx));
    let presented = service_presentation(&window, cx);
    assert_eq!(
        presented.actions,
        vec![ServiceAction::Start, ServiceAction::Uninstall]
    );
    // The connection is checked afresh and offers no step that could reverse the Stop.
    assert!(matches!(
        window.read_with(cx, |settings, _| settings
            .local_core
            .as_ref()
            .map(|row| row.current_status())),
        Some(LocalCoreStatus::Failed(CoreError::Stopped))
    ));
    assert!(!presentation(&window, cx).check);
}

#[gpui::test]
fn start_restores_a_stopped_core_without_asking(cx: &mut TestAppContext) {
    let (window, core, service, cx) = open_machines_with_service(Ok(stopped()), cx);
    let checks = core.refreshes();

    click(START_SERVICE_SELECTOR, cx);

    assert_eq!(service.changes(), vec![ServiceCall::Start]);
    assert_eq!(service_presentation(&window, cx).state, "Running");
    assert_eq!(core.refreshes(), checks + 1);
    assert_eq!(status(&window, cx), "Running");
}

#[gpui::test]
fn restart_asks_first_then_restarts(cx: &mut TestAppContext) {
    let (window, _core, service, cx) = open_machines_with_service(Ok(running()), cx);

    choose(RESTART_SELECTOR, cx);
    assert!(alert_open(RESTART_CONFIRM_SELECTOR, cx));
    click_modal(RESTART_CANCEL_SELECTOR, cx);
    assert!(service.changes().is_empty());

    choose(RESTART_SELECTOR, cx);
    click_modal(RESTART_CONFIRM_SELECTOR, cx);

    assert_eq!(service.changes(), vec![ServiceCall::Restart]);
    assert_eq!(service_presentation(&window, cx).state, "Running");
}

#[gpui::test]
fn uninstall_asks_first_then_offers_install_again(cx: &mut TestAppContext) {
    let (window, _core, service, cx) = open_machines_with_service(Ok(stopped()), cx);

    choose(UNINSTALL_SELECTOR, cx);
    assert!(alert_open(UNINSTALL_CONFIRM_SELECTOR, cx));
    click_modal(UNINSTALL_CANCEL_SELECTOR, cx);
    assert!(service.changes().is_empty());

    choose(UNINSTALL_SELECTOR, cx);
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
        settings.install_core_service(build.directory.clone(), cx);
    });
    assert_eq!(
        service_presentation(&window, cx).explanation.as_ref(),
        "Installing SpaceTerm Core without starting it\u{2026}"
    );
    cx.run_until_parked();

    assert_eq!(
        service.changes(),
        vec![
            ServiceCall::Uninstall,
            ServiceCall::Install(build.directory)
        ]
    );
    assert_eq!(status(&window, cx), "Stopped");
    assert_eq!(
        service_presentation(&window, cx).actions,
        vec![ServiceAction::Start, ServiceAction::Uninstall]
    );
}

#[gpui::test]
fn a_service_change_waits_for_the_connection_and_the_reverse(cx: &mut TestAppContext) {
    let (window, core, service, cx) = open_machines_with_service(Ok(stopped()), cx);
    cx.run_until_parked();
    let checks = core.refreshes();
    let connection = core.hold_refresh();
    window.update(cx, |settings, cx| settings.check_local_core(cx));
    cx.run_until_parked();
    connection.wait_started(&cx.cx);
    window.update(cx, |settings, cx| {
        settings.run_core_service(ServiceAction::Start, cx)
    });
    cx.run_until_parked();
    assert!(
        service.changes().is_empty(),
        "a held connection read excludes Start"
    );
    assert_eq!(core.refreshes(), checks + 1);
    connection.release(Err(CoreError::Stopped));
    cx.run_until_parked();
    assert!(service.changes().is_empty());

    let start = service.hold_start();
    window.update(cx, |settings, cx| {
        settings.run_core_service(ServiceAction::Start, cx)
    });
    cx.run_until_parked();
    start.wait_started(&cx.cx);
    assert_eq!(service.changes(), [ServiceCall::Start]);
    let checks = core.refreshes();
    window.update(cx, |settings, cx| settings.check_local_core(cx));
    cx.run_until_parked();
    assert_eq!(
        core.refreshes(),
        checks,
        "a held Start excludes a connection read"
    );
    core.answer_refresh(Ok(connected(5)));
    start.release(Ok(running()));
    cx.run_until_parked();
    assert_eq!(
        core.refreshes(),
        checks + 1,
        "completion reads the new connection"
    );
    assert_eq!(status(&window, cx), "Running");
}

/// A step underway keeps the state and doesn't name itself before it has run a second. The test
/// scheduler runs every runnable task before it advances its clock, so a local step can't be held
/// past the delay here.
#[gpui::test]
fn a_service_step_keeps_the_state_and_waits_before_naming_itself(cx: &mut TestAppContext) {
    let (window, _core, _service, cx) = open_machines_with_service(Ok(stopped()), cx);
    assert!(!rendered(SUBTITLE_PROGRESS, cx));

    window.update(cx, |settings, cx| {
        settings.run_core_service(ServiceAction::Start, cx);
    });
    draw(cx);
    assert!(!rendered(SUBTITLE_PROGRESS, cx));
    assert!(!rendered("settings-server-status-progress-track", cx));
    assert_eq!(status(&window, cx), "Stopped");
    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Starting");
    assert_eq!(presented.working, Some(ServiceAction::Start));

    cx.run_until_parked();
    assert_eq!(status(&window, cx), "Running");
    assert!(!rendered(SUBTITLE_PROGRESS, cx));
}

#[gpui::test]
fn an_unresolved_service_explains_itself_and_offers_nothing(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let (window, cx) = open_settings(
        Some(OfferedMachine {
            connection: Ok(Arc::clone(&core) as Arc<dyn LocalCoreControl>),
            service: Some(Err(CoreError::StorageUnavailable)),
            update: None,
            command_line_tool: None,
        }),
        cx,
    );

    click(NAVIGATION_SELECTOR, cx);

    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Unknown");
    assert!(presented.actions.is_empty());
    for selector in [SERVICE_CHECK_SELECTOR, INSTALL_SELECTOR] {
        assert!(
            !row_offers(selector, cx),
            "{selector} should not be offered"
        );
    }
}

#[gpui::test]
fn settings_search_reaches_the_service_by_its_startup_names(cx: &mut TestAppContext) {
    for query in ["launch agent", "systemd", "linger"] {
        let core = ScriptedCore::new(Ok(connected(2)));
        let service = if query == "launch agent" {
            ScriptedService::new(Ok(running()))
        } else {
            ScriptedService::with_boot_startup(Ok(running()))
        };
        let (window, cx) = open_settings(offered_with_service(&core, &service), cx);

        search(&window, query, cx);

        assert_eq!(
            window.read_with(cx, |settings, _| settings.active_section),
            SettingsSectionId::Machines,
            "{query}"
        );
        assert_eq!(status(&window, cx), "Running", "{query}");
        assert!(rendered(SERVICE_ROW_SELECTOR, cx), "{query}");
        assert!(service.changes().is_empty(), "{query}");
    }
}

#[gpui::test]
fn assistive_technology_hears_the_core_state_and_stop_asks_first(cx: &mut TestAppContext) {
    use gpui::accesskit::Action;
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    let (window, _core, service, cx) = open_machines_with_service(Ok(running()), cx);
    let tree = A11yTree::read(cx);
    assert!(
        tree.with_role("Status")
            .iter()
            .any(|status| status["aria"]["label"]
                .as_str()
                .is_some_and(|label| label.ends_with(": Running"))),
        "the Status row announces the server and its state"
    );

    // Stop lives in the Status menu. Choosing it from assistive technology asks first, with
    // Cancel focused, so Escape keeps the server running.
    click(MENU_SELECTOR, cx);
    let tree = A11yTree::read(cx);
    perform(cx, tree.node("Stop SpaceTerm Core\u{2026}"), Action::Click);
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
    assert_eq!(status(&window, cx), "Running");
}

#[gpui::test]
fn assistive_technology_hears_which_build_install_installs(cx: &mut TestAppContext) {
    use spaceterm_ui::a11y_testing::A11yTree;

    let (_window, _core, _service, cx) = open_machines_with_service(Ok(not_installed()), cx);
    click(MENU_SELECTOR, cx);

    assert!(
        A11yTree::read(cx)
            .find("Install 1.4.0 (aaaaaaa)…")
            .is_some(),
        "the menu names the build Install installs"
    );
}

#[gpui::test]
fn the_status_menu_offers_stopping_only_a_running_machine(cx: &mut TestAppContext) {
    let (_window, _core, _service, cx) = open_machines_with_service(Ok(stopped()), cx);

    click(MENU_SELECTOR, cx);
    let tree = spaceterm_ui::a11y_testing::A11yTree::read(cx);

    assert_eq!(
        tree.node("Stop SpaceTerm Core\u{2026}")["aria"]["disabled"],
        true
    );
    assert_eq!(
        tree.node("Restart SpaceTerm Core\u{2026}")["aria"]["disabled"],
        true
    );
    assert_ne!(
        tree.node("Uninstall SpaceTerm Core\u{2026}")["aria"]["disabled"],
        true
    );
}

#[test]
fn boot_wording_names_boot_and_logout_and_keeps_lingering_after_uninstall() {
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
        "SpaceTerm Core keeps running after SpaceTerm quits. While lingering stays on for your \
         account, it also starts when the computer starts and keeps running after you log out."
    );
    // Installing may need authorization for boot startup, and SpaceTerm says it asks first.
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
    assert_eq!(
        stop.detail.as_ref(),
        "All Terminal Sessions on it end for every client. Workspaces are kept, and SpaceTerm Core \
         stays stopped until you start it again."
    );
    let uninstall =
        ServiceConfirmation::for_action(ServiceAction::Uninstall, false, boot).expect("uninstall");
    assert_eq!(
        uninstall.detail.as_ref(),
        "It no longer starts when the computer starts. Workspaces are kept, so you can install it \
         again later."
    );
}

#[test]
fn the_authorization_prompt_names_the_required_permission_and_matching_action() {
    let install = AuthorizationPrompt::for_request(
        &AuthorizedRequest::Install(retained("1.4.0", 'a').directory),
        Some("Version 1.4.0 (commit aaaaaaa)"),
    );
    assert_eq!(install.confirm, "Authorize and Install");
    assert_eq!(
        install.message,
        "Enter an administrator password so your account\u{2019}s services keep running after you log out."
    );
    assert_eq!(
        install.detail,
        "This applies to every service your account runs and stays on if you uninstall SpaceTerm Core."
    );
    let start = AuthorizationPrompt::for_request(&AuthorizedRequest::Start, None);
    assert_eq!(start.confirm, "Authorize and Start");
    assert_eq!(start.message, install.message);
    assert_eq!(start.detail, install.detail);
}

#[gpui::test]
fn a_boot_installation_waits_for_explained_authorization_then_installs_its_build(
    cx: &mut TestAppContext,
) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let service = ScriptedService::with_boot_startup(Ok(not_installed()));
    let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    let build = retained("1.4.0", 'a').directory;

    install(cx);

    // The refusal asks the operating system nothing by itself.
    assert_eq!(service.changes(), vec![ServiceCall::Install(build.clone())]);
    assert_eq!(status(&window, cx), "Needs authorization");
    let presented = service_presentation(&window, cx);
    assert_eq!(presented.state, "Authorization required");
    assert_eq!(presented.tone, CaptionTone::Warning);
    assert_eq!(
        presented.explanation.as_ref(),
        "Choose Authorize and Install to run SpaceTerm Core at startup."
    );
    assert_eq!(presented.actions, vec![ServiceAction::AuthorizeInstall]);

    click(AUTHORIZE_INSTALL_SELECTOR, cx);
    assert!(alert_open(AUTHORIZE_CONFIRM_SELECTOR, cx));
    assert_eq!(service.changes().len(), 1, "the explanation asks nothing");
    let checks = core.refreshes();

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
    assert_eq!(status(&window, cx), "Running");
    let presented = service_presentation(&window, cx);
    assert_eq!(presented.authorization, None);
    assert!(
        presented
            .explanation
            .contains("keeps running after you log out")
    );
    assert_eq!(core.refreshes(), checks + 1);
}

#[gpui::test]
fn cancelling_the_operating_systems_prompt_changes_nothing_and_keeps_the_stop(
    cx: &mut TestAppContext,
) {
    let core = ScriptedCore::new(Err(CoreError::Stopped));
    let service = ScriptedService::with_boot_startup(Ok(stopped()));
    let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
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
    assert_eq!(presented.state, "Authorization required");
    assert_eq!(
        presented.explanation.as_ref(),
        "Authorization didn\u{2019}t finish. Choose Authorize and Start to try again."
    );
    assert_eq!(
        presented.actions,
        vec![ServiceAction::AuthorizeStart, ServiceAction::Uninstall]
    );
    assert!(!presentation(&window, cx).check);

    // Presenting the section again only reads. The Stop stands until an explicit Start.
    click("settings-navigation-settings-section-interface", cx);
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
fn a_stopped_boot_service_reinstalls_stopped_and_only_start_asks_for_authorization(
    cx: &mut TestAppContext,
) {
    let (window, _core, service, cx) = {
        let core = ScriptedCore::new(Err(CoreError::Stopped));
        let service = ScriptedService::with_boot_startup(Ok(uninstalled()));
        let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
        click(NAVIGATION_SELECTOR, cx);
        (window, core, service, cx)
    };
    let build = retained("1.4.0", 'a').directory;

    install(cx);

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
fn a_boot_service_restart_refused_after_stopping_offers_to_authorize_and_start(
    cx: &mut TestAppContext,
) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let service = ScriptedService::with_boot_startup(Ok(running()));
    let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
    click(NAVIGATION_SELECTOR, cx);

    choose(RESTART_SELECTOR, cx);
    click_modal(RESTART_CONFIRM_SELECTOR, cx);

    assert_eq!(service.changes(), vec![ServiceCall::Restart]);
    let presented = service_presentation(&window, cx);
    assert_eq!(
        presented.explanation.as_ref(),
        "Choose Authorize and Start to run SpaceTerm Core at startup."
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

    let (window, _core, service, cx) = {
        let core = ScriptedCore::new(Ok(connected(2)));
        let service = ScriptedService::with_boot_startup(Ok(not_running()));
        let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
        click(NAVIGATION_SELECTOR, cx);
        (window, core, service, cx)
    };
    click(START_SERVICE_SELECTOR, cx);

    // The status announces why the server did not start.
    assert!(A11yTree::read(cx).with_role("Status").iter().any(|status| {
        status["aria"]["label"]
            .as_str()
            .is_some_and(|label| label.ends_with(": Needs authorization"))
    }));
    // The authorization step is offered from the Server row's ⋯ menu.
    click(MENU_SELECTOR, cx);
    let tree = A11yTree::read(cx);
    let authorize = tree.node("Authorize and Start\u{2026}");
    assert_ne!(authorize["aria"]["disabled"], true);

    // The explanation opens with Cancel focused, so Return and Escape ask nothing.
    perform(cx, authorize, Action::Click);
    cx.run_until_parked();
    assert!(alert_open(AUTHORIZE_CANCEL_SELECTOR, cx));
    let tree = A11yTree::read(cx);
    let prompt = tree.node("Run SpaceTerm Core at Startup?");
    assert_eq!(prompt["aria"]["role"], "AlertDialog");
    assert_eq!(
        prompt["aria"]["description"],
        "Enter an administrator password so your account\u{2019}s services keep running after you log out."
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

#[gpui::test]
fn an_admitted_start_finishes_after_settings_closes(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Ok(connected(1)));
    let service = ScriptedService::new(Ok(stopped()));
    let offered = offered_with_service(&core, &service);
    {
        let (settings, visual) = open_settings(offered.clone(), cx);
        settings.update(visual, |settings, cx| {
            settings.run_core_service(ServiceAction::Start, cx)
        });
        let released = settings.downgrade();
        drop(settings);
        visual.update(|window, _| window.remove_window());
        visual.run_until_parked();
        assert!(
            released.upgrade().is_none(),
            "completion must not retain Settings"
        );
    }
    assert_eq!(service.changes(), [ServiceCall::Start]);
    let (settings, visual) = open_settings(offered, cx);
    let presentation = service_presentation(&settings, visual);
    assert_eq!(presentation.state, "Running");
    assert_eq!(service.changes(), [ServiceCall::Start]);
}

#[gpui::test]
fn a_failed_admitted_start_survives_settings_reopening(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Ok(connected(1)));
    let service = ScriptedService::new(Ok(stopped()));
    service.refuse_next(CoreError::StartupFailed);
    let offered = offered_with_service(&core, &service);
    {
        let (settings, visual) = open_settings(offered.clone(), cx);
        settings.update(visual, |settings, cx| {
            settings.run_core_service(ServiceAction::Start, cx)
        });
        drop(settings);
        visual.update(|window, _| window.remove_window());
        visual.run_until_parked();
    }
    let (settings, visual) = open_settings(offered, cx);
    click(NAVIGATION_SELECTOR, visual);
    assert_eq!(
        settings.read_with(visual, |settings, _| settings
            .core_service
            .as_ref()
            .unwrap()
            .state
            .failure),
        Some(CoreError::StartupFailed)
    );
    assert_eq!(service.changes(), [ServiceCall::Start]);
}

#[gpui::test]
fn an_admitted_start_notifies_a_waiting_workspace_after_settings_closes(cx: &mut TestAppContext) {
    use crate::ui::workspace_manager::{
        WorkspaceManager, WorkspaceManagerAdapters, launch_testing,
    };

    let core = ScriptedCore::new(Ok(connected(1)));
    let service = ScriptedService::new(Ok(stopped()));
    let (settings, visual) = open_settings(offered_with_service(&core, &service), cx);
    let launch = Arc::new(launch_testing::ScriptedLaunch::default());
    launch.answer(Err(CoreError::Stopped));
    // The second observation verifies notification without opening a server connection.
    launch.answer(Err(CoreError::ServiceUnavailable));
    let mut local_core = launch_testing::unanswered();
    local_core.launch = launch.clone();
    // The Workspace Window's Updates control reads this computer's server through the host's
    // local update observation.
    visual
        .cx
        .update(crate::ui::updates::machine_rows::testing::install_up_to_date);
    let (_workspace, _) = visual.cx.add_window_view(|window, cx| {
        WorkspaceManager::new_with_adapters(
            std::env::temp_dir(),
            WorkspaceManagerAdapters {
                local_filesystem: crate::platform::local_filesystem::LocalFilesystemAuthority::testing(),
                key_input: Rc::new(crate::terminal::GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
                native_services: crate::terminal::native_services::testing::adapters(),
                lifecycle: crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(),
                directory_selection: Rc::new(crate::directory_selection::GpuiDirectorySelection),
                window_drag: Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
                local_core,
                core_directories: crate::ui::directory_picker::core_directory_testing::matching(),
            },
            crate::settings::sidebar::SidebarState::Expanded,
            window,
            cx,
        )
    });
    assert_eq!(launch.steps(), ["run"]);
    settings.update(visual, |settings, cx| {
        settings.run_core_service(ServiceAction::Start, cx)
    });
    drop(settings);
    visual.update(|window, _| window.remove_window());
    visual.run_until_parked();
    assert_eq!(service.changes(), [ServiceCall::Start]);
    assert_eq!(
        launch.steps(),
        ["run", "run"],
        "completion wakes the existing launch and never issues another Start"
    );
}

/// Whether the Start automatically switch reads as on, as assistive technology hears it.
fn starts_automatically(cx: &mut VisualTestContext) -> bool {
    spaceterm_ui::a11y_testing::A11yTree::read(cx)
        .with_role("Switch")
        .iter()
        .find(|switch| switch["aria"]["label"] == "Start automatically")
        .expect("the Start automatically switch is rendered")["aria"]["toggled"]
        == "True"
}

/// Start automatically owns its change until the change finishes. Leaving Servers and coming
/// back while the change is still queued asks for a passive read, which waits rather than taking
/// the change's place, so the switch shows the change's result and works again without reopening
/// Settings.
#[gpui::test]
fn a_passive_read_waits_for_an_automatic_start_change(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let service = ScriptedService::new(Ok(running()));
    let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert!(starts_automatically(cx));

    // The switch asks for the change, then the person visits Interface and returns to Servers,
    // all before the change's work runs.
    cx.update(|presenting, cx| {
        window.update(cx, |settings, cx| {
            settings.set_local_automatic_start(false, presenting, cx);
            settings.select_section(SettingsSectionId::Interface, cx);
            settings.select_section(SettingsSectionId::Machines, cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(
        service.changes(),
        [ServiceCall::SetAutomaticStart(false)],
        "the change runs to completion"
    );
    assert!(
        !starts_automatically(cx),
        "the switch shows the change's result"
    );

    click(AUTO_START_SELECTOR, cx);
    assert_eq!(
        service.changes(),
        [
            ServiceCall::SetAutomaticStart(false),
            ServiceCall::SetAutomaticStart(true)
        ],
        "the switch works again without reopening Settings"
    );
    assert!(starts_automatically(cx));
}

/// Flipping Start automatically while a read of the service runs moves the switch at once. The
/// change follows once the read confirms the service, and a read that can't moves the switch
/// back without changing anything.
#[gpui::test]
fn start_automatically_follows_a_read_that_confirms_the_service(cx: &mut TestAppContext) {
    let core = ScriptedCore::new(Ok(connected(2)));
    let service = ScriptedService::new(Ok(running()));
    let (window, cx) = open_settings(offered_with_service(&core, &service), cx);
    click(NAVIGATION_SELECTOR, cx);
    assert!(starts_automatically(cx));

    // What the switch shows, read without running any background work. Reading the
    // accessibility tree runs it.
    let shown = |cx: &mut VisualTestContext| {
        window.read_with(cx, |settings, _| {
            settings
                .core_service
                .as_ref()
                .and_then(|row| row.automatic_start_shown())
        })
    };

    cx.update(|presenting, cx| {
        window.update(cx, |settings, cx| {
            settings.check_core_service(cx);
            settings.set_local_automatic_start(false, presenting, cx);
        });
    });
    assert!(
        service.changes().is_empty(),
        "nothing changes before the read answers"
    );
    assert_eq!(shown(cx), Some(false), "the switch moves at once");
    cx.run_until_parked();
    assert_eq!(service.changes(), [ServiceCall::SetAutomaticStart(false)]);
    assert!(!starts_automatically(cx));

    service.report(Err(CoreError::ServiceUnavailable));
    cx.update(|presenting, cx| {
        window.update(cx, |settings, cx| {
            settings.check_core_service(cx);
            settings.set_local_automatic_start(true, presenting, cx);
        });
    });
    assert_eq!(shown(cx), Some(true), "the switch moves at once");
    cx.run_until_parked();
    assert_eq!(
        service.changes(),
        [ServiceCall::SetAutomaticStart(false)],
        "a read that couldn't confirm the service changes nothing"
    );
    assert!(!starts_automatically(cx), "the switch moves back");
}

/// Start Server chosen while returning to Servers reads the server waits for the reads, then
/// starts the server once, since the read still offers Start.
#[gpui::test]
fn start_machine_chosen_during_a_read_starts_once_the_read_offers_it(cx: &mut TestAppContext) {
    let (window, _core, service, cx) = open_machines_with_service(Ok(stopped()), cx);
    assert_eq!(status(&window, cx), "Stopped");

    cx.update(|presenting, cx| {
        window.update(cx, |settings, cx| {
            settings.select_section(SettingsSectionId::Interface, cx);
            settings.select_section(SettingsSectionId::Machines, cx);
            settings.request_core_service(ServiceAction::Start, presenting, cx);
        });
    });
    assert!(service.changes().is_empty(), "the read finishes first");

    cx.run_until_parked();
    assert_eq!(service.changes(), [ServiceCall::Start]);
    assert_eq!(status(&window, cx), "Running");
}

/// A read that finds the server already running drops the Start chosen during it.
#[gpui::test]
fn start_machine_chosen_during_a_read_that_finds_it_running_does_nothing(cx: &mut TestAppContext) {
    let (window, _core, service, cx) = open_machines_with_service(Ok(stopped()), cx);
    service.report(Ok(running()));

    cx.update(|presenting, cx| {
        window.update(cx, |settings, cx| {
            settings.check_core_service(cx);
            settings.request_core_service(ServiceAction::Start, presenting, cx);
        });
    });
    cx.run_until_parked();

    assert!(service.changes().is_empty());
    assert_eq!(status(&window, cx), "Running");
}

/// The Server row keeps its height whether it offers a button or not.
#[gpui::test]
fn the_machine_row_keeps_its_height_with_or_without_a_button(cx: &mut TestAppContext) {
    use crate::ui::settings_window::core_update::{UPDATE_SELECTOR, UpdateFacts};

    let (window, _core, update, cx) = open_machines_with_update(cx);
    cx.run_until_parked();
    assert!(
        rendered(UPDATE_SELECTOR, cx),
        "an available update offers Update"
    );
    let with_button = cx
        .debug_bounds(ROW_SELECTOR)
        .expect("the Server row renders");

    update.change(|facts: &mut UpdateFacts| {
        if let Ok(included) = &mut facts.included {
            included.approved = true;
        }
    });
    window.update(cx, |settings, cx| settings.check_core_update(cx));
    cx.run_until_parked();

    assert!(
        !rendered(UPDATE_SELECTOR, cx),
        "an up-to-date server offers no button"
    );
    assert_eq!(
        cx.debug_bounds(ROW_SELECTOR)
            .expect("the Server row renders")
            .size,
        with_button.size
    );
}

/// A read that succeeds makes the Start automatically change that waited on it, even while an
/// earlier operation's failure is still shown.
#[gpui::test]
fn a_successful_read_makes_a_waiting_change_after_an_earlier_failure(cx: &mut TestAppContext) {
    let (window, _core, service, cx) = open_machines_with_service(Ok(running()), cx);
    service.refuse_next(CoreError::ServiceUnavailable);
    choose(STOP_SELECTOR, cx);
    click_modal(STOP_CONFIRM_SELECTOR, cx);
    assert_eq!(service.changes(), [ServiceCall::Stop]);
    assert_eq!(
        service_presentation(&window, cx).tone,
        CaptionTone::Error,
        "the refused Stop explains itself"
    );

    cx.update(|presenting, cx| {
        window.update(cx, |settings, cx| {
            settings.check_core_service(cx);
            settings.set_local_automatic_start(false, presenting, cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(
        service.changes(),
        [ServiceCall::Stop, ServiceCall::SetAutomaticStart(false)],
        "the successful read lets the waiting change through"
    );
    assert!(!starts_automatically(cx));
}
