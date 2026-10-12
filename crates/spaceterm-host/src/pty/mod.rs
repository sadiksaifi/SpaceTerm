//! Native PTY lifecycle contracts and platform-selected POSIX adapters.

#[cfg(target_os = "linux")]
pub mod linux_pty_host;
#[cfg(target_os = "macos")]
pub mod macos_pty_host;
pub mod native_pty;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod spawn;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod unix_pty;

pub use native_pty::*;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use unix_pty::UnixNativePtyAdapterFactory;

#[cfg(test)]
pub(crate) mod shell_resources_fixture;

#[cfg(all(
    test,
    feature = "native-tests",
    any(target_os = "macos", target_os = "linux")
))]
mod launch_tests;
