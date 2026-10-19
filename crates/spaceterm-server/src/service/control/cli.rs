//! Owning-account service commands shared by headless and desktop executables.

use super::{Action, Controller};
use crate::{
    ServerError,
    service::{LingerAuthorization, Service, ServiceStatus},
};
use std::{ffi::OsString, io::Write};

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
    Destructive { action: Action, confirmed: bool },
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

/// Execute service arguments excluding the executable name, including the command and identity.
///
/// `identity` is the caller's compiled application identity. Arguments must match it exactly.
/// Validation precedes account-path resolution and service access. Successful operations return
/// 0; an unconfirmed destructive action prints its effects and returns 2 without changing state.
pub fn run(identity: &str, args: &[OsString], output: &mut impl Write) -> Result<u8, Error> {
    let request = Request::parse(identity, args)?;
    let controller = Controller::new(Service::for_identity(identity)?);
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
         IDENTITY must be {identity}, this executable's compiled application identity.\n\
         Service controls use the owning account's authority without pairing or a credential store.\n\
         Stop, Restart and Uninstall show their effects and exit 2 without --confirm.\n\
         To authorize the action, put --confirm after IDENTITY. They end Terminal Sessions\n\
         for every client and keep Workspaces and verified server installations.\n\
         Linux service-enable-linger authorizes only account lingering; SpaceTerm never runs as root.\n\
         Install or Start explicitly after authorization. Status and setup-state preserve Stop."
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
