use std::{io::Write as _, process::ExitCode};

use spaceterm_server::{
    account_startup::production_run,
    deployment::{ServerInvocation, USAGE, query_output},
};

#[tokio::main]
async fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    #[cfg(feature = "test-support")]
    if arguments.len() == 2 && arguments[0] == "--test-account-root" {
        return exit_result(
            spaceterm_server::account_startup::test_support::run(std::path::Path::new(
                &arguments[1],
            ))
            .await,
        );
    }
    #[cfg(feature = "test-support")]
    if arguments.len() == 3
        && arguments[0] == "--test-account-root"
        && arguments[2] == "owner-enrollment"
    {
        let supervisor = match spaceterm_server::account_startup::test_support::supervisor(
            std::path::Path::new(&arguments[1]),
            std::sync::Arc::new(spaceterm_server::account_startup::test_support::StartupAbsence),
        ) {
            Ok(supervisor) => supervisor,
            Err(failure) => {
                eprintln!("{}", failure.classification());
                return ExitCode::FAILURE;
            }
        };
        return exit_native(
            spaceterm_server::deployment::run_native_with_supervisor(
                ServerInvocation::OwnerEnrollment,
                supervisor,
            )
            .await,
        );
    }
    let invocation = match ServerInvocation::parse(arguments) {
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
    if let Some(result) = spaceterm_server::deployment::run_native(invocation).await {
        return exit_native(result);
    }
    exit_result(production_run().await)
}

fn exit_native(
    result: Result<(), spaceterm_server::deployment::owner::OwnerEndpointFailure>,
) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("{}", failure.classification());
            ExitCode::FAILURE
        }
    }
}

fn exit_result(
    result: Result<spaceterm_server::shutdown::ShutdownReport, spaceterm_server::StartupFailure>,
) -> ExitCode {
    match result {
        Ok(report)
            if report.cleanup_confirmed
                && report.listeners_closed
                && report.started_writes_finished
                && !report.recovery_required =>
        {
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!("server cleanup incomplete");
            ExitCode::FAILURE
        }
        Err(failure) => {
            eprintln!("{}", failure.classification());
            ExitCode::FAILURE
        }
    }
}
