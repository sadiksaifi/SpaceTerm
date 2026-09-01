pub(crate) mod acceptance_observation;
pub(crate) mod app_paths;

pub(crate) mod finder_fallback;
pub(crate) mod workspace_directory;
pub(crate) mod workspace_picker_filesystem;

#[cfg(target_os = "macos")]
pub(crate) mod macos_attention;

#[cfg(target_os = "macos")]
pub(crate) mod macos_application;

#[cfg(target_os = "macos")]
pub(crate) mod macos_accessibility;

#[cfg(target_os = "macos")]
pub(crate) mod ssh_askpass;

#[cfg(target_os = "macos")]
pub(crate) mod macos_askpass_transport;

#[cfg(target_os = "macos")]
pub(crate) mod macos_keyboard;

#[cfg(target_os = "macos")]
pub(crate) mod macos_locale;

#[cfg(target_os = "macos")]
pub(crate) mod macos_notification;

#[cfg(target_os = "macos")]
pub(crate) mod macos_pasteboard;

#[cfg(target_os = "macos")]
pub(crate) mod macos_quick_look;

#[cfg(target_os = "macos")]
pub(crate) mod macos_render_lifecycle;

#[cfg(target_os = "macos")]
pub(crate) mod macos_pty;

#[cfg(target_os = "macos")]
pub(crate) mod macos_secure_input;

#[cfg(target_os = "macos")]
pub(crate) mod macos_scroll;

#[cfg(target_os = "macos")]
pub(crate) mod macos_services;

#[cfg(target_os = "macos")]
pub(crate) mod macos_system_settings;

#[cfg(target_os = "macos")]
pub(crate) mod macos_window_drag;

#[cfg(target_os = "macos")]
pub(crate) mod shell_integration;

#[cfg(not(target_os = "macos"))]
compile_error!("SpaceTerm currently supports macOS only");
