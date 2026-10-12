//! Headless client executable. Native startup is never replaced with test composition.

use std::{ffi::OsString, process::ExitCode, sync::Arc};

use spaceterm_client_native::cli::{
    arguments::{MAX_ARGUMENTS, parse},
    bindings::{
        UnavailableNativeBindings, UnavailablePairingBindings, UnavailableProtocolManagement,
        invoking_account_key,
    },
    output::emit_failure,
    runner::{CliDependencies, Dispatch, dispatch},
    tty::TerminalIo,
};

#[tokio::main]
async fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os()
        .skip(1)
        .take(MAX_ARGUMENTS + 1)
        .collect();
    let json = arguments
        .iter()
        .take_while(|argument| *argument == "--json" || *argument == "--non-interactive")
        .any(|argument| argument == "--json");
    let mut io = TerminalIo;
    let invocation = match parse(arguments) {
        Ok(value) => value,
        Err(failure) => {
            let failure = emit_failure(failure, json, &mut io)
                .err()
                .unwrap_or(failure);
            return ExitCode::from(failure.exit_code());
        }
    };
    let dependencies = CliDependencies::new(
        invoking_account_key(),
        Arc::new(UnavailableProtocolManagement),
        Arc::new(UnavailableNativeBindings::default()),
        Arc::new(UnavailablePairingBindings),
    );
    match dispatch(invocation, &dependencies, &mut io).await {
        Dispatch::Exit(exit) => ExitCode::from(exit.code()),
        Dispatch::Serve => {
            // This client-only target cannot enter daemon composition. The separate
            // server executable retains its protected native-startup path.
            let failure = spaceterm_client_native::cli::CliFailure::NativeUnavailable;
            let _ = emit_failure(failure, json, &mut io);
            ExitCode::from(failure.exit_code())
        }
    }
}
