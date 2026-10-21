mod check;

use spaceterm_server::{
    ServerError,
    service::{
        LingerAuthorization, RetainedInstallation, Service,
        control::{cli, display_name},
        maintenance::{self, RemoteBuild, Reply},
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
    SshDiscover,
    SshConnect,
    SshPrepare,
    SshInstall {
        archive: &'a Path,
        signature: &'a Path,
        digest: &'a str,
    },
    SshStage {
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
            (Some("service-serve"), []) => Ok(Self::ServiceServe),
            (Some("setup-state"), []) => Ok(Self::SetupState),
            (Some("status"), []) => Ok(Self::Status),
            (Some("ssh-discover"), []) => Ok(Self::SshDiscover),
            (Some("ssh-connect"), []) => Ok(Self::SshConnect),
            (Some("ssh-prepare"), []) => Ok(Self::SshPrepare),
            (Some("ssh-enable-linger"), []) => Ok(Self::SshEnableLinger(
                LingerAuthorization::RemoteNoninteractive,
            )),
            (Some("ssh-enable-linger-password"), []) => {
                Ok(Self::SshEnableLinger(LingerAuthorization::RemoteStdin))
            }
            (Some(command @ ("ssh-install" | "ssh-stage")), [archive, signature, digest])
                if !archive.as_encoded_bytes().starts_with(b"-")
                    && !signature.as_encoded_bytes().starts_with(b"-") =>
            {
                let archive = Path::new(archive);
                let signature = Path::new(signature);
                let digest = digest.to_str().ok_or(ServerError::Protocol)?;
                Ok(if command == "ssh-stage" {
                    Self::SshStage {
                        archive,
                        signature,
                        digest,
                    }
                } else {
                    Self::SshInstall {
                        archive,
                        signature,
                        digest,
                    }
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
    service.provision_candidate(archive, signature, digest, 1, &["workspace_policy"])
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
               setup-state IDENTITY                        Set up nonsecret state (client feature)\n\
               status IDENTITY                             Connect to an existing server (client feature)\n\
               ssh-discover IDENTITY                       Discover existing account state\n\
               ssh-connect IDENTITY                        Carry protocol over non-PTY stdio\n\
               ssh-prepare IDENTITY                        Start approved service, preserving Stop\n\
               ssh-install IDENTITY ARCHIVE SIGNATURE SHA256\n\
               ssh-stage IDENTITY ARCHIVE SIGNATURE SHA256 Retain a verified candidate only\n\
               ssh-maintain IDENTITY                       Answer one maintenance request over stdio\n\
               ssh-enable-linger IDENTITY                  Try account boot authorization without prompting\n\
               ssh-enable-linger-password IDENTITY         Authorize account boot using standard input\n\
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
        println!(
            "{}",
            serde_json::json!({
                "schema": 1,
                "identity": IDENTITY,
                "version": env!("SPACETERM_SERVER_VERSION"),
                "commit": env!("SPACETERM_SERVER_COMMIT"),
                "target": env!("SPACETERM_SERVER_TARGET"),
                "protocol_major": 1,
                "capabilities": ["workspace_policy"],
            })
        );
        return Ok(ExitCode::SUCCESS);
    }
    if args == ["--check"] {
        check::run()?;
        return Ok(ExitCode::SUCCESS);
    }
    if args
        .first()
        .is_some_and(|command| command == "ssh-maintain")
    {
        return maintenance::run(
            IDENTITY,
            &args,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        )
        .map(ExitCode::from);
    }
    if args.first().is_some_and(|command| {
        command.as_encoded_bytes().starts_with(b"service-")
            && command != "service-install"
            && command != "service-serve"
    }) {
        return cli::run(IDENTITY, &args, &mut std::io::stdout().lock()).map(ExitCode::from);
    }
    let request = Request::parse(&args)?;
    match request {
        Request::SshDiscover => {
            let discovery = spaceterm_server::account_bridge::discover(IDENTITY)?;
            let mut value = serde_json::to_value(discovery).map_err(|_| ServerError::Protocol)?;
            value["target"] = env!("SPACETERM_SERVER_TARGET").into();
            value["version"] = env!("SPACETERM_SERVER_VERSION").into();
            value["commit"] = env!("SPACETERM_SERVER_COMMIT").into();
            println!("{value}");
        }
        Request::SshConnect => spaceterm_server::account_bridge::run(IDENTITY)?,
        Request::SshPrepare => {
            Service::for_identity(IDENTITY)?.connect()?;
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
        Request::SshStage {
            archive,
            signature,
            digest,
        } => {
            // Staging retains the verified candidate only. It installs, activates and starts
            // nothing, so the approved runtime and its Terminal Sessions continue.
            let candidate = stage(
                &Service::for_identity(IDENTITY)?,
                archive,
                signature,
                digest,
            )?;
            let reply = Reply::Staged {
                candidate: RemoteBuild::describe(&candidate)?,
            };
            println!(
                "{}",
                serde_json::to_string(&reply).map_err(|_| ServerError::Protocol)?
            );
        }
        Request::SshEnableLinger(authorization) => {
            Service::for_identity(IDENTITY)?.enable_linger(authorization)?
        }
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
            cli::write_status(
                &mut std::io::stdout().lock(),
                display_name(IDENTITY),
                status,
            )?;
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
            return Err(ServerError::Protocol.into());
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn staging_and_maintenance_require_the_compiled_identity_and_their_exact_arguments() {
        let args = ["ssh-stage", IDENTITY, "/archive", "/signature", "sha256"].map(OsString::from);
        assert!(matches!(
            Request::parse(&args),
            Ok(Request::SshStage { digest, .. }) if digest == "sha256"
        ));
        let args =
            ["ssh-install", IDENTITY, "/archive", "/signature", "sha256"].map(OsString::from);
        assert!(matches!(
            Request::parse(&args),
            Ok(Request::SshInstall { digest, .. }) if digest == "sha256"
        ));
        for malformed in [
            vec!["ssh-stage", IDENTITY],
            vec![
                "ssh-stage",
                "foreign-identity",
                "/archive",
                "/signature",
                "sha256",
            ],
            vec!["ssh-stage", IDENTITY, "--archive", "/signature", "sha256"],
            vec![
                "ssh-stage",
                IDENTITY,
                "/archive",
                "/signature",
                "sha256",
                "extra",
            ],
            vec!["ssh-maintain", IDENTITY, "--confirm"],
            vec!["ssh-maintain", "foreign-identity"],
        ] {
            let malformed: Vec<_> = malformed.into_iter().map(OsString::from).collect();
            assert!(matches!(
                Request::parse(&malformed),
                Err(ServerError::Protocol)
            ));
        }
    }

    #[test]
    fn a_maintenance_request_for_another_identity_is_refused_as_one_reply() {
        let args = ["ssh-maintain", "foreign-identity"].map(OsString::from);
        let mut output = Vec::new();
        assert_eq!(
            maintenance::run(IDENTITY, &args, &mut &b"{}"[..], &mut output),
            Ok(0)
        );
        assert_eq!(
            serde_json::from_slice::<Reply>(&output).unwrap(),
            Reply::Refused(spaceterm_server::service::maintenance::MaintenanceRefusal::Protocol)
        );
    }
}
