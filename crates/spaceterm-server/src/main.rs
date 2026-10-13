use std::{io::Write as _, process::ExitCode};

use spaceterm_server::{
    StartupFailure,
    deployment::{ServerInvocation, USAGE, query_output},
    production_startup,
};

fn main() -> ExitCode {
    let invocation = match ServerInvocation::parse(std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(_) => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match query_output(invocation) {
        Ok(Some(output)) => {
            return if std::io::stdout().lock().write_all(&output).is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Ok(None) => {}
        Err(failure) => {
            eprintln!("{}", failure.classification());
            return ExitCode::FAILURE;
        }
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
