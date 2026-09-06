//! The sole production selector of desktop policy and native capabilities.
use crate::app::{HostComposition, HostCompositionParts, StartupDependencies};
use crate::desktop_profile::{DesktopProfile, DesktopProfileError};
use crate::terminal::{NativeTerminalSessionFactory, OptionAsAltPolicy};
use gpui::{TitlebarOptions, point, px};
use std::{rc::Rc, sync::Arc};

pub(crate) fn main() {
    let code = crate::app::dispatch_or_prepare_application(
        super::macos_askpass_transport::dispatch_helper_from_environment,
        || crate::app::launch(compose),
    )
    .unwrap_or_else(|code| code);
    if code != 0 {
        std::process::exit(code);
    }
}

fn desktop_profile(
    locale: Rc<dyn super::locale::LocaleDirection>,
) -> Result<DesktopProfile, DesktopProfileError> {
    DesktopProfile::new(
        spaceterm_ui::ModalDesktopPolicy::mac_os(),
        spaceterm_ui::ModalKeybindingProfile::MacOs,
        spaceterm_ui::TextInputKeybindingProfile::MacOs,
        super::macos_keybindings::bindings(),
        locale,
    )
}
#[cfg(test)]
pub(crate) fn testing_desktop_profile(direction: spaceterm_ui::TextDirection) -> DesktopProfile {
    desktop_profile(Rc::new(super::locale::FixedLocaleDirection(direction)))
        .expect("valid desktop profile")
}

fn compose(startup: StartupDependencies) -> Result<HostComposition, DesktopProfileError> {
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
    let session_factory = Rc::new(NativeTerminalSessionFactory::new(
        Arc::new(super::macos_pty::MacosNativePtyAdapterFactory),
        super::shell_launch::ShellLaunchPlanner::new(
            super::launch_host::user_shell().into(),
            super::launch_host::resource_root(),
        ),
        Arc::new(super::macos_pasteboard::MacosOsc52ClipboardFactory),
    ));
    let remote_workspace = startup.remote_backend_factory(Arc::new(
        super::macos_askpass_transport::AskPassWindowFactory,
    ));
    HostComposition::new(HostCompositionParts {
        profile: desktop_profile(Rc::new(super::macos_locale::ApplicationLocale))?,
        home_directory: startup.home_directory,
        session_factory,
        adapters: crate::app::ApplicationCapabilities {
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
