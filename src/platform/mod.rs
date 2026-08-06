#[cfg(target_os = "macos")]
pub(crate) mod macos_keyboard;

#[cfg(target_os = "macos")]
pub(crate) mod macos_pasteboard;

#[cfg(target_os = "macos")]
pub(crate) mod macos_pty;

#[cfg(target_os = "macos")]
pub(crate) mod macos_secure_input;

#[cfg(not(target_os = "macos"))]
compile_error!("SpaceTerm currently supports macOS only");
