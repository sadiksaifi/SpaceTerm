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
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error("The service command output could not be written.")]
    Output,
}

#[derive(Debug, Eq, PartialEq)]
enum Request {
    Status,
    Start,
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
    let request = Request::parse(identity, args)?;
    let service = Service::for_identity(identity)?;
    let controller = Controller::new(service.clone());
    let status = match request {
        Request::Status => controller.status()?,
        Request::Start => controller.start()?,
        Request::EnableLinger => {
            controller.enable_linger(LingerAuthorization::Terminal)?;
            writeln!(
                output,
                "Boot startup authorized for this account. Install or Start the server explicitly."
            )
            .map_err(|_| Error::Output)?;
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
                    "This exact server artifact is already approved. No update is needed."
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
            let confirmation = controller.prepare(action)?;
            if !confirmed {
                writeln!(
                    output,
                    "{}\n{}\n{}\nNo changes made. Run again with --confirm to proceed.",
                    confirmation.title(),
                    confirmation.message(),
                    confirmation.detail()
                )
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
    let digest = candidate
        .directory
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|v| crate::service::runtime::digest_valid(v))
        .ok_or(ServerError::RuntimeUnavailable)?;
    writeln!(output, "{}\n{}\n{}\nArchive SHA-256: {digest}\nNo changes made. Repeat this exact command with --confirm {token} to proceed.", wording.title(), wording.message(), wording.detail()).map_err(|_| Error::Output)
}

fn write_activation(output: &mut impl Write, status: &ActivationStatus) -> Result<(), Error> {
    let message = match status.phase {
        ActivationPhase::Activating => {
            "Update admitted. Finish Update resolves this same operation."
        }
        ActivationPhase::Completed { running: true } => "The approved server update is running.",
        ActivationPhase::Completed { running: false } => {
            "The runtime change is complete. The server is not running."
        }
        ActivationPhase::Failed { can_return: true } => {
            "The update failed before readiness. A separately confirmed return to the previous runtime is available."
        }
        ActivationPhase::Failed { can_return: false } => {
            "The update failed. Returning to the previous runtime could not be proved safe."
        }
        ActivationPhase::Uncertain => {
            "The update outcome could not be confirmed. Finish Update checks the original operation without repeating destructive dispatch."
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
        Ok(Some(status)) => write_activation(output, &status)?,
        Ok(None) => writeln!(output, "There is no admitted server update to finish.")
            .map_err(|_| Error::Output)?,
        Err(error) => {
            if let Some(status) = service.activation_status()? {
                write_activation(output, &status)?;
            }
            return Err(error.into());
        }
    }
    Ok(0)
}

/// Shared service-command usage and authorization guidance for a compiled application identity.
pub fn help(identity: &str) -> String {
    format!(
        "Service controls (no desktop or display required):\n\
           service-status IDENTITY                     Read status without starting the server\n\
           service-start IDENTITY                      Start explicitly and restore automatic startup\n\
           service-stop IDENTITY [--confirm]            Stop and suspend automatic startup\n\
           service-restart IDENTITY [--confirm]         Restart the approved installation\n\
           service-uninstall IDENTITY [--confirm]       Remove service registration\n\
           service-enable-linger IDENTITY               Authorize Linux boot startup using scoped sudo\n\n\
           service-update IDENTITY RETAINED_INSTALLATION [--confirm SNAPSHOT]\n\
           service-recover-update IDENTITY              Finish the admitted exact-target update\n\
           service-return-update IDENTITY [--confirm SNAPSHOT]\n\n\
         IDENTITY must be {identity}, this executable's compiled application identity.\n\
         Service controls use the owning account's authority without pairing or a credential store.\n\
         Stop, Restart and Uninstall show their effects and exit 2 without --confirm.\n\
         To authorize the action, put --confirm after IDENTITY. They end Terminal Sessions\n\
         for every client and keep Workspaces and verified server installations.\n\
         Linux service-enable-linger authorizes only account lingering; SpaceTerm never runs as root.\n\
         Install or Start explicitly after authorization. Status and setup-state preserve Stop.\n\
         Update and return previews print an exact SNAPSHOT token; changed observations require a new preview.\n\
         Return requires proof that the candidate never reached ready and saved server state is unchanged.\n\
         Return keeps the server stopped. Ended Terminal Sessions never return."
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
        "Stopped. It stays stopped until you start it."
    } else {
        "Not running."
    };
    writeln!(output, "{name} server: {phase}").map_err(|_| Error::Output)?;
    writeln!(
        output,
        "Installed: {}; enabled: {}; running: {}; stopped: {}.",
        status.installed, status.enabled, status.running, status.stopped
    )
    .map_err(|_| Error::Output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    const IDENTITY: &str = "spaceterm-development";

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
                Err(Error::Server(ServerError::Protocol))
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
                    Err(Error::Server(ServerError::Protocol))
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
                Err(Error::Server(ServerError::Protocol))
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
                    Err(Error::Server(ServerError::Protocol))
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
                Err(Error::Server(ServerError::Protocol))
            );
        }
    }

    #[test]
    fn stopped_status_explains_how_to_resume() {
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
        assert_eq!(
            output,
            b"SpaceTerm Development server: Stopped. It stays stopped until you start it.\n\
              Installed: true; enabled: false; running: false; stopped: true.\n"
        );
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
        assert_eq!(
            error.to_string(),
            "The service command output could not be written."
        );
        assert!(std::error::Error::source(&error).is_none());
    }
}
