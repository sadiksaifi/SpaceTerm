//! The sole production selector of desktop policy and native capabilities.
use crate::app::{
    HostComposition, HostCompositionParts, StartupDependencies, StartupDependenciesError,
};
use crate::application_identity::ApplicationIdentity;
use crate::desktop_profile::{
    ControlKeybindingProfiles, DesktopPresentation, DesktopProfile, DesktopProfileError,
    DesktopWording,
};
use crate::terminal::{NativeTerminalSessionFactory, OptionAsAltPolicy};
use gpui::TitlebarOptions;
use std::{path::PathBuf, rc::Rc, sync::Arc};

pub(crate) fn main() {
    let identity = ApplicationIdentity::current();
    let code = match super::unix_askpass_transport::dispatch_helper_from_environment() {
        Some(code) => code,
        None => crate::app::launch(capture_startup_dependencies(identity), |startup| {
            compose(startup, identity)
        }),
    };
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
            "/usr/bin:/bin".into(),
        )
        .map_err(|_| StartupDependenciesError::Paths)?,
        paths,
        executable,
        super::unix_ssh_process::UnixSshProcessAdapter,
        Arc::new(super::unix_local_socket::UnixControlSocketProbe),
        Arc::new(super::unix_host_config_filesystem::UnixHostConfigFilesystem),
    )
}

fn desktop_profile(
    locale: Rc<dyn super::locale::LocaleDirection>,
) -> Result<DesktopProfile, DesktopProfileError> {
    Ok(DesktopProfile::new(
        spaceterm_ui::ModalDesktopPolicy::mac_os(),
        ControlKeybindingProfiles::new(
            spaceterm_ui::ModalKeybindingProfile::MacOs,
            spaceterm_ui::MenuKeybindingProfile::MacOs,
            spaceterm_ui::CommandPaletteKeybindingProfile::MacOs,
            spaceterm_ui::ComboBoxKeybindingProfile::MacOs,
            spaceterm_ui::TextInputKeybindingProfile::MacOs,
        ),
        crate::desktop_profile::default_keymap::profile(
            Rc::new(super::macos_keyboard_layout::MacosKeyboardLayout),
            super::macos_reserved_shortcuts::shortcuts(),
        )
        .map_err(|_| DesktopProfileError::InvalidCombination)?,
        DesktopPresentation::new(
            DesktopWording {
                file_preview: "Quick Look",
                operating_system_name: "macOS",
                system_directory_selection: "Choose in Finder…",
            },
            Rc::new(super::macos_shortcut_glyphs::MacosShortcutFormatter),
            crate::desktop_profile::ShortcutSelection::NativeMenu,
        ),
        locale,
    )
    .with_fonts(crate::host_fonts::HostFonts {
        ui_family: ".SystemUIFont".into(),
        system_monospace_family: "Menlo".into(),
        terminal_families: &[
            crate::bundled_font::FAMILY,
            "JetBrainsMono Nerd Font",
            "JetBrainsMono Nerd Font Mono",
            "JetBrains Mono",
        ],
        emoji_family: "Apple Color Emoji".into(),
        bundled_ui_faces: &[],
    }))
}

fn compose(
    startup: StartupDependencies<super::unix_ssh_process::UnixSshProcessAdapter>,
    identity: ApplicationIdentity,
) -> Result<HostComposition, DesktopProfileError> {
    let settings_storage = startup.settings_storage();
    let settings_file = startup.settings_file();
    let activity: Rc<dyn crate::platform::application_activity::ApplicationActivity> =
        Rc::new(crate::platform::macos_application::MacosApplicationActivity);
    let lifecycle = crate::ui::pane_lifecycle::PaneLifecycleDependencies {
        attention: crate::terminal::attention_runtime::AttentionRuntime::new(
            Box::new(crate::platform::macos_attention::AppKitAudioBell),
            Box::new(crate::platform::macos_attention::AppKitDockAttention::default()),
            Box::new(
                crate::terminal::attention_notification::AttentionNotifications::new(Arc::new(
                    crate::platform::macos_notification::UserNotificationAdapter::new(identity),
                )),
            ),
            Rc::clone(&activity),
        ),
        secure_input: crate::terminal::secure_input::SecureInputHandle::new(Box::new(
            crate::platform::macos_secure_input::MacosSecureInputAdapter::new(),
        )),
        activity,
        visibility: Rc::new(crate::platform::macos_render_lifecycle::MacosWindowVisibilityFactory),
        wheel: Rc::new(crate::platform::macos_scroll::MacosWheelPhaseEnrichment),
    };
    let paths = crate::local_path::LocalPathSemantics::Posix;
    let local_filesystem = super::local_filesystem::LocalFilesystemAuthority::new(
        paths,
        Arc::new(super::unix_local_identity::UnixLocalIdentity),
    );
    let session_factory = Rc::new(NativeTerminalSessionFactory::new(
        Arc::new(super::unix_pty::UnixNativePtyAdapterFactory::new(Arc::new(
            super::macos_pty_host::MacosPtyHost,
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
        super::unix_askpass_transport::AskPassWindowFactory,
    ));
    HostComposition::new(HostCompositionParts {
        profile: desktop_profile(Rc::new(super::macos_locale::ApplicationLocale))?,
        home_directory: startup.home_directory,
        session_factory,
        adapters: crate::app::ApplicationCapabilities {
            updates: update_adapter(),
            selected_files: Some(Arc::new(super::macos_selected_file::MacosSelectedFileOpener)),
            settings_file: Some(settings_file),
            application_menu: Rc::new(
                super::macos_application_menu::MacosApplicationMenuAdapter::new(identity),
            ),
            application_quit: Rc::new(
                super::macos_application_quit::MacosApplicationQuitAdapter::new(),
            ),
            local_filesystem,
            key_input: Rc::new(
                super::macos_keyboard::MacosTerminalKeyInputAdapterFactory::new(
                    OptionAsAltPolicy::default(),
                ),
            ),
            accessibility: Rc::new(
                super::macos_accessibility::MacosTerminalAccessibilityAdapterFactory,
            ),
            native_services: crate::terminal::native_services::NativeServiceAdapters {
                selection_clipboard: Rc::new(super::macos_pasteboard::MacosSelectionClipboard),
                file_insertion: crate::terminal::native_services::file_insertion::FileInsertionPolicy {
                    paths,
                    shell: crate::terminal::native_services::file_insertion::ShellInsertionDialect::Posix,
                },
                file_clipboard: Rc::new(super::macos_pasteboard::MacosFileClipboard { paths }),
                file_preview: Rc::new(super::macos_quick_look::MacosQuickLookFactory),
            },
            lifecycle,
            microphone_access: if cfg!(any(
                feature = "development-app",
                feature = "appearance-exerciser"
            )) {
                None
            } else {
                Some(Rc::new(
                    super::macos_microphone_access::MacosMicrophoneAccess::new(),
                ))
            },
            theme_registry: Some(Arc::new(super::https_transport::HttpsTransport::new())),
            remote_workspace,
        },
        services: Rc::new(super::macos_services::NativeServicesRegistration),
        window_movement: Rc::new(super::macos_window_drag::WindowMovementFactory),
        window_frame: super::macos_window_frame::window_frame_geometry(),
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: None,
        }),
    })
    .map(|host| {
        host.with_appearance(
            settings_storage,
            Rc::new(super::macos_appearance::MacosAppearancePlatform),
        )
    })
}

fn update_adapter() -> Rc<dyn crate::updates::UpdateAdapter> {
    #[cfg(feature = "development-app")]
    if let Ok(value) = std::env::var("SPACETERM_UPDATE_PREVIEW")
        && let Some(scenario) = crate::updates::preview::Scenario::parse(&value)
    {
        return Rc::new(crate::updates::preview::PreviewUpdates::new(scenario));
    }
    Rc::new(super::macos_updates::MacosUpdates::new())
}

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use super::*;

    #[test]
    fn macos_shell_capture_preserves_mode_compatibility_and_inherited_values() {
        use std::ffi::OsStr;
        let resources = crate::terminal::testing::ShellResourcesFixture::new();
        for (
            shell,
            mode,
            inherited_key,
            inherited_value,
            expected_key,
            expected_value,
            integrated,
        ) in [
            (
                "/bin/bash",
                None,
                "ENV",
                Some("/captured/env"),
                "SPACETERM_BASH_ENV",
                None,
                false,
            ),
            (
                "/custom/bash",
                Some(" OFF "),
                "ENV",
                Some("/captured/env"),
                "SPACETERM_BASH_ENV",
                None,
                false,
            ),
            (
                "/custom/bash",
                None,
                "ENV",
                Some("/captured/env"),
                "SPACETERM_BASH_ENV",
                Some("/captured/env"),
                true,
            ),
            (
                "/bin/zsh",
                None,
                "ZDOTDIR",
                Some("/captured/zsh"),
                "SPACETERM_ZSH_ZDOTDIR",
                Some("/captured/zsh"),
                true,
            ),
            (
                "/custom/fish",
                None,
                "XDG_DATA_DIRS",
                Some("/captured/share::"),
                "XDG_DATA_DIRS",
                Some("/captured/share::"),
                true,
            ),
            (
                "/custom/fish",
                None,
                "XDG_DATA_DIRS",
                None,
                "XDG_DATA_DIRS",
                Some("/usr/local/share:/usr/share"),
                true,
            ),
        ] {
            let mut reads = Vec::new();
            let planner = super::super::launch_host::shell_launch_planner_with(
                shell.into(),
                resources.path().to_path_buf(),
                |key| {
                    reads.push(key.to_owned());
                    if key == "SPACETERM_SHELL_INTEGRATION" {
                        mode.map(Into::into)
                    } else if key == inherited_key {
                        inherited_value.map(Into::into)
                    } else {
                        None
                    }
                },
            );
            assert_eq!(
                reads,
                [
                    "SPACETERM_SHELL_INTEGRATION",
                    "XDG_DATA_DIRS",
                    "ZDOTDIR",
                    "ENV"
                ]
            );
            // Planning repeatedly uses the captured values after the reader has been dropped.
            for _ in 0..2 {
                let launch = planner.local(resources.path()).unwrap();
                let lookup = |key: &str| {
                    launch
                        .environment()
                        .iter()
                        .find(|(name, _)| name == key)
                        .map(|(_, value)| value.as_os_str())
                };
                assert_eq!(
                    lookup("SPACETERM_SHELL_INTEGRATION_VERSION").is_some(),
                    integrated,
                    "{shell}"
                );
                let expected = expected_value.map(|value| {
                    if expected_key == "XDG_DATA_DIRS" {
                        let mut combined =
                            resources.path().join("shell-integration").into_os_string();
                        combined.push(":");
                        combined.push(value);
                        combined
                    } else {
                        value.into()
                    }
                });
                assert_eq!(lookup(expected_key), expected.as_deref(), "{shell}");
                if !integrated {
                    assert_eq!(launch.arguments(), [OsStr::new("-l")]);
                }
            }
        }
    }

    #[gpui::test]
    fn desktop_profile_should_preserve_macos_shortcuts_and_wording(cx: &mut gpui::TestAppContext) {
        use crate::ui::{
            ClosePane, CloseTab, CreateTab, NewWorkspace, SplitDown, SplitRight, SwitchWorkspace,
            TogglePaneZoom,
        };

        cx.update(|cx| {
            crate::ui::initialize_controls(cx).unwrap();
            desktop_profile(Rc::new(crate::platform::locale::FixedLocaleDirection(
                spaceterm_ui::TextDirection::LeftToRight,
            )))
            .unwrap()
            .install(cx);
            let fonts = crate::host_fonts::HostFonts::get(cx);
            assert_eq!(fonts.ui_family, ".SystemUIFont");
            assert_eq!(fonts.system_monospace_family, "Menlo");
            assert_eq!(
                fonts.terminal_families,
                [
                    crate::bundled_font::FAMILY,
                    "JetBrainsMono Nerd Font",
                    "JetBrainsMono Nerd Font Mono",
                    "JetBrains Mono",
                ]
            );
            assert_eq!(fonts.emoji_family, "Apple Color Emoji");
            assert!(fonts.bundled_ui_faces.is_empty());
            gpui::BorrowAppContext::update_global::<DesktopPresentation, _>(
                cx,
                |presentation, cx| presentation.refresh(cx),
            );
            let presentation = DesktopPresentation::get(cx);
            assert_eq!(
                presentation.shortcut(&SwitchWorkspace).as_deref(),
                Some("⇧⌘K")
            );
            assert_eq!(
                presentation.shortcut(&crate::ui::ToggleSidebar).as_deref(),
                Some("⌘B")
            );
            assert_eq!(presentation.shortcut(&NewWorkspace).as_deref(), Some("⌘N"));
            assert_eq!(
                presentation
                    .shortcut(&crate::ui::NewRemoteWorkspace)
                    .as_deref(),
                Some("⇧⌘N")
            );
            assert_eq!(presentation.shortcut(&CreateTab).as_deref(), Some("⌘T"));
            assert_eq!(
                presentation.shortcut(&spaceterm_ui::EditCopy).as_deref(),
                Some("⌘C")
            );
            assert_eq!(presentation.shortcut(&SplitRight).as_deref(), Some("⌘D"));
            assert_eq!(presentation.shortcut(&SplitDown).as_deref(), Some("⇧⌘D"));
            assert_eq!(
                presentation.shortcut(&TogglePaneZoom).as_deref(),
                Some("⇧⌘↩")
            );
            assert_eq!(presentation.shortcut(&ClosePane).as_deref(), Some("⌘W"));
            assert_eq!(presentation.shortcut(&CloseTab).as_deref(), Some("⇧⌘W"));
            #[cfg(feature = "appearance-exerciser")]
            assert_eq!(
                presentation
                    .shortcut(&crate::ui::appearance_exerciser::ToggleAppearancePreview)
                    .as_deref(),
                Some("⌥⌘C")
            );
            assert_eq!(presentation.wording().file_preview, "Quick Look");
            assert_eq!(presentation.wording().operating_system_name, "macOS");
            assert_eq!(
                presentation.wording().system_directory_selection,
                "Choose in Finder…"
            );
        });
    }
}
