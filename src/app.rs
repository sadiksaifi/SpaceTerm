use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gpui::{
    App, AppContext, Bounds, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, px,
    size,
};

use crate::platform::app_directories::AppDirectoryEnvironment;
use crate::platform::app_paths::AppPaths;
use crate::platform::application_menu::{
    ApplicationMenuAdapter, ApplicationMenuCommand, ApplicationMenuError,
};
use crate::platform::application_quit::{
    ApplicationQuitAdapter, ApplicationQuitDecision, ApplicationQuitHandler,
    ApplicationQuitRequest, QuitAnswer, QuitButtons, QuitPrompt, StatusAreaName,
};
use crate::ssh::command::OpenSshExecutable;
use crate::ssh::host_config::HostConfigFilesystem;
use crate::ssh::process::SshProcessAdapter;
use crate::ssh::startup_environment::StartupSshEnvironment;
use crate::terminal::{NativeServiceOrigin, NativeServiceStatus, SelectionCopy};
use crate::ui::{NewWorkspace, SwitchWorkspace, WorkspaceManager};

struct NativeLocalUpdates;

impl crate::ui::updates::LocalUpdateObservation for NativeLocalUpdates {
    fn observe(&self) -> crate::ui::updates::LocalUpdateFacts {
        let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
        let service = spaceterm_server::service::Service::for_identity(identity);
        let server = service
            .as_ref()
            .ok()
            .and_then(|service| service.passive_server_id().ok().flatten());
        let update = if crate::local_startup::owned_foreground() {
            Ok(("Development Server".into(), false))
        } else {
            service
                .as_ref()
                .map_err(|error| *error)
                .and_then(crate::ui::settings_window::server_update::passive_update)
        };
        crate::ui::updates::LocalUpdateFacts { server, update }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StartupDependenciesError {
    #[error("Home directory unavailable")]
    MissingHome,
    #[error("Home directory is not absolute")]
    RelativeHome,
    #[error("application paths are unavailable")]
    Paths,
}

pub(crate) struct StartupDependencies<A: SshProcessAdapter> {
    paths: Arc<AppPaths>,
    pub(crate) home_directory: PathBuf,
    ssh_environment: StartupSshEnvironment,
    executable: OpenSshExecutable,
    process_adapter: A,
    host_config_filesystem: Arc<dyn HostConfigFilesystem>,
}

impl<A: SshProcessAdapter> StartupDependencies<A> {
    pub(crate) fn server_context_paths(&self) -> Arc<AppPaths> {
        Arc::clone(&self.paths)
    }

    pub(crate) fn settings_storage(&self) -> Arc<dyn crate::settings::storage::SettingsStorage> {
        Arc::new(crate::settings::storage::ConfigSettingsStorage::new(
            Arc::clone(&self.paths),
        ))
    }
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only macOS composition creates a background image store"
        )
    )]
    pub(crate) fn background_image_store(
        &self,
        inspector: Arc<dyn crate::background_image::ImageInspector>,
    ) -> Arc<crate::background_image::BackgroundImageStore> {
        Arc::new(crate::background_image::BackgroundImageStore::new(
            Arc::clone(&self.paths),
            inspector,
        ))
    }
    /// The same file the settings storage reads and writes, as other programs open it.
    pub(crate) fn settings_file(
        &self,
    ) -> Rc<dyn crate::platform::settings_file::SettingsFileAccess> {
        Rc::new(crate::platform::settings_file::SystemSettingsFile::new(
            self.paths.directories().settings_file(),
            &self.home_directory,
        ))
    }
    pub(crate) fn capture(
        path_environment: AppDirectoryEnvironment,
        ssh_environment: StartupSshEnvironment,
        paths: AppPaths,
        executable: OpenSshExecutable,
        process_adapter: A,
        host_config_filesystem: Arc<dyn HostConfigFilesystem>,
    ) -> Result<Self, StartupDependenciesError> {
        let home_directory = path_environment
            .home
            .as_deref()
            .map(PathBuf::from)
            .ok_or(StartupDependenciesError::MissingHome)?;
        if !home_directory.is_absolute() {
            return Err(StartupDependenciesError::RelativeHome);
        }
        Ok(Self {
            paths: Arc::new(paths),
            home_directory,
            ssh_environment,
            executable,
            process_adapter,
            host_config_filesystem,
        })
    }

    /// The desktop's direct network connections. Construction has no effects: nothing reads
    /// saved routes, asks for a credential, or connects until the first Workspace Window opens.
    pub(crate) fn direct_servers(&self) -> crate::direct_connections::DirectConnections {
        crate::direct_connections::DirectConnections::with_credentials(
            crate::application_identity::ApplicationIdentity::current().directory_name(),
            Arc::new(spaceterm_connections::records::PrivateRecords::new(
                Arc::new(crate::direct_connection_storage::DesktopRecordFile::new(
                    Arc::clone(&self.paths),
                )),
            )),
            spaceterm_connections::native(
                crate::application_identity::ApplicationIdentity::current().directory_name(),
            )
            .unwrap_or_else(|_| Arc::new(spaceterm_connections::credentials::Unavailable)),
        )
    }

    /// Captures setup dependencies without contacting a server.
    pub(crate) fn ssh_setup_factory(
        &self,
        askpass: Arc<dyn crate::ssh::askpass::AskPassWindowFactory>,
        artifacts: Option<PathBuf>,
        direct: crate::direct_connections::DirectConnections,
    ) -> Rc<dyn crate::ui::ssh_setup::SshSetupFactory>
    where
        A: crate::ssh::provisioning::SshServerBootstrap + Clone + 'static,
    {
        Rc::new(NativeSshSetupFactory {
            paths: Arc::clone(&self.paths),
            home: self.home_directory.clone(),
            startup: self.ssh_environment.clone(),
            executable: self.executable.clone(),
            adapter: self.process_adapter.clone(),
            filesystem: Arc::clone(&self.host_config_filesystem),
            askpass,
            artifacts,
            direct,
        })
    }
}

struct NativeSshSetupFactory<A: SshProcessAdapter> {
    paths: Arc<AppPaths>,
    home: PathBuf,
    startup: StartupSshEnvironment,
    executable: OpenSshExecutable,
    adapter: A,
    filesystem: Arc<dyn HostConfigFilesystem>,
    askpass: Arc<dyn crate::ssh::askpass::AskPassWindowFactory>,
    artifacts: Option<PathBuf>,
    direct: crate::direct_connections::DirectConnections,
}
impl<A> crate::ui::ssh_setup::SshSetupFactory for NativeSshSetupFactory<A>
where
    A: SshProcessAdapter + crate::ssh::provisioning::SshServerBootstrap + Clone + 'static,
{
    fn backend(&self) -> Option<Arc<dyn crate::ssh::provisioning::SshSetupBackend>> {
        use crate::ssh::provisioning::{
            NativeSshSetup, NativeSshSetupConfig, ServerArtifactCatalog,
        };
        Some(Arc::new(NativeSshSetup::new(
            NativeSshSetupConfig {
                paths: Arc::clone(&self.paths),
                home: self.home.clone(),
                startup: self.startup.clone(),
                executable: self.executable.clone(),
                adapter: self.adapter.clone(),
                filesystem: Arc::clone(&self.filesystem),
                bootstrap: Arc::new(self.adapter.clone()),
                artifacts: ServerArtifactCatalog::new(self.artifacts.clone()),
            },
            self.direct.clone(),
        )))
    }
    fn prompts(
        &self,
        window: &Window,
        cx: &mut App,
    ) -> Option<Arc<dyn crate::ssh::askpass::AskPassAttemptFactory>> {
        self.askpass.create(window, cx).ok()
    }
    fn hosts(&self) -> Arc<dyn crate::ui::ssh_setup::SshHostSource> {
        Arc::new(NativeSshHosts {
            filesystem: Arc::clone(&self.filesystem),
            roots: crate::ssh::host_config::HostConfigRoots {
                user: self.home.join(".ssh/config"),
                system: PathBuf::from("/etc/ssh/ssh_config"),
                home: self.home.clone(),
            },
        })
    }
}

struct NativeSshHosts {
    filesystem: Arc<dyn HostConfigFilesystem>,
    roots: crate::ssh::host_config::HostConfigRoots,
}

impl crate::ui::ssh_setup::SshHostSource for NativeSshHosts {
    fn hosts(&self) -> Vec<crate::domain::SshDestination> {
        crate::ssh::host_config::discover_ssh_hosts(
            self.filesystem.as_ref(),
            &self.roots,
            crate::ssh::host_config::HostDiscoveryLimits::default(),
        )
        .hosts
        .into_iter()
        .filter_map(|host| {
            crate::domain::SshDestination::new(host.alias().as_str().to_owned()).ok()
        })
        .collect()
    }
}

actions!(
    spaceterm,
    [
        ShowAboutApplication,
        OpenApplicationHelp,
        QuitApplication,
        CloseAllWindows,
        DismissApplication,
        HideApplication,
        HideOtherApplications,
        ShowAllApplications,
        MinimizeWindow,
        ZoomActiveWindow,
        BringAllWindowsToFront,
        ToggleFullScreen
    ]
);

pub(crate) fn init(
    cx: &mut App,
    application_menu: Rc<dyn ApplicationMenuAdapter>,
    application_quit: Rc<dyn ApplicationQuitAdapter>,
) -> Result<(), crate::platform::application_quit::ApplicationQuitError> {
    install_application_menu_actions(cx, Rc::clone(&application_menu));
    let quit_coordinator = install_application_quit(cx, Rc::clone(&application_quit))?;
    // GPUI otherwise exits on its own when the last window closes outside macOS, before the
    // client can stay in its status area. Only the client's quit policy below ends it.
    cx.set_quit_mode(gpui::QuitMode::Explicit);
    let quits = application_quit.last_window_policy()
        == crate::platform::application_quit::LastWindowPolicy::Quit;
    let adapter = application_quit.clone();
    let pending = Rc::new(std::cell::Cell::new(false));
    cx.on_window_closed(move |cx, id| {
        if let Some(index) = cx
            .default_global::<QuitPromptHosts>()
            .0
            .iter()
            .position(|host| *host == id)
        {
            cx.default_global::<QuitPromptHosts>().0.remove(index);
            return;
        }
        if cx.windows().is_empty() && !pending.replace(true) {
            let adapter = adapter.clone();
            let pending = pending.clone();
            cx.defer(move |cx| {
                pending.set(false);
                if !cx.windows().is_empty() {
                    return;
                }
                if take_dismissal(cx) {
                    crate::status_menu::show_in_dock(cx, false);
                }
                // A status item keeps a windowless client reachable. Without one, the client
                // exits rather than run where nothing can reach it.
                if quits && !crate::status_menu::available(cx) {
                    adapter.request_quit(cx);
                }
            });
        }
    })
    .detach();
    // A real quit ends the view a closed window left. Only a closed window keeps one.
    cx.on_app_quit(|cx| {
        crate::ui::remembered_view::forget(cx);
        async {}
    })
    .detach();
    crate::ui::settings_window::init(cx);
    #[cfg(feature = "developer-tools")]
    crate::ui::developer_workbench::init(cx);
    crate::ui::updates::init(cx);
    cx.on_action(switch_workspace_from_global_action);
    cx.on_action(|_: &CloseAllWindows, cx| {
        // Window action dispatch temporarily owns its Window, so close after it returns.
        cx.defer(close_all_windows);
    });
    cx.on_action(move |_: &DismissApplication, cx| {
        let coordinator = Rc::clone(&quit_coordinator);
        // Window action dispatch temporarily owns its Window, so the choice reads every window
        // after it returns.
        cx.defer(move |cx| coordinator.dismiss(cx));
    });
    cx.on_action(move |_: &QuitApplication, cx| {
        let application_quit = Rc::clone(&application_quit);
        // Window action dispatch temporarily owns its Window. Read all Workspace
        // close facts after that Window has returned to the application registry.
        cx.defer(move |cx| application_quit.request_quit(cx));
    });
    cx.on_action(|_: &HideApplication, cx| cx.hide());
    cx.on_action(|_: &HideOtherApplications, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApplications, cx| cx.unhide_other_apps());
    cx.on_action(minimize_active_window);
    cx.on_action(toggle_active_window_full_screen);
    if let Err(error) = application_menu.install(cx) {
        eprintln!("failed to install the application menu: {error}");
    }
    application_menu.follow_chosen_commands(Rc::new(menu_command_chosen), cx);
    crate::keybindings::runtime::attach_application_menu(application_menu, cx);
    Ok(())
}

/// The person chose a menu-bar command, which acts on the active window. A Workspace Window that
/// still holds a remembered choice takes it as the person's input.
pub(crate) fn menu_command_chosen(cx: &mut App) {
    if let Some(window) =
        open_active_window(cx).and_then(|window| window.downcast::<WorkspaceManager>())
    {
        let _ = window.update(cx, |manager, _, cx| manager.person_input(cx));
    }
}

fn switch_workspace_from_global_action(_: &SwitchWorkspace, cx: &mut App) {
    let Some(workspace) = workspace_windows(cx).into_iter().next() else {
        return;
    };
    cx.defer(move |cx| {
        if workspace
            .update(cx, |manager, window, cx| {
                window.activate_window();
                manager.open_workspace_switcher(window, cx);
            })
            .is_err()
        {
            eprintln!("failed to open the Workspace Switcher from a global action");
        }
    });
}

fn install_application_menu_actions(
    cx: &mut App,
    application_menu: Rc<dyn ApplicationMenuAdapter>,
) {
    let about = Rc::clone(&application_menu);
    cx.on_action(move |_: &ShowAboutApplication, cx| {
        // A host without a native About panel presents SpaceTerm's own.
        match about.perform(ApplicationMenuCommand::ShowAbout, cx) {
            Err(ApplicationMenuError::Unavailable) => crate::ui::about_window::open_or_activate(cx),
            Err(error) => eprintln!("failed to perform an application menu command: {error}"),
            Ok(()) => {}
        }
    });
    let help = Rc::clone(&application_menu);
    cx.on_action(move |_: &OpenApplicationHelp, cx| {
        perform_application_menu_command(help.as_ref(), ApplicationMenuCommand::OpenHelp, cx);
    });
    let zoom = Rc::clone(&application_menu);
    cx.on_action(move |_: &ZoomActiveWindow, cx| {
        perform_application_menu_command(
            zoom.as_ref(),
            ApplicationMenuCommand::ZoomActiveWindow,
            cx,
        );
    });
    cx.on_action(move |_: &BringAllWindowsToFront, cx| {
        perform_application_menu_command(
            application_menu.as_ref(),
            ApplicationMenuCommand::BringAllWindowsToFront,
            cx,
        );
    });
}

fn perform_application_menu_command(
    application_menu: &dyn ApplicationMenuAdapter,
    command: ApplicationMenuCommand,
    cx: &mut App,
) {
    if let Err(error) = application_menu.perform(command, cx) {
        eprintln!("failed to perform an application menu command: {error}");
    }
}

fn minimize_active_window(_: &MinimizeWindow, cx: &mut App) {
    let Some(active_window) = cx.active_window() else {
        return;
    };
    cx.defer(move |cx| {
        if active_window
            .update(cx, |_, window, _| window.minimize_window())
            .is_err()
        {
            eprintln!("failed to minimize the active SpaceTerm window");
        }
    });
}

fn toggle_active_window_full_screen(_: &ToggleFullScreen, cx: &mut App) {
    let Some(active_window) = cx.active_window() else {
        return;
    };
    cx.defer(move |cx| {
        if active_window
            .update(cx, |_, window, _| window.toggle_fullscreen())
            .is_err()
        {
            eprintln!("failed to toggle full screen for the active SpaceTerm window");
        }
    });
}

fn install_application_quit(
    cx: &mut App,
    adapter: Rc<dyn ApplicationQuitAdapter>,
) -> Result<Rc<ApplicationQuitCoordinator>, crate::platform::application_quit::ApplicationQuitError>
{
    let coordinator = Rc::new(ApplicationQuitCoordinator {
        adapter: Rc::downgrade(&adapter),
        state: RefCell::new(ApplicationQuitCoordinatorState::Idle),
        status_area: adapter.status_area_name(),
        sessions: crate::local_startup::owned_foreground()
            .then(|| Arc::new(DevelopmentQuitSessions) as Arc<dyn QuitSessions>),
    });
    let handler = Rc::clone(&coordinator);
    adapter.install(ApplicationQuitHandler::new(
        cx,
        Rc::new(move |cx, request| handler.request(request, cx)),
    ))?;
    Ok(coordinator)
}

#[derive(Clone)]
pub(crate) struct ApplicationQuitAfterSave {
    settle: Rc<ApplicationQuitSaveSettlement>,
}

type ApplicationQuitSaveSettlement = dyn Fn(&mut App, ApplicationQuitSaveOutcome);

impl ApplicationQuitAfterSave {
    pub(crate) fn new(settle: impl Fn(&mut App, ApplicationQuitSaveOutcome) + 'static) -> Self {
        Self {
            settle: Rc::new(settle),
        }
    }

    pub(crate) fn saved(&self, cx: &mut App) {
        (self.settle)(cx, ApplicationQuitSaveOutcome::Saved);
    }

    pub(crate) fn failed(&self, cx: &mut App) {
        (self.settle)(cx, ApplicationQuitSaveOutcome::Failed);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplicationQuitSaveOutcome {
    Saved,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplicationQuitCoordinatorState {
    Idle,
    Observing,
    Prompting,
    SessionEnding,
    WaitingForSettings,
    WaitingForUpdater,
    /// Waiting for a Stop or Restart the status item admitted.
    WaitingForOperations,
}

struct ApplicationQuitCoordinator {
    adapter: Weak<dyn ApplicationQuitAdapter>,
    state: RefCell<ApplicationQuitCoordinatorState>,
    status_area: StatusAreaName,
    /// Source Development's foreground Server and its Terminal Sessions end with the client.
    /// An installed client exits while its independently supervised Server keeps running.
    sessions: Option<Arc<dyn QuitSessions>>,
}

/// Observes the run's authoritative live Sessions independently of open client windows.
trait QuitSessions: Send + Sync {
    fn observe(
        &self,
    ) -> Result<spaceterm_server::hierarchy::CloseInventory, spaceterm_server::ServerError>;
}

struct DevelopmentQuitSessions;

impl QuitSessions for DevelopmentQuitSessions {
    fn observe(
        &self,
    ) -> Result<spaceterm_server::hierarchy::CloseInventory, spaceterm_server::ServerError> {
        use spaceterm_server::{ServerConnection, ServerError};
        let identity = crate::application_identity::ApplicationIdentity::current().directory_name();
        let server = ServerConnection::for_identity(identity)?;
        // The server pushes the inventory when it admits the link.
        let (_, revision) = server.link()?.next_inventory(0)?;
        revision
            .inventory
            .close
            .clone()
            .ok_or(ServerError::Protocol)
    }
}

/// The Terminal Sessions a quit would end, as the server reported them.
#[derive(Clone)]
struct ApplicationQuitSnapshot {
    sessions: Option<spaceterm_server::hierarchy::CloseInventory>,
}

impl ApplicationQuitSnapshot {
    /// Whether the person's approval of `self` still covers `current`: no Session started since.
    fn authorizes(&self, current: &Self) -> bool {
        match (&self.sessions, &current.sessions) {
            (None, None) => true,
            (Some(authorized), Some(current)) => {
                authorized.boot == current.boot
                    && current
                        .sessions
                        .iter()
                        .filter(|pane| pane.session.is_some())
                        .all(|pane| authorized.sessions.contains(pane))
            }
            _ => false,
        }
    }
}

impl ApplicationQuitCoordinator {
    fn request(
        self: &Rc<Self>,
        request: ApplicationQuitRequest,
        cx: &mut App,
    ) -> ApplicationQuitDecision {
        match request {
            ApplicationQuitRequest::Dismissal => self.dismiss(cx),
            ApplicationQuitRequest::Exit => self.exit(cx),
            ApplicationQuitRequest::SessionEnd => {
                *self.state.borrow_mut() = ApplicationQuitCoordinatorState::SessionEnding;
                return ApplicationQuitDecision::Proceed;
            }
        }
        ApplicationQuitDecision::Cancel
    }

    fn ready(&self, cx: &App) -> bool {
        *self.state.borrow() == ApplicationQuitCoordinatorState::Idle
            && !application_has_pending_close_confirmation(cx)
    }

    /// Every actual user exit requires the quit warning, including an idle or windowless client.
    fn exit(self: &Rc<Self>, cx: &mut App) {
        if self.ready(cx) {
            self.observe(None, cx);
        }
    }

    fn dismiss(self: &Rc<Self>, cx: &mut App) {
        if !self.ready(cx) {
            return;
        }
        if crate::status_menu::available(cx) {
            close_all_windows(cx);
        } else {
            self.exit(cx);
        }
    }

    fn observe(self: &Rc<Self>, authorization: Option<ApplicationQuitSnapshot>, cx: &mut App) {
        let Some(sessions) = self.sessions.clone() else {
            self.review(
                authorization,
                ApplicationQuitSnapshot { sessions: None },
                cx,
            );
            return;
        };
        *self.state.borrow_mut() = ApplicationQuitCoordinatorState::Observing;
        let read = cx
            .background_executor()
            .spawn(async move { sessions.observe() });
        let coordinator = Rc::clone(self);
        cx.spawn(async move |cx| {
            let result = read.await;
            cx.update(|cx| {
                if *coordinator.state.borrow() != ApplicationQuitCoordinatorState::Observing {
                    return;
                }
                *coordinator.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
                match result {
                    Ok(sessions) => {
                        let snapshot = ApplicationQuitSnapshot {
                            sessions: Some(sessions),
                        };
                        coordinator.review(authorization, snapshot, cx);
                    }
                    Err(_) => coordinator.observation_failed(cx),
                }
            });
        })
        .detach();
    }

    fn review(
        self: &Rc<Self>,
        authorization: Option<ApplicationQuitSnapshot>,
        current: ApplicationQuitSnapshot,
        cx: &mut App,
    ) {
        if application_has_pending_close_confirmation(cx) {
            return;
        }
        if authorization
            .as_ref()
            .is_some_and(|authorization| authorization.authorizes(&current))
        {
            self.finish(current, cx);
        } else {
            self.present_confirmation(current, cx);
        }
    }

    fn observation_failed(self: &Rc<Self>, cx: &mut App) {
        let Some(adapter) = self.adapter.upgrade() else {
            return;
        };
        let notice = QuitPrompt {
            title: "Couldn\u{2019}t check Terminal Sessions".to_owned(),
            detail: "Reconnect to the development server, then try again.".to_owned(),
            buttons: QuitButtons::CancelOnly,
        };
        let Some((answer, place)) = present_quit_prompt(adapter.as_ref(), &notice, cx) else {
            return;
        };
        *self.state.borrow_mut() = ApplicationQuitCoordinatorState::Prompting;
        let coordinator = Rc::clone(self);
        cx.spawn(async move |cx| {
            answer.await;
            cx.update(|cx| {
                if *coordinator.state.borrow() == ApplicationQuitCoordinatorState::Prompting {
                    *coordinator.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
                    cancel_quit_prompt(place, cx);
                } else {
                    close_quit_prompt_host(place, cx);
                }
            });
        })
        .detach();
    }

    fn complete_after_settings_save(
        self: &Rc<Self>,
        authorization: Option<ApplicationQuitSnapshot>,
        outcome: ApplicationQuitSaveOutcome,
        cx: &mut App,
    ) {
        if *self.state.borrow() != ApplicationQuitCoordinatorState::WaitingForSettings {
            return;
        }
        if matches!(outcome, ApplicationQuitSaveOutcome::Failed) {
            *self.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
            return;
        }
        // The save completion can run inside the Settings Window's update. Read and prompt
        // after that window returns to GPUI so a changed target can be confirmed there again.
        let coordinator = Rc::clone(self);
        cx.defer(move |cx| {
            if *coordinator.state.borrow() != ApplicationQuitCoordinatorState::WaitingForSettings {
                return;
            }
            *coordinator.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
            if !application_has_pending_close_confirmation(cx) {
                coordinator.observe(authorization, cx);
            }
        });
    }

    fn finish(self: &Rc<Self>, current: ApplicationQuitSnapshot, cx: &mut App) {
        let coordinator = Rc::clone(self);
        let updater_authorization = current.clone();
        if crate::updates::prepare_before_quit(
            cx,
            Rc::new(move |cx| {
                if *coordinator.state.borrow() != ApplicationQuitCoordinatorState::WaitingForUpdater
                {
                    return;
                }
                *coordinator.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
                coordinator.wait_for_settings(Some(updater_authorization.clone()), cx);
            }),
        ) {
            *self.state.borrow_mut() = ApplicationQuitCoordinatorState::WaitingForUpdater;
            return;
        }
        // An admitted Stop or Restart finishes before the client exits. The client stays
        // usable meanwhile, so the wait ends with the same checks and save as this one.
        if crate::status_menu::operations_running(cx) {
            *self.state.borrow_mut() = ApplicationQuitCoordinatorState::WaitingForOperations;
            let coordinator = Rc::clone(self);
            crate::status_menu::after_operations(cx, move |cx| {
                if *coordinator.state.borrow()
                    != ApplicationQuitCoordinatorState::WaitingForOperations
                {
                    return;
                }
                *coordinator.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
                coordinator.wait_for_settings(Some(current), cx);
            });
            return;
        }
        if let Some(adapter) = self.adapter.upgrade() {
            adapter.confirm_quit(cx);
        }
    }

    fn wait_for_settings(
        self: &Rc<Self>,
        authorization: Option<ApplicationQuitSnapshot>,
        cx: &mut App,
    ) {
        *self.state.borrow_mut() = ApplicationQuitCoordinatorState::WaitingForSettings;
        let coordinator = Rc::clone(self);
        crate::ui::settings_window::quit_when_saved(
            cx,
            ApplicationQuitAfterSave::new(move |cx, outcome| {
                coordinator.complete_after_settings_save(authorization.clone(), outcome, cx);
            }),
        );
    }

    fn present_confirmation(self: &Rc<Self>, snapshot: ApplicationQuitSnapshot, cx: &mut App) {
        let mut state = self.state.borrow_mut();
        if *state != ApplicationQuitCoordinatorState::Idle {
            return;
        }
        *state = ApplicationQuitCoordinatorState::Prompting;
        drop(state);
        let Some(adapter) = self.adapter.upgrade() else {
            *self.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
            return;
        };
        let prompt = quit_prompt(self.status_area, snapshot.sessions.as_ref());
        let Some((answer, place)) = present_quit_prompt(adapter.as_ref(), &prompt, cx) else {
            *self.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
            return;
        };
        let coordinator = Rc::clone(self);
        cx.spawn(async move |cx| {
            let confirmed = answer.await == Some(QuitAnswer::Quit);
            cx.update(|cx| {
                if *coordinator.state.borrow() != ApplicationQuitCoordinatorState::Prompting {
                    close_quit_prompt_host(place, cx);
                    return;
                }
                if confirmed {
                    close_quit_prompt_host(place, cx);
                    coordinator.wait_for_settings(Some(snapshot), cx);
                } else {
                    *coordinator.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
                    cancel_quit_prompt(place, cx);
                }
            });
        })
        .detach();
    }
}

/// Temporary warning windows never trigger another last-window quit when they close.
#[derive(Default)]
struct QuitPromptHosts(Vec<gpui::WindowId>);
impl gpui::Global for QuitPromptHosts {}

/// Presents a quit warning without creating a Workspace or Terminal Session, for a windowless
/// client on a host that shows the warning only in a window.
struct QuitPromptHost;

impl gpui::Render for QuitPromptHost {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::Styled as _;
        spaceterm_ui::ModalLayer::new(
            gpui::div()
                .size_full()
                .bg(crate::ui::appearance::gpui_color(
                    crate::ui::appearance::chrome(cx)
                        .colors
                        .title_bar_background,
                )),
        )
    }
}

const QUIT_PROMPT_HOST_WIDTH: f32 = 480.0;
const QUIT_PROMPT_HOST_HEIGHT: f32 = 280.0;

fn open_quit_prompt_host(cx: &mut App) -> gpui::Result<gpui::WindowHandle<QuitPromptHost>> {
    let title = crate::application_identity::ApplicationIdentity::current().display_name();
    let window_size = size(px(QUIT_PROMPT_HOST_WIDTH), px(QUIT_PROMPT_HOST_HEIGHT));
    let host = cx.open_window(
        WindowOptions {
            app_id: window_application_id(),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                window_size,
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            kind: gpui::WindowKind::Normal,
            is_resizable: false,
            is_minimizable: false,
            ..WindowOptions::default()
        },
        |_, cx| cx.new(|_| QuitPromptHost),
    )?;
    cx.default_global::<QuitPromptHosts>()
        .0
        .push(host.window_id());
    cx.activate(true);
    Ok(host)
}

/// Where a quit warning appears.
#[derive(Clone, Copy)]
enum QuitPromptPlace {
    /// A sheet on one of the client's windows.
    Window,
    /// A temporary window opened for a windowless client.
    Host(gpui::WindowHandle<QuitPromptHost>),
    /// By itself, while the client has no window.
    Alone,
}

/// Asks `prompt` as a sheet on the front window. A windowless client asks it by itself, or in a
/// temporary window on a host that shows it only in a window. Returns `None` when nothing could
/// present it.
fn present_quit_prompt(
    adapter: &dyn ApplicationQuitAdapter,
    prompt: &QuitPrompt,
    cx: &mut App,
) -> Option<(gpui::Task<Option<QuitAnswer>>, QuitPromptPlace)> {
    let (window, place) = match open_active_window(cx).or_else(|| cx.windows().into_iter().next()) {
        Some(window) => (window, QuitPromptPlace::Window),
        None => {
            if let Some(answer) = adapter.present_windowless_confirmation(prompt, cx) {
                return Some((answer, QuitPromptPlace::Alone));
            }
            let host = open_quit_prompt_host(cx).ok()?;
            (host.into(), QuitPromptPlace::Host(host))
        }
    };
    // The menu bar item can ask while another app is in front, and a window made key there
    // receives no keys until the client takes the keyboard.
    adapter.take_keyboard(cx);
    let answer = window.update(cx, |_, window, cx| {
        window.activate_window();
        adapter.present_confirmation(prompt, window, cx)
    });
    match answer {
        Ok(answer) => Some((answer, place)),
        Err(_) => {
            cancel_quit_prompt(place, cx);
            None
        }
    }
}

fn close_quit_prompt_host(place: QuitPromptPlace, cx: &mut App) {
    if let QuitPromptPlace::Host(host) = place {
        let _ = host.update(cx, |_, window, _| window.remove_window());
    }
}

/// Cancel must leave the client reachable if the status item disappeared. Reopen its normal
/// presentation before removing the warning host so no intermediate last-window close can quit.
fn cancel_quit_prompt(place: QuitPromptPlace, cx: &mut App) {
    let prompt = match place {
        QuitPromptPlace::Host(host) => Some(gpui::AnyWindowHandle::from(host)),
        QuitPromptPlace::Window | QuitPromptPlace::Alone => None,
    };
    let otherwise_windowless = |cx: &App| {
        cx.windows()
            .into_iter()
            .all(|window| Some(window) == prompt)
    };
    if !crate::status_menu::available(cx) && otherwise_windowless(cx) {
        // Restoring also returns the client to the Dock, which keeps it reachable even when
        // no window opens. As with Open in the status item, activating also brings back a
        // client its warning hid.
        activate_default_window(cx, None);
        cx.activate(true);
        // If normal presentation is temporarily unavailable, retain the visible host rather
        // than leave a client that has neither a window nor a status item.
        if otherwise_windowless(cx) {
            if let QuitPromptPlace::Host(host) = place {
                // The retained host is now the client's reachable window. Its next native
                // close must follow normal last-window policy instead of being suppressed.
                cx.default_global::<QuitPromptHosts>()
                    .0
                    .retain(|id| *id != host.window_id());
            }
            return;
        }
    }
    close_quit_prompt_host(place, cx);
}

fn quit_prompt(
    status_area: StatusAreaName,
    sessions: Option<&spaceterm_server::hierarchy::CloseInventory>,
) -> QuitPrompt {
    let name = crate::application_identity::ApplicationIdentity::current().display_name();
    let count = sessions.map_or(0, |sessions| {
        sessions
            .sessions
            .iter()
            .filter(|pane| pane.session.is_some())
            .count()
    });
    let place = status_area.phrase();
    let detail = match (sessions, count) {
        (None, _) => format!(
            "{name} leaves the {place}. Your Terminal Sessions keep running, so everything is where you left it."
        ),
        (Some(_), 0) => format!(
            "{name} leaves the {place} and stops its development server. No Terminal Sessions are running."
        ),
        (Some(_), 1) => "This development build runs its own server. Quitting ends its 1 Terminal Session and stops its running commands.".to_owned(),
        (Some(_), count) => format!(
            "This development build runs its own server. Quitting ends its {count} Terminal Sessions and stops their running commands."
        ),
    };
    QuitPrompt {
        title: format!("Quit {name}?"),
        detail,
        buttons: if count > 0 {
            QuitButtons::DestructiveQuit
        } else {
            QuitButtons::Quit
        },
    }
}

fn application_has_pending_close_confirmation(cx: &App) -> bool {
    workspace_windows(cx).iter().any(|window| {
        window
            .read(cx)
            .is_ok_and(WorkspaceManager::has_pending_close_confirmation)
    })
}

pub(crate) fn open(
    cx: &mut App,
    host: &HostComposition,
) -> Result<gpui::WindowHandle<WorkspaceManager>, RuntimeError> {
    if cx
        .try_global::<crate::updates::UpdateService>()
        .is_some_and(|service| {
            service.0.read(cx).launch_state() != crate::updates::LaunchState::Open
        })
    {
        return Err(RuntimeError::Initialization);
    }
    let adapters = crate::ui::WorkspaceManagerAdapters {
        local_filesystem: host.adapters.local_filesystem.clone(),
        key_input: Rc::clone(&host.adapters.key_input),
        accessibility: Rc::clone(&host.adapters.accessibility),
        native_services: host.adapters.native_services.clone(),
        lifecycle: crate::ui::pane_lifecycle::PaneLifecycleDependencies {
            permission_setup: host.permission_setup.get().cloned(),
            ..host.adapters.lifecycle.clone()
        },
        directory_selection: Rc::new(crate::directory_selection::GpuiDirectorySelection),
        window_drag: host.window_movement.create(),
        local_server: host.adapters.local_server.clone(),
        server_directories: crate::ui::directory_picker::owning_server_directories(),
    };
    let home_directory = host.home_directory.clone();
    let default_sidebar = cx
        .try_global::<crate::ui::appearance_runtime::AppearanceRuntime>()
        .map(|runtime| runtime.settings.snapshot().candidate.sidebar.default_state)
        .unwrap_or_default();
    let ssh_setup = host.adapters.ssh_setup.clone();
    let direct_servers = host.adapters.direct_servers.clone();
    // A window reopened while the client kept running presents the view the last one left.
    let remembered = crate::ui::remembered_view::take_workspace_window(cx);
    let mut options = workspace_window_options(host, cx);
    let placement = remembered
        .as_ref()
        .and_then(|view| view.placement.as_ref())
        .map(|placement| placement.apply(&mut options, cx));
    let result = cx.open_window(options, |window, cx| {
        if let Some(placement) = placement {
            placement.finish(window);
        }
        crate::ui::updates::install_local_updates(Arc::new(NativeLocalUpdates), cx);
        // Both enrollment methods publish into this paired connection registry.
        if let Some(direct) = direct_servers.clone() {
            crate::ui::direct_servers::install(direct, cx);
            // Paired server cards stop, restart and update each server over its service lane.
            crate::ui::settings_window::remote_admin::install(
                crate::ui::settings_window::remote_admin::lane_source(),
                cx,
            );
        }
        // The Settings window supplies prompts for each explicit setup attempt.
        if let Some(factory) = ssh_setup.clone() {
            crate::ui::ssh_setup::install(factory, cx);
        }
        // Resuming is quiet: a route or unpair that needs a credential prompt waits for Resume.
        if let Some(direct) = crate::ui::direct_servers::installed(cx) {
            direct.resume_retained();
        }
        let workspace_manager = cx.new(|cx| {
            WorkspaceManager::new_with_adapters(
                home_directory,
                adapters,
                default_sidebar,
                window,
                cx,
            )
        });
        workspace_manager.update(cx, |workspace_manager, cx| {
            workspace_manager.focus(window, cx);
            if let Some(view) = remembered {
                workspace_manager.restore_remembered(view, window, cx);
            }
        });
        let close_manager = workspace_manager.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            close_manager
                .update(cx, |manager, cx| manager.should_close_window(window, cx))
                .unwrap_or(true)
        });
        if let Some(services) = &host.services
            && let Err(error) = services.install(
                window,
                Rc::new(WorkspaceServicesEndpoint {
                    app: cx.to_async(),
                    window: window.window_handle(),
                    owner: workspace_manager.downgrade(),
                }),
            )
        {
            eprintln!("failed to install the Services responder: {error}");
        }
        workspace_manager
    });

    let window = result.map_err(|_| RuntimeError::WindowOpen)?;
    cx.activate(true);
    Ok(window)
}

fn workspace_window_options(host: &HostComposition, cx: &App) -> WindowOptions {
    let appearance = crate::ui::appearance::chrome(cx);
    let workspace_titlebar_height = crate::ui::WorkspaceFrame::for_appearance(appearance, cx)
        .top_chrome_height(appearance.top_height());
    let workspace_traffic_light_position = host
        .window_frame
        .workspace_traffic_light_position(workspace_titlebar_height);
    let bounds = Bounds::centered(None, size(px(900.0), px(580.0)), cx);
    host.window_chrome.options(
        crate::platform::window_chrome::WindowRole::Workspace,
        WindowOptions {
            app_id: window_application_id(),
            window_background: crate::ui::appearance_runtime::window_background(cx),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.0), px(260.0))),
            titlebar: host
                .window_chrome
                .titlebar
                .as_ref()
                .map(|titlebar| TitlebarOptions {
                    title: titlebar.title.clone(),
                    appears_transparent: titlebar.appears_transparent,
                    traffic_light_position: workspace_traffic_light_position
                        .or(titlebar.traffic_light_position),
                }),
            // The Workspace chrome moves the window through its own drag regions. AppKit must not
            // also move it from the titlebar strip, where a Tab press starts a Tab drag.
            app_owns_titlebar_drag: true,
            ..WindowOptions::default()
        },
        cx,
    )
}
/// The host's active window while it is still open. A host can keep naming a window the client
/// already closed, such as the window that asked the previous quit question.
fn open_active_window(cx: &App) -> Option<gpui::AnyWindowHandle> {
    cx.active_window()
        .filter(|active| cx.windows().contains(active))
}

/// Every live Workspace window, resolved at call time from GPUI's own registry. A Settings window
/// is never counted.
fn workspace_windows(cx: &App) -> Vec<gpui::WindowHandle<WorkspaceManager>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<WorkspaceManager>())
        .collect()
}

/// Close All Windows was chosen and its windows are still closing. Once none remains, the
/// client leaves the Dock and stays in the menu bar.
#[derive(Default)]
struct Dismissal(bool);
impl gpui::Global for Dismissal {}

fn take_dismissal(cx: &mut App) -> bool {
    std::mem::take(&mut cx.default_global::<Dismissal>().0)
}

/// A window refused to close, so the client stays windowed.
pub(crate) fn dismissal_cancelled(cx: &mut App) {
    cx.default_global::<Dismissal>().0 = false;
}

/// Closes every window the way its own close button does, so a window that asks first still
/// asks. The client keeps running with its status item; it neither quits nor stops the server.
fn close_all_windows(cx: &mut App) {
    cx.default_global::<Dismissal>().0 = true;
    for window in cx.windows() {
        if let Some(workspace) = window.downcast::<WorkspaceManager>() {
            let _ = workspace.update(cx, |manager, window, cx| {
                manager.request_window_close(window, cx);
            });
        } else if !crate::ui::settings_window::request_window_close(window, cx)
            && !crate::ui::server_control_window::request_window_close(window, cx)
            && !crate::ui::updates::request_launch_window_close(window, cx)
        {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }
    // With nothing to close, the client is menu-only at once.
    if cx.windows().is_empty() && take_dismissal(cx) {
        crate::status_menu::show_in_dock(cx, false);
    }
}

/// Returns a menu-only client to the Dock before it presents a window again.
pub(crate) fn leave_menu_only(cx: &mut App) {
    dismissal_cancelled(cx);
    crate::status_menu::show_in_dock(cx, true);
}

fn restore_default_window(cx: &mut App, host: &HostComposition) {
    if !workspace_windows(cx).is_empty() {
        return;
    }
    if let Err(error) = open(cx, host) {
        eprintln!("failed to restore the default SpaceTerm window: {error}");
    }
}

struct DefaultWorkspaceWindowHost(Rc<HostComposition>);
impl gpui::Global for DefaultWorkspaceWindowHost {}

/// Reopening belongs to application lifecycle, including when only Settings remains open.
#[allow(
    dead_code,
    reason = "external activation sources are selected by host composition"
)]
pub(crate) fn activate_default_window(cx: &mut App, token: Option<&str>) {
    leave_menu_only(cx);
    if let Some(host) = cx
        .try_global::<DefaultWorkspaceWindowHost>()
        .map(|host| host.0.clone())
    {
        restore_default_window(cx, &host);
    }
    if let Some(handle) = crate::ui::updates::front_workspace_window(cx) {
        let _ = handle.update(cx, |_, window, _| {
            if let Some(token) = token {
                window.activate_window_with_token(token);
            } else {
                window.activate_window();
            }
        });
    }
}

fn install_headless_window_actions(cx: &mut App, host: Rc<HostComposition>) {
    cx.set_global(DefaultWorkspaceWindowHost(host.clone()));
    cx.on_action(move |_: &NewWorkspace, cx| new_workspace_without_window(cx, &host));
}

/// New Workspace with no Workspace Window opens one that creates the Workspace, unlike opening
/// SpaceTerm, which only presents this client's attached Workspaces.
fn new_workspace_without_window(cx: &mut App, host: &HostComposition) {
    if !workspace_windows(cx).is_empty() {
        return;
    }
    leave_menu_only(cx);
    match open(cx, host) {
        Ok(window) => {
            let _ = window.update(cx, |manager, window, cx| {
                manager.create_workspace_when_presented(window, cx);
            });
        }
        Err(error) => eprintln!("failed to open a SpaceTerm window for a new Workspace: {error}"),
    }
}

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a desktop Services Adapter calls back into the Workspace"
    )
)]
#[derive(Clone)]
struct WorkspaceServicesEndpoint {
    app: gpui::AsyncApp,
    window: gpui::AnyWindowHandle,
    owner: gpui::WeakEntity<WorkspaceManager>,
}

impl crate::terminal::native_services::services::ServiceEndpoint for WorkspaceServicesEndpoint {
    fn status(&self) -> NativeServiceStatus {
        self.app
            .update(|cx| {
                self.window.update(cx, |root, window, cx| {
                    let Ok(manager) = root.downcast::<WorkspaceManager>() else {
                        return NativeServiceStatus::default();
                    };
                    if manager.entity_id() != self.owner.entity_id() {
                        return NativeServiceStatus::default();
                    }
                    manager.update(cx, |manager, cx| manager.native_service_status(window, cx))
                })
            })
            .ok()
            .unwrap_or_default()
    }

    fn selection(&self, origin: NativeServiceOrigin) -> Option<SelectionCopy> {
        self.app
            .update(|cx| {
                self.window.update(cx, |root, window, cx| {
                    let Ok(manager) = root.downcast::<WorkspaceManager>() else {
                        return None;
                    };
                    if manager.entity_id() != self.owner.entity_id() {
                        return None;
                    }
                    manager.update(cx, |manager, cx| {
                        manager.native_service_selection(origin, window, cx)
                    })
                })
            })
            .ok()
            .flatten()
    }

    fn insert_text(&self, origin: NativeServiceOrigin, text: String) -> bool {
        self.app
            .update(|cx| {
                self.window.update(cx, |root, window, cx| {
                    let Ok(manager) = root.downcast::<WorkspaceManager>() else {
                        return false;
                    };
                    if manager.entity_id() != self.owner.entity_id() {
                        return false;
                    }
                    manager.update(cx, |manager, cx| {
                        manager.insert_native_service_text(origin, text, window, cx)
                    })
                })
            })
            .ok()
            .unwrap_or(false)
    }
}

/// The desktop application identifier every Operating-System Window carries, which Wayland
/// and X11 desktop shells match against the installed desktop entry.
pub(crate) fn window_application_id() -> Option<String> {
    Some(
        crate::application_identity::ApplicationIdentity::current()
            .application_id()
            .to_owned(),
    )
}

/// Application-scoped capabilities shared by every Operating-System Window.
#[derive(Clone)]
pub(crate) struct ApplicationCapabilities {
    pub(crate) updates: Rc<dyn crate::updates::UpdateAdapter>,
    pub(crate) application_menu: Rc<dyn ApplicationMenuAdapter>,
    pub(crate) desktop_events: Rc<dyn crate::platform::desktop_events::DesktopEventAdapter>,
    pub(crate) application_quit: Rc<dyn ApplicationQuitAdapter>,
    pub(crate) selected_files: Arc<dyn crate::platform::selected_file::SelectedFileOpener>,
    /// Opens and watches the settings file the person edits in their own editor.
    pub(crate) settings_file: Rc<dyn crate::platform::settings_file::SettingsFileAccess>,
    pub(crate) local_filesystem: crate::platform::local_filesystem::LocalFilesystemAuthority,
    pub(crate) key_input: Rc<dyn crate::terminal::TerminalKeyInputAdapterFactory>,
    pub(crate) accessibility:
        Rc<dyn crate::platform::terminal_accessibility::TerminalAccessibilityAdapterFactory>,
    pub(crate) native_services: crate::terminal::native_services::NativeServiceAdapters,
    pub(crate) lifecycle: crate::ui::pane_lifecycle::PaneLifecycleDependencies,
    /// Reads and requests the microphone and camera grants programs in a Terminal Session inherit.
    pub(crate) capture_access: Option<Rc<dyn crate::platform::capture_access::CaptureAccess>>,
    /// Reads and recovers the System Permissions programs in a Terminal Session inherit.
    pub(crate) permission_access:
        Option<Rc<dyn crate::platform::permission_access::PermissionAccess>>,
    /// Docks the Setup Guide on System Settings' window during a Permission Setup.
    pub(crate) setup_guide: Option<Arc<dyn crate::platform::setup_guide_host::SetupGuideHost>>,
    /// Reaches the Zed extension registry when the person browses it for Terminal Themes.
    pub(crate) theme_registry: Arc<dyn crate::theme_registry::RegistryTransport>,
    /// Runs git and the GitHub CLI for Repository Status.
    pub(crate) repository_status:
        Option<crate::ui::repository_status_store::RepositoryStatusAdapters>,
    /// Keeps the Background Image copies where Workspace windows can present one.
    pub(crate) background_images: Option<Arc<crate::background_image::BackgroundImageStore>>,
    /// Launches and presents the account-local SpaceTerm Server in the Workspace Window.
    pub(crate) local_server: crate::ui::LocalServerAdapters,
    /// Supplies temporary SSH provisioning to Add Server.
    pub(crate) ssh_setup: Option<Rc<dyn crate::ui::ssh_setup::SshSetupFactory>>,
    /// The desktop's direct network connections, installed by its first Workspace Window.
    /// `None` offers no direct network connections.
    pub(crate) direct_servers: Option<crate::direct_connections::DirectConnections>,
    /// Private presentation context, independent of saved connection authority and Settings.
    pub(crate) server_context_paths: Option<Arc<AppPaths>>,
    /// Reads and installs this build's command for Settings. `None` omits its row.
    pub(crate) command_line_tool: Option<Arc<crate::command_line_tool::CommandLineTool>>,
    /// Answers `spaceterm open` and `spaceterm show` from this account. `None` listens nowhere,
    /// as when another desktop of this identity already listens.
    pub(crate) desktop_channel: Option<Arc<dyn crate::desktop_channel::DesktopChannelEndpoint>>,
}

/// The startup-supplied file opener available to explicit local file selections.
pub(crate) struct SelectedFileAccess(
    pub(crate) Arc<dyn crate::platform::selected_file::SelectedFileOpener>,
);

impl gpui::Global for SelectedFileAccess {}

/// Complete constructor wiring. This value defines no platform operations.
pub(crate) struct HostCompositionParts {
    pub(crate) profile: crate::desktop_profile::DesktopProfile,
    pub(crate) home_directory: PathBuf,
    pub(crate) adapters: ApplicationCapabilities,
    pub(crate) services:
        Option<Rc<dyn crate::platform::services_registration::ServicesRegistration>>,
    pub(crate) window_movement: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
    pub(crate) window_frame: crate::platform::window_frame::WindowFrameGeometry,
    pub(crate) window_chrome: crate::platform::window_chrome::WindowChrome,
}
/// Installs the host's native status item once the application runs. Window movement serves any
/// window the status item opens.
pub(crate) type StatusMenuInstaller =
    fn(&mut App, Rc<dyn crate::platform::window_movement::WindowMovementFactory>);

pub(crate) struct HostComposition {
    modal_prompts: bool,
    profile: crate::desktop_profile::DesktopProfile,
    home_directory: PathBuf,
    adapters: ApplicationCapabilities,
    services: Option<Rc<dyn crate::platform::services_registration::ServicesRegistration>>,
    window_movement: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
    window_frame: crate::platform::window_frame::WindowFrameGeometry,
    window_chrome: crate::platform::window_chrome::WindowChrome,
    appearance: Option<(
        Arc<dyn crate::settings::storage::SettingsStorage>,
        Rc<dyn crate::platform::appearance::AppearancePlatform>,
    )>,
    /// Shows the native status item beside the client's windows, where the host has one.
    status_menu: Option<StatusMenuInstaller>,
    /// The application's one Permission Setup, created once the application runs when the host
    /// composes permission access and a Setup Guide.
    permission_setup:
        std::cell::OnceCell<gpui::Entity<crate::ui::permission_setup::PermissionSetup>>,
}
impl HostComposition {
    pub(crate) fn with_status_menu(mut self, install: StatusMenuInstaller) -> Self {
        self.status_menu = Some(install);
        self
    }
    pub(crate) fn with_modal_prompts(mut self, enabled: bool) -> Self {
        self.modal_prompts = enabled;
        self
    }
    pub(crate) fn with_appearance(
        mut self,
        storage: Arc<dyn crate::settings::storage::SettingsStorage>,
        platform: Rc<dyn crate::platform::appearance::AppearancePlatform>,
    ) -> Self {
        self.appearance = Some((storage, platform));
        self
    }
    pub(crate) fn new(
        parts: HostCompositionParts,
    ) -> Result<Self, crate::desktop_profile::DesktopProfileError> {
        use crate::desktop_profile::DesktopProfileError;
        if !parts.home_directory.is_absolute() {
            return Err(DesktopProfileError::InvalidCombination);
        }
        if parts
            .window_chrome
            .titlebar
            .as_ref()
            .is_some_and(|titlebar| {
                titlebar.traffic_light_position.is_some() && !titlebar.appears_transparent
            })
        {
            return Err(DesktopProfileError::InvalidCombination);
        }
        Ok(Self {
            modal_prompts: false,
            profile: parts.profile,
            home_directory: parts.home_directory,
            adapters: parts.adapters,
            services: parts.services,
            window_movement: parts.window_movement,
            window_frame: parts.window_frame,
            window_chrome: parts.window_chrome,
            appearance: None,
            status_menu: None,
            permission_setup: std::cell::OnceCell::new(),
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum RuntimeError {
    #[error("UI initialization failed")]
    Initialization,
    #[error("Window creation failed")]
    WindowOpen,
}

#[derive(Debug, thiserror::Error)]
enum LaunchError {
    #[error("{0}")]
    Dependencies(StartupDependenciesError),
    #[error("{0}")]
    Desktop(crate::desktop_profile::DesktopProfileError),
    #[error("{0}")]
    Runtime(RuntimeError),
}

/// The host supplies only its composition constructor after helper dispatch has declined.
pub(crate) fn launch<A: SshProcessAdapter>(
    startup: Result<StartupDependencies<A>, StartupDependenciesError>,
    compose: impl FnOnce(
        StartupDependencies<A>,
    ) -> Result<HostComposition, crate::desktop_profile::DesktopProfileError>,
) -> i32 {
    let result = startup
        .map_err(LaunchError::Dependencies)
        .and_then(|startup| compose(startup).map_err(LaunchError::Desktop))
        .and_then(|host| run(host).map_err(LaunchError::Runtime));
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("failed to start SpaceTerm: {error}");
            2
        }
    }
}

pub(crate) fn run(host: HostComposition) -> Result<(), RuntimeError> {
    let failure = Rc::new(std::cell::Cell::new(None));
    let reported_failure = Rc::clone(&failure);
    let host = Rc::new(host);
    let reopened_host = Rc::clone(&host);
    let application = gpui_platform::application().with_assets(spaceterm_ui::EmbeddedAssets);
    // Platform callbacks may precede initial Workspace creation. Hold one transient delivery
    // until startup finishes, then use the same sheet as every explicit Add Server entry.
    let (invitations, pending_invitations) = async_channel::bounded(1);
    if let Some(link) = crate::invitation_links::from_arguments(std::env::args_os().skip(1)) {
        let _ = invitations.try_send(link);
    }
    application.on_open_urls(move |urls| {
        if let Some(link) = urls
            .into_iter()
            .find(|url| crate::invitation_links::is_pairing_link(url))
        {
            let _ = invitations.try_send(link);
        }
    });
    let pending_invitations = Rc::new(RefCell::new(Some(pending_invitations)));
    let desktop_channel = RefCell::new(host.adapters.desktop_channel.clone());
    application.on_reopen(move |cx| {
        leave_menu_only(cx);
        restore_default_window(cx, &reopened_host);
    });
    application.run(move |cx| {
        if let Err(error) = initialize_application(cx, &host) {
            reported_failure.set(Some(error));
            cx.quit();
        } else {
            install_headless_window_actions(cx, Rc::clone(&host));
            if let Some(install) = host.status_menu {
                install(cx, Rc::clone(&host.window_movement));
            }
            let service = cx.global::<crate::updates::UpdateService>().0.clone();
            service.update(cx, |updates, cx| {
                updates.begin_launch(
                    Rc::new(move |cx| {
                        if let Err(error) = open_initial_workspace(cx, &host) {
                            reported_failure.set(Some(error));
                            cx.quit();
                            return;
                        }
                        if let Some(endpoint) = desktop_channel.borrow_mut().take() {
                            crate::ui::serve_desktop_requests(
                                endpoint,
                                crate::application_identity::ApplicationIdentity::current()
                                    .directory_name(),
                                cx,
                            );
                        }
                        if let Some(receiver) = pending_invitations.borrow_mut().take() {
                            cx.spawn(async move |cx| {
                                while let Ok(link) = receiver.recv().await {
                                    cx.update(|cx| {
                                        leave_menu_only(cx);
                                        crate::ui::settings_window::open_add_server(
                                            Some(link),
                                            None,
                                            cx,
                                        );
                                    });
                                }
                            })
                            .detach();
                        }
                    }),
                    cx,
                );
            });
            if service.read(cx).launch_state() != crate::updates::LaunchState::Open {
                crate::ui::updates::show_launch(cx);
            }
        }
    });
    failure.get().map_or(Ok(()), Err)
}

pub(crate) fn initialize_application(
    cx: &mut App,
    host: &HostComposition,
) -> Result<(), RuntimeError> {
    if host.modal_prompts {
        crate::ui::application_prompt::install(cx);
    }
    cx.set_global(host.window_frame);
    cx.set_global(host.window_chrome.clone());
    host.window_chrome.install_controls(cx);
    crate::updates::ApplicationUpdates::install(Rc::clone(&host.adapters.updates), cx);
    cx.set_global(SelectedFileAccess(Arc::clone(
        &host.adapters.selected_files,
    )));
    // Native menu construction reads the keymap; install both before Settings I/O and fonts.
    let keymap = host.profile.install(cx);
    crate::keybindings::runtime::install(keymap, cx);
    gpui::BorrowAppContext::update_global::<crate::desktop_profile::DesktopPresentation, _>(
        cx,
        |presentation, cx| {
            presentation.refresh(cx);
        },
    );
    if let Some(services) = &host.services
        && let Err(error) = services.register()
    {
        eprintln!("failed to register Services: {error}");
    }
    #[cfg(feature = "developer-tools")]
    crate::ui::developer_workbench::configure_window_chrome(Rc::clone(&host.window_movement), cx);
    crate::ui::about_window::configure_window_chrome(Rc::clone(&host.window_movement), cx);
    if let Some(paths) = &host.adapters.server_context_paths {
        crate::ui::settings_window::configure_server_context(Arc::clone(paths), cx);
    }
    if let Some(tool) = &host.adapters.command_line_tool {
        crate::ui::settings_window::configure_command_line_tool(Arc::clone(tool), cx);
    }
    if let (Some(access), Some(guide)) =
        (&host.adapters.permission_access, &host.adapters.setup_guide)
    {
        let setup = crate::ui::permission_setup::PermissionSetup::create(
            Rc::clone(access),
            Arc::clone(guide),
            cx,
        );
        let _ = host.permission_setup.set(setup);
    }
    crate::ui::settings_window::configure_window_chrome(
        Rc::clone(&host.window_movement),
        crate::ui::settings_window::PermissionCapabilities {
            capture: host.adapters.capture_access.clone(),
            system_permissions: host.adapters.permission_access.clone(),
            permission_setup: host.permission_setup.get().cloned(),
        },
        crate::theme_registry::ZedThemeRegistry::new(Arc::clone(&host.adapters.theme_registry)),
        cx,
    );
    host.adapters.desktop_events.install(cx);
    init(
        cx,
        Rc::clone(&host.adapters.application_menu),
        Rc::clone(&host.adapters.application_quit),
    )
    .map_err(|_| RuntimeError::Initialization)?;
    crate::ui::appearance_runtime::register_fonts(cx).map_err(|_| RuntimeError::Initialization)?;
    if let Some((storage, platform)) = &host.appearance {
        let settings = crate::settings::Settings::load(Arc::clone(storage));
        let service = cx.global::<crate::updates::UpdateService>().0.clone();
        service.update(cx, |updates, _| updates.attach_settings(settings.clone()));
        crate::ui::appearance_runtime::install(settings.clone(), Rc::clone(platform), cx)
            .map_err(|_| RuntimeError::Initialization)?;
        crate::keybindings::runtime::follow(&settings, cx);
        crate::ui::settings_file::SettingsFile::install(
            settings.clone(),
            Rc::clone(&host.adapters.settings_file),
            cx,
        );
        if let Some(adapters) = &host.adapters.repository_status {
            crate::ui::repository_status_store::install(&settings, adapters.clone(), cx);
        }
        if let Some(store) = &host.adapters.background_images {
            crate::ui::background_image_runtime::install(settings.clone(), Arc::clone(store), cx);
        }
    }
    crate::ui::initialize_controls(cx).map_err(|_| RuntimeError::Initialization)?;
    Ok(())
}

#[cfg(test)]
fn start_application(
    cx: &mut App,
    host: &HostComposition,
) -> Result<gpui::WindowHandle<WorkspaceManager>, RuntimeError> {
    initialize_application(cx, host)?;
    open_initial_workspace(cx, host)
}

fn open_initial_workspace(
    cx: &mut App,
    host: &HostComposition,
) -> Result<gpui::WindowHandle<WorkspaceManager>, RuntimeError> {
    let workspace = open(cx, host)?;
    crate::ui::settings_recovery::offer_at_launch(workspace, cx);
    #[cfg(feature = "developer-tools")]
    crate::ui::developer_workbench::open_at_launch(cx);
    Ok(workspace)
}

#[cfg(test)]
mod tests {
    use gpui::{Action, ClipboardItem, Keystroke, TestAppContext};

    use super::*;
    use crate::platform::application_quit::testing::AskingOnWindow;
    use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};
    use crate::terminal::{SelectionCopy, TerminalSessionFactory, WorkspaceTerminalSessionFactory};
    use crate::ui::TerminalPane;

    fn application_menu() -> Rc<dyn ApplicationMenuAdapter> {
        Rc::new(
            crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
        )
    }

    fn application_quit() -> Rc<dyn ApplicationQuitAdapter> {
        Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        )
    }

    #[test]
    fn installed_quit_warning_says_terminal_sessions_keep_running() {
        let name = crate::application_identity::ApplicationIdentity::current().display_name();
        for (status_area, place) in [
            (StatusAreaName::MenuBar, "menu bar"),
            (StatusAreaName::StatusArea, "status area"),
        ] {
            let prompt = quit_prompt(status_area, None);
            assert_eq!(prompt.title, format!("Quit {name}?"));
            assert_eq!(
                prompt.detail,
                format!(
                    "{name} leaves the {place}. Your Terminal Sessions keep running, so everything is where you left it."
                )
            );
            assert_eq!(prompt.buttons, QuitButtons::Quit);
        }
    }

    #[test]
    fn development_quit_warning_counts_the_terminal_sessions_it_ends() {
        let name = crate::application_identity::ApplicationIdentity::current().display_name();
        let idle = quit_prompt(StatusAreaName::MenuBar, Some(&quit_sessions(1, &[None])));
        assert_eq!(
            idle.detail,
            format!(
                "{name} leaves the menu bar and stops its development server. No Terminal Sessions are running."
            )
        );
        assert_eq!(
            idle.buttons,
            QuitButtons::Quit,
            "quitting ends no Terminal Session"
        );
        let status_area_idle =
            quit_prompt(StatusAreaName::StatusArea, Some(&quit_sessions(1, &[None])));
        assert_eq!(
            status_area_idle.detail,
            format!(
                "{name} leaves the status area and stops its development server. No Terminal Sessions are running."
            )
        );
        let one = quit_prompt(StatusAreaName::MenuBar, Some(&quit_sessions(1, &[Some(1)])));
        assert_eq!(
            one.detail,
            "This development build runs its own server. Quitting ends its 1 Terminal Session and stops its running commands."
        );
        assert_eq!(one.buttons, QuitButtons::DestructiveQuit);
    }

    /// A client whose status item is showing and that has no window open.
    fn windowless_client(
        cx: &mut TestAppContext,
    ) -> Rc<crate::platform::application_quit::testing::RecordingApplicationQuitAdapter> {
        let quit = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        cx.update(crate::ui::init).unwrap();
        cx.update(|cx| {
            init(cx, application_menu(), quit.clone()).unwrap();
            crate::status_menu::set_available(cx, true);
        });
        quit
    }

    #[gpui::test]
    fn windowless_dismissal_is_silent_and_actual_exit_always_asks(cx: &mut TestAppContext) {
        let quit = windowless_client(cx);
        quit.simulate_native_request(ApplicationQuitRequest::Dismissal);
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt());
        assert!(cx.update(|cx| cx.windows().is_empty()));
        for answer in ["Cancel", "Quit"] {
            quit.simulate_native_request(ApplicationQuitRequest::Exit);
            cx.run_until_parked();
            let (title, _) = cx
                .pending_prompt()
                .expect("even an idle client confirms exit");
            let name = crate::application_identity::ApplicationIdentity::current().display_name();
            assert_eq!(title, format!("Quit {name}?"));
            let windows = cx.update(|cx| cx.windows());
            assert_eq!(windows.len(), 1);
            assert!(windows[0].downcast::<WorkspaceManager>().is_none());
            cx.simulate_prompt_answer(answer);
            cx.run_until_parked();
            assert!(cx.update(|cx| cx.windows().is_empty()));
        }
        assert_eq!(quit.confirmations(), 1);
    }

    /// A windowless client whose status item is showing, on a host that asks the warning by
    /// itself. The returned list records each change to its Dock presence.
    fn windowless_client_asking_alone(
        cx: &mut TestAppContext,
    ) -> (
        Rc<crate::platform::application_quit::testing::RecordingApplicationQuitAdapter>,
        Rc<RefCell<Vec<bool>>>,
    ) {
        let quit = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::presenting_windowless(
                crate::platform::application_quit::LastWindowPolicy::KeepRunning,
            ),
        );
        let dock = Rc::new(RefCell::new(Vec::new()));
        cx.update(crate::ui::init).unwrap();
        cx.update(|cx| {
            init(cx, application_menu(), quit.clone()).unwrap();
            let recorded = Rc::clone(&dock);
            crate::status_menu::set_dock(
                cx,
                Rc::new(move |visible| recorded.borrow_mut().push(visible)),
            );
            crate::status_menu::set_available(cx, true);
        });
        (quit, dock)
    }

    #[gpui::test]
    fn windowless_exit_asks_by_itself_and_stays_in_the_menu_bar(cx: &mut TestAppContext) {
        let (quit, dock) = windowless_client_asking_alone(cx);
        let name = crate::application_identity::ApplicationIdentity::current().display_name();
        // A modal loop that ends without a button answers nothing, as Cancel does.
        for answer in [Some(QuitAnswer::Cancel), None, Some(QuitAnswer::Quit)] {
            quit.simulate_native_request(ApplicationQuitRequest::Exit);
            cx.run_until_parked();
            let prompts = quit.windowless_prompts();
            assert_eq!(prompts.len(), 1, "even an idle client confirms exit");
            assert_eq!(prompts[0].title, format!("Quit {name}?"));
            assert_eq!(prompts[0].buttons, QuitButtons::Quit);
            assert!(!cx.has_pending_prompt());
            assert!(
                cx.update(|cx| cx.windows().is_empty()),
                "asking opens no window"
            );
            quit.answer_windowless(answer);
            cx.run_until_parked();
            assert!(cx.update(|cx| cx.windows().is_empty()), "{answer:?}");
        }
        assert_eq!(quit.confirmations(), 1, "only Quit exits");
        assert!(dock.borrow().is_empty(), "the client never joins the Dock");
    }

    /// The coordinator takes the keyboard before it makes the window key and presents the
    /// warning on it, because the menu bar item can ask while another app is in front.
    const ASKING_ON_A_WINDOW: [AskingOnWindow; 2] =
        [AskingOnWindow::TookKeyboard, AskingOnWindow::Presented];

    #[gpui::test]
    fn exit_with_a_window_open_asks_on_that_window(cx: &mut TestAppContext) {
        let (quit, dock) = windowless_client_asking_alone(cx);
        cx.add_window(|_, _| gpui::EmptyView);
        for answer in ["Cancel", "Quit"] {
            quit.simulate_native_request(ApplicationQuitRequest::Exit);
            cx.run_until_parked();
            assert!(
                cx.has_pending_prompt(),
                "the warning is a sheet on the window"
            );
            assert_eq!(quit.take_asking_on_window(), ASKING_ON_A_WINDOW);
            assert!(quit.windowless_prompts().is_empty());
            assert_eq!(
                cx.update(|cx| cx.windows().len()),
                1,
                "no other window opens"
            );
            cx.simulate_prompt_answer(answer);
            cx.run_until_parked();
            assert_eq!(cx.update(|cx| cx.windows().len()), 1);
        }
        assert_eq!(quit.confirmations(), 1);
        assert!(dock.borrow().is_empty());
    }

    #[gpui::test]
    fn windowless_development_quit_reports_an_unavailable_reading_by_itself(
        cx: &mut TestAppContext,
    ) {
        let (quit, dock) = windowless_client_asking_alone(cx);
        let coordinator =
            development_coordinator(&quit, vec![Err(spaceterm_server::ServerError::NotRunning)]);
        cx.update(|cx| coordinator.exit(cx));
        cx.run_until_parked();
        let prompts = quit.windowless_prompts();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].title, "Couldn\u{2019}t check Terminal Sessions");
        assert_eq!(prompts[0].buttons, QuitButtons::CancelOnly);
        assert!(cx.update(|cx| cx.windows().is_empty()));
        quit.answer_windowless(Some(QuitAnswer::Cancel));
        cx.run_until_parked();
        assert_eq!(
            *coordinator.state.borrow(),
            ApplicationQuitCoordinatorState::Idle
        );
        assert_eq!(quit.confirmations(), 0);
        assert!(cx.update(|cx| cx.windows().is_empty()));
        assert!(dock.borrow().is_empty());
    }

    #[gpui::test]
    fn development_notice_on_a_window_takes_the_keyboard_first(cx: &mut TestAppContext) {
        let (quit, _) = windowless_client_asking_alone(cx);
        cx.add_window(|_, _| gpui::EmptyView);
        let coordinator =
            development_coordinator(&quit, vec![Err(spaceterm_server::ServerError::NotRunning)]);
        cx.update(|cx| coordinator.exit(cx));
        cx.run_until_parked();
        assert_eq!(
            cx.pending_prompt().unwrap().0,
            "Couldn\u{2019}t check Terminal Sessions"
        );
        assert!(quit.windowless_prompts().is_empty());
        assert_eq!(quit.take_asking_on_window(), ASKING_ON_A_WINDOW);
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    }

    type QuitReading =
        Result<spaceterm_server::hierarchy::CloseInventory, spaceterm_server::ServerError>;

    struct ScriptedQuitSessions(std::sync::Mutex<std::collections::VecDeque<QuitReading>>);
    impl QuitSessions for ScriptedQuitSessions {
        fn observe(&self) -> QuitReading {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected server observation")
        }
    }

    fn development_quit_coordinator(
        cx: &mut TestAppContext,
        readings: Vec<QuitReading>,
    ) -> (
        Rc<ApplicationQuitCoordinator>,
        Rc<crate::platform::application_quit::testing::RecordingApplicationQuitAdapter>,
    ) {
        let quit = windowless_client(cx);
        (development_coordinator(&quit, readings), quit)
    }

    fn development_coordinator(
        quit: &Rc<crate::platform::application_quit::testing::RecordingApplicationQuitAdapter>,
        readings: Vec<QuitReading>,
    ) -> Rc<ApplicationQuitCoordinator> {
        let adapter: Rc<dyn ApplicationQuitAdapter> = quit.clone();
        Rc::new(ApplicationQuitCoordinator {
            adapter: Rc::downgrade(&adapter),
            state: RefCell::new(ApplicationQuitCoordinatorState::Idle),
            status_area: StatusAreaName::MenuBar,
            sessions: Some(Arc::new(ScriptedQuitSessions(std::sync::Mutex::new(
                readings.into(),
            )))),
        })
    }

    fn quit_sessions(
        boot: u8,
        sessions: &[Option<u8>],
    ) -> spaceterm_server::hierarchy::CloseInventory {
        use spaceterm_server::hierarchy::{CloseInventory, ConfirmedPane};
        CloseInventory {
            boot: [boot; 32],
            sessions: sessions
                .iter()
                .enumerate()
                .map(|(index, session)| ConfirmedPane {
                    id: [index as u8 + 1; 32],
                    session: session.map(|id| [id; 32]),
                })
                .collect(),
            controls: Vec::new(),
        }
    }

    #[gpui::test]
    fn development_quit_counts_live_server_sessions_without_windows(cx: &mut TestAppContext) {
        let sessions = quit_sessions(1, &[Some(1), None, Some(3), Some(4)]);
        let (coordinator, quit) =
            development_quit_coordinator(cx, vec![Ok(sessions.clone()), Ok(sessions)]);
        assert!(cx.update(|cx| workspace_windows(cx).is_empty()));
        cx.update(|cx| coordinator.exit(cx));
        cx.run_until_parked();
        let (_, detail) = cx
            .pending_prompt()
            .expect("server-owned work confirms without windows");
        assert_eq!(
            detail,
            "This development build runs its own server. Quitting ends its 3 Terminal Sessions and stops their running commands."
        );
        cx.simulate_prompt_answer("Quit");
        cx.run_until_parked();
        assert_eq!(quit.confirmations(), 1);
        assert!(cx.update(|cx| workspace_windows(cx).is_empty()));
    }

    #[gpui::test]
    fn development_quit_rechecks_hidden_sessions_after_confirmation(cx: &mut TestAppContext) {
        for changed in [quit_sessions(1, &[Some(2)]), quit_sessions(2, &[Some(1)])] {
            let (coordinator, quit) = development_quit_coordinator(
                cx,
                vec![Ok(quit_sessions(1, &[Some(1)])), Ok(changed)],
            );
            cx.update(|cx| coordinator.exit(cx));
            cx.run_until_parked();
            cx.simulate_prompt_answer("Quit");
            cx.run_until_parked();
            assert!(
                cx.has_pending_prompt(),
                "a replacement or restarted server needs fresh approval"
            );
            assert_eq!(quit.confirmations(), 0);
            cx.simulate_prompt_answer("Cancel");
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn development_quit_fails_safely_when_server_observation_fails(cx: &mut TestAppContext) {
        for readings in [
            vec![Err(spaceterm_server::ServerError::NotRunning)],
            vec![
                Ok(quit_sessions(1, &[])),
                Err(spaceterm_server::ServerError::NotRunning),
            ],
        ] {
            let initial_succeeds = readings.len() > 1;
            let (coordinator, quit) = development_quit_coordinator(cx, readings);
            cx.update(|cx| coordinator.exit(cx));
            cx.run_until_parked();
            if initial_succeeds {
                cx.simulate_prompt_answer("Quit");
                cx.run_until_parked();
            }
            assert_eq!(
                cx.pending_prompt().unwrap().0,
                "Couldn\u{2019}t check Terminal Sessions"
            );
            assert_eq!(quit.confirmations(), 0);
            cx.simulate_prompt_answer("Cancel");
            cx.run_until_parked();
            assert_eq!(
                *coordinator.state.borrow(),
                ApplicationQuitCoordinatorState::Idle
            );
        }
    }

    #[gpui::test]
    fn system_session_end_never_prompts_even_during_another_quit(cx: &mut TestAppContext) {
        let quit = windowless_client(cx);
        quit.simulate_native_request(ApplicationQuitRequest::Exit);
        cx.run_until_parked();
        assert_eq!(
            quit.simulate_native_request(ApplicationQuitRequest::SessionEnd),
            ApplicationQuitDecision::Proceed
        );
        cx.simulate_prompt_answer("Quit");
        cx.run_until_parked();
        assert_eq!(quit.confirmations(), 0);
    }

    #[gpui::test]
    fn last_window_policy_waits_for_all_windows_and_requests_quit_once(cx: &mut TestAppContext) {
        struct Quit(std::cell::Cell<usize>);
        impl ApplicationQuitAdapter for Quit {
            fn last_window_policy(&self) -> crate::platform::application_quit::LastWindowPolicy {
                crate::platform::application_quit::LastWindowPolicy::Quit
            }
            fn install(
                &self,
                _: ApplicationQuitHandler,
            ) -> Result<(), crate::platform::application_quit::ApplicationQuitError> {
                Ok(())
            }
            fn request_quit(&self, _: &mut App) {
                self.0.set(self.0.get() + 1);
            }
            fn confirm_quit(&self, _: &mut App) {}
        }
        let quit = Rc::new(Quit(std::cell::Cell::new(0)));
        cx.update(crate::ui::init).unwrap();
        cx.update(|cx| init(cx, application_menu(), quit.clone()).unwrap());
        let first = cx.add_window(|_, _| gpui::EmptyView);
        let second = cx.add_window(|_, _| gpui::EmptyView);
        cx.update(|cx| {
            first
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();
        assert_eq!(quit.0.get(), 0);
        cx.update(|cx| {
            second
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();
        assert_eq!(quit.0.get(), 1);
    }

    #[gpui::test]
    fn quit_cancel_keeps_a_window_reachable_when_restoration_is_unavailable(
        cx: &mut TestAppContext,
    ) {
        let quit = Rc::new(crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::with_last_window_policy(
            crate::platform::application_quit::LastWindowPolicy::Quit,
        ));
        cx.update(crate::ui::init).unwrap();
        cx.update(|cx| init(cx, application_menu(), quit.clone()).unwrap());
        quit.simulate_native_request(ApplicationQuitRequest::Exit);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt());
        let window = cx.update(|cx| {
            assert_eq!(
                cx.windows().len(),
                1,
                "an unavailable normal window cannot leave the client unreachable"
            );
            cx.windows()[0]
        });
        cx.update(|cx| {
            window
                .update(cx, |_, window, _| window.remove_window())
                .unwrap()
        });
        cx.run_until_parked();
        assert!(
            cx.has_pending_prompt(),
            "closing the retained window still follows quit policy"
        );
        assert_eq!(quit.confirmations(), 0);
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    }

    /// Counts quit requests under one last-window policy.
    struct CountingQuit {
        policy: crate::platform::application_quit::LastWindowPolicy,
        requests: std::cell::Cell<usize>,
    }

    impl ApplicationQuitAdapter for CountingQuit {
        fn last_window_policy(&self) -> crate::platform::application_quit::LastWindowPolicy {
            self.policy
        }
        fn install(
            &self,
            _: ApplicationQuitHandler,
        ) -> Result<(), crate::platform::application_quit::ApplicationQuitError> {
            Ok(())
        }
        fn request_quit(&self, _: &mut App) {
            self.requests.set(self.requests.get() + 1);
        }
        fn confirm_quit(&self, _: &mut App) {}
    }

    fn close_all_windows_with(
        policy: crate::platform::application_quit::LastWindowPolicy,
        status_area: bool,
        cx: &mut TestAppContext,
    ) -> (Rc<CountingQuit>, Rc<RefCell<Vec<bool>>>) {
        let quit = Rc::new(CountingQuit {
            policy,
            requests: std::cell::Cell::new(0),
        });
        let dock = Rc::new(RefCell::new(Vec::new()));
        cx.update(crate::ui::init).unwrap();
        cx.update(|cx| {
            init(cx, application_menu(), quit.clone()).unwrap();
            let recorded = Rc::clone(&dock);
            crate::status_menu::set_dock(
                cx,
                Rc::new(move |visible| recorded.borrow_mut().push(visible)),
            );
            crate::status_menu::set_available(cx, status_area);
        });
        cx.add_window(|_, _| gpui::EmptyView);
        cx.add_window(|_, _| gpui::EmptyView);
        cx.update(|cx| cx.dispatch_action(&CloseAllWindows));
        cx.run_until_parked();
        assert!(cx.update(|cx| cx.windows().is_empty()));
        (quit, dock)
    }

    #[gpui::test]
    fn close_all_windows_leaves_the_dock_and_keeps_the_client_running(cx: &mut TestAppContext) {
        let (quit, dock) = close_all_windows_with(
            crate::platform::application_quit::LastWindowPolicy::KeepRunning,
            true,
            cx,
        );
        assert_eq!(quit.requests.get(), 0);
        assert_eq!(*dock.borrow(), [false]);

        // Opening SpaceTerm again returns the same client to the Dock.
        cx.update(|cx| activate_default_window(cx, None));
        assert_eq!(*dock.borrow(), [false, true]);
    }

    #[gpui::test]
    fn close_all_windows_keeps_a_client_its_status_item_can_reach(cx: &mut TestAppContext) {
        let (quit, _) = close_all_windows_with(
            crate::platform::application_quit::LastWindowPolicy::Quit,
            true,
            cx,
        );
        assert_eq!(quit.requests.get(), 0);
    }

    #[gpui::test]
    fn close_all_windows_exits_a_client_no_status_item_can_reach(cx: &mut TestAppContext) {
        let (quit, _) = close_all_windows_with(
            crate::platform::application_quit::LastWindowPolicy::Quit,
            false,
            cx,
        );
        assert_eq!(quit.requests.get(), 1);
    }

    #[gpui::test]
    fn losing_the_status_area_exits_only_a_windowless_client(cx: &mut TestAppContext) {
        let quit = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        cx.update(crate::ui::init).unwrap();
        cx.update(|cx| init(cx, application_menu(), quit.clone()).unwrap());
        let window = cx.add_window(|_, _| gpui::EmptyView);
        cx.update(|cx| crate::status_menu::set_available(cx, true));
        cx.update(|cx| crate::status_menu::set_available(cx, false));
        cx.run_until_parked();
        // A window still reaches the client, and a status-area host can return.
        assert_eq!(quit.requests(), 0);

        cx.update(|cx| crate::status_menu::set_available(cx, true));
        cx.update(|cx| {
            window
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();
        assert_eq!(
            quit.requests(),
            0,
            "the status item reaches a windowless client"
        );
        cx.update(|cx| crate::status_menu::set_available(cx, false));
        cx.run_until_parked();
        assert_eq!(quit.requests(), 1, "nothing reaches the client any more");
        assert!(
            cx.has_pending_prompt(),
            "status loss still needs exit approval"
        );
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt());
        assert_eq!(quit.confirmations(), 0);
    }

    #[gpui::test]
    fn quitting_waits_for_an_admitted_server_operation(cx: &mut TestAppContext) {
        let quit = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        cx.update(crate::ui::init).unwrap();
        cx.update(|cx| init(cx, application_menu(), quit.clone()).unwrap());
        cx.update(crate::status_menu::begin_operation);
        cx.update(|cx| cx.dispatch_action(&QuitApplication));
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Quit");
        cx.run_until_parked();
        assert_eq!(quit.confirmations(), 0, "a running Stop is never cut off");

        cx.update(crate::status_menu::end_operation);
        cx.run_until_parked();
        assert_eq!(quit.confirmations(), 1);
    }

    #[gpui::test]
    fn configured_shortcuts_should_bind_global_application_actions(cx: &mut TestAppContext) {
        cx.update(crate::ui::init).expect("UI initialization");
        cx.update(|cx| init(cx, application_menu(), application_quit()).unwrap());
        let expected = [
            ("cmd-q", DismissApplication.name()),
            ("cmd-h", HideApplication.name()),
            ("alt-cmd-h", HideOtherApplications.name()),
            ("cmd-m", MinimizeWindow.name()),
            ("ctrl-cmd-f", ToggleFullScreen.name()),
            ("fn-f", ToggleFullScreen.name()),
        ];
        let actual = cx.update(|cx| {
            expected
                .iter()
                .map(|(shortcut, _)| {
                    let keystroke = Keystroke::parse(shortcut).unwrap_or_else(|error| {
                        panic!("invalid test shortcut {shortcut}: {error}")
                    });
                    let bindings = cx.all_bindings_for_input(&[keystroke]);
                    (
                        *shortcut,
                        bindings
                            .last()
                            .map(|binding| binding.action().name())
                            .unwrap_or(""),
                    )
                })
                .collect::<Vec<_>>()
        });

        assert_eq!(actual.as_slice(), expected);
    }

    #[gpui::test]
    fn native_copy_command_dispatches_semantic_copy_to_the_terminal(cx: &mut TestAppContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        cx.update(|cx| init(cx, application_menu(), application_quit()).unwrap());
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(records.clone()).with_selection_copy_response(Ok(
                Some(SelectionCopy {
                    plain_text: "native command copy".into(),
                    html: None,
                }),
            )),
        );
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_local_directory(PathBuf::from(
                "/tmp/spaceterm-native-copy-command-test",
            )),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, cx| pane.focus(window, cx));
        });
        cx.run_until_parked();
        cx.write_to_clipboard(ClipboardItem::new_string("stale clipboard".to_owned()));

        cx.simulate_keystrokes("cmd-c");
        cx.run_until_parked();

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("native command copy".to_owned())
        );
    }

    #[gpui::test]
    fn native_application_actions_should_dispatch_through_the_application_menu_adapter(
        cx: &mut TestAppContext,
    ) {
        let menu = Rc::new(
            crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
        );
        let adapter: Rc<dyn ApplicationMenuAdapter> = menu.clone();
        cx.update(crate::ui::init).unwrap();
        cx.update(|cx| init(cx, adapter, application_quit()).unwrap());

        cx.update(|cx| {
            cx.dispatch_action(&ShowAboutApplication);
            cx.dispatch_action(&OpenApplicationHelp);
            cx.dispatch_action(&ZoomActiveWindow);
            cx.dispatch_action(&BringAllWindowsToFront);
        });

        assert_eq!(
            menu.commands(),
            [
                ApplicationMenuCommand::ShowAbout,
                ApplicationMenuCommand::OpenHelp,
                ApplicationMenuCommand::ZoomActiveWindow,
                ApplicationMenuCommand::BringAllWindowsToFront,
            ]
        );
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use crate::platform::services_registration::{ServicesRegistration, ServicesRegistrationError};
    use crate::platform::window_movement::{
        OperatingSystemWindowDragPlatform, RecordingOperatingSystemWindowDragPlatform,
        WindowMovementFactory,
    };
    use crate::terminal::TerminalSessionFactory;
    use crate::terminal::native_services::services::ServiceEndpoint;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingServices {
        calls: RefCell<Vec<&'static str>>,
        endpoints: RefCell<Vec<Rc<dyn ServiceEndpoint>>>,
        windows: RefCell<Vec<gpui::AnyWindowHandle>>,
    }
    impl ServicesRegistration for RecordingServices {
        fn register(&self) -> Result<(), ServicesRegistrationError> {
            self.calls.borrow_mut().push("register");
            Ok(())
        }
        fn install(
            &self,
            window: &gpui::Window,
            endpoint: Rc<dyn ServiceEndpoint>,
        ) -> Result<(), ServicesRegistrationError> {
            assert_eq!(self.calls.borrow().first(), Some(&"register"));
            self.calls.borrow_mut().push("install");
            self.windows.borrow_mut().push(window.window_handle());
            self.endpoints.borrow_mut().push(endpoint);
            Ok(())
        }
    }
    #[derive(Default)]
    struct RecordingMovement(RefCell<Vec<Rc<dyn OperatingSystemWindowDragPlatform>>>);
    impl WindowMovementFactory for RecordingMovement {
        fn create(&self) -> Rc<dyn OperatingSystemWindowDragPlatform> {
            let movement: Rc<dyn OperatingSystemWindowDragPlatform> =
                Rc::new(RecordingOperatingSystemWindowDragPlatform::default());
            self.0.borrow_mut().push(Rc::clone(&movement));
            movement
        }
    }
    /// Opens no selected file.
    struct NoSelectedFiles;
    impl crate::platform::selected_file::SelectedFileOpener for NoSelectedFiles {
        fn open(
            &self,
            _: &std::path::Path,
        ) -> Result<std::fs::File, crate::platform::selected_file::SelectedFileOpenError> {
            Err(crate::platform::selected_file::SelectedFileOpenError)
        }
    }
    /// Delivers no native desktop callbacks.
    struct NoDesktopEvents;
    impl crate::platform::desktop_events::DesktopEventAdapter for NoDesktopEvents {
        fn install(&self, _: &mut App) {}
    }
    fn parts(
        services: Rc<RecordingServices>,
        movement: Rc<RecordingMovement>,
    ) -> HostCompositionParts {
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            crate::terminal::testing::TestTerminalSessionFactory::new(Default::default()),
        );
        HostCompositionParts {
            profile: crate::desktop_profile::testing_profile(spaceterm_ui::TextDirection::LeftToRight),
            home_directory: std::env::temp_dir(),
            adapters: ApplicationCapabilities {
                updates: Rc::new(crate::updates::testing::RecordingAdapter::default()),
                selected_files: Arc::new(NoSelectedFiles),
                settings_file: Rc::new(
                    crate::platform::settings_file::testing::RecordingSettingsFile::default(),
                ),
                application_menu: Rc::new(
                    crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
                ),
                desktop_events: Rc::new(NoDesktopEvents),
                application_quit: Rc::new(
                    crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
                ),
                local_filesystem: crate::platform::local_filesystem::LocalFilesystemAuthority::testing(),
                key_input: Rc::new(crate::terminal::GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
                native_services: crate::terminal::native_services::testing::adapters(),
                lifecycle: crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(),
                capture_access: None,
                permission_access: None,
                setup_guide: None,
                theme_registry: Arc::new(crate::theme_registry::testing::MemoryTransport::default()),
                repository_status: None,
                background_images: None,
                local_server: crate::ui::launch_testing::presenting(session_factory),
                ssh_setup: None,
                direct_servers: None,
                server_context_paths: None,
                command_line_tool: None,
                desktop_channel: None,
            },
            services: Some(services),
            window_movement: movement,
            window_frame: crate::platform::window_frame::WindowFrameGeometry::default(),
            window_chrome: crate::platform::window_chrome::WindowChrome::native(None),
        }
    }
    #[test]
    fn invalid_composition_has_only_closed_failure_classification() {
        let mut invalid_directory = parts(Rc::default(), Rc::default());
        invalid_directory.home_directory = "sensitive-relative-value".into();
        let mut invalid_titlebar = parts(Rc::default(), Rc::default());
        invalid_titlebar.window_chrome =
            crate::platform::window_chrome::WindowChrome::native(Some(gpui::TitlebarOptions {
                traffic_light_position: Some(gpui::point(gpui::px(1.0), gpui::px(1.0))),
                appears_transparent: false,
                ..gpui::TitlebarOptions::default()
            }));
        for parts in [invalid_directory, invalid_titlebar] {
            let error = HostComposition::new(parts).err().unwrap();
            assert_eq!(
                error,
                crate::desktop_profile::DesktopProfileError::InvalidCombination
            );
            assert_eq!(
                error.to_string(),
                "desktop policy and capabilities disagree"
            );
        }
    }
    #[gpui::test]
    fn runtime_registers_once_and_installs_distinct_exact_window_endpoints(
        cx: &mut gpui::TestAppContext,
    ) {
        let services = Rc::new(RecordingServices::default());
        let movement = Rc::new(RecordingMovement::default());
        let host = HostComposition::new(parts(Rc::clone(&services), Rc::clone(&movement))).unwrap();
        let (first, second) = cx.update(|cx| {
            let first = start_application(cx, &host).unwrap();
            let second = open(cx, &host).unwrap();
            (first, second)
        });
        cx.run_until_parked();
        assert_eq!(*services.calls.borrow(), ["register", "install", "install"]);
        assert!(*services.windows.borrow() == [first.into(), second.into()]);
        assert!(!Rc::ptr_eq(
            &movement.0.borrow()[0],
            &movement.0.borrow()[1]
        ));
        cx.update(|cx| {
            first
                .update(cx, |manager, _, cx| {
                    manager.assert_application_capabilities(&host.adapters, cx)
                })
                .unwrap();
            second
                .update(cx, |manager, _, cx| {
                    manager.assert_application_capabilities(&host.adapters, cx)
                })
                .unwrap();
            first
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();
        assert_eq!(
            services.endpoints.borrow()[0].status(),
            NativeServiceStatus::default()
        );
        assert!(cx.update(|cx| second.update(cx, |_, _, _| ()).is_ok()));
    }

    #[gpui::test]
    fn the_first_workspace_window_after_launch_opens_windowed_at_the_default_size(
        cx: &mut gpui::TestAppContext,
    ) {
        let host = host_with_settings();
        let window = cx.update(|cx| start_application(cx, &host).unwrap());
        cx.run_until_parked();

        let is_maximized = cx.update(|cx| {
            window
                .update(cx, |_, window, _| window.is_maximized())
                .unwrap()
        });
        assert!(!is_maximized, "the workspace window should open windowed");

        let initial_size = cx.update(|cx| {
            window
                .update(cx, |_, window, _| window.window_bounds().get_bounds().size)
                .unwrap()
        });
        assert_eq!(initial_size, size(px(900.0), px(580.0)));
    }

    #[gpui::test]
    fn a_workspace_window_reopened_after_closing_returns_where_it_was(
        cx: &mut gpui::TestAppContext,
    ) {
        let host = host_with_settings();
        let window = cx.update(|cx| start_application(cx, &host).unwrap());
        cx.run_until_parked();
        let moved = Bounds {
            origin: gpui::point(px(120.0), px(90.0)),
            size: size(px(1100.0), px(720.0)),
        };
        let bounds = |window: gpui::WindowHandle<WorkspaceManager>,
                      cx: &mut gpui::TestAppContext| {
            window
                .update(cx, |_, window, _| window.window_bounds())
                .unwrap()
        };
        let close = |window: gpui::WindowHandle<WorkspaceManager>,
                     cx: &mut gpui::TestAppContext| {
            window
                .update(cx, |manager, window, cx| {
                    manager.request_window_close(window, cx)
                })
                .unwrap();
            cx.run_until_parked();
            assert!(cx.update(|cx| workspace_windows(cx).is_empty()));
        };
        window
            .update(cx, |_, window, _| window.set_bounds(moved, None))
            .unwrap();
        close(window, cx);

        let reopened = cx.update(|cx| open(cx, &host).unwrap());
        cx.run_until_parked();
        assert_eq!(bounds(reopened, cx), WindowBounds::Windowed(moved));

        // A real quit drops the view, so the next launch opens at the default size again.
        close(reopened, cx);
        cx.update(crate::ui::remembered_view::forget);
        let relaunched = cx.update(|cx| open(cx, &host).unwrap());
        cx.run_until_parked();
        assert_eq!(
            bounds(relaunched, cx).get_bounds().size,
            size(px(900.0), px(580.0))
        );
    }

    #[gpui::test]
    fn workspace_window_should_leave_titlebar_moves_to_its_drag_regions(
        cx: &mut gpui::TestAppContext,
    ) {
        let host = host_with_settings();
        let options = cx.update(|cx| {
            start_application(cx, &host).unwrap();
            workspace_window_options(&host, cx)
        });

        assert!(options.app_owns_titlebar_drag);
    }

    #[gpui::test]
    fn new_workspace_action_should_restore_window_and_create_a_workspace_when_headless(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};

        let records = TestTerminalSessionRecords::default();
        let services = Rc::new(RecordingServices::default());
        let mut wiring = parts(Rc::clone(&services), Rc::default());
        wiring.adapters.local_server = crate::ui::launch_testing::presenting(Rc::new(
            TestTerminalSessionFactory::new(records.clone()),
        ));
        let host = Rc::new(HostComposition::new(wiring).unwrap());
        let original = cx.update(|cx| {
            let original = start_application(cx, &host).unwrap();
            install_headless_window_actions(cx, Rc::clone(&host));
            original
        });
        cx.run_until_parked();
        cx.update(|cx| {
            original
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();

        assert!(cx.update(|cx| cx.is_action_available(&NewWorkspace)));
        cx.update(|cx| cx.dispatch_action(&NewWorkspace));
        cx.run_until_parked();

        // The restored window presents the server's Default Workspace and creates the one New
        // Workspace asked for.
        cx.update(|cx| {
            let restored = cx.windows()[0].downcast::<WorkspaceManager>().unwrap();
            assert_eq!(restored.read(cx).unwrap().workspace_count(), 2);
        });

        assert_eq!(
            (
                cx.windows().len(),
                records.session_count(),
                services.calls.borrow().clone(),
            ),
            (1, 1, vec!["register", "install", "install"]),
            "the provisional Workspace starts no Terminal Session before server admission"
        );
    }

    #[gpui::test]
    fn default_window_restoration_should_not_duplicate_an_existing_window(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};

        let records = TestTerminalSessionRecords::default();
        let services = Rc::new(RecordingServices::default());
        let mut wiring = parts(Rc::clone(&services), Rc::default());
        wiring.adapters.local_server = crate::ui::launch_testing::presenting(Rc::new(
            TestTerminalSessionFactory::new(records.clone()),
        ));
        let host = HostComposition::new(wiring).unwrap();
        cx.update(|cx| {
            start_application(cx, &host).unwrap();
            restore_default_window(cx, &host);
        });
        cx.run_until_parked();

        assert_eq!(
            (
                cx.windows().len(),
                records.session_count(),
                services.calls.borrow().clone(),
            ),
            (1, 1, vec!["register", "install"])
        );
    }

    fn host_with_settings() -> Rc<HostComposition> {
        Rc::new(
            HostComposition::new(parts(Rc::default(), Rc::default()))
                .unwrap()
                .with_appearance(
                    Arc::new(crate::settings::storage::testing::MemoryStorage::default()),
                    Rc::new(
                        crate::platform::appearance::testing::RecordingAppearancePlatform::default(
                        ),
                    ),
                ),
        )
    }

    fn host_with_storage(
        storage: Arc<crate::settings::storage::testing::MemoryStorage>,
    ) -> HostComposition {
        HostComposition::new(parts(Rc::default(), Rc::default()))
            .unwrap()
            .with_appearance(
                storage,
                Rc::new(
                    crate::platform::appearance::testing::RecordingAppearancePlatform::default(),
                ),
            )
    }

    #[test]
    fn startup_registers_bundled_terminal_fonts_before_catalog_discovery() {
        let cx = crate::ui::appearance_runtime::font_catalog_test_app();
        let host = host_with_settings();
        cx.update(|cx| {
            initialize_application(cx, &host).unwrap();
            assert_eq!(
                crate::ui::appearance_runtime::current(cx)
                    .terminal
                    .typography
                    .regular
                    .primary_family,
                "SpaceTerm Default"
            );
        });
    }

    fn click(selector: &'static str, cx: &mut gpui::VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} should be painted"));
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn launch_offers_settings_recovery_once_for_malformed_settings(cx: &mut gpui::TestAppContext) {
        let storage = Arc::new(crate::settings::storage::testing::MemoryStorage::default());
        storage.corrupt();
        let host = host_with_storage(storage.clone());
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        let cx = &mut gpui::VisualTestContext::from_window(workspace.into(), cx);
        cx.run_until_parked();
        assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));

        click("modal-action-settings-recovery-reset", cx);

        assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
        assert_eq!(
            storage.backup().as_deref(),
            Some(crate::settings::storage::testing::CORRUPT_DOCUMENT)
        );
        assert!(storage.document().is_some());
        let status = cx.update(|_, cx| {
            cx.global::<crate::ui::appearance_runtime::AppearanceRuntime>()
                .settings
                .snapshot()
                .status
        });
        assert_eq!(status, None);
        // Only launch asks. A reopened Workspace window does not repeat the prompt.
        let reopened = cx.update(|_, cx| open(cx, &host).unwrap());
        let cx = &mut gpui::VisualTestContext::from_window(reopened.into(), cx);
        cx.run_until_parked();
        assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    }

    #[gpui::test]
    fn the_first_workspace_window_after_launch_opens_its_sidebar_in_the_default_state(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut document = crate::settings::SettingsDocument::default();
        document.sidebar.default_state = crate::settings::sidebar::SidebarState::Collapsed;
        let host = host_with_storage(
            crate::settings::storage::testing::MemoryStorage::with_document(&document),
        );
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        cx.run_until_parked();
        let sidebar_visible = |workspace: gpui::WindowHandle<WorkspaceManager>,
                               cx: &mut gpui::TestAppContext| {
            workspace
                .read_with(cx, |manager, cx| manager.sidebar_visible(cx))
                .unwrap()
        };
        assert!(!sidebar_visible(workspace, cx));
        {
            let cx = &mut gpui::VisualTestContext::from_window(workspace.into(), cx);
            let root = cx.debug_bounds("workspace-manager").unwrap();
            let content = cx.debug_bounds("tab-manager-content").unwrap();
            assert_eq!(
                content.origin.x, root.origin.x,
                "the first Tab lays out without the collapsed sidebar"
            );
        }

        cx.update(|cx| {
            let settings = &cx
                .global::<crate::ui::appearance_runtime::AppearanceRuntime>()
                .settings;
            let committed = settings.snapshot().committed;
            let mut expanded = (*committed).clone();
            expanded.sidebar.default_state = crate::settings::sidebar::SidebarState::Expanded;
            settings
                .update_committed(committed.revision, expanded)
                .unwrap()
                .run()
                .unwrap();
        });
        cx.run_until_parked();
        assert!(
            !sidebar_visible(workspace, cx),
            "an open window keeps its sidebar when the default changes"
        );
        let close = |workspace: gpui::WindowHandle<WorkspaceManager>,
                     cx: &mut gpui::TestAppContext| {
            workspace
                .update(cx, |manager, window, cx| {
                    manager.request_window_close(window, cx)
                })
                .unwrap();
            cx.run_until_parked();
        };

        // Reopening while the client keeps running presents the sidebar the window left.
        close(workspace, cx);
        let reopened = cx.update(|cx| open(cx, &host).unwrap());
        cx.run_until_parked();
        assert!(!sidebar_visible(reopened, cx));

        // After a real quit, the next launch's window follows Settings again.
        close(reopened, cx);
        cx.update(crate::ui::remembered_view::forget);
        let relaunched = cx.update(|cx| open(cx, &host).unwrap());
        cx.run_until_parked();
        assert!(sidebar_visible(relaunched, cx));
    }

    #[gpui::test]
    fn settings_recovery_can_be_declined_without_changing_the_file(cx: &mut gpui::TestAppContext) {
        use crate::settings::storage::SettingsStorage as _;
        let storage = Arc::new(crate::settings::storage::testing::MemoryStorage::default());
        storage.corrupt();
        let host = host_with_storage(storage.clone());
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        let cx = &mut gpui::VisualTestContext::from_window(workspace.into(), cx);
        cx.run_until_parked();

        click("modal-action-settings-recovery-not-now", cx);

        assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
        assert_eq!(storage.backup(), None);
        assert_eq!(
            storage.read().unwrap().unwrap().bytes,
            crate::settings::storage::testing::CORRUPT_DOCUMENT
        );
        assert_eq!(storage.writes(), 0);
        let status = cx.update(|_, cx| {
            cx.global::<crate::ui::appearance_runtime::AppearanceRuntime>()
                .settings
                .snapshot()
                .status
        });
        assert!(status.is_some_and(crate::settings::SettingsError::is_malformed));
    }

    #[gpui::test]
    fn launch_does_not_offer_recovery_for_settings_it_cannot_safely_replace(
        cx: &mut gpui::TestAppContext,
    ) {
        let storage = Arc::new(crate::settings::storage::testing::MemoryStorage::default());
        storage.fail_reads(Some(crate::settings::storage::StorageError::Unsafe));
        let host = host_with_storage(storage);
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        let cx = &mut gpui::VisualTestContext::from_window(workspace.into(), cx);
        cx.run_until_parked();

        assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    }

    #[gpui::test]
    fn launch_does_not_offer_recovery_for_settings_from_a_newer_spaceterm(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut newer = serde_json::to_value(crate::settings::SettingsDocument::default()).unwrap();
        let version = newer["schema_version"].as_u64().unwrap();
        newer["schema_version"] = serde_json::json!(version + 1);
        let storage = crate::settings::storage::testing::MemoryStorage::with_bytes(
            serde_json::to_vec(&newer).unwrap(),
        );
        let host = host_with_storage(storage);
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        let cx = &mut gpui::VisualTestContext::from_window(workspace.into(), cx);
        cx.run_until_parked();

        assert!(!cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    }

    #[gpui::test]
    fn launch_gate_prevents_workspace_creation_and_reopen_until_offline_fallback(
        cx: &mut gpui::TestAppContext,
    ) {
        let adapter = Rc::new(crate::updates::testing::RecordingAdapter::available());
        let mut host = host_with_settings();
        Rc::get_mut(&mut host).unwrap().adapters.updates = adapter.clone();
        cx.update(|cx| {
            initialize_application(cx, &host).unwrap();
            let service = cx.global::<crate::updates::UpdateService>().0.clone();
            let resumed_host = host.clone();
            service.update(cx, |updates, cx| {
                updates.begin_launch(
                    Rc::new(move |cx| {
                        open_initial_workspace(cx, &resumed_host).unwrap();
                    }),
                    cx,
                )
            });
            assert!(open(cx, &host).is_err());
            restore_default_window(cx, &host);
            assert!(workspace_windows(cx).is_empty());
        });
        adapter.emit(crate::updates::UpdateEvent::Failed(
            crate::updates::UpdateError::Check,
        ));
        adapter.emit(crate::updates::UpdateEvent::Finished);
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(workspace_windows(cx).len(), 1));
    }

    #[gpui::test]
    fn quitting_during_startup_check_does_not_open_a_workspace(cx: &mut gpui::TestAppContext) {
        quit_during_startup_update(cx, false);
    }

    #[gpui::test]
    fn quitting_during_startup_download_retires_the_launch_timeout(cx: &mut gpui::TestAppContext) {
        quit_during_startup_update(cx, true);
    }

    fn quit_during_startup_update(
        cx: &mut gpui::TestAppContext,
        delayed_download_cancellation: bool,
    ) {
        use crate::updates::{ApplicationUpdates, UpdateEvent, UpdateService};

        let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
        let quit = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        let mut host = host_with_settings();
        let adapters = &mut Rc::get_mut(&mut host).unwrap().adapters;
        adapters.updates = updater.clone();
        adapters.application_quit = quit.clone();
        cx.update(|cx| {
            initialize_application(cx, &host).unwrap();
            let service = cx.global::<UpdateService>().0.clone();
            let resumed_host = host.clone();
            service.update(cx, |updates, cx| {
                updates.begin_launch(
                    Rc::new(move |cx| {
                        open_initial_workspace(cx, &resumed_host).unwrap();
                    }),
                    cx,
                );
            });
        });
        if delayed_download_cancellation {
            updater.emit(UpdateEvent::ReleaseMetadata {
                published_at: crate::updates::now() - 2 * crate::updates::policy::DAY,
                prepared: false,
            });
            updater.emit(UpdateEvent::Available("0.1.1".into()));
        }
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&QuitApplication));
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        assert_eq!(
            updater.cancellations.get(),
            0,
            "Cancel must keep startup intact"
        );
        cx.simulate_prompt_answer("Quit");
        cx.run_until_parked();
        assert_eq!(updater.cancellations.get(), 1);
        assert_eq!(quit.confirmations(), 0);
        if delayed_download_cancellation {
            assert_eq!(updater.downloads.get(), 1);
            cx.executor()
                .advance_clock(std::time::Duration::from_secs(31));
            cx.run_until_parked();
            assert!(cx.update(|cx| workspace_windows(cx).is_empty()));
            // A launch view that fails to open must not revive startup during quit either.
            cx.update(ApplicationUpdates::release_launch);
        }
        updater.emit(UpdateEvent::Finished);
        cx.run_until_parked();
        assert!(cx.update(|cx| workspace_windows(cx).is_empty()));
        assert!(!cx.has_pending_prompt());
        assert_eq!(quit.confirmations(), 1);
    }

    #[gpui::test]
    fn startup_installs_menu_with_shortcuts_before_reading_settings(cx: &mut gpui::TestAppContext) {
        use crate::platform::application_menu::ApplicationMenuError;
        use crate::platform::secure_filesystem::{PrivateFileSnapshot, SecureEntryIdentity};
        use crate::settings::storage::{SettingsStorage, StorageCommit, StorageError};
        use gpui::Action as _;
        use std::sync::atomic::{AtomicBool, Ordering};

        #[derive(Clone, Default)]
        struct StartupProbe(Arc<AtomicBool>);

        impl ApplicationMenuAdapter for StartupProbe {
            fn install(&self, cx: &mut App) -> Result<(), ApplicationMenuError> {
                let shortcut = gpui::Keystroke::parse("cmd-q").unwrap();
                assert!(
                    cx.all_bindings_for_input(&[shortcut])
                        .iter()
                        .any(|binding| { binding.action().name() == DismissApplication.name() })
                );
                assert!(cx.is_action_available(&crate::ui::settings_window::OpenSettings));
                self.0.store(true, Ordering::SeqCst);
                Ok(())
            }

            fn perform(
                &self,
                _: ApplicationMenuCommand,
                _: &mut App,
            ) -> Result<(), ApplicationMenuError> {
                Err(ApplicationMenuError::Unavailable)
            }
        }

        impl SettingsStorage for StartupProbe {
            fn quarantine(&self) -> Result<(), crate::settings::storage::StorageError> {
                Err(crate::settings::storage::StorageError::Unavailable)
            }
            fn read(&self) -> Result<Option<PrivateFileSnapshot>, StorageError> {
                assert!(
                    self.0.load(Ordering::SeqCst),
                    "native menu installation must precede Settings I/O"
                );
                Ok(None)
            }

            fn write(
                &self,
                _: &[u8],
                _: Option<&SecureEntryIdentity>,
            ) -> Result<StorageCommit, StorageError> {
                Err(StorageError::Unavailable)
            }
        }

        let probe = StartupProbe::default();
        let mut wiring = parts(Rc::default(), Rc::default());
        wiring.adapters.application_menu = Rc::new(probe.clone());
        let host = HostComposition::new(wiring).unwrap().with_appearance(
            Arc::new(probe),
            Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
        );
        cx.update(|cx| start_application(cx, &host).unwrap());
    }

    #[gpui::test]
    fn opening_settings_twice_presents_one_window(cx: &mut gpui::TestAppContext) {
        let host = host_with_settings();
        cx.update(|cx| start_application(cx, &host).unwrap());
        cx.run_until_parked();

        cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
        cx.run_until_parked();

        assert_eq!(cx.update(|cx| cx.windows().len()), 2);
        assert_eq!(cx.update(|cx| workspace_windows(cx).len()), 1);
        let settings_window = cx.update(|cx| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<crate::ui::settings_window::SettingsWindow>())
                .expect("Settings window")
        });
        let mut settings_cx = gpui::VisualTestContext::from_window(settings_window.into(), cx);
        assert_eq!(
            settings_cx.window_title().as_deref(),
            Some("Settings"),
            "transparent client chrome must retain the native window identity"
        );
    }

    #[gpui::test]
    fn density_preview_should_reposition_open_workspace_and_settings_traffic_lights(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::appearance::ChromeDensity;
        use crate::platform::window_frame::{TrafficLightPlacement, WindowFrameGeometry};
        use crate::settings::SettingsDocument;
        use gpui::{point, px};

        let geometry = WindowFrameGeometry::new(Some(16.0))
            .with_outer_edge_width(1.0)
            .with_traffic_lights(
                TrafficLightPlacement::new(point(px(15.5), px(14.0)), px(41.0), px(78.0)),
                TrafficLightPlacement::new(point(px(12.0), px(11.0)), px(36.0), px(78.0)),
            );
        let mut wiring = parts(Rc::default(), Rc::default());
        wiring.window_frame = geometry;
        let host = HostComposition::new(wiring).unwrap().with_appearance(
            Arc::new(crate::settings::storage::testing::MemoryStorage::default()),
            Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
        );
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
        cx.run_until_parked();
        let settings_window = cx.update(|cx| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<crate::ui::settings_window::SettingsWindow>())
                .expect("Settings window")
        });

        assert_eq!(
            (
                cx.traffic_light_position_updates(workspace.into()),
                cx.traffic_light_position_updates(settings_window.into()),
            ),
            (
                vec![point(px(15.5), px(14.0))],
                vec![point(px(12.0), px(11.0))],
            )
        );

        let settings = cx.update(|cx| {
            cx.global::<crate::ui::appearance_runtime::AppearanceRuntime>()
                .settings
                .clone()
        });
        let token = settings.begin_preview(0).unwrap();
        let mut candidate = SettingsDocument::default();
        candidate.appearance.window.density = ChromeDensity::Comfortable;
        settings.update_preview(&token, candidate).unwrap();
        cx.run_until_parked();
        let (workspace_expected, settings_expected) = cx.update(|cx| {
            let appearance = crate::ui::appearance::chrome(cx);
            let workspace_height = crate::ui::WorkspaceFrame::for_appearance(appearance, cx)
                .top_chrome_height(appearance.top_height());
            (
                geometry
                    .workspace_traffic_light_position(workspace_height)
                    .unwrap(),
                geometry
                    .sidebar_window_traffic_light_position(appearance.top_height())
                    .unwrap(),
            )
        });

        assert_eq!(
            (
                cx.traffic_light_position_updates(workspace.into()),
                cx.traffic_light_position_updates(settings_window.into()),
            ),
            (
                vec![point(px(15.5), px(14.0)), workspace_expected],
                vec![point(px(12.0), px(11.0)), settings_expected],
            )
        );
    }

    #[gpui::test]
    fn switch_workspace_without_focused_content_does_not_redispatch(cx: &mut gpui::TestAppContext) {
        assert_switch_workspace_without_focused_content(cx, false);
    }

    #[gpui::test]
    fn switch_workspace_from_settings_without_target_focus_does_not_redispatch(
        cx: &mut gpui::TestAppContext,
    ) {
        assert_switch_workspace_without_focused_content(cx, true);
    }

    fn assert_switch_workspace_without_focused_content(
        cx: &mut gpui::TestAppContext,
        from_settings: bool,
    ) {
        let host = host_with_settings();
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        cx.run_until_parked();
        cx.update(|cx| {
            workspace
                .update(cx, |_, window, cx| window.blur(cx))
                .unwrap();
        });
        if from_settings {
            cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
            cx.run_until_parked();
        }
        let fallback_visits = Rc::new(std::cell::Cell::new(0));
        let observed_visits = Rc::clone(&fallback_visits);
        cx.update(|cx| {
            // Global bubble listeners run newest first. Allow the real fallback once, but
            // stop a second dispatch so a regression fails rather than hanging the executor.
            cx.on_action(move |_: &SwitchWorkspace, cx| {
                let visits = observed_visits.get() + 1;
                observed_visits.set(visits);
                if visits == 1 {
                    cx.propagate();
                }
            });
            cx.dispatch_action(&SwitchWorkspace);
        });
        cx.run_until_parked();

        assert_eq!(
            fallback_visits.get(),
            1,
            "Workspace fallback must not redispatch itself"
        );
        assert!(cx.update(|cx| {
            workspace
                .update(cx, |_, window, cx| {
                    window.is_window_active() && spaceterm_ui::window_combo_box_is_open(window, cx)
                })
                .unwrap()
        }));
    }

    #[gpui::test]
    fn switch_workspace_action_should_open_the_switcher_from_settings(
        cx: &mut gpui::TestAppContext,
    ) {
        let host = host_with_settings();
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
        cx.run_until_parked();

        cx.update(|cx| cx.dispatch_action(&crate::ui::SwitchWorkspace));
        cx.run_until_parked();

        assert_eq!(
            cx.update(|cx| {
                workspace
                    .update(cx, |_, window, cx| {
                        (
                            window.is_window_active(),
                            spaceterm_ui::window_combo_box_is_open(window, cx),
                        )
                    })
                    .unwrap()
            }),
            (true, true)
        );
    }

    #[gpui::test]
    fn a_settings_window_does_not_stand_in_for_a_workspace_window(cx: &mut gpui::TestAppContext) {
        let host = host_with_settings();
        let workspace = cx.update(|cx| {
            let workspace = start_application(cx, &host).unwrap();
            install_headless_window_actions(cx, Rc::clone(&host));
            workspace
        });
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
        cx.run_until_parked();

        // Close the only Workspace window while Settings stays open.
        cx.update(|cx| {
            workspace
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
        assert!(cx.update(|cx| workspace_windows(cx).is_empty()));

        // Both paths that restore a Workspace must still fire.
        cx.update(|cx| cx.dispatch_action(&NewWorkspace));
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| workspace_windows(cx).len()), 1);

        cx.update(|cx| {
            workspace_windows(cx)[0]
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();
        cx.update(|cx| restore_default_window(cx, &host));
        cx.run_until_parked();

        assert_eq!(cx.update(|cx| workspace_windows(cx).len()), 1);
    }

    #[gpui::test]
    fn last_window_quit_cancel_restores_workspace_window(cx: &mut gpui::TestAppContext) {
        windowless_quit_cancel_restores_workspace_window(cx, false, false);
    }

    #[gpui::test]
    fn status_area_loss_quit_cancel_restores_workspace_window(cx: &mut gpui::TestAppContext) {
        windowless_quit_cancel_restores_workspace_window(cx, true, false);
    }

    #[gpui::test]
    fn last_window_quit_cancel_restores_workspace_window_after_asking_alone(
        cx: &mut gpui::TestAppContext,
    ) {
        windowless_quit_cancel_restores_workspace_window(cx, false, true);
    }

    #[gpui::test]
    fn status_area_loss_quit_cancel_restores_workspace_window_after_asking_alone(
        cx: &mut gpui::TestAppContext,
    ) {
        windowless_quit_cancel_restores_workspace_window(cx, true, true);
    }

    fn windowless_quit_cancel_restores_workspace_window(
        cx: &mut gpui::TestAppContext,
        lose_status_area: bool,
        asks_alone: bool,
    ) {
        use crate::platform::application_quit::{LastWindowPolicy, testing};
        let quit = Rc::new(if asks_alone {
            testing::RecordingApplicationQuitAdapter::presenting_windowless(LastWindowPolicy::Quit)
        } else {
            testing::RecordingApplicationQuitAdapter::with_last_window_policy(
                LastWindowPolicy::Quit,
            )
        });
        let mut host = host_with_settings();
        let adapters = &mut Rc::get_mut(&mut host).unwrap().adapters;
        adapters.application_quit = quit.clone();
        adapters.local_server = crate::ui::launch_testing::unanswered();
        let original = cx.update(|cx| {
            let original = start_application(cx, &host).unwrap();
            install_headless_window_actions(cx, Rc::clone(&host));
            crate::status_menu::set_available(cx, lose_status_area);
            original
        });
        cx.run_until_parked();
        cx.update(|cx| {
            original
                .update(cx, |_, window, _| window.remove_window())
                .unwrap()
        });
        cx.run_until_parked();
        if lose_status_area {
            assert!(!cx.has_pending_prompt());
            cx.update(|cx| crate::status_menu::set_available(cx, false));
            cx.run_until_parked();
        }
        if asks_alone {
            assert_eq!(quit.windowless_prompts().len(), 1);
            assert!(cx.update(|cx| cx.windows().is_empty()));
            quit.answer_windowless(Some(QuitAnswer::Cancel));
        } else {
            assert!(cx.has_pending_prompt());
            cx.update(|cx| {
                assert!(
                    workspace_windows(cx).is_empty(),
                    "displaying the warning creates no Workspace Window"
                );
                assert_eq!(cx.windows().len(), 1);
                assert!(cx.windows()[0].downcast::<QuitPromptHost>().is_some());
            });
            assert_eq!(quit.confirmations(), 0);
            cx.simulate_prompt_answer("Cancel");
        }
        cx.run_until_parked();
        assert!(
            !cx.has_pending_prompt() && quit.windowless_prompts().is_empty(),
            "Cancel must not start another quit"
        );
        cx.update(|cx| {
            let restored = workspace_windows(cx);
            assert_eq!(
                restored.len(),
                1,
                "Cancel restores reachable presentation without a status item"
            );
            assert_eq!(
                cx.windows().len(),
                1,
                "the temporary warning host closes after restoration"
            );
            assert_ne!(restored[0], original);
            assert!(
                restored[0]
                    .update(cx, |_, window, _| window.is_window_active())
                    .unwrap()
            );
        });
        assert_eq!(quit.requests(), 1);
        assert_eq!(quit.confirmations(), 0);
    }

    #[gpui::test]
    fn application_and_dock_quit_close_server_windows_without_a_dialog(
        cx: &mut gpui::TestAppContext,
    ) {
        let quit = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        let host = {
            let mut wiring = parts(Rc::default(), Rc::default());
            wiring.adapters.application_quit = quit.clone();
            HostComposition::new(wiring).unwrap()
        };
        cx.update(|cx| {
            start_application(cx, &host).unwrap();
        });
        cx.run_until_parked();
        cx.update(|cx| crate::status_menu::set_available(cx, true));
        for dock in [false, true] {
            if dock {
                cx.update(|cx| {
                    open(cx, &host).unwrap();
                });
                cx.run_until_parked();
                quit.simulate_native_request(ApplicationQuitRequest::Dismissal);
            } else {
                cx.update(|cx| cx.dispatch_action(&DismissApplication));
            }
            cx.run_until_parked();
            assert!(!cx.has_pending_prompt());
            assert!(cx.update(|cx| cx.windows().is_empty()));
            assert_eq!(quit.confirmations(), 0);
        }
    }

    #[gpui::test]
    fn window_dispatched_quit_asks_before_quitting(cx: &mut gpui::TestAppContext) {
        let quit = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        let mut host = host_with_settings();
        Rc::get_mut(&mut host).unwrap().adapters.application_quit = quit.clone();
        Rc::get_mut(&mut host).unwrap().adapters.local_server =
            crate::ui::launch_testing::unanswered();
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());

        cx.update(|cx| {
            workspace
                .update(cx, |_, window, cx| {
                    window.dispatch_action(Box::new(QuitApplication), cx);
                })
                .unwrap();
        });
        cx.run_until_parked();
        assert!(
            cx.has_pending_prompt(),
            "a quit the Workspace Window dispatches asks first"
        );
        assert_eq!(quit.confirmations(), 0);
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert!(cx.update(|cx| workspace.read(cx).is_ok()));
        assert_eq!(quit.confirmations(), 0);
    }

    #[gpui::test]
    fn a_settings_only_window_leaves_application_quit_unblocked(cx: &mut gpui::TestAppContext) {
        let quit = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        let mut host = host_with_settings();
        Rc::get_mut(&mut host).unwrap().adapters.application_quit = quit.clone();
        Rc::get_mut(&mut host).unwrap().adapters.local_server =
            crate::ui::launch_testing::unanswered();
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&QuitApplication));
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(quit.confirmations(), 0);

        cx.update(|cx| {
            workspace
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();

        // A Settings-only client still confirms exit, then saves Settings and finishes.
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
        // TestAppContext does not deliver another App action with only Settings open. The native
        // adapter invokes the same installed quit coordinator and records its deferred decision.
        assert_eq!(
            quit.simulate_native_request(ApplicationQuitRequest::Exit),
            ApplicationQuitDecision::Cancel
        );
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Quit");
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt());
        assert_eq!(quit.requests(), 2);
        assert_eq!(quit.confirmations(), 1);
    }

    #[gpui::test]
    fn cancelling_quit_keeps_the_ready_update_available(cx: &mut gpui::TestAppContext) {
        let adapter = Rc::new(crate::updates::testing::RecordingAdapter::available());
        let mut host = host_with_settings();
        Rc::get_mut(&mut host).unwrap().adapters.updates = adapter.clone();
        Rc::get_mut(&mut host).unwrap().adapters.local_server =
            crate::ui::launch_testing::unanswered();
        let _workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        let service = cx.update(|cx| cx.global::<crate::updates::UpdateService>().0.clone());
        service.update(cx, |updates, cx| updates.check(false, cx));
        adapter.emit(crate::updates::UpdateEvent::Available("0.1.1".into()));
        adapter.emit(crate::updates::UpdateEvent::Ready);
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&QuitApplication));
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(adapter.deferred_installs.get(), 0);
        assert_eq!(adapter.cancellations.get(), 0);
        assert!(service.read_with(cx, |updates, _| matches!(
            updates.state(),
            crate::updates::UpdateState::Ready { .. }
        )));
    }

    #[gpui::test]
    fn application_quit_suppresses_repeated_requests_while_settings_are_saving(
        cx: &mut gpui::TestAppContext,
    ) {
        let adapter: Rc<dyn ApplicationQuitAdapter> = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        let coordinator = Rc::new(ApplicationQuitCoordinator {
            adapter: Rc::downgrade(&adapter),
            state: RefCell::new(ApplicationQuitCoordinatorState::WaitingForSettings),
            status_area: StatusAreaName::MenuBar,
            sessions: None,
        });

        let decision = cx.update(|cx| coordinator.request(ApplicationQuitRequest::Dismissal, cx));

        assert_eq!(decision, ApplicationQuitDecision::Cancel);
        assert!(!cx.has_pending_prompt());
    }

    #[gpui::test]
    fn application_quit_returns_to_idle_after_settings_save_failure(cx: &mut gpui::TestAppContext) {
        let adapter: Rc<dyn ApplicationQuitAdapter> = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        let coordinator = Rc::new(ApplicationQuitCoordinator {
            adapter: Rc::downgrade(&adapter),
            state: RefCell::new(ApplicationQuitCoordinatorState::WaitingForSettings),
            status_area: StatusAreaName::MenuBar,
            sessions: None,
        });

        cx.update(|cx| {
            coordinator.complete_after_settings_save(None, ApplicationQuitSaveOutcome::Failed, cx);
        });

        assert_eq!(
            *coordinator.state.borrow(),
            ApplicationQuitCoordinatorState::Idle
        );
    }

    #[cfg(all(test, target_os = "macos", feature = "native-tests"))]
    mod macos_adapter_tests {
        include!("platform/macos_adapter_tests/traffic_lights.rs");
    }
}
