//! The sole production selector of Linux desktop policy and native capabilities.
use crate::app::{
    HostComposition, HostCompositionParts, StartupDependencies, StartupDependenciesError,
};
use crate::application_identity::ApplicationIdentity;
use crate::desktop_profile::{
    ControlKeybindingProfiles, DesktopPresentation, DesktopProfile, DesktopProfileError,
    DesktopWording,
};
use crate::terminal::NativeTerminalSessionFactory;
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

fn desktop_profile(
    locale: Rc<dyn super::locale::LocaleDirection>,
) -> Result<DesktopProfile, DesktopProfileError> {
    let layout = Rc::new(super::linux_keyboard_layout::LinuxKeyboardLayout);
    let snapshot = super::keyboard_layout::KeyboardLayoutAdapter::snapshot(layout.as_ref())
        .map_err(|_| DesktopProfileError::InvalidCombination)?;
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
            Rc::new(super::linux_shortcut_text::LinuxShortcutFormatter::new(
                snapshot,
            )),
            crate::desktop_profile::ShortcutSelection::TerminalSurface,
        ),
        locale,
    ))
}

fn compose(
    startup: StartupDependencies<super::unix_ssh_process::UnixSshProcessAdapter>,
    identity: ApplicationIdentity,
) -> Result<HostComposition, DesktopProfileError> {
    let settings_storage = startup.settings_storage();
    let settings_file = startup.settings_file();
    let activity: Rc<dyn crate::platform::application_activity::ApplicationActivity> =
        Rc::new(super::linux_application::LinuxApplicationActivity);
    let lifecycle = crate::ui::pane_lifecycle::PaneLifecycleDependencies {
        attention: crate::terminal::attention_runtime::AttentionRuntime::new(
            Box::new(super::linux_attention::LinuxAudioBell),
            Box::new(super::linux_attention::LinuxWindowAttention),
            Box::new(
                crate::terminal::attention_notification::AttentionNotifications::new(Arc::new(
                    super::linux_notification::LinuxNotificationAdapter,
                )),
            ),
            Rc::clone(&activity),
        ),
        secure_input: crate::terminal::secure_input::SecureInputHandle::new(Box::new(
            super::linux_secure_input::LinuxSecureInputAdapter,
        )),
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
        super::unix_askpass_transport::AskPassWindowFactory,
    ));
    HostComposition::new(HostCompositionParts {
        profile: desktop_profile(Rc::new(super::linux_locale::LinuxLocale::capture(|key| {
            std::env::var(key).ok()
        })))?,
        home_directory: startup.home_directory,
        session_factory,
        adapters: crate::app::ApplicationCapabilities {
            updates: Rc::new(crate::updates::UnavailableUpdates),
            selected_files: None,
            settings_file: Some(settings_file),
            application_menu: Rc::new(
                super::linux_application_menu::LinuxApplicationMenuAdapter::new(identity),
            ),
            application_quit: Rc::new(
                super::linux_application_quit::LinuxApplicationQuitAdapter::default(),
            ),
            local_filesystem,
            key_input: Rc::new(super::linux_keyboard::LinuxTerminalKeyInputAdapterFactory::new()),
            accessibility: Rc::new(
                super::linux_accessibility::LinuxTerminalAccessibilityAdapterFactory,
            ),
            native_services: crate::terminal::native_services::NativeServiceAdapters {
                text_clipboard: Rc::new(super::linux_clipboard::LinuxTextClipboard),
                selection_clipboard: Rc::new(super::linux_clipboard::LinuxSelectionClipboard),
                file_insertion: crate::terminal::native_services::file_insertion::FileInsertionPolicy {
                    paths,
                    shell: crate::terminal::native_services::file_insertion::ShellInsertionDialect::Posix,
                },
                file_clipboard: Rc::new(super::linux_clipboard::LinuxFileClipboard),
                file_preview: Rc::new(super::linux_file_preview::LinuxFilePreviewFactory),
            },
            lifecycle,
            microphone_access: None,
            theme_registry: Some(Arc::new(super::https_transport::HttpsTransport::new())),
            remote_workspace,
        },
        services: Rc::new(super::linux_services::LinuxServicesRegistration),
        window_movement: Rc::new(super::linux_window_drag::LinuxWindowMovementFactory),
        window_frame: super::window_frame::WindowFrameGeometry::new(None),
        titlebar: None,
    })
    .map(|host| {
        host.with_appearance(
            settings_storage,
            Rc::new(super::linux_appearance::LinuxAppearancePlatform),
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
            desktop_profile(Rc::new(crate::platform::locale::FixedLocaleDirection(
                spaceterm_ui::TextDirection::LeftToRight,
            )))
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
                (
                    presentation.shortcut(&crate::ui::ActivateTab1),
                    "Ctrl+Shift+1",
                ),
                (
                    presentation.shortcut(&spaceterm_ui::EditCopy),
                    "Ctrl+Shift+C",
                ),
                (
                    presentation.shortcut(&crate::ui::settings_window::OpenSettings),
                    "Ctrl+Shift+,",
                ),
            ] {
                assert_eq!(shortcut.as_deref(), Some(label));
            }
            assert_eq!(presentation.wording().file_preview, "Preview");
            assert_eq!(presentation.wording().operating_system_name, "Linux");
            assert_eq!(
                presentation.wording().system_directory_selection,
                "Choose Directory…"
            );
        });
    }
}
