//! PTY and VT contracts used by headless hosts.

pub use crate::platform::local_filesystem::LocalFilesystemAuthority;
pub use crate::platform::native_pty::NativePtyAdapterFactory;
pub use crate::platform::shell_integration::{
    ShellEnvironment, ShellIntegrationMode, ShellIntegrationPolicy,
};
pub use crate::platform::shell_launch::ShellLaunchPlanner;
pub use crate::terminal::emulator::ScreenSnapshot;
pub use crate::terminal::geometry::{
    BackingScale, CellGridSize, LogicalCellSize, LogicalSize, TerminalGeometry,
};
pub use crate::terminal::key::{KeyAction, KeyInput, PhysicalKey};
pub use crate::terminal::metadata::LocalMachine;
pub use crate::terminal::session::{
    LocalTerminalLaunchPlan, NativeTerminalSessionFactory, TerminalAppearanceUpdate,
    TerminalRuntime, TerminalSessionEvent,
};

/// Returns the native PTY adapter used by the desktop and headless hosts.
pub fn native_pty_factory() -> std::sync::Arc<dyn NativePtyAdapterFactory> {
    crate::platform::runtime_pty_factory()
}

/// Returns authority for explicit local file and directory operations on this host.
pub fn local_filesystem() -> LocalFilesystemAuthority {
    crate::platform::runtime_local_filesystem()
}

mod server;
#[cfg(all(test, feature = "desktop"))]
pub(crate) use server::metadata::encode as encode_server_metadata;
#[cfg(all(test, feature = "desktop"))]
pub(crate) use server::screen_encoder;
#[cfg(all(test, feature = "desktop"))]
pub(crate) use server::testing as directory_testing;
pub use server::{Served, ServerSessionContext, WorkspaceHost};

#[cfg(all(test, feature = "desktop"))]
pub(crate) use server::HostCommands;

#[cfg(all(test, feature = "desktop"))]
pub(crate) use server::Observed;

#[cfg(all(test, feature = "desktop"))]
pub(crate) use crate::terminal::native_services::hyperlink::HyperlinkKind;
#[cfg(feature = "desktop")]
pub(crate) use crate::terminal::native_services::hyperlink::HyperlinkTarget;
