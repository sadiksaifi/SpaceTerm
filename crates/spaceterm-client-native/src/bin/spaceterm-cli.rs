//! Headless client executable. Native startup is never replaced with test composition.

use std::{ffi::OsString, process::ExitCode, sync::Arc};

use spaceterm_client_native::NativeClientProvider;
use spaceterm_client_native::cli::{
    arguments::{CliCommand, MAX_ARGUMENTS, parse},
    output::{emit_failure, emit_text},
    runner::{CliDependencies, Dispatch, dispatch},
    tty::TerminalIo,
};

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            let failure = spaceterm_client_native::cli::CliFailure::IoUnavailable;
            let _ = emit_failure(failure, false, &mut TerminalIo);
            return ExitCode::from(failure.exit_code());
        }
    };
    let exit = runtime.block_on(run());
    // Cancelled blocking HTTPS work must not delay Ctrl-C for its download
    // deadline. Service effects already have their original durable host record.
    runtime.shutdown_background();
    exit
}

async fn run() -> ExitCode {
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
    // Help and version do not require a native account, registration or manager.
    let text = match invocation.command {
        CliCommand::Help => Some(("usage", spaceterm_client_native::cli::output::USAGE)),
        CliCommand::Version => Some((
            "version",
            spaceterm_client_native::cli::bindings::CLI_VERSION,
        )),
        _ => None,
    };
    if let Some((kind, text)) = text {
        return match emit_text(kind, text, json, &mut io) {
            Ok(()) => ExitCode::SUCCESS,
            Err(failure) => ExitCode::from(failure.exit_code()),
        };
    }
    let mode = if invocation.non_interactive {
        spaceterm_host::account_service::SudoMode::NonInteractive
    } else {
        spaceterm_host::account_service::SudoMode::Interactive
    };
    let provider = match NativeClientProvider::with_authorization(Some(Arc::new(
        spaceterm_host::account_service::SudoLingerAuthorization::new(mode),
    ))) {
        Ok(provider) => provider,
        Err(failure) => {
            let _ = emit_failure(failure, json, &mut io);
            return ExitCode::from(failure.exit_code());
        }
    };
    let consumer = provider.cli_consumer();
    let dependencies = CliDependencies::new(
        Ok(provider.key()),
        consumer.clone(),
        provider.management(),
        consumer,
    );
    // The signal task also runs while a controlling-terminal read blocks the
    // dispatch thread. Exiting never acknowledges or reissues admitted work.
    let interrupt = tokio::spawn(async move {
        let failure = if tokio::signal::ctrl_c().await.is_ok() {
            spaceterm_client_native::cli::CliFailure::OutcomePending
        } else {
            spaceterm_client_native::cli::CliFailure::IoUnavailable
        };
        let _ = emit_failure(failure, json, &mut TerminalIo);
        std::process::exit(i32::from(failure.exit_code()));
    });
    let result = dispatch(invocation, &dependencies, &mut io).await;
    interrupt.abort();
    match result {
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
