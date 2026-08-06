#[cfg(target_os = "macos")]
pub(crate) mod macos_keyboard;

#[cfg(target_os = "macos")]
pub(crate) mod macos_pasteboard;

#[cfg(target_os = "macos")]
pub(crate) mod macos_pty;

#[cfg(target_os = "macos")]
pub(crate) mod macos_secure_input;

#[cfg(target_os = "macos")]
pub(crate) mod macos_scroll;

#[cfg(target_os = "macos")]
pub(crate) mod shell_integration;

#[cfg(not(target_os = "macos"))]
compile_error!("SpaceTerm currently supports macOS only");
