//! Owning-account service commands shared by headless and desktop executables.

use super::{Action, Controller, UpdateChange, UpdateWording};
use crate::direct::{DEFAULT_PORT, DirectError, DirectStatus, ListenerState, NetworkPolicy};
use crate::{
    ServerError,
    service::{
        LingerAuthorization, RetainedInstallation, Service, ServiceStatus,
        activation::{ActivationPhase, ActivationStatus},
    },
};
use std::{
    ffi::OsString,
    io::Write,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

/// Bounded service-command failures, including output that could not be written.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("Invalid arguments. See --help.")]
    InvalidArguments,
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error("Could not write service command output.")]
    Output,
    #[error("That address isn't on this computer.")]
    AddressNotHere,
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
    /// Shows network access, or changes it.
    NetworkAccess(Option<Exposure>),
}

/// Where paired devices may connect from, as a service command sets it.
#[derive(Debug, Eq, PartialEq)]
enum Exposure {
    Off,
    /// An address of this computer, or the unspecified address for every interface. Without a
    /// port the port saved before stays.
    At {
        ip: IpAddr,
        port: Option<u16>,
    },
}

impl Exposure {
    fn parse(value: &OsString) -> Result<Self, ServerError> {
        let value = value.to_str().ok_or(ServerError::Protocol)?;
        match value {
            "off" => Ok(Self::Off),
            "all" => Ok(Self::At {
                ip: std::net::Ipv4Addr::UNSPECIFIED.into(),
                port: None,
            }),
            _ => {
                if let Ok(address) = value.parse::<SocketAddr>() {
                    NetworkPolicy::new(address)?;
                    return Ok(Self::At {
                        ip: address.ip(),
                        port: Some(address.port()),
                    });
                }
                let ip = value.parse::<IpAddr>().map_err(|_| ServerError::Protocol)?;
                NetworkPolicy::new(SocketAddr::new(ip, DEFAULT_PORT))?;
                Ok(Self::At { ip, port: None })
            }
        }
    }
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
            (Some("service-network-access"), []) => Ok(Self::NetworkAccess(None)),
            (Some("service-network-access"), [value]) => {
                Ok(Self::NetworkAccess(Some(Exposure::parse(value)?)))
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
        Request::NetworkAccess(change) => {
            let status = match change {
                None => crate::provisioning::direct_status(identity)?,
                Some(Exposure::Off) => {
                    crate::provisioning::set_network_access(identity, None, false)?
                }
                Some(Exposure::At { ip, port }) => {
                    let port = match port {
                        Some(port) => port,
                        None => crate::provisioning::direct_status(identity)?
                            .policy
                            .map_or(DEFAULT_PORT, |policy| policy.bind.port()),
                    };
                    let policy =
                        NetworkPolicy::new(SocketAddr::new(ip, port)).map_err(ServerError::from)?;
                    crate::provisioning::set_network_access(identity, Some(policy), true).map_err(
                        |error| match error {
                            ServerError::Direct(DirectError::InvalidAddress) => {
                                Error::AddressNotHere
                            }
                            error => error.into(),
                        },
                    )?
                }
            };
            return write_network_access(output, controller.display_name(), &status);
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

/// Writes where the server listens for paired devices. Returns 1 when it can't listen at the
/// address it was given.
fn write_network_access(
    output: &mut impl Write,
    name: &str,
    status: &DirectStatus,
) -> Result<u8, Error> {
    let at = |address: SocketAddr| {
        if address.ip().is_unspecified() {
            format!("all interfaces, port {}", address.port())
        } else {
            address.to_string()
        }
    };
    let configured = status.policy.as_ref().map(|policy| policy.bind);
    let (message, code) = match (&status.listener, configured) {
        (ListenerState::Listening { addr }, _) => (format!("Listening on {}.", at(*addr)), 0),
        (ListenerState::Starting { addr }, _) => (
            format!(
                "Listening on {} once devices connected at the previous address disconnect.",
                at(*addr)
            ),
            0,
        ),
        (ListenerState::Failed(DirectError::InvalidAddress), Some(address)) => (
            format!("{} is no longer on this computer.", address.ip()),
            1,
        ),
        (ListenerState::Failed(_), Some(address)) => {
            (format!("Couldn't listen on {}.", at(address)), 1)
        }
        (ListenerState::Failed(_), None) => ("Couldn't listen on the network.".to_owned(), 1),
        (ListenerState::Off, _) => ("Off.".to_owned(), 0),
    };
    writeln!(output, "{name} network access: {message}").map_err(|_| Error::Output)?;
    Ok(code)
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
         service-return-update IDENTITY [--confirm SNAPSHOT]\n\
         service-network-access IDENTITY [all|IP|IP:PORT|[IPV6]:PORT|off]\n\n\
         IDENTITY is {identity}. Stop, Restart, Uninstall and Update end all Terminal Sessions. Without --confirm they print a preview and exit 2. Update and return previews print the SNAPSHOT to pass to --confirm.\n\n\
         service-network-access shows or sets where paired devices connect, and needs a running server. With all, the server listens on every interface for IPv4 and IPv6: the local network, a VPN or tailnet such as Tailscale, public networks, and any network this computer joins later. Only paired devices are admitted. With an IP, it listens on that one address of this computer; an IPv6 address with a port takes brackets, as in [fd00::5]:9231. Without a port, the port saved before stays; the default is UDP {DEFAULT_PORT}. Widening to all at the same port keeps connected devices. Any other change, and off, disconnects devices connected over the network; pairings stay."
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
    fn network_access_accepts_all_off_or_one_address_and_the_compiled_identity() {
        let unspecified: IpAddr = std::net::Ipv4Addr::UNSPECIFIED.into();
        for (value, exposure) in [
            (
                "all",
                Exposure::At {
                    ip: unspecified,
                    port: None,
                },
            ),
            ("off", Exposure::Off),
            (
                "192.168.1.206",
                Exposure::At {
                    ip: "192.168.1.206".parse().unwrap(),
                    port: None,
                },
            ),
            (
                "0.0.0.0:9300",
                Exposure::At {
                    ip: unspecified,
                    port: Some(9300),
                },
            ),
            (
                "[::]:9231",
                Exposure::At {
                    ip: std::net::Ipv6Addr::UNSPECIFIED.into(),
                    port: Some(9231),
                },
            ),
            (
                "fd00::5",
                Exposure::At {
                    ip: "fd00::5".parse().unwrap(),
                    port: None,
                },
            ),
        ] {
            let args = ["service-network-access", IDENTITY, value].map(OsString::from);
            assert_eq!(
                Request::parse(IDENTITY, &args),
                Ok(Request::NetworkAccess(Some(exposure))),
                "{value}"
            );
        }
        let args = ["service-network-access", IDENTITY].map(OsString::from);
        assert_eq!(
            Request::parse(IDENTITY, &args),
            Ok(Request::NetworkAccess(None))
        );
        for args in [
            vec!["service-network-access", IDENTITY, "on"],
            vec!["service-network-access", IDENTITY, "ALL"],
            vec!["service-network-access", IDENTITY, "192.168.1.206:0"],
            vec!["service-network-access", IDENTITY, "224.0.0.1"],
            vec!["service-network-access", IDENTITY, "server.example"],
            vec!["service-network-access", IDENTITY, "all", "--confirm"],
            vec!["service-network-access", "spaceterm", "all"],
        ] {
            let args: Vec<_> = args.into_iter().map(OsString::from).collect();
            assert_eq!(
                run(IDENTITY, &args, &mut Vec::new()),
                Err(Error::InvalidArguments)
            );
        }
    }

    #[test]
    fn network_access_reports_every_interface_one_address_and_failures() {
        let status = |policy: Option<&str>, listener: ListenerState| DirectStatus {
            setup_generation: 0,
            pin: crate::direct::ServerPin {
                build: IDENTITY.into(),
                server: [1; 32],
                public_key: [2; 32],
                certificate: [3; 32],
            },
            policy: policy.map(|bind| NetworkPolicy::new(bind.parse().unwrap()).unwrap()),
            enabled: policy.is_some(),
            server_name: "Penguin".into(),
            listener,
            network_addresses: Err(DirectError::Transport),
            abandoned_setups: Vec::new(),
        };
        let every: SocketAddr = "0.0.0.0:9231".parse().unwrap();
        let one: SocketAddr = "192.168.1.206:9231".parse().unwrap();
        for (status, expected, code) in [
            (
                status(
                    Some("0.0.0.0:9231"),
                    ListenerState::Listening { addr: every },
                ),
                "Listening on all interfaces, port 9231.",
                0,
            ),
            (
                status(
                    Some("192.168.1.206:9231"),
                    ListenerState::Listening { addr: one },
                ),
                "Listening on 192.168.1.206:9231.",
                0,
            ),
            (
                status(
                    Some("192.168.1.206:9231"),
                    ListenerState::Failed(DirectError::InvalidAddress),
                ),
                "192.168.1.206 is no longer on this computer.",
                1,
            ),
            (
                status(
                    Some("0.0.0.0:9231"),
                    ListenerState::Failed(DirectError::Transport),
                ),
                "Couldn't listen on all interfaces, port 9231.",
                1,
            ),
            (status(None, ListenerState::Off), "Off.", 0),
        ] {
            let mut output = Vec::new();
            assert_eq!(
                write_network_access(&mut output, "SpaceTerm Development", &status),
                Ok(code)
            );
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("SpaceTerm Development network access: {expected}\n")
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
