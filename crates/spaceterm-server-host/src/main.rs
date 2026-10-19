mod check;

use spaceterm_server::{
    ServerError,
    service::{
        LingerAuthorization, Service, ServiceStatus,
        control::{Action, Controller},
    },
};
use std::{ffi::OsString, path::Path, process::ExitCode};

const IDENTITY: &str = env!("SPACETERM_SERVER_IDENTITY");

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

enum Request<'a> {
    Serve,
    ServiceServe,
    SetupState,
    Status,
    Install(&'a Path),
    Service(ServiceRequest),
}

enum ServiceRequest {
    Status,
    Start,
    EnableLinger,
    Destructive { action: Action, confirmed: bool },
}

impl<'a> Request<'a> {
    fn parse(args: &'a [OsString]) -> Result<Self, ServerError> {
        let [command, identity, options @ ..] = args else {
            return Err(ServerError::Protocol);
        };
        if identity != IDENTITY {
            return Err(ServerError::Protocol);
        }
        match (command.to_str(), options) {
            (Some("serve"), []) => Ok(Self::Serve),
            (Some("service-serve"), []) => Ok(Self::ServiceServe),
            (Some("setup-state"), []) => Ok(Self::SetupState),
            (Some("status"), []) => Ok(Self::Status),
            (Some("service-status"), []) => Ok(Self::Service(ServiceRequest::Status)),
            (Some("service-start"), []) => Ok(Self::Service(ServiceRequest::Start)),
            (Some("service-enable-linger"), []) => Ok(Self::Service(ServiceRequest::EnableLinger)),
            (Some("service-install"), [installation])
                if !installation.as_encoded_bytes().starts_with(b"-") =>
            {
                Ok(Self::Install(Path::new(installation)))
            }
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
                Ok(Self::Service(ServiceRequest::Destructive {
                    action,
                    confirmed,
                }))
            }
            _ => Err(ServerError::Protocol),
        }
    }
}

fn run() -> Result<ExitCode, ServerError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == ["--help"] {
        println!(
            "SpaceTerm Server\n\n\
             Usage: spaceterm-server COMMAND {IDENTITY} [OPTIONS]\n\n\
             Service controls (no desktop or display required):\n\
               service-status IDENTITY                     Read status without starting the server\n\
               service-start IDENTITY                      Start explicitly and restore automatic startup\n\
               service-stop IDENTITY [--confirm]            Stop and suspend automatic startup\n\
               service-restart IDENTITY [--confirm]         Restart the approved installation\n\
               service-uninstall IDENTITY [--confirm]       Remove service registration\n\
               service-install IDENTITY RETAINED_INSTALLATION\n\
               service-enable-linger IDENTITY               Authorize Linux boot startup using scoped sudo\n\n\
             Other commands:\n\
               serve IDENTITY                              Run in the foreground\n\
               setup-state IDENTITY                        Set up nonsecret state (client feature)\n\
               status IDENTITY                             Connect to an existing server (client feature)\n\
               --version | --build-info | --check | --help\n\n\
             IDENTITY must be {IDENTITY}, this executable's compiled application identity.\n\
             Service controls use the owning account's authority without pairing or a credential store.\n\
             Stop, Restart and Uninstall show their effects and exit 2 without --confirm.\n\
             To authorize the action, put --confirm after IDENTITY. They end Terminal Sessions\n\
             for every client and keep Workspaces and verified server installations.\n\
             Linux service-enable-linger authorizes only account lingering; SpaceTerm never runs as root.\n\
             Install or Start explicitly after authorization. Status and setup-state preserve Stop."
        );
        return Ok(ExitCode::SUCCESS);
    }
    if args == ["--version"] {
        println!("SpaceTerm Server {}", env!("SPACETERM_SERVER_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }
    if args == ["--build-info"] {
        println!(
            "{}",
            serde_json::json!({
                "schema": 1,
                "identity": IDENTITY,
                "version": env!("SPACETERM_SERVER_VERSION"),
                "commit": env!("SPACETERM_SERVER_COMMIT"),
                "target": env!("SPACETERM_SERVER_TARGET"),
            })
        );
        return Ok(ExitCode::SUCCESS);
    }
    if args == ["--check"] {
        check::run()?;
        return Ok(ExitCode::SUCCESS);
    }
    let request = Request::parse(&args)?;
    match request {
        Request::ServiceServe => {
            check::resources()?;
            spaceterm_server::service::run(
                IDENTITY,
                spaceterm::runtime::SharedWorkspaceHost::account_service()?,
            )?;
        }
        Request::Serve => {
            check::resources()?;
            spaceterm_server::run_local_with_panes(
                IDENTITY,
                spaceterm::runtime::SharedWorkspaceHost::local()?,
            )?;
        }
        Request::Install(installation) => {
            let service = Service::for_identity(IDENTITY)?;
            let status = service.install(installation)?;
            print_status(Controller::new(service).display_name(), status);
        }
        Request::Service(request) => {
            let controller = Controller::new(Service::for_identity(IDENTITY)?);
            let status = match request {
                ServiceRequest::Status => controller.status()?,
                ServiceRequest::Start => controller.start()?,
                ServiceRequest::EnableLinger => {
                    controller.enable_linger(LingerAuthorization::Terminal)?;
                    println!(
                        "Boot startup authorized for this account. Install or Start the server explicitly."
                    );
                    return Ok(ExitCode::SUCCESS);
                }
                ServiceRequest::Destructive { action, confirmed } => {
                    let confirmation = controller.prepare(action)?;
                    if !confirmed {
                        println!(
                            "{}\n{}\n{}\nNo changes made. Run again with --confirm to proceed.",
                            confirmation.title(),
                            confirmation.message(),
                            confirmation.detail()
                        );
                        return Ok(ExitCode::from(2));
                    }
                    controller.confirm(confirmation)?
                }
            };
            print_status(controller.display_name(), status);
        }
        Request::SetupState | Request::Status => {
            #[cfg(feature = "client")]
            {
                let client = spaceterm_server::LocalServer::for_identity(IDENTITY)?;
                if matches!(request, Request::SetupState) {
                    client.setup()?;
                    println!(
                        "Local server state is set up. Startup and Stop intent are unchanged."
                    );
                } else {
                    let status = client.refresh()?;
                    println!(
                        "Connected to the local server; protocol {}.",
                        status.protocol_major
                    );
                }
            }
            #[cfg(not(feature = "client"))]
            return Err(ServerError::Protocol);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn print_status(name: &str, status: ServiceStatus) {
    let phase = if !status.installed {
        "Not installed."
    } else if status.running {
        "Running."
    } else if status.stopped {
        "Stopped. It stays stopped until you run service-start."
    } else {
        "Not running."
    };
    println!("{name} server: {phase}");
    println!(
        "Installed: {}; enabled: {}; running: {}; stopped: {}.",
        status.installed, status.enabled, status.running, status.stopped
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_execution_requires_a_trailing_confirmation_flag() {
        for (command, expected_action) in [
            ("service-stop", Action::Stop),
            ("service-restart", Action::Restart),
            ("service-uninstall", Action::Uninstall),
        ] {
            let preview = [command, IDENTITY].map(OsString::from);
            assert!(matches!(
                Request::parse(&preview),
                Ok(Request::Service(ServiceRequest::Destructive { action, confirmed: false }))
                    if action == expected_action
            ));
            let confirmed = [command, IDENTITY, "--confirm"].map(OsString::from);
            assert!(matches!(
                Request::parse(&confirmed),
                Ok(Request::Service(ServiceRequest::Destructive { action, confirmed: true }))
                    if action == expected_action
            ));
            for malformed in [
                vec![command, "--confirm", IDENTITY],
                vec!["--confirm", command, IDENTITY],
                vec![command, IDENTITY, "--yes"],
                vec![command, IDENTITY, "--confirm", "--confirm"],
                vec![command, IDENTITY, "--confirm", "unexpected"],
            ] {
                let malformed: Vec<_> = malformed.into_iter().map(OsString::from).collect();
                assert!(matches!(
                    Request::parse(&malformed),
                    Err(ServerError::Protocol)
                ));
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
            assert!(matches!(Request::parse(&args), Err(ServerError::Protocol)));
        }
    }
}
