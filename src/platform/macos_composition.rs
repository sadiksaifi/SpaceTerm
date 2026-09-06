//! The sole production selector of desktop policy and native capabilities.
use crate::app::{
    HostComposition, HostCompositionParts, StartupDependencies, StartupDependenciesError,
};
use crate::desktop_profile::{DesktopProfile, DesktopProfileError};
use crate::terminal::{NativeTerminalSessionFactory, OptionAsAltPolicy};
use gpui::{TitlebarOptions, point, px};
use std::{path::PathBuf, rc::Rc, sync::Arc};

pub(crate) fn main() {
    let code = crate::app::dispatch_or_prepare_application(
        super::macos_askpass_transport::dispatch_helper_from_environment,
        || {
            let observation = match super::macos_observation::discover() {
                Ok(observation) => observation,
                Err(_) => {
                    eprintln!("acceptance observation configuration failed");
                    return 2;
                }
            };
            crate::app::launch(capture_startup_dependencies(), move |startup| {
                compose(startup, observation)
            })
        },
    )
    .unwrap_or_else(|code| code);
    if code != 0 {
        std::process::exit(code);
    }
}

fn capture_startup_dependencies() -> Result<
    StartupDependencies<super::macos_ssh_process::MacOsSshProcessAdapter>,
    StartupDependenciesError,
> {
    let path_environment = super::app_paths::AppPathEnvironment::capture();
    let path_host_facts = runtime_path_host_facts(&path_environment, || {
        std::fs::canonicalize(std::env::temp_dir())
    })?;
    let executable = crate::ssh::command::OpenSshExecutable::new(PathBuf::from("/usr/bin/ssh"))
        .map_err(|_| StartupDependenciesError::Paths)?;
    StartupDependencies::capture(
        path_environment,
        &path_host_facts,
        Arc::new(super::macos_secure_filesystem::MacosSecureFilesystem),
        executable,
        super::macos_ssh_process::MacOsSshProcessAdapter,
        Arc::new(super::macos_control_socket::MacosControlSocketProbe),
        Arc::new(super::macos_host_config_filesystem::MacosHostConfigFilesystem),
    )
}

fn runtime_path_host_facts(
    environment: &super::app_paths::AppPathEnvironment,
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
    DesktopProfile::new(
        spaceterm_ui::ModalDesktopPolicy::mac_os(),
        spaceterm_ui::ModalKeybindingProfile::MacOs,
        spaceterm_ui::TextInputKeybindingProfile::MacOs,
        crate::desktop_profile::keybindings::bindings(),
        locale,
    )
}

fn compose(
    startup: StartupDependencies<super::macos_ssh_process::MacOsSshProcessAdapter>,
    observation: Option<crate::observation::AuthenticatedObservation>,
) -> Result<HostComposition, DesktopProfileError> {
    let activity: Rc<dyn crate::platform::application_activity::ApplicationActivity> =
        Rc::new(crate::platform::macos_application::MacosApplicationActivity);
    let lifecycle = crate::ui::pane_lifecycle::PaneLifecycleDependencies {
        observation,
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
    let local_filesystem = super::local_filesystem::LocalFilesystemAuthority::new(Arc::new(
        super::macos_local_identity::MacosLocalIdentity,
    ));
    let session_factory = Rc::new(NativeTerminalSessionFactory::new(
        Arc::new(super::macos_pty::MacosNativePtyAdapterFactory),
        super::shell_launch::ShellLaunchPlanner::new(
            super::launch_host::user_shell().into(),
            super::launch_host::resource_root(),
        ),
        Arc::new(super::macos_pasteboard::MacosOsc52ClipboardFactory),
        local_filesystem.clone(),
        super::launch_host::local_hostname(),
    ));
    let remote_workspace = startup.remote_backend_factory(Arc::new(
        super::macos_askpass_transport::AskPassWindowFactory,
    ));
    HostComposition::new(HostCompositionParts {
        profile: desktop_profile(Rc::new(super::macos_locale::ApplicationLocale))?,
        home_directory: startup.home_directory,
        session_factory,
        adapters: crate::app::ApplicationCapabilities {
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
                file_clipboard: Rc::new(super::macos_pasteboard::MacosFileClipboard),
                quick_look: Rc::new(super::macos_quick_look::MacosQuickLookFactory),
            },
            lifecycle,
            finder: Rc::new(super::finder_fallback::NativeFinderFallback),
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
        services: Some(Rc::new(super::macos_services::NativeServicesRegistration)),
        window_movement: Some(Rc::new(super::macos_window_drag::WindowMovementFactory)),
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.0), px(11.0))),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::app_paths::{AppPathEnvironment, AppPaths};

    #[test]
    fn runtime_facts_should_not_consult_an_unused_temporary_fallback() {
        let environment = AppPathEnvironment {
            home: Some("/Users/test".into()),
            xdg_runtime_dir: Some("/private/runtime".into()),
            ..AppPathEnvironment::default()
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
        let environment = AppPathEnvironment {
            xdg_runtime_dir: Some("relative/runtime".into()),
            ..AppPathEnvironment::default()
        };
        let result = runtime_path_host_facts(&environment, || {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });

        assert!(matches!(result, Err(StartupDependenciesError::Paths)));
    }
}
