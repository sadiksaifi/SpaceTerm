//! Fixed one-request native manager. Production remains behind the reviewed source gate.

use spaceterm_host::native_management::{facade::entry, helper_lifetime::HelperLifetimeGuard};
use std::process::ExitCode;

fn main() -> ExitCode {
    // Guard installation precedes argument collection, stdin, authorization, and output.
    let lifetime = match HelperLifetimeGuard::install() {
        Ok(lifetime) => lifetime,
        Err(failure) => return ExitCode::from(failure as u8),
    };
    let arguments: Vec<_> = std::env::args_os().skip(1).take(4).collect();
    ExitCode::from(entry::production(&arguments, lifetime))
}
