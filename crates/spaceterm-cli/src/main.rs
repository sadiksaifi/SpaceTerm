//! The headless executable. Command policy lives in the CLI library and server modules.
mod check;
mod provision;

use clap::{CommandFactory, Parser};
use spaceterm_cli::{
    args::{self, Cli, Command, ProvisionCommand, ServerCommand},
    contract::{self, Code, Details, Empty, Envelope, Failure},
};
use spaceterm_server::{
    ServerError,
    service::{CliUninstall, Service},
};
use std::{ffi::OsString, io::Write, process::ExitCode};

const IDENTITY: &str = env!("SPACETERM_SERVER_IDENTITY");

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(error) => return usage(&argv, &error),
    };
    let command = cli.command.path();
    // The server and its helper keep the default signal handling.
    if !matches!(
        cli.command,
        Command::Server(ServerCommand::Run { .. } | ServerCommand::Provision(_))
    ) && let Err(failure) = spaceterm_cli::interrupt::install(command, (&cli.global).into())
    {
        return finish(command, cli.global.json, failure);
    }
    #[cfg(target_os = "macos")]
    if let Err(error) = resolve_launcher() {
        return finish(command, cli.global.json, contract::from_server_error(error));
    }
    if let args::Command::Connection(command) = &cli.command {
        let result = spaceterm_cli::connection::execute(
            command,
            &cli.global,
            env!("SPACETERM_SERVER_IDENTITY"),
        );
        return ExitCode::from(result.render(
            (&cli.global).into(),
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock(),
        ));
    }
    if let Err(failure) = validate_route(&cli) {
        return finish(cli.command.path(), cli.global.json, failure);
    }
    match &cli.command {
        Command::Completion { shell } => {
            let mut output = Vec::new();
            clap_complete::generate(*shell, &mut Cli::command(), "spaceterm", &mut output);
            return match std::io::stdout().lock().write_all(&output) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => output_failed(&error),
            };
        }
        Command::Server(ServerCommand::Run {
            service,
            owned,
            identity,
        }) => {
            if identity.as_deref().is_some_and(|value| value != IDENTITY)
                || (*owned && IDENTITY != "spaceterm-development")
            {
                return finish(
                    command,
                    cli.global.json,
                    Failure::new(Code::InvalidArguments),
                );
            }
            if remote(&cli) {
                return finish(command, cli.global.json, Failure::new(Code::LocalOnly));
            }
            return match run_server(*service, *owned) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => finish(command, cli.global.json, contract::from_server_error(error)),
            };
        }
        Command::Server(ServerCommand::Provision(provision)) => {
            if remote(&cli) {
                return finish(command, cli.global.json, Failure::new(Code::LocalOnly));
            }
            match provision {
                ProvisionCommand::Retain { installation } => {
                    let retained = Service::for_identity(IDENTITY)
                        .and_then(|service| service.retain_cli(installation));
                    return match retained {
                        Ok(installed) => complete(
                            &cli,
                            Envelope::ok(
                                command,
                                None,
                                serde_json::json!({"launcher_installed":installed}),
                            ),
                            if installed {
                                "The CLI launcher is installed."
                            } else {
                                "The existing launcher belongs to another tool and was kept."
                            },
                        ),
                        Err(error) => {
                            finish(command, cli.global.json, contract::from_server_error(error))
                        }
                    };
                }
                ProvisionCommand::Uninstall { confirm } => {
                    let result = Service::for_identity(IDENTITY)
                        .and_then(|service| service.uninstall_cli(confirm.as_deref()));
                    return match result {
                        Ok(CliUninstall::Completed) => complete(
                            &cli,
                            Envelope::ok(command, None, Empty {}),
                            "The server service and managed CLI launcher are uninstalled. Workspaces and retained installations remain.",
                        ),
                        Ok(CliUninstall::Preview {
                            token,
                            expires_at,
                            sessions,
                        }) => {
                            let effects = contract::ServiceEffects {
                                ends_sessions: sessions.len() as u32,
                                sessions: sessions
                                    .iter()
                                    .map(|id| id.iter().map(|byte| format!("{byte:02x}")).collect())
                                    .collect(),
                                start_requires_machine: true,
                            };
                            let expires_at = match i64::try_from(expires_at)
                                .ok()
                                .and_then(|value| jiff::Timestamp::from_second(value).ok())
                            {
                                Some(timestamp) => timestamp.to_string(),
                                None => {
                                    return finish(
                                        command,
                                        cli.global.json,
                                        Failure::new(Code::StorageUnavailable),
                                    );
                                }
                            };
                            let text = format!(
                                "Uninstall the server? All Terminal Sessions on it end for every client. Workspaces and retained installations remain.\nRepeat with --confirm {token}."
                            );
                            complete(
                                &cli,
                                Envelope::confirmation(
                                    command,
                                    None,
                                    contract::Confirmation {
                                        token,
                                        expires_at,
                                        effects,
                                    },
                                ),
                                &text,
                            )
                        }
                        Err(error) => {
                            finish(command, cli.global.json, contract::from_server_error(error))
                        }
                    };
                }
                _ => {
                    return match provision::run(provision) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(provision::Error::Failure(failure)) => {
                            finish(command, cli.global.json, failure)
                        }
                        Err(provision::Error::Output(error)) => output_failed(&error),
                    };
                }
            }
        }
        _ => {}
    }
    if let args::Command::Server(command) = &cli.command
        && !matches!(
            command,
            args::ServerCommand::Run { .. } | args::ServerCommand::Provision(_)
        )
    {
        let reply =
            spaceterm_cli::server::run(env!("SPACETERM_SERVER_IDENTITY"), &cli.global, command);
        return ExitCode::from(reply.write(
            (&cli.global).into(),
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock(),
        ));
    }
    if let Some(output) = spaceterm_cli::io::start::start(&cli) {
        return ExitCode::from(output.write(
            (&cli.global).into(),
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock(),
        ));
    }
    if let Some(output) = spaceterm_cli::work::start::start(&cli) {
        return ExitCode::from(output.write(
            (&cli.global).into(),
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock(),
        ));
    }
    finish(command, cli.global.json, Failure::new(Code::Unsupported))
}

fn remote(cli: &Cli) -> bool {
    cli.global
        .server
        .clone()
        .or_else(|| std::env::var("SPACETERM_SERVER").ok())
        .is_some_and(|value| value != "local")
}

#[cfg(target_os = "macos")]
fn resolve_launcher() -> Result<(), ServerError> {
    use std::os::unix::process::CommandExt;
    let executable = std::env::current_exe().map_err(|_| ServerError::StartupFailed)?;
    let resolved = executable
        .canonicalize()
        .map_err(|_| ServerError::StartupFailed)?;
    if resolved != executable {
        // macOS reports the symlink used to launch the image. Re-execution makes resource
        // lookup and Session context use the retained executable's installation directory.
        let _ = std::process::Command::new(resolved)
            .args(std::env::args_os().skip(1))
            .exec();
        return Err(ServerError::StartupFailed);
    }
    Ok(())
}

fn run_server(service: bool, owned: bool) -> Result<(), ServerError> {
    check::resources()?;
    let panes = spaceterm_server::panes(|sync| {
        Ok(spaceterm::runtime::Served::new(
            spaceterm::runtime::SharedWorkspaceHost::account_service(sync)?
                .with_session_context(spaceterm::runtime::ServerSessionContext::current(IDENTITY)?),
        ))
    });
    if service {
        spaceterm_server::service::run(IDENTITY, panes)
    } else if owned {
        spaceterm_server::run_owned_development(panes, std::io::stdin(), || {
            let mut output = std::io::stdout().lock();
            output
                .write_all(b"spaceterm-development-ready-v1\n")
                .and_then(|()| output.flush())
                .map_err(|_| ServerError::StartupFailed)
        })
    } else {
        spaceterm_server::run_local_with_panes(IDENTITY, panes)
    }
}

fn complete<D: serde::Serialize>(cli: &Cli, envelope: Envelope<D>, text: &str) -> ExitCode {
    ExitCode::from(contract::render(
        &envelope,
        &format!("{text}\n"),
        "",
        (&cli.global).into(),
        spaceterm_cli::interrupt::finish(envelope.exit()),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    ))
}

fn validate_route(cli: &Cli) -> Result<(), Failure> {
    let environment = if cli.global.server.is_none() {
        std::env::var_os("SPACETERM_SERVER")
    } else {
        None
    };
    let environment = environment
        .as_ref()
        .map(|value| {
            value.to_str().ok_or_else(|| {
                Failure::with(
                    Code::InvalidArguments,
                    Details::Usage(contract::UsageDetails {
                        argument: Some("--server".into()),
                        kind: contract::UsageKind::InvalidUtf8,
                    }),
                )
            })
        })
        .transpose()?;
    let selector =
        spaceterm_cli::connection::route::selected(cli.global.server.as_deref(), environment)?;
    spaceterm_cli::connection::route::validate_command(&cli.command, selector)
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
fn partial_path(argv: &[OsString]) -> String {
    let Ok(matches) = Cli::command()
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
    let envelope = Envelope::error(command, None, failure, Empty {});
    ExitCode::from(contract::render(
        &envelope,
        "",
        "",
        contract::Mode {
            json,
            ..contract::Mode::default()
        },
        spaceterm_cli::interrupt::finish(envelope.exit()),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    ))
}
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
