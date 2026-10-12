//! Fixed native management facts and lifecycle orchestration.
//!
//! A request is comparison data, never OS authorization. Installation owns the
//! protected journal and authority ports. No native production port is selected here.

#![deny(missing_docs)]

#[path = "native_management/controller.rs"]
pub mod controller;

#[path = "native_management/helper_lifetime.rs"]
pub mod helper_lifetime;

#[path = "native_management/request.rs"]
pub mod request;

#[path = "native_management/stdin.rs"]
pub mod stdin;

/// CLI authentication policy. SpaceTerm never receives the sudo password.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SudoMode {
    /// Use sudo's controlling terminal.
    Interactive,
    /// Refuse any authentication prompt.
    NonInteractive,
}

#[path = "cli_management/sudo.rs"]
pub mod cli_sudo;
#[path = "linux_management/mod.rs"]
pub mod linux;
#[path = "macos_management/mod.rs"]
pub mod macos;

/// Fixed client-facing management ports supplied by native-management C1.
#[allow(
    dead_code,
    reason = "legacy source-included tests do not consume the additive facade"
)]
#[path = "native_management/facade.rs"]
pub mod facade;
#[allow(
    unused_imports,
    reason = "public facade reexports also compile in legacy source-included tests"
)]
pub use facade::{GuiNativeManagement, NativeAction, NativeManagement};
