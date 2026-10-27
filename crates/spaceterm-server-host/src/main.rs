mod check;

use spaceterm_server::{
    ServerError,
    service::{
        LingerAuthorization, RetainedInstallation, Service,
        control::{cli, display_name},
    },
};
use std::{ffi::OsString, path::Path, process::ExitCode};

const IDENTITY: &str = env!("SPACETERM_SERVER_IDENTITY");
include!(concat!(env!("OUT_DIR"), "/build-info.rs"));

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
    DevelopmentServe,
    ServiceServe,
    SetupState,
    Status,
    Install(&'a Path),
    SshDiscover,
    SetupDirectStatus,
    SetupDirectPrepare(&'a str),
    SetupDirect(&'a str),
    SetupDirectCancel,
    SetupStart,
    SshInstall {
        archive: &'a Path,
        signature: &'a Path,
        digest: &'a str,
    },
    SshEnableLinger(LingerAuthorization),
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
            (Some("development-serve"), []) if IDENTITY == "spaceterm-development" => {
                Ok(Self::DevelopmentServe)
            }
            (Some("service-serve"), []) => Ok(Self::ServiceServe),
            (Some("setup-state"), []) => Ok(Self::SetupState),
            (Some("status"), []) => Ok(Self::Status),
            (Some("ssh-discover"), []) => Ok(Self::SshDiscover),
            (Some("setup-direct-status"), []) => Ok(Self::SetupDirectStatus),
            (Some("setup-direct-cancel"), []) => Ok(Self::SetupDirectCancel),
            (Some("setup-direct-prepare"), [address]) => {
                let address = address.to_str().ok_or(ServerError::Protocol)?;
                spaceterm_server::direct::NetworkPolicy::parse(address)?;
                Ok(Self::SetupDirectPrepare(address))
            }
            (Some("setup-direct"), [address]) => {
                let address = address.to_str().ok_or(ServerError::Protocol)?;
                spaceterm_server::direct::NetworkPolicy::parse(address)?;
                Ok(Self::SetupDirect(address))
            }
            (Some("setup-start"), []) => Ok(Self::SetupStart),
            (Some("ssh-enable-linger"), []) => Ok(Self::SshEnableLinger(
                LingerAuthorization::RemoteNoninteractive,
            )),
            (Some("ssh-enable-linger-password"), []) => {
                Ok(Self::SshEnableLinger(LingerAuthorization::RemoteStdin))
            }
            (Some("ssh-install"), [archive, signature, digest])
                if !archive.as_encoded_bytes().starts_with(b"-")
                    && !signature.as_encoded_bytes().starts_with(b"-") =>
            {
                let archive = Path::new(archive);
                let signature = Path::new(signature);
                let digest = digest.to_str().ok_or(ServerError::Protocol)?;
                Ok(Self::SshInstall {
                    archive,
                    signature,
                    digest,
                })
            }
            (Some("service-install"), [installation])
                if !installation.as_encoded_bytes().starts_with(b"-") =>
            {
                Ok(Self::Install(Path::new(installation)))
            }
            _ => Err(ServerError::Protocol),
        }
    }
}

/// Verify and retain one exact signed candidate for this account's installation store.
fn stage(
    service: &Service,
    archive: &Path,
    signature: &Path,
    digest: &str,
) -> Result<RetainedInstallation, ServerError> {
    service.provision_candidate(archive, signature, digest, PROTOCOL_MAJOR, CAPABILITIES)
}

fn setup_pipe() -> Result<(), ServerError> {
    // SAFETY: isatty only observes the process's standard descriptors.
    if unsafe { libc::isatty(libc::STDIN_FILENO) != 0 || libc::isatty(libc::STDOUT_FILENO) != 0 } {
        return Err(ServerError::Protocol);
    }
    Ok(())
}

fn read_setup<T: serde::de::DeserializeOwned>() -> Result<T, ServerError> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| ServerError::Protocol)?;
    if bytes.len() > 4096 {
        return Err(ServerError::Protocol);
    }
    serde_json::from_slice(&bytes).map_err(|_| ServerError::Protocol)
}

fn write_setup(value: &impl serde::Serialize) -> Result<(), ServerError> {
    use std::io::Write;
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value).map_err(|_| ServerError::Protocol)?;
    output.write_all(b"\n").map_err(|_| ServerError::Protocol)
}

fn run() -> Result<ExitCode, cli::Error> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == ["--help"] {
        println!(
            "SpaceTerm Server\n\n\
             Usage: spaceterm-server COMMAND {IDENTITY} [OPTIONS]\n\n\
             {}\n\n\
             Other commands:\n\
               service-install IDENTITY RETAINED_INSTALLATION\n\
               serve IDENTITY                              Run in the foreground\n\
               setup-state IDENTITY                        Set up server state\n\
               status IDENTITY                             Connect to the server\n\
               ssh-discover IDENTITY                       Find the server\n\
               setup-start IDENTITY                        Start the approved server from stdin\n\
               ssh-install IDENTITY ARCHIVE SIGNATURE SHA256\n\
               setup-direct-status IDENTITY                Observe enrollment trust\n\
               setup-direct-prepare IDENTITY IP:PORT       Reserve setup enrollment from stdin\n\
               setup-direct IDENTITY IP:PORT               Expose and enroll a client public key from stdin\n\
               setup-direct-cancel IDENTITY                Cancel a setup operation from stdin\n\
               ssh-enable-linger IDENTITY                  Allow boot startup\n\
               ssh-enable-linger-password IDENTITY         Allow boot startup (password on stdin)\n\
               --version | --build-info | --check | --help",
            cli::help(IDENTITY)
        );
        return Ok(ExitCode::SUCCESS);
    }
    if args == ["--version"] {
        println!("SpaceTerm Server {}", env!("SPACETERM_SERVER_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }
    if args == ["--build-info"] {
        print!(
            "{}",
            include_str!(concat!(env!("OUT_DIR"), "/build-info.json"))
        );
        return Ok(ExitCode::SUCCESS);
    }
    if args == ["--check"] {
        check::run()?;
        return Ok(ExitCode::SUCCESS);
    }
    if args.first().is_some_and(|command| {
        command.as_encoded_bytes().starts_with(b"service-")
            && command != "service-install"
            && command != "service-serve"
    }) {
        return cli::run(IDENTITY, &args, &mut std::io::stdout().lock()).map(ExitCode::from);
    }
    let request = Request::parse(&args).map_err(|_| cli::Error::InvalidArguments)?;
    match request {
        Request::SshDiscover => {
            let discovery = spaceterm_server::provisioning::discover(IDENTITY)?;
            let mut value = serde_json::to_value(discovery).map_err(|_| ServerError::Protocol)?;
            value["target"] = env!("SPACETERM_SERVER_TARGET").into();
            value["version"] = env!("SPACETERM_SERVER_VERSION").into();
            value["commit"] = env!("SPACETERM_SERVER_COMMIT").into();
            println!("{value}");
        }
        Request::SetupDirectStatus => {
            setup_pipe()?;
            write_setup(&spaceterm_server::provisioning::direct_status(IDENTITY)?)?;
        }
        Request::SetupDirect(address) => {
            setup_pipe()?;
            let request = read_setup::<spaceterm_server::provisioning::SetupRequest>()?;
            let policy = spaceterm_server::direct::NetworkPolicy::parse(address)
                .map_err(ServerError::from)?;
            write_setup(&spaceterm_server::provisioning::setup_direct(
                IDENTITY, policy, request,
            )?)?;
        }
        Request::SetupDirectPrepare(address) => {
            setup_pipe()?;
            let request = read_setup::<spaceterm_server::provisioning::SetupRequest>()?;
            let policy = spaceterm_server::direct::NetworkPolicy::parse(address)
                .map_err(ServerError::from)?;
            spaceterm_server::provisioning::prepare_direct(IDENTITY, policy, request)?;
            println!("{{\"prepared\":true}}");
        }
        Request::SetupDirectCancel => {
            setup_pipe()?;
            let request = read_setup::<spaceterm_server::provisioning::SetupCancellation>()?;
            write_setup(&spaceterm_server::provisioning::cancel_direct(
                IDENTITY, request,
            )?)?;
        }
        Request::SetupStart => {
            setup_pipe()?;
            let approved = read_setup::<spaceterm_server::service::SetupStartObservation>()?;
            write_setup(&Service::for_identity(IDENTITY)?.start_for_setup(&approved)?)?;
        }
        Request::SshInstall {
            archive,
            signature,
            digest,
        } => {
            let service = Service::for_identity(IDENTITY)?;
            let candidate = stage(&service, archive, signature, digest)?;
            service.install(&candidate.directory)?;
        }
        Request::SshEnableLinger(authorization) => {
            Service::for_identity(IDENTITY)?.enable_linger(authorization)?
        }
        Request::ServiceServe => {
            check::resources()?;
            spaceterm_server::service::run(
                IDENTITY,
                spaceterm_server::panes(|_| {
                    Ok(spaceterm::runtime::Served::new(
                        spaceterm::runtime::SharedWorkspaceHost::account_service()?,
                    ))
                }),
            )?;
        }
        Request::DevelopmentServe => {
            use std::io::Write;
            check::resources()?;
            spaceterm_server::run_owned_development(
                spaceterm_server::panes(|_| {
                    Ok(spaceterm::runtime::Served::new(
                        spaceterm::runtime::SharedWorkspaceHost::local()?,
                    ))
                }),
                std::io::stdin(),
                || {
                    let mut output = std::io::stdout().lock();
                    output
                        .write_all(b"spaceterm-development-ready-v1\n")
                        .and_then(|()| output.flush())
                        .map_err(|_| ServerError::StartupFailed)
                },
            )?;
        }
        Request::Serve => {
            check::resources()?;
            spaceterm_server::run_local_with_panes(
                IDENTITY,
                spaceterm_server::panes(|_| {
                    Ok(spaceterm::runtime::Served::new(
                        spaceterm::runtime::SharedWorkspaceHost::local()?,
                    ))
                }),
            )?;
        }
        Request::Install(installation) => {
            let service = Service::for_identity(IDENTITY)?;
            let status = service.install(installation)?;
            cli::write_status(
                &mut std::io::stdout().lock(),
                display_name(IDENTITY),
                status,
            )?;
        }
        Request::SetupState | Request::Status => {
            #[cfg(feature = "client")]
            {
                let client = spaceterm_server::ServerConnection::for_identity(IDENTITY)?;
                if matches!(request, Request::SetupState) {
                    client.setup()?;
                    println!("The local server is set up.");
                } else {
                    client.refresh()?;
                    println!("Connected to the local server.");
                }
            }
            #[cfg(not(feature = "client"))]
            return Err(ServerError::Protocol.into());
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_foreground_entry_is_restricted_to_the_compiled_development_identity() {
        for identity in ["spaceterm-development", "spaceterm-preflight", "spaceterm"] {
            let args = ["development-serve", identity].map(OsString::from);
            assert_eq!(
                matches!(Request::parse(&args), Ok(Request::DevelopmentServe)),
                identity == "spaceterm-development" && IDENTITY == "spaceterm-development",
            );
        }
        let args = ["development-serve", IDENTITY, "--confirm"].map(OsString::from);
        assert!(matches!(Request::parse(&args), Err(ServerError::Protocol)));
    }

    #[test]
    fn ssh_authorization_reads_passwords_only_after_the_explicit_password_command() {
        for (command, expected) in [
            (
                "ssh-enable-linger",
                LingerAuthorization::RemoteNoninteractive,
            ),
            (
                "ssh-enable-linger-password",
                LingerAuthorization::RemoteStdin,
            ),
        ] {
            let args = [command, IDENTITY].map(OsString::from);
            assert!(
                matches!(Request::parse(&args), Ok(Request::SshEnableLinger(actual)) if actual==expected)
            );
        }
    }

    #[test]
    fn confirmation_cannot_authorize_install_or_root_commands() {
        for command in [
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

    #[test]
    fn installation_requires_one_non_flag_path_after_the_compiled_identity() {
        let installation = "/private/retained-installation";
        let args = ["service-install", IDENTITY, installation].map(OsString::from);
        assert!(matches!(
            Request::parse(&args),
            Ok(Request::Install(path)) if path == Path::new(installation)
        ));
        for malformed in [
            vec!["service-install", IDENTITY],
            vec!["service-install", "foreign-identity", installation],
            vec!["service-install", IDENTITY, "--unknown"],
            vec!["service-install", IDENTITY, installation, "--confirm"],
            vec!["service-install", IDENTITY, installation, "extra"],
        ] {
            let args: Vec<_> = malformed.into_iter().map(OsString::from).collect();
            assert!(matches!(Request::parse(&args), Err(ServerError::Protocol)));
        }
    }

    #[test]
    fn installation_requires_the_compiled_identity_and_exact_arguments() {
        let args =
            ["ssh-install", IDENTITY, "/archive", "/signature", "sha256"].map(OsString::from);
        assert!(
            matches!(Request::parse(&args), Ok(Request::SshInstall { digest, .. }) if digest == "sha256")
        );
        for malformed in [
            vec!["ssh-install", IDENTITY],
            vec![
                "ssh-install",
                "foreign-identity",
                "/archive",
                "/signature",
                "sha256",
            ],
            vec!["ssh-install", IDENTITY, "--archive", "/signature", "sha256"],
            vec![
                "ssh-install",
                IDENTITY,
                "/archive",
                "/signature",
                "sha256",
                "extra",
            ],
        ] {
            let malformed: Vec<_> = malformed.into_iter().map(OsString::from).collect();
            assert!(matches!(
                Request::parse(&malformed),
                Err(ServerError::Protocol)
            ));
        }
    }
}
