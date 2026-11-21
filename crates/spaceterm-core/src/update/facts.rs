//! The Core's host platform and update facts contract.
pub use spaceterm_protocol::update::facts::*;

/// The platform this SpaceTerm Core build runs on.
#[cfg(target_os = "macos")]
pub const HOST_PLATFORM: Platform = Platform::MacosArm64;
/// The platform this SpaceTerm Core build runs on.
#[cfg(not(target_os = "macos"))]
pub const HOST_PLATFORM: Platform = Platform::LinuxX86_64;
