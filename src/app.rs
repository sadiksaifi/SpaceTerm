use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, actions, px, size,
};

use crate::platform::app_paths::{AppPathEnvironment, AppPathHostFacts, AppPaths};
use crate::platform::application_menu::{ApplicationMenuAdapter, ApplicationMenuCommand};
use crate::platform::control_socket::ControlSocketProbe;
use crate::platform::secure_filesystem::SecureFilesystem;
use crate::ssh::alias_usage::ActiveSshAliasRegistry;
use crate::ssh::command::{
    OpenSshExecutable, SshCapability, SshCapabilityProbe, SshUnavailableReason,
};
use crate::ssh::host_config::HostConfigFilesystem;
use crate::ssh::process::SshProcessAdapter;
use crate::ssh::startup_environment::StartupSshEnvironment;
use crate::terminal::{
    NativeServiceOrigin, NativeServiceStatus, SelectionCopy, TerminalSessionFactory,
};
use crate::ui::{
    CreateScratchWorkspace, NativeRemoteWorkspaceFlowBackendFactory, NewWorkspace,
    RemoteWorkspaceSshRuntime, WorkspaceManager,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum StartupDependenciesError {
    #[error("the user home directory is unavailable")]
    MissingHome,
    #[error("the user home directory is not absolute")]
    RelativeHome,
    #[error("application paths are unavailable")]
    Paths,
}

pub(crate) struct StartupDependencies<A: SshProcessAdapter> {
    paths: Arc<AppPaths>,
    pub(crate) home_directory: PathBuf,
    ssh_environment: StartupSshEnvironment,
    ssh_capability: SshCapability,
    active_aliases: ActiveSshAliasRegistry,
    executable: OpenSshExecutable,
    process_adapter: A,
    control_socket_probe: Arc<dyn ControlSocketProbe>,
    host_config_filesystem: Arc<dyn HostConfigFilesystem>,
}

impl<A: SshProcessAdapter> StartupDependencies<A> {
    #[expect(
        clippy::too_many_arguments,
        reason = "startup consumes independently captured host facts and capabilities"
    )]
    pub(crate) fn capture(
        path_environment: AppPathEnvironment,
        ssh_environment: StartupSshEnvironment,
        path_host_facts: &AppPathHostFacts,
        secure_filesystem: Arc<dyn SecureFilesystem>,
        executable: OpenSshExecutable,
        process_adapter: A,
        control_socket_probe: Arc<dyn ControlSocketProbe>,
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
        let ssh_capability = SshCapabilityProbe::from_startup(
            executable.clone(),
            home_directory.clone(),
            &ssh_environment,
            process_adapter.clone(),
        )
        .map(|runner| runner.probe_blocking())
        .unwrap_or(SshCapability::Unavailable(
            SshUnavailableReason::ProbeFailed,
        ));
        Ok(Self {
            paths: Arc::new(
                AppPaths::resolve(&path_environment, path_host_facts, secure_filesystem)
                    .map_err(|_| StartupDependenciesError::Paths)?,
            ),
            home_directory,
            ssh_environment,
            ssh_capability,
            active_aliases: ActiveSshAliasRegistry::default(),
            executable,
            process_adapter,
            control_socket_probe,
            host_config_filesystem,
        })
    }

    pub(crate) fn remote_backend_factory(
        &self,
        askpass: Arc<dyn crate::platform::askpass::AskPassWindowFactory>,
    ) -> Arc<dyn crate::ui::remote_workspace_flow::RemoteWorkspaceFlowBackendFactory> {
        Arc::new(NativeRemoteWorkspaceFlowBackendFactory::new(
            RemoteWorkspaceSshRuntime {
                paths: Arc::clone(&self.paths),
                local_home: self.home_directory.clone(),
                startup_environment: self.ssh_environment.clone(),
                startup_capability: self.ssh_capability.clone(),
                aliases: self.active_aliases.clone(),
                executable: self.executable.clone(),
                process_adapter: self.process_adapter.clone(),
                control_socket_probe: Arc::clone(&self.control_socket_probe),
                host_config_filesystem: Arc::clone(&self.host_config_filesystem),
            },
            askpass,
        ))
    }
}

actions!(
    spaceterm,
    [
        ShowAboutApplication,
        OpenApplicationHelp,
        QuitApplication,
        HideApplication,
        HideOtherApplications,
        ShowAllApplications,
        MinimizeWindow,
        ZoomActiveWindow,
        BringAllWindowsToFront,
        ToggleFullScreen
    ]
);

pub(crate) fn init(cx: &mut App, application_menu: Rc<dyn ApplicationMenuAdapter>) {
    install_application_menu_actions(cx, Rc::clone(&application_menu));
    cx.on_action(request_application_quit);
    cx.on_action(|_: &HideApplication, cx| cx.hide());
    cx.on_action(|_: &HideOtherApplications, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApplications, cx| cx.unhide_other_apps());
    cx.on_action(minimize_active_window);
    cx.on_action(toggle_active_window_full_screen);
    if let Err(error) = application_menu.install(cx) {
        eprintln!("failed to install the application menu: {error}");
    }
}

fn install_application_menu_actions(
    cx: &mut App,
    application_menu: Rc<dyn ApplicationMenuAdapter>,
) {
    let about = Rc::clone(&application_menu);
    cx.on_action(move |_: &ShowAboutApplication, _| {
        perform_application_menu_command(about.as_ref(), ApplicationMenuCommand::ShowAbout);
    });
    let help = Rc::clone(&application_menu);
    cx.on_action(move |_: &OpenApplicationHelp, _| {
        perform_application_menu_command(help.as_ref(), ApplicationMenuCommand::OpenHelp);
    });
    let zoom = Rc::clone(&application_menu);
    cx.on_action(move |_: &ZoomActiveWindow, _| {
        perform_application_menu_command(zoom.as_ref(), ApplicationMenuCommand::ZoomActiveWindow);
    });
    cx.on_action(move |_: &BringAllWindowsToFront, _| {
        perform_application_menu_command(
            application_menu.as_ref(),
            ApplicationMenuCommand::BringAllWindowsToFront,
        );
    });
}

fn perform_application_menu_command(
    application_menu: &dyn ApplicationMenuAdapter,
    command: ApplicationMenuCommand,
) {
    if let Err(error) = application_menu.perform(command) {
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

fn request_application_quit(_: &QuitApplication, cx: &mut App) {
    cx.defer(|cx| {
        // Resolve live roots at dispatch time, including inactive windows. GPUI owns the
        // window registry, so a removed window cannot leave stale application quit authority.
        let confirmation_window = application_quit_confirmation_window(cx);
        if let Some(handle) = confirmation_window {
            let _ = handle.update(cx, |manager, window, cx| {
                window.activate_window();
                manager.request_application_quit(window, cx);
            });
        } else {
            cx.quit();
        }
    });
}

fn application_quit_confirmation_window(cx: &App) -> Option<gpui::WindowHandle<WorkspaceManager>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<WorkspaceManager>())
        .find(|window| {
            window
                .read(cx)
                .is_ok_and(|manager| manager.blocks_unconfirmed_application_quit(cx))
        })
}

pub(crate) fn open(
    cx: &mut App,
    host: &HostComposition,
) -> Result<gpui::WindowHandle<WorkspaceManager>, RuntimeError> {
    let adapters = crate::ui::WorkspaceManagerAdapters {
        local_filesystem: host.adapters.local_filesystem.clone(),
        key_input: Rc::clone(&host.adapters.key_input),
        accessibility: Rc::clone(&host.adapters.accessibility),
        native_services: host.adapters.native_services.clone(),
        lifecycle: host.adapters.lifecycle.clone(),
        directory_selection: Rc::new(crate::directory_selection::GpuiDirectorySelection),
        permission_recovery: host.adapters.permission_recovery.clone(),
        remote_workspace: Arc::clone(&host.adapters.remote_workspace),
        window_drag: host.window_movement.create(),
    };
    let session_factory = Rc::clone(&host.session_factory);
    let home_directory = host.home_directory.clone();
    let bounds = Bounds::centered(None, size(px(900.0), px(580.0)), cx);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.0), px(260.0))),
            titlebar: host.titlebar.as_ref().map(|titlebar| TitlebarOptions {
                title: titlebar.title.clone(),
                appears_transparent: titlebar.appears_transparent,
                traffic_light_position: titlebar.traffic_light_position,
            }),
            ..WindowOptions::default()
        },
        |window, cx| {
            let workspace_manager = cx.new(|cx| {
                WorkspaceManager::new_with_adapters(
                    session_factory,
                    home_directory,
                    adapters,
                    window,
                    cx,
                )
            });
            workspace_manager.update(cx, |workspace_manager, cx| {
                workspace_manager.focus(window, cx);
            });
            let close_manager = workspace_manager.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                close_manager
                    .update(cx, |manager, cx| manager.should_close_window(window, cx))
                    .unwrap_or(true)
            });
            if let Err(error) = host.services.install(
                window,
                Rc::new(WorkspaceServicesEndpoint {
                    app: cx.to_async(),
                    window: window.window_handle(),
                    owner: workspace_manager.downgrade(),
                }),
            ) {
                eprintln!("failed to install the Services responder: {error}");
            }
            workspace_manager
        },
    );

    let window = result.map_err(|_| RuntimeError::WindowOpen)?;
    cx.activate(true);
    Ok(window)
}

fn restore_default_window(cx: &mut App, host: &HostComposition) {
    if !cx.windows().is_empty() {
        return;
    }
    if let Err(error) = open(cx, host) {
        eprintln!("failed to restore the default SpaceTerm window: {error}");
    }
}

fn install_headless_window_actions(cx: &mut App, host: Rc<HostComposition>) {
    let new_workspace_host = Rc::clone(&host);
    cx.on_action(move |_: &NewWorkspace, cx| {
        restore_default_window(cx, &new_workspace_host);
    });
    cx.on_action(move |_: &CreateScratchWorkspace, cx| {
        restore_default_window(cx, &host);
    });
}

#[cfg(test)]
mod tests {
    use gpui::{Action, ClipboardItem, Keystroke, TestAppContext};

    use super::*;
    use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};
    use crate::terminal::{SelectionCopy, WorkspaceTerminalSessionFactory};
    use crate::ui::TerminalPane;

    fn application_menu() -> Rc<dyn ApplicationMenuAdapter> {
        Rc::new(
            crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
        )
    }

    #[gpui::test]
    fn configured_shortcuts_should_bind_global_application_actions(cx: &mut TestAppContext) {
        cx.update(crate::ui::init).expect("UI initialization");
        cx.update(|cx| init(cx, application_menu()));
        let expected = [
            ("cmd-q", QuitApplication.name()),
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
        cx.update(|cx| init(cx, application_menu()));
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(records.clone()).with_selection_copy_response(Ok(
                Some(SelectionCopy {
                    plain_text: "native command copy".to_owned(),
                    html: None,
                }),
            )),
        );
        let session_factory = WorkspaceTerminalSessionFactory::new_local(
            session_factory,
            crate::terminal::testing::test_workspace_directory(PathBuf::from(
                "/tmp/spaceterm-native-copy-command-test",
            )),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, _| pane.focus(window));
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
        cx.update(|cx| init(cx, adapter));

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
            .and_then(Result::ok)
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
            .and_then(Result::ok)
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
            .and_then(Result::ok)
            .unwrap_or(false)
    }
}

/// Application-scoped capabilities shared by every Operating-System Window.
#[derive(Clone)]
pub(crate) struct ApplicationCapabilities {
    pub(crate) application_menu: Rc<dyn ApplicationMenuAdapter>,
    pub(crate) local_filesystem: crate::platform::local_filesystem::LocalFilesystemAuthority,
    pub(crate) key_input: Rc<dyn crate::terminal::TerminalKeyInputAdapterFactory>,
    pub(crate) accessibility:
        Rc<dyn crate::platform::terminal_accessibility::TerminalAccessibilityAdapterFactory>,
    pub(crate) native_services: crate::terminal::native_services::NativeServiceAdapters,
    pub(crate) lifecycle: crate::ui::pane_lifecycle::PaneLifecycleDependencies,
    pub(crate) permission_recovery:
        Option<Rc<dyn crate::platform::permission_recovery::PermissionRecoveryOpener>>,
    pub(crate) remote_workspace:
        Arc<dyn crate::ui::remote_workspace_flow::RemoteWorkspaceFlowBackendFactory>,
}

/// Complete constructor wiring. This value defines no platform operations.
pub(crate) struct HostCompositionParts {
    pub(crate) profile: crate::desktop_profile::DesktopProfile,
    pub(crate) home_directory: PathBuf,
    pub(crate) session_factory: Rc<dyn TerminalSessionFactory>,
    pub(crate) adapters: ApplicationCapabilities,
    pub(crate) services: Rc<dyn crate::platform::services_registration::ServicesRegistration>,
    pub(crate) window_movement: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
    pub(crate) titlebar: Option<TitlebarOptions>,
}
pub(crate) struct HostComposition {
    profile: crate::desktop_profile::DesktopProfile,
    home_directory: PathBuf,
    session_factory: Rc<dyn TerminalSessionFactory>,
    adapters: ApplicationCapabilities,
    services: Rc<dyn crate::platform::services_registration::ServicesRegistration>,
    window_movement: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
    titlebar: Option<TitlebarOptions>,
}
impl HostComposition {
    pub(crate) fn new(
        parts: HostCompositionParts,
    ) -> Result<Self, crate::desktop_profile::DesktopProfileError> {
        use crate::desktop_profile::DesktopProfileError;
        if !parts.home_directory.is_absolute() {
            return Err(DesktopProfileError::InvalidCombination);
        }
        if parts.titlebar.as_ref().is_some_and(|titlebar| {
            titlebar.traffic_light_position.is_some() && !titlebar.appears_transparent
        }) {
            return Err(DesktopProfileError::InvalidCombination);
        }
        Ok(Self {
            profile: parts.profile,
            home_directory: parts.home_directory,
            session_factory: parts.session_factory,
            adapters: parts.adapters,
            services: parts.services,
            window_movement: parts.window_movement,
            titlebar: parts.titlebar,
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum RuntimeError {
    #[error("UI initialization failed")]
    Initialization,
    #[error("Operating-System Window creation failed")]
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
    let application = gpui::Application::new().with_assets(spaceterm_ui::EmbeddedAssets);
    application.on_reopen(move |cx| restore_default_window(cx, &reopened_host));
    application.run(move |cx| {
        if let Err(error) = start_application(cx, &host) {
            reported_failure.set(Some(error));
            cx.quit();
        } else {
            install_headless_window_actions(cx, Rc::clone(&host));
        }
    });
    failure.get().map_or(Ok(()), Err)
}

fn start_application(
    cx: &mut App,
    host: &HostComposition,
) -> Result<gpui::WindowHandle<WorkspaceManager>, RuntimeError> {
    crate::ui::initialize_controls(cx).map_err(|_| RuntimeError::Initialization)?;
    host.profile.install(cx);
    if let Err(error) = host.services.register() {
        eprintln!("failed to register Services: {error}");
    }
    init(cx, Rc::clone(&host.adapters.application_menu));
    open(cx, host)
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use crate::platform::services_registration::{ServicesRegistration, ServicesRegistrationError};
    use crate::platform::window_movement::{
        OperatingSystemWindowDragPlatform, RecordingOperatingSystemWindowDragPlatform,
        WindowMovementFactory,
    };
    use crate::terminal::native_services::services::ServiceEndpoint;
    use crate::ui::remote_workspace_flow::{
        RemoteWorkspaceFlowBackend, RemoteWorkspaceFlowBackendError,
        RemoteWorkspaceFlowBackendFactory,
    };
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
    struct UnavailableRemote;
    impl RemoteWorkspaceFlowBackendFactory for UnavailableRemote {
        fn unavailable_reason(&self) -> Option<String> {
            Some("Unavailable in this test".into())
        }
        fn create(
            &self,
            _: &gpui::Window,
            _: &mut App,
        ) -> Result<Arc<dyn RemoteWorkspaceFlowBackend>, RemoteWorkspaceFlowBackendError> {
            panic!("unavailable capability must not be constructed")
        }
    }
    fn parts(
        services: Rc<RecordingServices>,
        movement: Rc<RecordingMovement>,
    ) -> HostCompositionParts {
        HostCompositionParts {
            profile: crate::desktop_profile::testing_profile(spaceterm_ui::TextDirection::LeftToRight),
            home_directory: std::env::temp_dir(),
            session_factory: Rc::new(crate::terminal::testing::TestTerminalSessionFactory::new(Default::default())),
            adapters: ApplicationCapabilities {
                application_menu: Rc::new(
                    crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
                ),
                local_filesystem: crate::platform::local_filesystem::LocalFilesystemAuthority::testing(),
                key_input: Rc::new(crate::terminal::GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
                native_services: crate::terminal::native_services::testing::adapters(),
                lifecycle: crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(),
                permission_recovery: None,
                remote_workspace: Arc::new(UnavailableRemote),
            },
            services, window_movement: movement, titlebar: None,
        }
    }
    #[test]
    fn invalid_composition_has_only_closed_failure_classification() {
        let mut parts = parts(Rc::default(), Rc::default());
        parts.home_directory = "sensitive-relative-value".into();
        let error = HostComposition::new(parts).err().unwrap();
        assert_eq!(
            error.to_string(),
            "desktop policy and capabilities disagree"
        );
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
    fn new_workspace_action_should_restore_a_default_window_when_headless(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};

        let records = TestTerminalSessionRecords::default();
        let services = Rc::new(RecordingServices::default());
        let mut wiring = parts(Rc::clone(&services), Rc::default());
        wiring.session_factory = Rc::new(TestTerminalSessionFactory::new(records.clone()));
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

        cx.update(|cx| cx.dispatch_action(&NewWorkspace));
        cx.run_until_parked();

        assert_eq!(
            (
                cx.windows().len(),
                records.session_count(),
                services.calls.borrow().clone(),
            ),
            (1, 2, vec!["register", "install", "install"])
        );
    }

    #[gpui::test]
    fn create_scratch_workspace_action_should_restore_a_default_window_when_headless(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};

        let records = TestTerminalSessionRecords::default();
        let services = Rc::new(RecordingServices::default());
        let mut wiring = parts(Rc::clone(&services), Rc::default());
        wiring.session_factory = Rc::new(TestTerminalSessionFactory::new(records.clone()));
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

        cx.update(|cx| cx.dispatch_action(&CreateScratchWorkspace));
        cx.run_until_parked();

        assert_eq!(
            (
                cx.windows().len(),
                records.session_count(),
                services.calls.borrow().clone(),
            ),
            (1, 2, vec!["register", "install", "install"])
        );
    }

    #[gpui::test]
    fn new_workspace_menu_actions_should_remain_available_when_headless(
        cx: &mut gpui::TestAppContext,
    ) {
        let host = Rc::new(HostComposition::new(parts(Rc::default(), Rc::default())).unwrap());
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

        let available = cx.update(|cx| {
            (
                cx.is_action_available(&NewWorkspace),
                cx.is_action_available(&CreateScratchWorkspace),
            )
        });

        assert_eq!(available, (true, true));
    }

    #[gpui::test]
    fn default_window_restoration_should_not_duplicate_an_existing_window(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};

        let records = TestTerminalSessionRecords::default();
        let services = Rc::new(RecordingServices::default());
        let mut wiring = parts(Rc::clone(&services), Rc::default());
        wiring.session_factory = Rc::new(TestTerminalSessionFactory::new(records.clone()));
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

    #[gpui::test]
    fn application_quit_checks_inactive_windows_and_discards_removed_roots(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};
        let records = TestTerminalSessionRecords::default();
        let mut wiring = parts(Rc::default(), Rc::default());
        wiring.session_factory = Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let host = HostComposition::new(wiring).unwrap();
        let (running, idle) = cx.update(|cx| {
            let running = start_application(cx, &host).unwrap();
            let idle = open(cx, &host).unwrap();
            idle.update(cx, |_, window, _| window.activate_window())
                .unwrap();
            (running, idle)
        });
        cx.run_until_parked();
        let now = std::time::Instant::now();
        let mut metadata = crate::terminal::metadata::MetadataTracker::new(
            crate::local_path::LocalPathSemantics::Posix,
            "/tmp",
            "zsh",
            None,
            now,
        );
        assert!(metadata.apply_semantic_prompt("A", now));
        let mut screen =
            (*crate::terminal::ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix))
                .clone();
        screen.metadata = metadata.snapshot();
        records
            .event_sender(2)
            .unwrap()
            .try_send(crate::terminal::SessionEvent::Screen(Arc::new(screen)))
            .unwrap();
        cx.run_until_parked();

        cx.update(|cx| {
            assert!(cx.active_window() == Some(idle.into()));
            assert!(
                !idle
                    .read(cx)
                    .unwrap()
                    .blocks_unconfirmed_application_quit(cx)
            );
            assert_eq!(application_quit_confirmation_window(cx), Some(running));
            request_application_quit(&QuitApplication, cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(cx.active_window() == Some(running.into()));
            assert!(
                running
                    .update(cx, |_, window, cx| spaceterm_ui::window_modal_is_open(
                        window, cx
                    ))
                    .unwrap()
            );
            assert!(
                !idle
                    .update(cx, |_, window, cx| spaceterm_ui::window_modal_is_open(
                        window, cx
                    ))
                    .unwrap()
            );
        });
        cx.simulate_keystrokes(running.into(), "escape");
        cx.run_until_parked();
        assert!(records.dropped_session_ids().is_empty());
        cx.update(|cx| {
            // Remove the old owner before deferred quit dispatch. The remaining idle root
            // must not inherit its pending state or receive a stale confirmation.
            request_application_quit(&QuitApplication, cx);
            running
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(running.read(cx).is_err());
            assert_eq!(cx.windows().len(), 1);
            assert_eq!(application_quit_confirmation_window(cx), None);
            assert!(
                !idle
                    .update(cx, |_, window, cx| spaceterm_ui::window_modal_is_open(
                        window, cx
                    ))
                    .unwrap()
            );
        });
    }
}
