use std::process::ExitCode;

use spaceterm_server::{StartupFailure, production_startup};

fn main() -> ExitCode {
    match production_startup() {
        Err(StartupFailure::NativeCredentialDeliveryUnavailable) => {
            eprintln!("native credential delivery unavailable");
            ExitCode::FAILURE
        }
        Ok(()) => ExitCode::SUCCESS,
    }
}
