//! The sole production selector of Linux desktop policy and native capabilities.
use crate::app::{
    HostComposition, HostCompositionParts, StartupDependencies, StartupDependenciesError,
};
use crate::application_identity::ApplicationIdentity;
use crate::desktop_profile::{
    ControlKeybindingProfiles, DesktopPresentation, DesktopProfile, DesktopProfileError,
    DesktopWording, HostFeature,
};
use crate::terminal::NativeTerminalSessionFactory;
use std::{path::PathBuf, rc::Rc, sync::Arc};

pub(crate) fn main() {
    let identity = ApplicationIdentity::current();
    let (events, desktop_events) = super::linux_desktop_events::LinuxDesktopEvents::new();
    let bus = super::linux_session_bus::SessionBus::connect().ok();
    let token = std::env::var("XDG_ACTIVATION_TOKEN")
        .ok()
        .or_else(|| std::env::var("DESKTOP_STARTUP_ID").ok());
    if let Some(bus) = &bus
        && super::linux_application_instance::forward_if_secondary(
            bus,
            identity,
            super::linux_application_instance::InstanceLaunch::from_arguments(
                std::env::args_os().skip(1),
            ),
            events.clone(),
            token,
        )
        .unwrap_or(false)
    {
        return;
    }
    let code = crate::app::launch(capture_startup_dependencies(identity), |startup| {
        compose(startup, identity, bus, events, desktop_events)
    });
    if code != 0 {
        std::process::exit(code);
    }
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
    let executable = crate::ssh::command::OpenSshExecutable::new(PathBuf::from("/usr/bin/ssh"))
        .map_err(|_| StartupDependenciesError::Paths)?;
    StartupDependencies::capture(
        path_environment,
        crate::ssh::startup_environment::StartupSshEnvironment::from_environment(
            |key| std::env::var_os(key),
            "/usr/local/bin:/usr/bin:/bin".into(),
        )
        .map_err(|_| StartupDependenciesError::Paths)?,
        paths,
        executable,
        super::unix_ssh_process::UnixSshProcessAdapter,
        Arc::new(super::unix_local_socket::UnixControlSocketProbe),
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
                operating_system_name: "Linux",
                system_directory_selection: "Choose Directory…",
            },
            Rc::new(super::linux_shortcut_text::LinuxShortcutFormatter),
            crate::desktop_profile::ShortcutSelection::TerminalSurface,
            &[
                HostFeature::Updates,
                HostFeature::MicrophoneAccess,
                HostFeature::SystemPermissions,
            ],
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
) -> Result<HostComposition, DesktopProfileError> {
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
            Box::new(super::linux_attention::LinuxWindowAttention(events.clone())),
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
    };
    let paths = crate::local_path::LocalPathSemantics::Posix;
    let local_filesystem = super::local_filesystem::LocalFilesystemAuthority::new(
        paths,
        Arc::new(super::unix_local_identity::UnixLocalIdentity),
    );
    let session_factory = Rc::new(NativeTerminalSessionFactory::new(
        Arc::new(super::unix_pty::UnixNativePtyAdapterFactory::new(Arc::new(
            super::linux_pty_host::LinuxPtyHost,
        ))),
        super::launch_host::shell_launch_planner(),
        local_filesystem.clone(),
        crate::terminal::metadata::LocalMachine::new(
            super::launch_host::local_user().as_deref(),
            super::launch_host::local_hostname().as_deref(),
            startup.home_directory.to_str(),
        ),
    ));
    let remote_workspace = startup.remote_backend_factory(Arc::new(
        super::unix_askpass_transport::AskPassWindowFactory::new(
            super::launch_host::running_executable(),
        ),
    ));
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
        session_factory,
        adapters: crate::app::ApplicationCapabilities {
            updates: Rc::new(crate::updates::UnavailableUpdates),
            selected_files: Some(Arc::new(super::unix_selected_file::UnixSelectedFileOpener)),
            settings_file: Some(settings_file),
            application_menu: Rc::new(
                super::linux_application_menu::LinuxApplicationMenuAdapter::new(identity, desktop_events),
            ),
            application_quit: Rc::new(
                super::linux_application_quit::LinuxApplicationQuitAdapter::default(),
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
            microphone_access: None,
            theme_registry: Some(Arc::new(super::https_transport::HttpsTransport::new())),
            remote_workspace,
        },
        services: None,
        window_movement: Rc::new(super::linux_window_drag::LinuxWindowMovementFactory),
        window_frame: super::window_frame::WindowFrameGeometry::new(Some(controls.style.corner_radius())).with_outer_edge_width(1.0),
        window_chrome: super::window_chrome::WindowChrome::client().with_controls(controls),
    })
    .map(|host| {
        host.with_modal_prompts(true).with_appearance(
            settings_storage,
            appearance,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert_eq!(presentation.wording().operating_system_name, "Linux");
            assert_eq!(
                presentation.wording().system_directory_selection,
                "Choose Directory…"
            );
            for feature in [
                HostFeature::Updates,
                HostFeature::MicrophoneAccess,
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
            assert_eq!(customizable.len(), 66);
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
        let factory = crate::terminal::WorkspaceTerminalSessionFactory::new_local(
            Rc::new(crate::terminal::testing::TestTerminalSessionFactory::new(
                records.clone(),
            )),
            crate::terminal::testing::test_local_directory(PathBuf::from("/tmp/linux-keymap-test")),
        );
        use crate::terminal::TerminalKeyInputAdapterFactory;
        let prepared = factory.prepare_child_launch().unwrap();
        let (pane, cx) = cx.add_window_view(|window, cx| {
            crate::ui::TerminalPane::new_with_prepared_launch(
                factory, prepared,
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

    #[gpui::test]
    fn linux_installed_copy_binding_forwards_host_modifiers(cx: &mut gpui::TestAppContext) {
        use crate::terminal::testing::RecordedSessionCommand;
        let (_pane, cx, records) = linux_terminal_pane(cx);
        cx.simulate_keystrokes("ctrl-shift-c");
        cx.run_until_parked();
        let copies = records
            .commands()
            .into_iter()
            .filter_map(|call| {
                if let RecordedSessionCommand::CopyOrForward(modifiers) = call.command {
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
        use crate::terminal::testing::{RecordedSessionCommand, TerminalEmulator};
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
                        if let RecordedSessionCommand::Key(input) = call.command {
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
