//! Exec-separated account-only process creation for a single server startup.

use spaceterm_host::native_deployment::contracts::NativeFailure;
use std::process::ExitCode;

fn main() -> ExitCode {
    let result = if std::env::args_os().len() != 1 {
        Err(NativeFailure::InvalidRecord)
    } else {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            // SAFETY: this is the exec-separated binary entry, before application work or
            // credential access. The launcher transfers exclusive ownership of fd 3.
            unsafe { spaceterm_host::native_spawn::run_inherited() }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(NativeFailure::NativeUnavailable)
        }
    };
    // Native failures have bounded numeric classifications. Never emit arguments, environment,
    // paths or native error text; ordinary child output travels only through its intended stream.
    ExitCode::from(result.err().map_or(0, |failure| failure as u8))
}
