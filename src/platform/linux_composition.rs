//! The sole production selector of Linux desktop policy and native capabilities.
use crate::app::{
    HostComposition, HostCompositionParts, StartupDependencies, StartupDependenciesError,
};
use crate::application_identity::{ApplicationIdentity, UpdateSource};
use crate::desktop_profile::{
    ControlKeybindingProfiles, DesktopPresentation, DesktopProfile, DesktopProfileError,
    DesktopWording, HostFeature,
};
use std::{rc::Rc, sync::Arc};

pub(crate) fn main() {
    let identity = ApplicationIdentity::current();
    let (events, desktop_events) = super::linux_desktop_events::LinuxDesktopEvents::new(identity);
    let bus = super::linux_session_bus::SessionBus::connect().ok();
    let token = std::env::var("XDG_ACTIVATION_TOKEN")
        .ok()
        .or_else(|| std::env::var("DESKTOP_STARTUP_ID").ok());
    if let Some(bus) = &bus
        && super::linux_application_instance::forward_launch_if_secondary(
            bus,
            identity,
            super::linux_application_instance::InstanceLaunch::from_arguments(
                std::env::args_os().skip(1),
            ),
            events.clone(),
            token,
            crate::invitation_links::from_arguments(std::env::args_os().skip(1)),
        )
        .unwrap_or(false)
    {
        return;
    }
    let relaunch = super::linux_updates::Relaunch::default();
    let installed = relaunch.clone();
    let code = crate::app::launch(capture_startup_dependencies(identity), |startup| {
        compose(startup, identity, bus, events, desktop_events, installed)
    });
    if code != 0 {
        std::process::exit(code);
    }
    // An installed update quit the application to replace this process with the new release.
    // If exec fails, the next launch starts the new release.
    if let Some(executable) = relaunch.take() {
        let _ = super::linux_updates::relaunch(&executable);
    }
}

fn update_adapter(
    identity: ApplicationIdentity,
    events: super::linux_desktop_events::DesktopEventSender,
    relaunch: super::linux_updates::Relaunch,
) -> Rc<dyn crate::updates::UpdateAdapter> {
    match identity.update_source() {
        UpdateSource::SignedFeed => Rc::new(super::linux_updates::LinuxUpdates::new(
            Arc::new(super::https_transport::HttpsTransport::new()),
            identity,
            update_history_paths(identity),
            relaunch,
            move || events.quit(),
        )),
        UpdateSource::Simulation => simulated_updates(),
        UpdateSource::Unavailable => Rc::new(crate::updates::UnavailableUpdates),
    }
}

/// The updater keeps its history in the private state directory of the identity.
fn update_history_paths(identity: ApplicationIdentity) -> Option<Arc<super::app_paths::AppPaths>> {
    let directories =
        super::app_directories::AppDirectories::resolve(identity.directory_name()).ok()?;
    super::app_paths::AppPaths::from_directories(
        directories,
        super::unix_local_socket::LOCAL_IPC_PATH_MAXIMUM,
        Arc::new(super::unix_secure_filesystem::UnixSecureFilesystem),
    )
    .ok()
    .map(Arc::new)
}

#[cfg(feature = "developer-tools")]
fn simulated_updates() -> Rc<dyn crate::updates::UpdateAdapter> {
    match std::env::var("SPACETERM_UPDATE_PREVIEW")
        .ok()
        .and_then(|value| crate::updates::preview::Scenario::parse(&value))
    {
        Some(scenario) => Rc::new(crate::updates::preview::PreviewUpdates::new(scenario)),
        None => Rc::new(crate::updates::UnavailableUpdates),
    }
}

#[cfg(not(feature = "developer-tools"))]
fn simulated_updates() -> Rc<dyn crate::updates::UpdateAdapter> {
    Rc::new(crate::updates::UnavailableUpdates)
}

fn capture_startup_dependencies(
    identity: ApplicationIdentity,
) -> Result<
    StartupDependencies<super::unix_ssh_process::UnixSshProcessAdapter>,
    StartupDependenciesError,
> {
    let path_environment = super::app_directories::AppDirectoryEnvironment::capture();
    let directories = super::app_directories::AppDirectories::resolve(identity.directory_name())
        .map_err(|_| StartupDependenciesError::Paths)?;
    let secure_filesystem: Arc<dyn super::secure_filesystem::SecureFilesystem> =
        Arc::new(super::unix_secure_filesystem::UnixSecureFilesystem);
    let paths = super::app_paths::AppPaths::from_directories(
        directories,
        super::unix_local_socket::LOCAL_IPC_PATH_MAXIMUM,
        Arc::clone(&secure_filesystem),
    )
    .map_err(|_| StartupDependenciesError::Paths)?;
    let inherited_path = std::env::var_os("PATH");
    let executable = super::linux_ssh_executable::capture(inherited_path.as_deref())
        .map_err(|_| StartupDependenciesError::Paths)?;
    StartupDependencies::capture(
        path_environment,
        crate::ssh::startup_environment::StartupSshEnvironment::from_environment(
            |key| {
                if key == "PATH" {
                    inherited_path.clone()
                } else {
                    std::env::var_os(key)
                }
            },
            "/usr/local/bin:/usr/bin:/bin".into(),
        )
        .map_err(|_| StartupDependenciesError::Paths)?,
        paths,
        executable,
        super::unix_ssh_process::UnixSshProcessAdapter,
        Arc::new(super::unix_host_config_filesystem::UnixHostConfigFilesystem),
    )
}

/// Replaces the key bindings with the Linux Desktop Profile's on US English, so a test drives the
/// Shortcuts a Linux desktop installs.
#[cfg(test)]
fn install_test_desktop_profile(cx: &mut gpui::App) {
    cx.clear_key_bindings();
    let profile = desktop_profile(
        Rc::new(crate::platform::locale::FixedLocaleDirection(
            spaceterm_ui::TextDirection::LeftToRight,
        )),
        crate::platform::keyboard_layout::testing::us(),
    )
    .expect("the Linux Desktop Profile is valid")
    .install(cx);
    crate::keybindings::runtime::install(profile, cx);
}

fn desktop_profile(
    locale: Rc<dyn super::locale::LocaleDirection>,
    layout: Rc<dyn super::keyboard_layout::KeyboardLayoutAdapter>,
) -> Result<DesktopProfile, DesktopProfileError> {
    Ok(DesktopProfile::new(
        spaceterm_ui::ModalDesktopPolicy::mac_os(),
        ControlKeybindingProfiles::new(
            spaceterm_ui::ModalKeybindingProfile::Linux,
            spaceterm_ui::MenuKeybindingProfile::Linux,
            spaceterm_ui::CommandPaletteKeybindingProfile::Linux,
            spaceterm_ui::ComboBoxKeybindingProfile::Linux,
            spaceterm_ui::TextInputKeybindingProfile::Linux,
        ),
        super::linux_default_keymap::profile(layout, super::linux_reserved_shortcuts::shortcuts())
            .map_err(|_| DesktopProfileError::InvalidCombination)?,
        DesktopPresentation::new(
            DesktopWording {
                file_preview: "Preview",
                this_computer: "This Computer",
                this_computer_phrase: "this computer",
                system_directory_selection: "Choose Directory…",
                reveal_file: "Show in Folder",
                read_only_update: (
                    "Reinstall SpaceTerm to Update",
                    "Run the SpaceTerm install script to install the update.",
                ),
                overdue_read_only_update: "Update overdue. Reinstall with the install script.",
            },
            Rc::new(super::linux_shortcut_text::LinuxShortcutFormatter),
            crate::desktop_profile::ShortcutSelection::TerminalSurface,
            &[
                HostFeature::ApplicationMenuBar,
                HostFeature::CaptureAccess,
                HostFeature::SystemPermissions,
            ],
            super::unix_local_time::local_time,
        ),
        locale,
    )
    .with_fonts(super::linux_fonts::capture()))
}

fn compose(
    startup: StartupDependencies<super::unix_ssh_process::UnixSshProcessAdapter>,
    identity: ApplicationIdentity,
    bus: Option<super::linux_session_bus::SessionBus>,
    events: super::linux_desktop_events::DesktopEventSender,
    desktop_events: super::linux_desktop_events::LinuxDesktopEvents,
    relaunch: super::linux_updates::Relaunch,
) -> Result<HostComposition, DesktopProfileError> {
    let updates = update_adapter(identity, events.clone(), relaunch);
    let appearance = Rc::new(super::linux_appearance::LinuxAppearancePlatform::new(
        bus.clone(),
    ));
    let settings_storage = startup.settings_storage();
    let settings_file = startup.settings_file();
    let activity: Rc<dyn crate::platform::application_activity::ApplicationActivity> =
        Rc::new(super::linux_application::LinuxApplicationActivity);
    let lifecycle = crate::ui::pane_lifecycle::PaneLifecycleDependencies {
        attention: crate::terminal::attention_runtime::AttentionRuntime::new(
            Box::new(super::linux_attention::LinuxAudioBell(events.clone())),
            Box::new(super::linux_attention::LinuxWindowAttention::new(
                events.clone(),
            )),
            Box::new(
                crate::terminal::attention_notification::AttentionNotifications::new(Arc::new(
                    super::linux_notification::LinuxNotificationAdapter::new(
                        bus.clone(),
                        identity,
                        events,
                    ),
                )),
            ),
            Rc::clone(&activity),
        ),
        secure_input: crate::terminal::secure_input::SecureInputHandle::unavailable(),
        activity,
        visibility: Rc::new(super::linux_window_visibility::LinuxWindowVisibilityFactory),
        wheel: Rc::new(super::linux_scroll::LinuxWheelPhaseEnrichment),
        permission_setup: None,
    };
    let paths = crate::local_path::LocalPathSemantics::Posix;
    let local_filesystem = super::local_filesystem::LocalFilesystemAuthority::new(
        paths,
        Arc::new(super::unix_local_identity::UnixLocalIdentity),
    );
    let direct_servers = startup.direct_servers();
    let ssh_setup = startup.ssh_setup_factory(
        Arc::new(super::unix_askpass_transport::AskPassWindowFactory::new(
            super::launch_host::running_executable(),
        )),
        super::launch_host::server_artifact_catalog(),
        direct_servers.clone(),
    );
    let server_context_paths = startup.server_context_paths();
    let command_line_tool = command_line_tool(identity, bus.clone());
    // Repository Status reads run on threads that the deterministic test executor rejects.
    let repository_status = (!cfg!(test)).then(|| {
        super::repository_status_host::repository_status_adapters(
            &startup.home_directory,
            Arc::new(
                super::linux_repository_tools::LinuxRepositoryToolDiscovery::new(
                    std::env::var_os("PATH").as_deref(),
                ),
            ),
        )
    });
    let controls = super::linux_window_style::capture(
        bus.as_ref(),
        &super::app_directories::DesktopResourceDirectories::capture(),
    );
    HostComposition::new(HostCompositionParts {
        profile: desktop_profile(
            Rc::new(super::linux_locale::LinuxLocale::capture(|key| std::env::var(key).ok())),
            Rc::new(super::linux_keyboard_layout::LinuxKeyboardLayout),
        )?,
        home_directory: startup.home_directory,
        adapters: crate::app::ApplicationCapabilities {
            updates,
            selected_files: Arc::new(super::unix_selected_file::UnixSelectedFileOpener),
            settings_file,
            application_menu: Rc::new(
                super::linux_application_menu::LinuxApplicationMenuAdapter,
            ),
            desktop_events: Rc::new(desktop_events),
            application_quit: Rc::new(
                super::linux_application_quit::LinuxApplicationQuitAdapter::new(|cx| cx.quit()),
            ),
            local_filesystem,
            key_input: Rc::new(super::linux_keyboard::LinuxTerminalKeyInputAdapterFactory::new()),
            accessibility: Rc::new(
                super::accesskit_terminal_accessibility::AccessKitTerminalAccessibilityAdapterFactory,
            ),
            native_services: crate::terminal::native_services::NativeServiceAdapters {
                text_clipboard: Rc::new(super::linux_clipboard::LinuxTextClipboard),
                primary_selection: Some(Rc::new(super::linux_clipboard::LinuxPrimarySelection(appearance.clone()))),
                selection_clipboard: Rc::new(super::linux_clipboard::LinuxSelectionClipboard),
                file_insertion: crate::terminal::native_services::file_insertion::FileInsertionPolicy {
                    paths,
                    shell: crate::terminal::native_services::file_insertion::ShellInsertionDialect::Posix,
                },
                file_clipboard: Rc::new(super::linux_clipboard::LinuxFileClipboard),
                file_preview: Rc::new(super::linux_file_preview::LinuxFilePreviewFactory::new(bus.clone())),
            },
            lifecycle,
            capture_access: None,
            permission_access: None,
            setup_guide: None,
            theme_registry: Arc::new(super::https_transport::HttpsTransport::new()),
            repository_status,
            background_images: None,
            local_server: crate::ui::LocalServerAdapters::account(),
            ssh_setup,
            direct_servers,
            server_context_paths,
            command_line_tool,
            desktop_channel: super::unix_desktop_channel::DesktopChannelListener::bind(
                identity.directory_name(),
            )
            .ok()
            .map(|listener| {
                Arc::new(listener) as Arc<dyn crate::desktop_channel::DesktopChannelEndpoint>
            }),
        },
        services: None,
        window_movement: Rc::new(super::linux_window_drag::LinuxWindowMovementFactory),
        window_frame: super::window_frame::WindowFrameGeometry::new(Some(controls.style.corner_radius())).with_outer_edge_width(1.0),
        window_chrome: super::window_chrome::WindowChrome::client().with_controls(controls),
    })
    .map(|host| {
        host.with_modal_prompts(true)
            .with_appearance(settings_storage, appearance)
            .with_status_menu(super::linux_status_menu::install)
    })
}

/// This build's command, installed through the account's service and shown in the file manager.
fn command_line_tool(
    identity: ApplicationIdentity,
    bus: Option<super::linux_session_bus::SessionBus>,
) -> Option<Arc<crate::command_line_tool::CommandLineTool>> {
    let files =
        super::account_launcher_files::AccountLauncherFiles::new(identity.directory_name()).ok()?;
    Some(Arc::new(crate::command_line_tool::CommandLineTool::new(
        identity.directory_name(),
        crate::command_line_tool::CommandLineToolAdapters {
            files: Arc::new(files),
            viewer: Arc::new(super::linux_file_viewer::FileManagerViewer::new(bus)),
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn linux_desktop_composition_installs_events_independently_of_the_menu(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::platform::app_directories::{AppDirectories, AppDirectoryEnvironment};
        let identity = ApplicationIdentity::current();
        let temporary = std::env::temp_dir().join(format!(
            "spaceterm-composition-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&temporary).unwrap();
        let environment = AppDirectoryEnvironment {
            home: Some(temporary.clone().into()),
            ..Default::default()
        };
        let directories = AppDirectories::resolve_xdg(
            identity.directory_name(),
            &environment,
            Some(temporary.clone()),
        )
        .unwrap();
        let startup = StartupDependencies::capture(
            environment,
            crate::ssh::startup_environment::StartupSshEnvironment::default(),
            super::super::app_paths::AppPaths::from_directories(
                directories,
                super::super::unix_local_socket::LOCAL_IPC_PATH_MAXIMUM,
                Arc::new(super::super::unix_secure_filesystem::UnixSecureFilesystem),
            )
            .unwrap(),
            crate::ssh::command::OpenSshExecutable::new(temporary.join("missing-ssh")).unwrap(),
            super::super::unix_ssh_process::UnixSshProcessAdapter,
            Arc::new(super::super::unix_host_config_filesystem::UnixHostConfigFilesystem),
        )
        .unwrap();
        let (sender, events) =
            super::super::linux_desktop_events::LinuxDesktopEvents::new(identity);
        let host = compose(
            startup,
            identity,
            None,
            sender,
            events,
            super::super::linux_updates::Relaunch::default(),
        )
        .unwrap();
        cx.update(|cx| crate::app::initialize_application(cx, &host).unwrap());
        assert_eq!(
            cx.app_identity(),
            Some((
                identity.application_id().into(),
                identity.display_name().into()
            ))
        );
        std::fs::remove_dir_all(temporary).unwrap();
    }

    #[gpui::test]
    fn desktop_profile_should_present_linux_shortcuts_and_wording(cx: &mut gpui::TestAppContext) {
        use crate::ui::{ClosePane, CloseTab, CreateTab, NewWorkspace, SwitchWorkspace};

        cx.update(|cx| {
            crate::ui::initialize_controls(cx).unwrap();
            desktop_profile(
                Rc::new(crate::platform::locale::FixedLocaleDirection(
                    spaceterm_ui::TextDirection::LeftToRight,
                )),
                crate::platform::keyboard_layout::testing::us(),
            )
            .unwrap()
            .install(cx);
            gpui::BorrowAppContext::update_global::<DesktopPresentation, _>(
                cx,
                |presentation, cx| presentation.refresh(cx),
            );
            let presentation = DesktopPresentation::get(cx);
            for (shortcut, label) in [
                (presentation.shortcut(&SwitchWorkspace), "Ctrl+Shift+K"),
                (presentation.shortcut(&NewWorkspace), "Ctrl+Shift+N"),
                (presentation.shortcut(&CreateTab), "Ctrl+Shift+T"),
                (presentation.shortcut(&ClosePane), "Ctrl+Shift+W"),
                (presentation.shortcut(&CloseTab), "Ctrl+Shift+Alt+W"),
                (presentation.shortcut(&crate::ui::ActivateTab1), "Alt+1"),
                (
                    presentation.shortcut(&crate::ui::ActivateWorkspace1),
                    "Ctrl+Alt+1",
                ),
                (presentation.shortcut(&crate::ui::NextTab), "Ctrl+Page Down"),
                (
                    presentation.shortcut(&crate::ui::ScrollPageUp),
                    "Shift+Page Up",
                ),
                (
                    presentation.shortcut(&crate::ui::IncreaseTerminalFontSize),
                    "Ctrl+=",
                ),
                (
                    presentation.shortcut(&crate::ui::settings_window::OpenKeyboardShortcuts),
                    "Ctrl+Shift+/",
                ),
                (
                    presentation.shortcut(&spaceterm_ui::EditCopy),
                    "Ctrl+Shift+C",
                ),
                (
                    presentation.shortcut(&crate::ui::settings_window::OpenSettings),
                    "Ctrl+,",
                ),
                (presentation.shortcut(&crate::app::ToggleFullScreen), "F11"),
            ] {
                assert_eq!(shortcut.as_deref(), Some(label));
            }
            assert_eq!(presentation.wording().file_preview, "Preview");
            assert_eq!(
                presentation.wording().system_directory_selection,
                "Choose Directory…"
            );
            assert!(presentation.has_feature(HostFeature::Updates));
            for feature in [
                HostFeature::ApplicationMenuBar,
                HostFeature::CaptureAccess,
                HostFeature::SystemPermissions,
            ] {
                assert!(!presentation.has_feature(feature), "{feature:?}");
            }
        });
    }

    #[gpui::test]
    fn linux_complete_profile_installs_the_expected_bindings(cx: &mut gpui::TestAppContext) {
        let actual = cx.update(|cx| {
            crate::ui::initialize_controls(cx).unwrap();
            desktop_profile(
                Rc::new(crate::platform::locale::FixedLocaleDirection(
                    spaceterm_ui::TextDirection::LeftToRight,
                )),
                crate::platform::keyboard_layout::testing::us(),
            )
            .unwrap()
            .install(cx);
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let customizable = keymap
                .bindings()
                .enumerate()
                .filter_map(|(index, binding)| {
                    (binding.meta() == Some(crate::keybindings::CUSTOMIZABLE_BINDINGS))
                        .then_some(index)
                })
                .collect::<Vec<_>>();
            assert_eq!(customizable.len(), 65);
            assert!(customizable.windows(2).all(|pair| pair[1] == pair[0] + 1));
            keymap
                .bindings()
                .map(|binding| {
                    let keys = binding
                        .keystrokes()
                        .iter()
                        .map(|key| key.unparse())
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!(
                        "{}\t{:?}\t{}",
                        keys,
                        binding.predicate(),
                        binding.action().name()
                    )
                })
                .collect::<Vec<_>>()
        });
        let expected = include_str!("../keybindings_baseline_linux.txt")
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        #[cfg(feature = "developer-tools")]
        let expected = {
            let mut expected = expected;
            let fixed_bindings = expected
                .iter()
                .position(|binding| binding == "ctrl-shift-c\tSome(Identifier(\"TerminalPane\"))\tspaceterm_text_input::Copy")
                .unwrap();
            expected.splice(fixed_bindings..fixed_bindings, [
                "ctrl-shift-w\tSome(Identifier(\"DeveloperWorkbench\"))\tspaceterm::CloseDeveloperWorkbench".to_owned(),
                "ctrl-w\tSome(Identifier(\"DeveloperWorkbench\"))\tspaceterm::CloseDeveloperWorkbench".to_owned(),
            ]);
            expected.extend([
                "ctrl-alt-shift-a\tNone\tspaceterm::OpenDeveloperWorkbench".to_owned(),
                "ctrl-alt-shift-c\tNone\tspaceterm::ToggleAppearancePreview".to_owned(),
            ]);
            expected
        };
        assert_eq!(actual, expected);
    }

    fn linux_terminal_pane(
        cx: &mut gpui::TestAppContext,
    ) -> (
        gpui::Entity<crate::ui::TerminalPane>,
        &mut gpui::VisualTestContext,
        crate::terminal::testing::TestTerminalSessionRecords,
    ) {
        cx.update(|cx| {
            crate::ui::init(cx).unwrap();
            install_test_desktop_profile(cx);
        });
        let records = crate::terminal::testing::TestTerminalSessionRecords::default();
        let factory = crate::terminal::WorkspaceTerminalSessionFactory::new_server(Rc::new(
            crate::terminal::testing::TestTerminalSessionFactory::new(records.clone()),
        ));
        use crate::terminal::TerminalKeyInputAdapterFactory;
        let (pane, cx) = cx.add_window_view(|window, cx| {
            crate::ui::TerminalPane::with_adapters(
                factory,
                super::super::linux_keyboard::LinuxTerminalKeyInputAdapterFactory::new().create(),
                &crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default(),
                crate::terminal::native_services::testing::adapters(),
                crate::ui::pane_lifecycle::PaneLifecycleDependencies::testing(), window, cx,
            )
        });
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, cx| pane.focus(window, cx));
        });
        cx.run_until_parked();
        (pane, cx, records)
    }

    #[cfg(feature = "developer-tools")]
    #[gpui::test]
    fn developer_chords_are_refused_by_the_shortcut_recorder(cx: &mut gpui::TestAppContext) {
        use crate::keybindings::{Command, Shortcut};
        use crate::settings::SettingsDocument;
        use crate::settings::storage::testing::MemoryStorage;
        use crate::ui::settings_window::SettingsWindow;
        let settings = crate::settings::Settings::load(MemoryStorage::with_document(
            &SettingsDocument::default(),
        ));
        struct RecordingMovement;
        impl super::super::window_movement::WindowMovementFactory for RecordingMovement {
            fn create(
                &self,
            ) -> Rc<dyn super::super::window_movement::OperatingSystemWindowDragPlatform>
            {
                Rc::new(super::super::window_movement::RecordingOperatingSystemWindowDragPlatform::default())
            }
        }
        cx.update(|cx| {
            crate::ui::appearance_runtime::install(
                settings.clone(),
                Rc::new(super::super::appearance::testing::RecordingAppearancePlatform::default()),
                cx,
            )
            .unwrap();
            crate::ui::init(cx).unwrap();
            install_test_desktop_profile(cx);
            crate::ui::settings_window::configure_window_chrome(
                Rc::new(RecordingMovement),
                Default::default(),
                crate::theme_registry::testing::offline(),
                cx,
            );
            crate::ui::settings_window::open_or_activate(None, cx);
        });
        cx.run_until_parked();
        let opened = cx
            .windows()
            .into_iter()
            .find_map(|window| window.downcast::<SettingsWindow>())
            .unwrap();
        let cx = &mut gpui::VisualTestContext::from_window(opened.into(), cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let click = |selector, cx: &mut gpui::VisualTestContext| {
            let center = cx.debug_bounds(selector).unwrap().center();
            cx.simulate_click(center, gpui::Modifiers::none());
            cx.run_until_parked();
        };
        click("settings-navigation-settings-section-keybindings", cx);
        for (chord, replacement) in [
            ("ctrl-shift-alt-a", "ctrl-shift-alt-j"),
            ("ctrl-shift-alt-c", "ctrl-shift-alt-l"),
        ] {
            let before = settings
                .snapshot()
                .committed
                .keybindings
                .get(Command::NewWorkspace)
                .cloned();
            click("settings-row-shortcut-new-workspace-control", cx);
            cx.simulate_keystrokes(chord);
            cx.run_until_parked();
            cx.executor()
                .advance_clock(std::time::Duration::from_secs(1));
            cx.run_until_parked();
            assert_eq!(
                settings
                    .snapshot()
                    .committed
                    .keybindings
                    .get(Command::NewWorkspace)
                    .cloned(),
                before
            );
            // A refused chord keeps recording, so a later valid chord is accepted without a click.
            cx.simulate_keystrokes(replacement);
            cx.run_until_parked();
            cx.executor()
                .advance_clock(std::time::Duration::from_secs(1));
            cx.run_until_parked();
            assert_eq!(
                settings
                    .snapshot()
                    .committed
                    .keybindings
                    .get(Command::NewWorkspace),
                Some(&Some(Shortcut::parse(replacement).unwrap()))
            );
            cx.simulate_keystrokes("escape");
        }
    }

    #[gpui::test]
    fn linux_function_keys_reach_terminal_unless_find_owns_navigation(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::testing::{RecordedCommand, TerminalEmulator};
        use crate::terminal::{FindDirection, KeyAction, PhysicalKey};
        let (_pane, cx, records) = linux_terminal_pane(cx);
        cx.simulate_keystrokes("f3 shift-f3 f9");
        let keys = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedCommand::Key(key) if key.action == KeyAction::Press => {
                    Some((key.physical_key, key.modifiers.shift))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                (PhysicalKey::F3, false),
                (PhysicalKey::F3, true),
                (PhysicalKey::F9, false)
            ]
        );
        let geometry = crate::terminal::geometry::TerminalGeometry::from_grid(
            crate::terminal::geometry::CellGridSize::new(80, 30),
            crate::terminal::geometry::LogicalCellSize::new(10.0, 20.0),
            crate::terminal::geometry::BackingScale::ONE,
        );
        let mut emulator = TerminalEmulator::new(geometry).unwrap();
        let bytes = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedCommand::Key(key) => Some(emulator.key(key).unwrap().bytes),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(bytes, b"\x1bOR\x1b[13;2~\x1b[20~");
        assert!(cx.update(|_, cx| {
            cx.key_bindings()
                .borrow()
                .bindings_for_action(&crate::ui::ToggleSidebar)
                .all(|binding| binding.keystrokes()[0].key() != "f9")
        }));
        cx.dispatch_action(crate::ui::OpenTerminalFind);
        cx.simulate_keystrokes("f3 shift-f3");
        let directions = records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedCommand::NavigateFind(_, direction) => Some(direction),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(directions, [FindDirection::Next, FindDirection::Previous]);
        // Closing Find restores the same function keys to the Terminal Session.
        cx.simulate_keystrokes("escape f3");
        assert_eq!(
            records
                .commands()
                .into_iter()
                .filter(|call| matches!(&call.command,
            RecordedCommand::Key(key) if key.action == KeyAction::Press))
                .count(),
            4
        );
    }

    #[gpui::test]
    fn linux_installed_copy_binding_forwards_host_modifiers(cx: &mut gpui::TestAppContext) {
        use crate::terminal::testing::RecordedCommand;
        let (_pane, cx, records) = linux_terminal_pane(cx);
        cx.simulate_keystrokes("ctrl-shift-c");
        cx.run_until_parked();
        let copies = records
            .commands()
            .into_iter()
            .filter_map(|call| {
                if let RecordedCommand::CopyOrForward(modifiers) = call.command {
                    Some(modifiers)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            copies,
            [crate::terminal::InputModifiers {
                control: true,
                shift: true,
                ..Default::default()
            }]
        );
    }

    #[gpui::test]
    fn linux_installed_keymap_preserves_unshifted_xterm_control_forms(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::terminal::testing::{RecordedCommand, TerminalEmulator};
        let (_pane, cx, records) = linux_terminal_pane(cx);
        let geometry = crate::terminal::geometry::TerminalGeometry::from_grid(
            crate::terminal::geometry::CellGridSize::new(80, 30),
            crate::terminal::geometry::LogicalCellSize::new(10.0, 20.0),
            crate::terminal::geometry::BackingScale::ONE,
        );
        let mut emulator = TerminalEmulator::new(geometry).unwrap();
        for (chord, scancode, character, expected) in [
            ("ctrl-2", 3, '2', b"\x00".as_slice()),
            ("ctrl-6", 7, '6', b"\x1e".as_slice()),
            ("ctrl-/", 53, '/', b"\x1f".as_slice()),
            ("ctrl-[", 26, '[', b"\x1b".as_slice()),
            ("ctrl-]", 27, ']', b"\x1d".as_slice()),
        ] {
            for native in [false, true] {
                let before = records.commands().len();
                if native {
                    let keystroke = gpui::Keystroke::parse(chord).unwrap();
                    let facts = gpui::NativeKeyEvent {
                        scancode,
                        unshifted: Some(character),
                        modifiers: gpui::Modifiers::control(),
                        ..Default::default()
                    };
                    cx.simulate_native_key_event(
                        gpui::KeyDownEvent {
                            keystroke: keystroke.clone(),
                            is_held: false,
                            prefer_character_input: false,
                        },
                        facts,
                    );
                    cx.simulate_native_key_event(gpui::KeyUpEvent { keystroke }, facts);
                } else {
                    cx.simulate_keystrokes(chord);
                }
                cx.run_until_parked();
                let inputs = records
                    .commands()
                    .into_iter()
                    .skip(before)
                    .filter_map(|call| {
                        if let RecordedCommand::Key(input) = call.command {
                            Some(input)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                assert!(
                    !inputs.is_empty(),
                    "{chord} must reach the Terminal Session"
                );
                let bytes = inputs
                    .into_iter()
                    .flat_map(|input| emulator.key(input).unwrap().bytes)
                    .collect::<Vec<_>>();
                assert_eq!(bytes, expected, "{chord}, native={native}");
            }
        }
    }
}
