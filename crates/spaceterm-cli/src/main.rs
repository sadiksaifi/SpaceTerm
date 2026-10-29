//! `spaceterm`: the SpaceTerm Server and the command line that controls it.
//!
//! Every command ends through the output contract in [`contract`]: one envelope with `--json`,
//! otherwise text, and an exit status from its class.
#[allow(dead_code, reason = "`server provision check` runs these checks")]
mod check;

use clap::Parser;
use spaceterm_cli::args::{self, Cli};
use spaceterm_cli::contract::{self, Code, Details, Empty, Envelope, Failure};
use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(error) => return usage(&argv, &error),
    };
    if let args::Command::Server(command) = &cli.command
        && !matches!(
            command,
            args::ServerCommand::Run { .. } | args::ServerCommand::Provision(_)
        )
    {
        let reply =
            spaceterm_cli::server::run(env!("SPACETERM_SERVER_IDENTITY"), &cli.global, command);
        return ExitCode::from(reply.write(cli.global.json, cli.global.quiet));
    }
    if let Some(output) = spaceterm_cli::work::start::start(&cli) {
        return ExitCode::from(output.write(
            cli.global.json,
            cli.global.quiet,
            cli.global.verbose,
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock(),
        ));
    }
    finish(
        cli.command.path(),
        cli.global.json,
        Failure::new(Code::Unsupported),
    )
}

/// Help and version print normally. Every other parse error is a bounded usage failure.
fn usage(argv: &[OsString], error: &clap::Error) -> ExitCode {
    use clap::error::ErrorKind;
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        return match write!(std::io::stdout().lock(), "{}", error.render()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => output_failed(&error),
        };
    }
    let json = argv
        .iter()
        .skip(1)
        .take_while(|argument| *argument != "--")
        .any(|argument| argument == "--json");
    finish(&partial_path(argv), json, args::usage_failure(error))
}

/// The dotted path of the subcommands clap recognized before the error, or empty.
fn partial_path(argv: &[OsString]) -> String {
    let Ok(matches) = <Cli as clap::CommandFactory>::command()
        .ignore_errors(true)
        .try_get_matches_from(argv)
    else {
        return String::new();
    };
    let mut path = Vec::new();
    let mut matches = &matches;
    while let Some((name, sub)) = matches.subcommand() {
        path.push(name.to_owned());
        matches = sub;
    }
    path.join(".")
}

fn finish(command: &str, json: bool, failure: Failure) -> ExitCode {
    let text = failure.text();
    let usage = match &failure.details {
        Details::Usage(details) => details.argument.clone(),
        _ => None,
    };
    let envelope = Envelope::error(command, None, failure, Empty {});
    let exit = envelope.exit();
    if json {
        let mut output = std::io::stdout().lock();
        let written = serde_json::to_writer(&mut output, &envelope)
            .map_err(std::io::Error::from)
            .and_then(|()| output.write_all(b"\n"))
            .and_then(|()| output.flush());
        if let Err(error) = written {
            return output_failed(&error);
        }
    } else {
        let mut errors = std::io::stderr().lock();
        let _ = writeln!(errors, "{text}");
        if let Some(argument) = usage {
            let _ = writeln!(errors, "argument: {argument}");
        }
    }
    ExitCode::from(exit)
}

/// A broken stdout pipe exits 141 without a message. Any other write failure is
/// `output_failed`.
fn output_failed(error: &std::io::Error) -> ExitCode {
    if error.kind() == std::io::ErrorKind::BrokenPipe {
        return ExitCode::from(contract::EXIT_BROKEN_PIPE);
    }
    let _ = writeln!(
        std::io::stderr().lock(),
        "{}",
        Failure::new(Code::OutputFailed).text()
    );
    ExitCode::from(Code::OutputFailed.exit())
}
