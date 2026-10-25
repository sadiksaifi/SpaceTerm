//! Owning-account service commands shared by headless and desktop executables.

use super::{Action, Controller, UpdateChange, UpdateWording};
use crate::{
    ServerError,
    service::{
        LingerAuthorization, RetainedInstallation, Service, ServiceStatus,
        activation::{ActivationPhase, ActivationStatus},
    },
};
use std::{ffi::OsString, io::Write, path::PathBuf};

/// Bounded service-command failures, including output that could not be written.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("Invalid arguments. See --help.")]
    InvalidArguments,
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error("Could not write service command output.")]
    Output,
}

#[derive(Debug, Eq, PartialEq)]
enum Request {
    Status,
    Start,
    AutomaticStart(bool),
    EnableLinger,
    Update {
        installation: PathBuf,
        confirmation: Option<String>,
    },
    RecoverUpdate,
    ReturnUpdate {
        confirmation: Option<String>,
    },
    Destructive {
        action: Action,
        confirmed: bool,
    },
}

impl Request {
    fn parse(expected_identity: &str, args: &[OsString]) -> Result<Self, ServerError> {
        let [command, identity, options @ ..] = args else {
            return Err(ServerError::Protocol);
        };
        if identity != expected_identity {
            return Err(ServerError::Protocol);
        }
        match (command.to_str(), options) {
            (Some("service-status"), []) => Ok(Self::Status),
            (Some("service-start"), []) => Ok(Self::Start),
            (Some("service-automatic-start"), [value]) if value == "on" => {
                Ok(Self::AutomaticStart(true))
            }
            (Some("service-automatic-start"), [value]) if value == "off" => {
                Ok(Self::AutomaticStart(false))
            }
            (Some("service-enable-linger"), []) => Ok(Self::EnableLinger),
            (Some("service-recover-update"), []) => Ok(Self::RecoverUpdate),
            (Some("service-update"), [installation, rest @ ..])
                if !installation.as_encoded_bytes().starts_with(b"-") =>
            {
                Ok(Self::Update {
                    installation: installation.into(),
                    confirmation: parse_confirmation(rest)?,
                })
            }
            (Some("service-return-update"), rest) => Ok(Self::ReturnUpdate {
                confirmation: parse_confirmation(rest)?,
            }),
            (Some(command), [] | [_]) => {
                let action = match command {
                    "service-stop" => Action::Stop,
                    "service-restart" => Action::Restart,
                    "service-uninstall" => Action::Uninstall,
                    _ => return Err(ServerError::Protocol),
                };
                let confirmed = match options {
                    [] => false,
                    [flag] if flag == "--confirm" => true,
                    _ => return Err(ServerError::Protocol),
                };
                Ok(Self::Destructive { action, confirmed })
            }
            _ => Err(ServerError::Protocol),
        }
    }
}

fn parse_confirmation(options: &[OsString]) -> Result<Option<String>, ServerError> {
    match options {
        [] => Ok(None),
        [flag, token] if flag == "--confirm" => token
            .to_str()
            .filter(|v| crate::service::runtime::digest_valid(v))
            .map(|v| Some(v.into()))
            .ok_or(ServerError::Protocol),
        _ => Err(ServerError::Protocol),
    }
}

/// Execute service arguments excluding the executable name, including the command and identity.
///
/// `identity` is the caller's compiled application identity. Arguments must match it exactly.
/// Validation precedes account-path resolution and service access. Successful operations return
/// 0; an unconfirmed destructive action prints its effects and returns 2 without changing state.
pub fn run(identity: &str, args: &[OsString], output: &mut impl Write) -> Result<u8, Error> {
    let request = Request::parse(identity, args).map_err(|_| Error::InvalidArguments)?;
    let service = Service::for_identity(identity)?;
    let controller = Controller::new(service.clone());
    let status = match request {
        Request::Status => controller.status()?,
        Request::Start => controller.start()?,
        Request::AutomaticStart(enabled) => controller.set_automatic_start(enabled)?,
        Request::EnableLinger => {
            controller.enable_linger(LingerAuthorization::Terminal)?;
            writeln!(output, "Boot startup authorized.").map_err(|_| Error::Output)?;
            return Ok(0);
        }
        Request::Update {
            installation,
            confirmation,
        } => {
            let digest = installation
                .file_name()
                .and_then(|v| v.to_str())
                .filter(|v| crate::service::runtime::digest_valid(v))
                .ok_or(ServerError::RuntimeUnavailable)?;
            if installation != service.paths.data.join("installations").join(digest) {
                return Err(ServerError::RuntimeUnavailable.into());
            }
            let (version, commit) = crate::service::runtime::describe(&installation, identity)?;
            let candidate = RetainedInstallation {
                directory: installation,
                version,
                commit,
            };
            let Some(approval) = service.prepare_activation(&candidate)? else {
                writeln!(
                    output,
                    "This server version is already installed. No update is needed."
                )
                .map_err(|_| Error::Output)?;
                return Ok(0);
            };
            let token = approval.confirmation_token()?;
            let wording = UpdateWording::new(
                identity,
                UpdateChange::Update,
                approval.previous(),
                approval.candidate(),
                approval.running(),
                approval.stopped(),
            );
            if let Some(confirmation) = confirmation {
                if confirmation != token {
                    return Err(ServerError::Changed.into());
                }
                return write_activation_result(
                    output,
                    &service,
                    service.activate(approval).map(Some),
                );
            }
            write_update_preview(output, &wording, &candidate, &token)?;
            return Ok(2);
        }
        Request::RecoverUpdate => {
            return write_activation_result(output, &service, service.recover_activation());
        }
        Request::ReturnUpdate { confirmation } => {
            let approval = service.prepare_return()?;
            let token = approval.confirmation_token()?;
            let wording = UpdateWording::new(
                identity,
                UpdateChange::Return,
                approval.previous(),
                approval.candidate(),
                false,
                true,
            );
            if let Some(confirmation) = confirmation {
                if confirmation != token {
                    return Err(ServerError::Changed.into());
                }
                return write_activation_result(
                    output,
                    &service,
                    service.return_to_previous(approval).map(Some),
                );
            }
            write_update_preview(output, &wording, approval.previous(), &token)?;
            return Ok(2);
        }
        Request::Destructive { action, confirmed } => {
            if action == Action::Stop {
                match service.stop_if_stopped() {
                    Ok(Some(status)) => {
                        write_status(output, controller.display_name(), status)?;
                        return Ok(0);
                    }
                    Ok(None) => {}
                    Err(ServerError::CleanupUnresolved) if confirmed => {}
                    Err(error) => return Err(error.into()),
                }
            }
            let confirmation = controller.prepare(action)?;
            if !confirmed {
                writeln!(output, "{}", confirmation.message()).map_err(|_| Error::Output)?;
                let detail = confirmation.detail();
                if !detail.is_empty() {
                    writeln!(output, "{detail}").map_err(|_| Error::Output)?;
                }
                writeln!(output, "Run again with --confirm to proceed.")
                    .map_err(|_| Error::Output)?;
                return Ok(2);
            }
            controller.confirm(confirmation)?
        }
    };
    write_status(output, controller.display_name(), status)?;
    Ok(0)
}

fn write_update_preview(
    output: &mut impl Write,
    wording: &UpdateWording,
    candidate: &RetainedInstallation,
    token: &str,
) -> Result<(), Error> {
    candidate
        .directory
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|v| crate::service::runtime::digest_valid(v))
        .ok_or(ServerError::RuntimeUnavailable)?;
    // The stopped-server CLI preview has distinct approved wording (#645).
    let question = if wording.change == UpdateChange::Update && wording.stopped && !wording.running
    {
        format!(
            "Update stopped {} server to {}?",
            wording.server, wording.candidate
        )
    } else {
        wording.message()
    };
    writeln!(output, "{question}").map_err(|_| Error::Output)?;
    let detail = wording.detail();
    if !detail.is_empty() {
        writeln!(output, "{detail}").map_err(|_| Error::Output)?;
    }
    writeln!(output, "Repeat this command with --confirm {token}.").map_err(|_| Error::Output)
}

fn write_activation(
    output: &mut impl Write,
    identity: &str,
    status: &ActivationStatus,
) -> Result<(), Error> {
    let message = match status.phase {
        ActivationPhase::Activating => "Update in progress. Run service-recover-update to finish.",
        ActivationPhase::Completed { running: true } => "Server updated and running.",
        ActivationPhase::Completed { running: false } => {
            "The server version changed. The server is not running."
        }
        ActivationPhase::Failed { can_return: true } => {
            return writeln!(
                output,
                "The update failed to start. Run service-return-update {identity} to review restoring the previous version."
            )
            .map_err(|_| Error::Output);
        }
        ActivationPhase::Failed { can_return: false } => {
            "The update failed. SpaceTerm can't safely restore the previous server version."
        }
        ActivationPhase::Uncertain => {
            return writeln!(
                output,
                "SpaceTerm couldn't confirm whether the update finished. Run service-recover-update {identity} to check it."
            )
            .map_err(|_| Error::Output);
        }
    };
    writeln!(output, "{message}").map_err(|_| Error::Output)
}

fn write_activation_result(
    output: &mut impl Write,
    service: &Service,
    result: Result<Option<ActivationStatus>, ServerError>,
) -> Result<u8, Error> {
    match result {
        Ok(Some(status)) => write_activation(output, &service.identity, &status)?,
        Ok(None) => {
            writeln!(output, "There is no unfinished server update.").map_err(|_| Error::Output)?
        }
        Err(error) => {
            if let Some(status) = service.activation_status()? {
                write_activation(output, &service.identity, &status)?;
            }
            return Err(error.into());
        }
    }
    Ok(0)
}

/// Shared service-command usage and authorization guidance for a compiled application identity.
pub fn help(identity: &str) -> String {
    format!(
        "Service controls:\n\
         service-status IDENTITY                     Show status\n\
         service-start IDENTITY                      Start the server\n\
         service-automatic-start IDENTITY on|off      Turn automatic startup on or off\n\
         service-stop IDENTITY [--confirm]            Stop the server\n\
         service-restart IDENTITY [--confirm]         Restart the server\n\
         service-uninstall IDENTITY [--confirm]       Uninstall the server\n\
         service-enable-linger IDENTITY               Allow startup at boot (Linux)\n\
         service-update IDENTITY RETAINED_INSTALLATION [--confirm SNAPSHOT]\n\
         service-recover-update IDENTITY              Finish an interrupted update\n\
         service-return-update IDENTITY [--confirm SNAPSHOT]\n\n\
         IDENTITY is {identity}. Stop, Restart, Uninstall and Update end all Terminal Sessions. Without --confirm they print a preview and exit 2. Update and return previews print the SNAPSHOT to pass to --confirm."
    )
}

/// Write the same bounded status summary after service controls and installation.
pub fn write_status(
    output: &mut impl Write,
    name: &str,
    status: ServiceStatus,
) -> Result<(), Error> {
    let phase = if !status.installed {
        "Not installed."
    } else if status.running {
        "Running."
    } else if status.stopped {
        "Stopped."
    } else {
        "Not running."
    };
    writeln!(output, "{name} server: {phase}").map_err(|_| Error::Output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    const IDENTITY: &str = "spaceterm-development";

    #[test]
    fn invalid_arguments_report_usage_without_echoing_private_input() {
        for args in [
            Vec::new(),
            vec!["service-private-command".into(), IDENTITY.into()],
            vec!["service-status".into(), "private-identity".into()],
        ] {
            let mut output = Vec::new();
            let error = run(IDENTITY, &args, &mut output).unwrap_err();
            assert_eq!(error.to_string(), "Invalid arguments. See --help.");
            assert!(output.is_empty());
        }
    }

    #[test]
    fn automatic_start_accepts_only_an_explicit_policy_and_compiled_identity() {
        for (value, enabled) in [("on", true), ("off", false)] {
            let args = ["service-automatic-start", IDENTITY, value].map(OsString::from);
            assert_eq!(
                Request::parse(IDENTITY, &args),
                Ok(Request::AutomaticStart(enabled))
            );
        }
        for args in [
            vec!["service-automatic-start", IDENTITY],
            vec!["service-automatic-start", IDENTITY, "true"],
            vec!["service-automatic-start", "spaceterm", "on"],
            vec!["service-automatic-start", IDENTITY, "off", "--confirm"],
        ] {
            let args: Vec<_> = args.into_iter().map(OsString::from).collect();
            assert_eq!(
                run(IDENTITY, &args, &mut Vec::new()),
                Err(Error::InvalidArguments)
            );
        }
    }

    #[test]
    fn update_confirmation_requires_an_exact_snapshot_and_compiled_identity() {
        let token = "a".repeat(64);
        let installation = PathBuf::from("/retained/exact-artifact");
        let args = [
            OsString::from("service-update"),
            IDENTITY.into(),
            installation.clone().into_os_string(),
            "--confirm".into(),
            token.clone().into(),
        ];
        assert_eq!(
            Request::parse(IDENTITY, &args),
            Ok(Request::Update {
                installation,
                confirmation: Some(token)
            })
        );
        for args in [
            vec![
                "service-update",
                IDENTITY,
                "/retained/exact-artifact",
                "--confirm",
            ],
            vec![
                "service-update",
                IDENTITY,
                "/retained/exact-artifact",
                "--confirm",
                "stale",
            ],
            vec!["service-return-update", IDENTITY, "--confirm"],
            vec!["service-recover-update", IDENTITY, "--confirm"],
            vec!["service-recover-update", "spaceterm"],
        ] {
            let args: Vec<_> = args.into_iter().map(OsString::from).collect();
            assert_eq!(
                run(IDENTITY, &args, &mut Vec::new()),
                Err(Error::InvalidArguments)
            );
        }
    }

    #[test]
    fn destructive_execution_requires_a_trailing_confirmation_flag() {
        for (command, expected_action) in [
            ("service-stop", Action::Stop),
            ("service-restart", Action::Restart),
            ("service-uninstall", Action::Uninstall),
        ] {
            let preview = [command, IDENTITY].map(OsString::from);
            assert_eq!(
                Request::parse(IDENTITY, &preview),
                Ok(Request::Destructive {
                    action: expected_action,
                    confirmed: false,
                })
            );
            let confirmed = [command, IDENTITY, "--confirm"].map(OsString::from);
            assert_eq!(
                Request::parse(IDENTITY, &confirmed),
                Ok(Request::Destructive {
                    action: expected_action,
                    confirmed: true,
                })
            );
            for malformed in [
                vec![command, "--confirm", IDENTITY],
                vec!["--confirm", command, IDENTITY],
                vec![command, IDENTITY, "--yes"],
                vec![command, IDENTITY, "--confirm", "--confirm"],
                vec![command, IDENTITY, "--confirm", "unexpected"],
            ] {
                let malformed: Vec<_> = malformed.into_iter().map(OsString::from).collect();
                assert_eq!(
                    run(IDENTITY, &malformed, &mut Vec::new()),
                    Err(Error::InvalidArguments)
                );
            }
        }
    }

    #[test]
    fn confirmation_cannot_authorize_other_service_operations() {
        for command in [
            "service-status",
            "service-start",
            "service-enable-linger",
            "service-install",
            "service-serve",
            "serve",
            "status",
            "setup-state",
        ] {
            let args = [command, IDENTITY, "--confirm"].map(OsString::from);
            assert_eq!(
                run(IDENTITY, &args, &mut Vec::new()),
                Err(Error::InvalidArguments)
            );
        }
    }

    #[test]
    fn non_destructive_commands_accept_only_the_expected_identity() {
        for (command, request) in [
            ("service-status", Request::Status),
            ("service-start", Request::Start),
            ("service-enable-linger", Request::EnableLinger),
        ] {
            let args = [command, IDENTITY].map(OsString::from);
            assert_eq!(Request::parse(IDENTITY, &args), Ok(request));
        }
    }

    #[test]
    fn invalid_arguments_are_refused_before_account_service_access() {
        for command in [
            "service-status",
            "service-start",
            "service-stop",
            "service-restart",
            "service-uninstall",
            "service-enable-linger",
        ] {
            for identity in [
                OsString::from("spaceterm"),
                OsString::from("private-foreign-identity"),
                OsString::from_vec(b"private-non-utf8-identity-\xff".to_vec()),
            ] {
                let mut output = Vec::new();
                assert_eq!(
                    run(IDENTITY, &[command.into(), identity], &mut output),
                    Err(Error::InvalidArguments)
                );
                assert!(output.is_empty());
            }
        }
        for args in [
            Vec::new(),
            vec!["service-status".into()],
            vec!["service-private-unknown-command".into(), IDENTITY.into()],
            vec![
                OsString::from_vec(b"service-private-non-utf8-\xff".to_vec()),
                IDENTITY.into(),
            ],
            vec!["service-install".into(), IDENTITY.into(), "/private".into()],
            vec!["service-serve".into(), IDENTITY.into()],
        ] {
            assert_eq!(
                run(IDENTITY, &args, &mut Vec::new()),
                Err(Error::InvalidArguments)
            );
        }
    }

    #[test]
    fn stopped_status_reports_the_server_phase() {
        let mut output = Vec::new();
        write_status(
            &mut output,
            "SpaceTerm Development",
            ServiceStatus {
                installed: true,
                stopped: true,
                ..ServiceStatus::default()
            },
        )
        .unwrap();
        assert_eq!(output, b"SpaceTerm Development server: Stopped.\n");
    }

    #[test]
    fn output_errors_discard_native_details() {
        struct RefusedOutput;

        impl Write for RefusedOutput {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("private-output-path"))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let error =
            write_status(&mut RefusedOutput, "SpaceTerm", ServiceStatus::default()).unwrap_err();
        assert_eq!(error, Error::Output);
        assert_eq!(error.to_string(), "Could not write service command output.");
        assert!(std::error::Error::source(&error).is_none());
    }
}
