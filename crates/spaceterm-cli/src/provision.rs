//! Installer, runtime-verifier and temporary SSH Setup steps. No public command grammar lives here.
use spaceterm_cli::{
    args::ProvisionCommand,
    contract::{self, Code, Failure},
};
use spaceterm_server::{
    ServerError,
    service::{LingerAuthorization, Service},
};
use std::io::{Read, Write};

const IDENTITY: &str = env!("SPACETERM_CLI_IDENTITY");

pub enum Error {
    Failure(Failure),
    Output(std::io::Error),
}
impl From<ServerError> for Error {
    fn from(error: ServerError) -> Self {
        Self::Failure(contract::from_server_error(error))
    }
}

fn setup_pipe() -> Result<(), Error> {
    // SAFETY: isatty only observes the process's standard descriptors.
    if unsafe { libc::isatty(libc::STDIN_FILENO) != 0 || libc::isatty(libc::STDOUT_FILENO) != 0 } {
        return Err(Error::Failure(Failure::new(Code::InvalidArguments)));
    }
    Ok(())
}
fn read_setup<T: serde::de::DeserializeOwned>() -> Result<T, Error> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| ServerError::Protocol)?;
    if bytes.len() > 4096 {
        return Err(ServerError::Protocol.into());
    }
    serde_json::from_slice(&bytes).map_err(|_| ServerError::Protocol.into())
}
fn write_setup(value: &impl serde::Serialize) -> Result<(), Error> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, value)
        .map_err(std::io::Error::from)
        .map_err(Error::Output)?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(Error::Output)
}

pub fn run(command: &ProvisionCommand) -> Result<(), Error> {
    match command {
        ProvisionCommand::BuildInfo => {
            std::io::stdout()
                .lock()
                .write_all(include_bytes!(concat!(env!("OUT_DIR"), "/build-info.json")))
                .map_err(Error::Output)?;
        }
        ProvisionCommand::PermissionReport => {
            let report = spaceterm_server::permissions::probe_report()
                .ok_or(Error::Failure(Failure::new(Code::Unsupported)))?;
            std::io::stdout()
                .lock()
                .write_all(report.as_bytes())
                .map_err(Error::Output)?;
        }
        ProvisionCommand::PermissionRequest { permission } => {
            spaceterm_server::permissions::request_in_process(permission.permission())
                .ok_or(Error::Failure(Failure::new(Code::Unsupported)))?
                .map_err(|failure| Error::Failure(contract::from_lane_code(failure.code())))?;
        }
        ProvisionCommand::Check => {
            crate::check::run()?;
            std::io::stdout()
                .lock()
                .write_all(b"Server checks passed.\n")
                .map_err(Error::Output)?;
        }
        ProvisionCommand::SshDiscover => {
            let discovery = spaceterm_server::provisioning::discover(IDENTITY)?;
            let mut value = serde_json::to_value(discovery).map_err(|_| ServerError::Protocol)?;
            value["target"] = env!("SPACETERM_CLI_TARGET").into();
            value["version"] = env!("SPACETERM_CLI_VERSION").into();
            value["commit"] = env!("SPACETERM_CLI_COMMIT").into();
            write_setup(&value)?;
        }
        ProvisionCommand::SshDirectStatus => {
            setup_pipe()?;
            write_setup(&spaceterm_server::provisioning::direct_status(IDENTITY)?)?;
        }
        ProvisionCommand::SshDirect { address } => {
            setup_pipe()?;
            let request = read_setup::<spaceterm_server::provisioning::SetupRequest>()?;
            let policy = spaceterm_server::direct::NetworkPolicy::parse(address)
                .map_err(ServerError::from)?;
            write_setup(&spaceterm_server::provisioning::setup_direct(
                IDENTITY, policy, request,
            )?)?;
        }
        ProvisionCommand::SshDirectPrepare { address } => {
            setup_pipe()?;
            let request = read_setup::<spaceterm_server::provisioning::SetupRequest>()?;
            let policy = spaceterm_server::direct::NetworkPolicy::parse(address)
                .map_err(ServerError::from)?;
            spaceterm_server::provisioning::prepare_direct(IDENTITY, policy, request)?;
            write_setup(&serde_json::json!({"prepared":true}))?;
        }
        ProvisionCommand::SshDirectCancel => {
            setup_pipe()?;
            let request = read_setup::<spaceterm_server::provisioning::SetupCancellation>()?;
            write_setup(&spaceterm_server::provisioning::cancel_direct(
                IDENTITY, request,
            )?)?;
        }
        ProvisionCommand::SshStart => {
            setup_pipe()?;
            let approved = read_setup::<spaceterm_server::service::SetupStartObservation>()?;
            write_setup(&Service::for_identity(IDENTITY)?.start_for_setup(&approved)?)?;
        }
        ProvisionCommand::SshInstall {
            archive,
            signature,
            sha256,
        } => {
            let service = Service::for_identity(IDENTITY)?;
            let candidate = service.provision_candidate(
                archive,
                signature,
                sha256,
                spaceterm_server::protocol::MAJOR,
            )?;
            service.install(&candidate.directory)?;
        }
        ProvisionCommand::SshEnableLinger { password_stdin } => {
            Service::for_identity(IDENTITY)?.enable_linger(if *password_stdin {
                LingerAuthorization::RemoteStdin
            } else {
                LingerAuthorization::RemoteNoninteractive
            })?;
        }
        ProvisionCommand::ServiceRun {
            operation,
            identity,
        } => {
            if identity != IDENTITY {
                return Err(Error::Failure(Failure::new(Code::InvalidArguments)));
            }
            let bytes = decode_id(operation)
                .ok_or_else(|| Error::Failure(Failure::new(Code::InvalidArguments)))?;
            spaceterm_server::service::helper::run(
                IDENTITY,
                &spaceterm_server::service::operations::OperationId(bytes),
            )?;
        }
        // Retention and uninstall use the envelope contract in main.rs.
        ProvisionCommand::Retain { .. } | ProvisionCommand::Uninstall { .. } => unreachable!(),
    }
    Ok(())
}

fn decode_id(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32 {
        return None;
    }
    let mut result = [0; 16];
    for (part, byte) in value.as_bytes().as_chunks::<2>().0.iter().zip(&mut result) {
        *byte = u8::from_str_radix(std::str::from_utf8(part).ok()?, 16).ok()?;
    }
    Some(result)
}
