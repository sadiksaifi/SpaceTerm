use std::{io::Write as _, process::ExitCode};

use spaceterm_server::{
    StartupFailure,
    deployment::{ServerInvocation, distribution_metadata},
    production_startup,
};

fn main() -> ExitCode {
    let invocation = match ServerInvocation::parse(std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(_) => {
            eprintln!("usage: spaceterm-server serve | --distribution-metadata");
            return ExitCode::FAILURE;
        }
    };
    if invocation == ServerInvocation::DistributionMetadata {
        return match distribution_metadata() {
            Ok(metadata) if std::io::stdout().lock().write_all(&metadata).is_ok() => {
                ExitCode::SUCCESS
            }
            _ => {
                eprintln!("server distribution metadata unavailable");
                ExitCode::FAILURE
            }
        };
    }
    let failure = match production_startup() {
        Err(failure) => failure,
        Ok(_startup) => {
            // TODO(#520): transfer account providers into the real runtime/actor
            // composition when its implementation and host lifecycle API land.
            // Opening a key and database alone is not authenticated readiness.
            StartupFailure::RuntimeCompositionUnavailable
        }
    };
    eprintln!("{}", failure.classification());
    ExitCode::FAILURE
}
