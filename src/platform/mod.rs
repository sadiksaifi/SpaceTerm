pub(crate) mod acceptance_observation;

#[cfg(target_os = "macos")]
pub(crate) mod fs_identity;

#[cfg(target_os = "macos")]
#[allow(unused_imports)]
pub(crate) use fs_identity::{canonical_identity, is_valid_local_directory};

#[cfg(target_os = "macos")]
pub(crate) mod macos_attention;

#[cfg(target_os = "macos")]
pub(crate) mod macos_application;

#[cfg(target_os = "macos")]
pub(crate) mod macos_accessibility;

#[cfg(target_os = "macos")]
pub(crate) mod macos_directory_picker;

#[cfg(target_os = "macos")]
pub(crate) mod macos_keyboard;

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
pub(crate) mod shell_integration;

#[cfg(not(target_os = "macos"))]
compile_error!("SpaceTerm currently supports macOS only");
