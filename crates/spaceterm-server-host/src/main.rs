mod check;

use spaceterm_server::{
    ServerError,
    service::{
        Service,
        control::{cli, display_name},
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
            (Some("service-install"), [installation])
                if !installation.as_encoded_bytes().starts_with(b"-") =>
            {
                Ok(Self::Install(Path::new(installation)))
            }
            _ => Err(ServerError::Protocol),
        }
    }
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
            })
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
}
