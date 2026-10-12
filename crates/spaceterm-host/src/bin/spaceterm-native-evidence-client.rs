//! Unavailable entry shell, owned by native-evidence E4.

use spaceterm_host::native_deployment::contracts::NativeFailure;
use std::process::ExitCode;

fn main() -> ExitCode {
    let failure = NativeFailure::NativeUnavailable;
    eprintln!("{failure}");
    ExitCode::from(failure as u8)
}
