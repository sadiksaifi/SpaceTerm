//! The sole production selector of desktop policy and native capabilities.
use crate::app::{
    HostComposition, HostCompositionParts, StartupDependencies, StartupDependenciesError,
};
use crate::desktop_profile::{
    ActionShortcut, ControlKeybindingProfiles, DesktopPresentation, DesktopProfile,
    DesktopProfileError, DesktopWording,
};
use crate::terminal::{NativeTerminalSessionFactory, OptionAsAltPolicy};
use gpui::{TitlebarOptions, point, px};
use std::{path::PathBuf, rc::Rc, sync::Arc};

pub(crate) fn main() {
    let code = match super::macos_askpass_transport::dispatch_helper_from_environment() {
        Some(code) => code,
        None => crate::app::launch(capture_startup_dependencies(), compose),
    };
    if code != 0 {
        std::process::exit(code);
    }
}

fn capture_startup_dependencies() -> Result<
    StartupDependencies<super::macos_ssh_process::MacOsSshProcessAdapter>,
    StartupDependenciesError,
> {
    let path_environment = super::app_directories::AppDirectoryEnvironment::capture();
    #[cfg(feature = "appearance-exerciser")]
    let mut directories =
        super::app_directories::AppDirectories::resolve(super::app_directories::APP_DIR_NAME)
            .map_err(|_| StartupDependenciesError::Paths)?;
    #[cfg(not(feature = "appearance-exerciser"))]
    let directories =
        super::app_directories::AppDirectories::resolve(super::app_directories::APP_DIR_NAME)
            .map_err(|_| StartupDependenciesError::Paths)?;
    #[cfg(feature = "appearance-exerciser")]
    isolate_appearance_exerciser_config(&mut directories)?;
    let secure_filesystem: Arc<dyn super::secure_filesystem::SecureFilesystem> =
        Arc::new(super::macos_secure_filesystem::MacosSecureFilesystem);
    let paths = super::app_paths::AppPaths::from_directories(
        directories,
        103,
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
        super::macos_ssh_process::MacOsSshProcessAdapter,
        Arc::new(super::macos_control_socket::MacosControlSocketProbe),
        Arc::new(super::macos_host_config_filesystem::MacosHostConfigFilesystem),
    )
}

#[cfg(feature = "appearance-exerciser")]
fn isolate_appearance_exerciser_config(
    directories: &mut super::app_directories::AppDirectories,
) -> Result<(), StartupDependenciesError> {
    use std::ffi::OsStr;
    use std::path::Component;

    if std::env::var_os("SPACETERM_APPEARANCE_EXERCISER").as_deref() != Some(OsStr::new("1")) {
        return Ok(());
    }
    let requested = std::env::var_os("SPACETERM_APPEARANCE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("spaceterm-appearance-exerciser"));
    let normal = requested.is_absolute()
        && requested
            .components()
            .all(|component| !matches!(component, Component::ParentDir | Component::CurDir))
        && requested.file_name() == Some(OsStr::new("spaceterm-appearance-exerciser"));
    if !normal {
        return Err(StartupDependenciesError::Paths);
    }
    let parent = requested.parent().ok_or(StartupDependenciesError::Paths)?;
    let root = std::fs::canonicalize(parent)
        .map_err(|_| StartupDependenciesError::Paths)?
        .join("spaceterm-appearance-exerciser");
    directories.config = root.join(super::app_directories::APP_DIR_NAME);
    Ok(())
}

#[cfg(test)]
fn runtime_path_host_facts(
    environment: &super::app_directories::AppDirectoryEnvironment,
    fallback: impl FnOnce() -> std::io::Result<PathBuf>,
) -> Result<super::app_paths::AppPathHostFacts, StartupDependenciesError> {
    if environment.configured_runtime_root().is_some() {
        return super::app_paths::AppPathHostFacts::without_runtime_fallback(103)
            .map_err(|_| StartupDependenciesError::Paths);
    }
    let root = fallback().map_err(|_| StartupDependenciesError::Paths)?;
    super::app_paths::AppPathHostFacts::new(root, 103).map_err(|_| StartupDependenciesError::Paths)
}

fn desktop_profile(
    locale: Rc<dyn super::locale::LocaleDirection>,
) -> Result<DesktopProfile, DesktopProfileError> {
    use crate::ui::OpenTerminalFind;
    use crate::ui::{
        ClosePane, CloseTab, CreateTab, NewWorkspace, SplitDown, SplitRight, SwitchWorkspace,
        TogglePaneZoom,
    };
    use spaceterm_ui::{EditCopy, EditPaste};

    DesktopProfile::new(
        spaceterm_ui::ModalDesktopPolicy::mac_os(),
        ControlKeybindingProfiles::new(
            spaceterm_ui::ModalKeybindingProfile::MacOs,
            spaceterm_ui::MenuKeybindingProfile::MacOs,
            spaceterm_ui::CommandPaletteKeybindingProfile::MacOs,
            spaceterm_ui::ComboBoxKeybindingProfile::MacOs,
            spaceterm_ui::TextInputKeybindingProfile::MacOs,
        ),
        crate::desktop_profile::keybindings::bindings(),
        DesktopPresentation::new(
            DesktopWording {
                directory_selection: "Choose with Finder",
                file_preview: "Quick Look",
            },
            "⌘↩",
            vec![
                ActionShortcut::new(crate::ui::ActivateWorkspace1, "⌃1"),
                ActionShortcut::new(crate::ui::ActivateWorkspace2, "⌃2"),
                ActionShortcut::new(crate::ui::ActivateWorkspace3, "⌃3"),
                ActionShortcut::new(crate::ui::ActivateWorkspace4, "⌃4"),
                ActionShortcut::new(crate::ui::ActivateWorkspace5, "⌃5"),
                ActionShortcut::new(crate::ui::ActivateWorkspace6, "⌃6"),
                ActionShortcut::new(crate::ui::ActivateWorkspace7, "⌃7"),
                ActionShortcut::new(crate::ui::ActivateWorkspace8, "⌃8"),
                ActionShortcut::new(crate::ui::ActivateWorkspace9, "⌃9"),
                ActionShortcut::new(SwitchWorkspace, "⌘K"),
                ActionShortcut::new(crate::ui::ToggleSidebar, "⌘B"),
                ActionShortcut::new(NewWorkspace, "⌘N"),
                ActionShortcut::new(crate::ui::NewRemoteWorkspace, "⇧⌘N"),
                ActionShortcut::new(CreateTab, "⌘T"),
                ActionShortcut::new(EditCopy, "⌘C"),
                ActionShortcut::new(EditPaste, "⌘V"),
                ActionShortcut::new(OpenTerminalFind, "⌘F"),
                ActionShortcut::new(SplitRight, "⌘D"),
                ActionShortcut::new(SplitDown, "⇧⌘D"),
                ActionShortcut::new(TogglePaneZoom, "⇧⌘↩"),
                ActionShortcut::new(ClosePane, "⌘W"),
                ActionShortcut::new(CloseTab, "⇧⌘W"),
                #[cfg(feature = "appearance-exerciser")]
                ActionShortcut::new(
                    crate::ui::appearance_exerciser::ToggleAppearancePreview,
                    "⌥⌘C",
                ),
            ],
        ),
        locale,
    )
}

fn compose(
    startup: StartupDependencies<super::macos_ssh_process::MacOsSshProcessAdapter>,
) -> Result<HostComposition, DesktopProfileError> {
    let settings_storage = startup.settings_storage();
    let activity: Rc<dyn crate::platform::application_activity::ApplicationActivity> =
        Rc::new(crate::platform::macos_application::MacosApplicationActivity);
    let lifecycle = crate::ui::pane_lifecycle::PaneLifecycleDependencies {
        attention: crate::terminal::attention_runtime::AttentionRuntime::new(
            Box::new(crate::platform::macos_attention::AppKitAudioBell),
            Box::new(crate::platform::macos_attention::AppKitDockAttention::default()),
            Box::new(
                crate::terminal::attention_notification::AttentionNotifications::new(Arc::new(
                    crate::platform::macos_notification::UserNotificationAdapter,
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
        Arc::new(super::macos_local_identity::MacosLocalIdentity),
    );
    let session_factory = Rc::new(NativeTerminalSessionFactory::new(
        Arc::new(super::macos_pty::MacosNativePtyAdapterFactory),
        super::launch_host::shell_launch_planner(),
        local_filesystem.clone(),
        crate::terminal::metadata::LocalMachine::new(
            super::launch_host::local_user().as_deref(),
            super::launch_host::local_hostname().as_deref(),
            startup.home_directory.to_str(),
        ),
    ));
    let remote_workspace = startup.remote_backend_factory(Arc::new(
        super::macos_askpass_transport::AskPassWindowFactory,
    ));
    HostComposition::new(HostCompositionParts {
        profile: desktop_profile(Rc::new(super::macos_locale::ApplicationLocale))?,
        home_directory: startup.home_directory,
        session_factory,
        adapters: crate::app::ApplicationCapabilities {
            application_menu: Rc::new(
                super::macos_application_menu::MacosApplicationMenuAdapter,
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
            permission_recovery: Some(Rc::new(
                super::permission_recovery::PermissionRecovery::new(
                    Box::new(super::macos_system_settings::NsWorkspaceUrlLauncher::default()),
                    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_FilesAndFolders",
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders",
                    "Open System Settings",
                ),
            )),
            remote_workspace,
        },
        services: Rc::new(super::macos_services::NativeServicesRegistration),
        window_movement: Rc::new(super::macos_window_drag::WindowMovementFactory),
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.0), px(11.0))),
        }),
    })
    .map(|host| {
        host.with_appearance(
            settings_storage,
            Rc::new(super::macos_appearance::MacosAppearancePlatform),
        )
    })
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;
    use crate::platform::app_directories::AppDirectoryEnvironment;
    use crate::platform::app_paths::AppPaths;

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

    #[test]
    fn runtime_facts_should_not_consult_an_unused_temporary_fallback() {
        let environment = AppDirectoryEnvironment {
            home: Some("/Users/test".into()),
            xdg_runtime_dir: Some("/private/runtime".into()),
            ..AppDirectoryEnvironment::default()
        };
        let consulted = std::cell::Cell::new(false);
        let facts = runtime_path_host_facts(&environment, || {
            consulted.set(true);
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        })
        .unwrap();
        let paths = AppPaths::resolve(
            &environment,
            &facts,
            Arc::new(super::super::macos_secure_filesystem::MacosSecureFilesystem),
        )
        .unwrap();

        assert!(!consulted.get());
        assert_eq!(
            paths.runtime(),
            std::path::Path::new("/private/runtime/spaceterm")
        );
    }

    #[test]
    fn runtime_facts_should_report_an_unavailable_required_temporary_fallback() {
        let environment = AppDirectoryEnvironment {
            xdg_runtime_dir: Some("relative/runtime".into()),
            ..AppDirectoryEnvironment::default()
        };
        let result = runtime_path_host_facts(&environment, || {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });

        assert!(matches!(result, Err(StartupDependenciesError::Paths)));
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
            let presentation = DesktopPresentation::get(cx);
            assert_eq!(presentation.shortcut(&SwitchWorkspace), "⌘K");
            assert_eq!(presentation.shortcut(&crate::ui::ToggleSidebar), "⌘B");
            assert_eq!(presentation.shortcut(&NewWorkspace), "⌘N");
            assert_eq!(presentation.shortcut(&crate::ui::NewRemoteWorkspace), "⇧⌘N");
            assert_eq!(presentation.shortcut(&CreateTab), "⌘T");
            assert_eq!(presentation.shortcut(&spaceterm_ui::EditCopy), "⌘C");
            assert_eq!(presentation.shortcut(&SplitRight), "⌘D");
            assert_eq!(presentation.shortcut(&SplitDown), "⇧⌘D");
            assert_eq!(presentation.shortcut(&TogglePaneZoom), "⇧⌘↩");
            assert_eq!(presentation.shortcut(&ClosePane), "⌘W");
            assert_eq!(presentation.shortcut(&CloseTab), "⇧⌘W");
            #[cfg(feature = "appearance-exerciser")]
            assert_eq!(
                presentation.shortcut(&crate::ui::appearance_exerciser::ToggleAppearancePreview),
                "⌥⌘C"
            );
            assert_eq!(presentation.command_palette_confirm_shortcut(), "⌘↩");
            assert_eq!(
                presentation.wording().directory_selection,
                "Choose with Finder"
            );
            assert_eq!(presentation.wording().file_preview, "Quick Look");
        });
    }
}
