//! The owning-host headless CLI. It never runs a server without native admission.
pub mod arguments;
pub mod bindings;
pub mod output;
pub mod runner;
pub mod tty;

pub use arguments::{CliCommand, CliInvocation, ConfirmationMode, parse};
pub use spaceterm_client::management_cli::ManagementFailure as CliFailure;

pub use runner::{CliDependencies, CliExit, CliIo, Dispatch, dispatch};
