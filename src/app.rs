use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gpui::{
    App, AppContext, Bounds, PromptButton, PromptLevel, TitlebarOptions, WindowBounds,
    WindowOptions, actions, px, size,
};

use crate::platform::app_directories::AppDirectoryEnvironment;
use crate::platform::app_paths::AppPaths;
use crate::platform::application_menu::{ApplicationMenuAdapter, ApplicationMenuCommand};
use crate::platform::application_quit::{
    ApplicationQuitAdapter, ApplicationQuitDecision, ApplicationQuitHandler,
};
use crate::platform::control_socket::ControlSocketProbe;
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
    NativeRemoteWorkspaceFlowBackendFactory, NewWorkspace, RemoteWorkspaceSshRuntime,
    SwitchWorkspace, WorkspaceManager,
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
    pub(crate) fn settings_storage(&self) -> Arc<dyn crate::settings::storage::SettingsStorage> {
        Arc::new(crate::settings::storage::ConfigSettingsStorage::new(
            Arc::clone(&self.paths),
        ))
    }
    pub(crate) fn capture(
        path_environment: AppDirectoryEnvironment,
        ssh_environment: StartupSshEnvironment,
        paths: AppPaths,
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
            paths: Arc::new(paths),
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

pub(crate) fn init(
    cx: &mut App,
    application_menu: Rc<dyn ApplicationMenuAdapter>,
    application_quit: Rc<dyn ApplicationQuitAdapter>,
) -> Result<(), crate::platform::application_quit::ApplicationQuitError> {
    install_application_menu_actions(cx, Rc::clone(&application_menu));
    install_application_quit(cx, Rc::clone(&application_quit))?;
    crate::ui::settings_window::init(cx);
    cx.on_action(switch_workspace_from_global_action);
    cx.on_action(move |_: &QuitApplication, cx| application_quit.request_quit(cx));
    cx.on_action(|_: &HideApplication, cx| cx.hide());
    cx.on_action(|_: &HideOtherApplications, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApplications, cx| cx.unhide_other_apps());
    cx.on_action(minimize_active_window);
    cx.on_action(toggle_active_window_full_screen);
    if let Err(error) = application_menu.install(cx) {
        eprintln!("failed to install the application menu: {error}");
    }
    Ok(())
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

fn install_application_quit(
    cx: &mut App,
    adapter: Rc<dyn ApplicationQuitAdapter>,
) -> Result<(), crate::platform::application_quit::ApplicationQuitError> {
    let coordinator = Rc::new(ApplicationQuitCoordinator {
        adapter: Rc::downgrade(&adapter),
        state: RefCell::new(ApplicationQuitCoordinatorState::Idle),
    });
    adapter.install(ApplicationQuitHandler::new(
        cx,
        Rc::new(move |cx| coordinator.request(cx)),
    ))
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
    Prompting,
    WaitingForSettings,
}

struct ApplicationQuitCoordinator {
    adapter: Weak<dyn ApplicationQuitAdapter>,
    state: RefCell<ApplicationQuitCoordinatorState>,
}

#[derive(Clone)]
struct ApplicationQuitSnapshot {
    facts: crate::close_confirmation::ApplicationCloseFacts,
    panes: Vec<ApplicationQuitPane>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ApplicationQuitPane {
    window: gpui::WindowHandle<WorkspaceManager>,
    facts: crate::close_confirmation::ApplicationPaneFacts,
}

impl ApplicationQuitSnapshot {
    fn authorizes(&self, current: &Self) -> bool {
        current.panes.iter().all(|current| {
            self.panes.iter().any(|authorized| {
                authorized.window == current.window
                    && authorized.facts.workspace_id == current.facts.workspace_id
                    && authorized.facts.tab_id == current.facts.tab_id
                    && authorized.facts.pane_id == current.facts.pane_id
                    && (!current.facts.has_running_work || authorized.facts.has_running_work)
            })
        })
    }
}

impl ApplicationQuitCoordinator {
    fn request(self: &Rc<Self>, cx: &mut App) -> ApplicationQuitDecision {
        if *self.state.borrow() != ApplicationQuitCoordinatorState::Idle {
            return ApplicationQuitDecision::Cancel;
        }
        if application_has_pending_close_confirmation(cx) {
            return ApplicationQuitDecision::Cancel;
        }
        let snapshot = application_quit_snapshot(cx);
        if snapshot.facts.requires_confirmation() {
            self.present_confirmation(snapshot, cx);
        } else {
            self.wait_for_settings(None, cx);
        }
        ApplicationQuitDecision::Cancel
    }

    fn complete_after_settings_save(
        self: &Rc<Self>,
        authorization: Option<ApplicationQuitSnapshot>,
        outcome: ApplicationQuitSaveOutcome,
        cx: &mut App,
    ) {
        let mut state = self.state.borrow_mut();
        if *state != ApplicationQuitCoordinatorState::WaitingForSettings {
            return;
        }
        *state = ApplicationQuitCoordinatorState::Idle;
        drop(state);
        if matches!(outcome, ApplicationQuitSaveOutcome::Failed) {
            return;
        }
        if application_has_pending_close_confirmation(cx) {
            return;
        }
        let current = application_quit_snapshot(cx);
        let authorized = authorization.map_or_else(
            || !current.facts.requires_confirmation(),
            |authorization| authorization.authorizes(&current),
        );
        if authorized {
            if let Some(adapter) = self.adapter.upgrade() {
                adapter.confirm_quit(cx);
            }
        } else {
            self.present_confirmation(current, cx);
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
        let windows = workspace_windows(cx);
        let confirmation_window = cx
            .active_window()
            .and_then(|window| window.downcast::<WorkspaceManager>())
            .or_else(|| windows.into_iter().next());
        let Some(confirmation_window) = confirmation_window else {
            *self.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
            return;
        };
        let count = snapshot.facts.pane_count;
        let noun = if count == 1 { "Pane" } else { "Panes" };
        let detail = format!(
            "Quit with {count} open {noun}? Open Workspaces and Tabs will close, and any running commands will stop."
        );
        let prompt = confirmation_window.update(cx, |_, window, cx| {
            window.activate_window();
            window.prompt(
                PromptLevel::Critical,
                "Quit SpaceTerm?",
                Some(&detail),
                &[
                    PromptButton::ok("Quit SpaceTerm"),
                    PromptButton::cancel("Cancel"),
                ],
                cx,
            )
        });
        let Ok(prompt) = prompt else {
            *self.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
            return;
        };
        let coordinator = Rc::clone(self);
        cx.spawn(async move |cx| {
            let confirmed = prompt.await == Ok(0);
            let _ = cx.update(|cx| {
                if confirmed {
                    coordinator.wait_for_settings(Some(snapshot), cx);
                } else {
                    *coordinator.state.borrow_mut() = ApplicationQuitCoordinatorState::Idle;
                }
            });
        })
        .detach();
    }
}

fn application_has_pending_close_confirmation(cx: &App) -> bool {
    workspace_windows(cx).iter().any(|window| {
        window
            .read(cx)
            .is_ok_and(WorkspaceManager::has_pending_close_confirmation)
    })
}

fn application_quit_snapshot(cx: &App) -> ApplicationQuitSnapshot {
    let mut facts = crate::close_confirmation::ApplicationCloseFacts::default();
    let mut panes = Vec::new();
    for window in workspace_windows(cx) {
        if let Ok(manager) = window.read(cx) {
            facts.merge(manager.application_close_facts(cx));
            panes.extend(
                manager
                    .application_pane_facts(cx)
                    .into_iter()
                    .map(|facts| ApplicationQuitPane { window, facts }),
            );
        }
    }
    ApplicationQuitSnapshot { facts, panes }
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
    let appearance = crate::ui::appearance::chrome(cx);
    let workspace_titlebar_height = crate::ui::WorkspaceFrame::for_appearance(appearance, cx)
        .top_chrome_height(appearance.top_height());
    let workspace_traffic_light_position = host
        .window_frame
        .workspace_traffic_light_position(workspace_titlebar_height);
    let bounds = Bounds::centered(None, size(px(900.0), px(580.0)), cx);
    let result = cx.open_window(
        WindowOptions {
            window_background: crate::ui::appearance_runtime::window_background(cx),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.0), px(260.0))),
            titlebar: host.titlebar.as_ref().map(|titlebar| TitlebarOptions {
                title: titlebar.title.clone(),
                appears_transparent: titlebar.appears_transparent,
                traffic_light_position: workspace_traffic_light_position
                    .or(titlebar.traffic_light_position),
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

/// Every live Workspace window, resolved at call time from GPUI's own registry.
///
/// A Settings window is not a Workspace window: it presents no Workspace, cannot host one, and must
/// never be counted as one when SpaceTerm decides whether a Workspace still exists.
fn workspace_windows(cx: &App) -> Vec<gpui::WindowHandle<WorkspaceManager>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<WorkspaceManager>())
        .collect()
}

fn restore_default_window(cx: &mut App, host: &HostComposition) {
    if !workspace_windows(cx).is_empty() {
        return;
    }
    if let Err(error) = open(cx, host) {
        eprintln!("failed to restore the default SpaceTerm window: {error}");
    }
}

fn install_headless_window_actions(cx: &mut App, host: Rc<HostComposition>) {
    cx.on_action(move |_: &NewWorkspace, cx| {
        if !workspace_windows(cx).is_empty() {
            return;
        }
        if let Err(error) = open(cx, &host) {
            eprintln!("failed to restore the default SpaceTerm window: {error}");
        }
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

    fn application_quit() -> Rc<dyn ApplicationQuitAdapter> {
        Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        )
    }

    #[gpui::test]
    fn configured_shortcuts_should_bind_global_application_actions(cx: &mut TestAppContext) {
        cx.update(crate::ui::init).expect("UI initialization");
        cx.update(|cx| init(cx, application_menu(), application_quit()).unwrap());
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
        cx.update(|cx| init(cx, application_menu(), application_quit()).unwrap());
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
            crate::terminal::testing::test_local_directory(PathBuf::from(
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
    pub(crate) application_quit: Rc<dyn ApplicationQuitAdapter>,
    pub(crate) selected_files: Option<Arc<dyn crate::platform::selected_file::SelectedFileOpener>>,
    pub(crate) local_filesystem: crate::platform::local_filesystem::LocalFilesystemAuthority,
    pub(crate) key_input: Rc<dyn crate::terminal::TerminalKeyInputAdapterFactory>,
    pub(crate) accessibility:
        Rc<dyn crate::platform::terminal_accessibility::TerminalAccessibilityAdapterFactory>,
    pub(crate) native_services: crate::terminal::native_services::NativeServiceAdapters,
    pub(crate) lifecycle: crate::ui::pane_lifecycle::PaneLifecycleDependencies,
    pub(crate) permission_recovery:
        Option<Rc<dyn crate::platform::permission_recovery::PermissionRecoveryOpener>>,
    pub(crate) microphone_access:
        Option<Rc<dyn crate::platform::microphone_access::MicrophoneAccess>>,
    pub(crate) remote_workspace:
        Arc<dyn crate::ui::remote_workspace_flow::RemoteWorkspaceFlowBackendFactory>,
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
    pub(crate) session_factory: Rc<dyn TerminalSessionFactory>,
    pub(crate) adapters: ApplicationCapabilities,
    pub(crate) services: Rc<dyn crate::platform::services_registration::ServicesRegistration>,
    pub(crate) window_movement: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
    pub(crate) window_frame: crate::platform::window_frame::WindowFrameGeometry,
    pub(crate) titlebar: Option<TitlebarOptions>,
}
pub(crate) struct HostComposition {
    profile: crate::desktop_profile::DesktopProfile,
    home_directory: PathBuf,
    session_factory: Rc<dyn TerminalSessionFactory>,
    adapters: ApplicationCapabilities,
    services: Rc<dyn crate::platform::services_registration::ServicesRegistration>,
    window_movement: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
    window_frame: crate::platform::window_frame::WindowFrameGeometry,
    titlebar: Option<TitlebarOptions>,
    appearance: Option<(
        Arc<dyn crate::settings::storage::SettingsStorage>,
        Rc<dyn crate::platform::appearance::AppearancePlatform>,
    )>,
}
impl HostComposition {
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
            window_frame: parts.window_frame,
            titlebar: parts.titlebar,
            appearance: None,
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
    cx.set_global(host.window_frame);
    if let Some(opener) = &host.adapters.selected_files {
        cx.set_global(SelectedFileAccess(Arc::clone(opener)));
    }
    if let Some((storage, platform)) = &host.appearance {
        let (settings, changed) = crate::settings::UserSettings::load(Arc::clone(storage));
        crate::ui::appearance_runtime::install(settings, changed, Rc::clone(platform), cx)
            .map_err(|_| RuntimeError::Initialization)?;
    }
    crate::ui::initialize_controls(cx).map_err(|_| RuntimeError::Initialization)?;
    host.profile.install(cx);
    if let Err(error) = host.services.register() {
        eprintln!("failed to register Services: {error}");
    }
    crate::ui::settings_window::configure_window_chrome(
        Rc::clone(&host.window_movement),
        host.adapters.microphone_access.clone(),
        cx,
    );
    init(
        cx,
        Rc::clone(&host.adapters.application_menu),
        Rc::clone(&host.adapters.application_quit),
    )
    .map_err(|_| RuntimeError::Initialization)?;
    let workspace = open(cx, host)?;
    #[cfg(feature = "appearance-exerciser")]
    crate::ui::appearance_exerciser::open(workspace, cx)
        .map_err(|_| RuntimeError::Initialization)?;
    Ok(workspace)
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
                selected_files: None,
                application_menu: Rc::new(
                    crate::platform::application_menu::testing::RecordingApplicationMenuAdapter::default(),
                ),
                application_quit: Rc::new(
                    crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
                ),
                local_filesystem: crate::platform::local_filesystem::LocalFilesystemAuthority::testing(),
                key_input: Rc::new(crate::terminal::GpuiTerminalKeyInputAdapterFactory::default()),
                accessibility: Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
                native_services: crate::terminal::native_services::testing::adapters(),
                lifecycle: crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(),
                permission_recovery: None,
                microphone_access: None,
                remote_workspace: Arc::new(UnavailableRemote),
            },
            services,
            window_movement: movement,
            window_frame: crate::platform::window_frame::WindowFrameGeometry::default(),
            titlebar: None,
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
    fn workspace_window_should_open_windowed_at_the_default_size(cx: &mut gpui::TestAppContext) {
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
    fn new_workspace_action_should_restore_window_with_one_local_workspace_when_headless(
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

        cx.update(|cx| {
            let restored = cx.windows()[0].downcast::<WorkspaceManager>().unwrap();
            assert_eq!(restored.read(cx).unwrap().workspace_count(), 1);
        });

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

        assert!(cx.update(|cx| cx.is_action_available(&NewWorkspace)));
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

    /// Settings storage for composition tests: nothing is retained and nothing is written.
    struct EmptySettingsStorage;

    impl crate::settings::storage::SettingsStorage for EmptySettingsStorage {
        fn read(
            &self,
        ) -> Result<
            Option<crate::platform::secure_filesystem::PrivateFileSnapshot>,
            crate::settings::storage::StorageError,
        > {
            Ok(None)
        }

        fn write(
            &self,
            _: &[u8],
            _: Option<&crate::platform::secure_filesystem::SecureEntryIdentity>,
        ) -> Result<crate::settings::storage::StorageCommit, crate::settings::storage::StorageError>
        {
            Err(crate::settings::storage::StorageError::Unavailable)
        }
    }

    fn host_with_settings() -> Rc<HostComposition> {
        Rc::new(
            HostComposition::new(parts(Rc::default(), Rc::default()))
                .unwrap()
                .with_appearance(
                    Arc::new(EmptySettingsStorage),
                    Rc::new(
                        crate::platform::appearance::testing::RecordingAppearancePlatform::default(
                        ),
                    ),
                ),
        )
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
        let settings = cx.update(|cx| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<crate::ui::settings_window::SettingsWindow>())
                .expect("Settings window")
        });
        let mut settings_cx = gpui::VisualTestContext::from_window(settings.into(), cx);
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
        use crate::appearance::{ChromeDensity, SettingsDocument};
        use crate::platform::window_frame::{TrafficLightPlacement, WindowFrameGeometry};
        use gpui::{point, px};

        let geometry = WindowFrameGeometry::new(Some(16.0))
            .with_outer_edge_width(1.0)
            .with_traffic_lights(
                TrafficLightPlacement::new(point(px(15.5), px(14.0)), px(41.0)),
                TrafficLightPlacement::new(point(px(12.0), px(11.0)), px(36.0)),
            );
        let mut wiring = parts(Rc::default(), Rc::default());
        wiring.window_frame = geometry;
        let host = HostComposition::new(wiring).unwrap().with_appearance(
            Arc::new(EmptySettingsStorage),
            Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
        );
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
        cx.run_until_parked();
        let settings = cx.update(|cx| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<crate::ui::settings_window::SettingsWindow>())
                .expect("Settings window")
        });

        assert_eq!(
            (
                cx.traffic_light_position_updates(workspace.into()),
                cx.traffic_light_position_updates(settings.into()),
            ),
            (
                vec![point(px(15.5), px(14.0))],
                vec![point(px(12.0), px(11.0))],
            )
        );

        let user_settings = cx.update(|cx| {
            cx.global::<crate::ui::appearance_runtime::AppearanceRuntime>()
                .settings
                .clone()
        });
        let token = user_settings.begin_preview(0).unwrap();
        let mut candidate = SettingsDocument::default();
        candidate.preferences.chrome.density = ChromeDensity::Comfortable;
        user_settings.update_preview(&token, candidate).unwrap();
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
                    .settings_traffic_light_position(appearance.top_height())
                    .unwrap(),
            )
        });

        assert_eq!(
            (
                cx.traffic_light_position_updates(workspace.into()),
                cx.traffic_light_position_updates(settings.into()),
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
            workspace.update(cx, |_, window, _| window.blur()).unwrap();
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
    fn a_settings_only_window_leaves_application_quit_unblocked(cx: &mut gpui::TestAppContext) {
        let host = host_with_settings();
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&QuitApplication));
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();

        cx.update(|cx| {
            workspace
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();

        // Only Settings remains. A window that presents no Workspace has no work to confirm, so
        // quit must proceed rather than wait on it.
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
        cx.update(|cx| cx.dispatch_action(&QuitApplication));
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt());
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
        });

        let decision = cx.update(|cx| coordinator.request(cx));

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
        });

        cx.update(|cx| {
            coordinator.complete_after_settings_save(None, ApplicationQuitSaveOutcome::Failed, cx);
        });

        assert_eq!(
            *coordinator.state.borrow(),
            ApplicationQuitCoordinatorState::Idle
        );
    }

    #[gpui::test]
    fn application_quit_does_not_move_running_work_between_authorized_panes(
        cx: &mut gpui::TestAppContext,
    ) {
        let host = host_with_settings();
        let window = cx.update(|cx| start_application(cx, &host).unwrap());
        let pane = |pane_id, has_running_work| ApplicationQuitPane {
            window,
            facts: crate::close_confirmation::ApplicationPaneFacts {
                workspace_id: crate::domain::WorkspaceId::new(1),
                tab_id: crate::domain::TabId::new(1),
                pane_id: crate::domain::PaneId::new(pane_id),
                has_running_work,
            },
        };
        let authorized = ApplicationQuitSnapshot {
            facts: Default::default(),
            panes: vec![pane(1, true), pane(2, false)],
        };
        let current = ApplicationQuitSnapshot {
            facts: Default::default(),
            panes: vec![pane(1, false), pane(2, true)],
        };

        assert!(!authorized.authorizes(&current));
    }

    #[gpui::test]
    fn application_quit_checks_inactive_windows_and_discards_removed_roots(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};
        let records = TestTerminalSessionRecords::default();
        let mut wiring = parts(Rc::default(), Rc::default());
        wiring.session_factory = Rc::new(TestTerminalSessionFactory::new(records.clone()));
        let application_quit = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        wiring.adapters.application_quit = application_quit.clone();
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
            Default::default(),
            now,
        );
        assert!(metadata.apply_semantic_prompt("A", now));
        let mut screen =
            (*crate::terminal::ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix))
                .clone();
        screen.metadata = metadata.snapshot();
        let screen = Arc::new(screen);
        for session_id in [1, 2] {
            records
                .event_sender(session_id)
                .unwrap()
                .try_send(crate::terminal::SessionEvent::Screen(Arc::clone(&screen)))
                .unwrap();
        }
        cx.run_until_parked();

        cx.update(|cx| {
            assert!(cx.active_window() == Some(idle.into()));
            cx.dispatch_action(&QuitApplication);
        });
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        assert!(cx.update(|cx| cx.active_window() == Some(idle.into())));
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert!(records.dropped_session_ids().is_empty());
        cx.update(|cx| {
            running
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(running.read(cx).is_err());
            assert_eq!(cx.windows().len(), 1);
        });
        assert_eq!(
            application_quit.simulate_native_request(),
            ApplicationQuitDecision::Cancel
        );
        assert_eq!(application_quit.confirmations(), 1);
        assert!(!cx.has_pending_prompt());
    }

    #[gpui::test]
    fn application_quit_revalidates_equal_count_window_replacement_after_settings_save(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::ui::settings_window::test_support::MemoryStorage;

        let storage = MemoryStorage::with_document(&crate::appearance::SettingsDocument::default());
        let application_quit = Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        );
        let mut wiring = parts(Rc::default(), Rc::default());
        wiring.adapters.application_quit = application_quit.clone();
        let host = HostComposition::new(wiring).unwrap().with_appearance(
            storage.clone(),
            Rc::new(crate::platform::appearance::testing::RecordingAppearancePlatform::default()),
        );
        let workspace = cx.update(|cx| start_application(cx, &host).unwrap());
        cx.run_until_parked();
        cx.update(|cx| cx.dispatch_action(&crate::ui::settings_window::OpenSettings));
        cx.run_until_parked();
        let settings = cx.update(|cx| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<crate::ui::settings_window::SettingsWindow>())
                .expect("Settings window")
        });
        let mut settings_cx = gpui::VisualTestContext::from_window(settings.into(), cx);
        let edit = settings_cx
            .debug_bounds("settings-chrome-density-comfortable")
            .expect("Settings control")
            .center();
        settings_cx.simulate_mouse_move(edit, None, gpui::Modifiers::none());
        settings_cx.simulate_click(edit, gpui::Modifiers::none());
        settings_cx.run_until_parked();
        let blocked = storage.block_next_write();

        settings_cx
            .cx
            .update(|cx| cx.dispatch_action(&QuitApplication));
        settings_cx.run_until_parked();
        assert!(settings_cx.has_pending_prompt());
        settings_cx.simulate_prompt_answer("Quit SpaceTerm");
        settings_cx.cx.update(|cx| {
            open(cx, &host).unwrap();
            workspace
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
        let release = std::thread::spawn(move || {
            blocked.wait_until_started();
            blocked.release();
        });
        settings_cx.run_until_parked();
        release.join().unwrap();

        assert!(settings_cx.has_pending_prompt());
        assert_eq!(application_quit.confirmations(), 0);
    }
}
