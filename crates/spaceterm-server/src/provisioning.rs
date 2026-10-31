//! Temporary account-authorized server provisioning.
use crate::direct::{
    DirectError, DirectStatus, Management, ManagementReply, NetworkPolicy, PairedDevice, ServerPin,
};
use crate::pane::control::ClientName;
use crate::{ServerError, host, protocol, storage::Directory};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One exact enrollment authorized by the owning account during Add Server.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    /// Encoded big-endian in operation[..8]; the remaining 24 bytes are client randomness.
    pub generation: u64,
    pub pin: ServerPin,
    pub expected_policy: Option<NetworkPolicy>,
    pub expected_enabled: bool,
    pub endpoint: String,
    pub operation: [u8; 32],
    pub public_key: [u8; 32],
    pub name: ClientName,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupCancellation {
    pub pin: ServerPin,
    pub operation: [u8; 32],
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupCancellationReply {
    pub cancelled: bool,
    pub pin: ServerPin,
    pub operation: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupReply {
    pub schema: u8,
    pub endpoint: String,
    pub paired: PairedDevice,
    pub server_name: String,
}

/// Observe the running server's pin before retaining a pending enrollment on the client.
pub fn direct_status(identity: &str) -> Result<DirectStatus, ServerError> {
    match manage(identity, SETUP, Management::Status)? {
        ManagementReply::Status(status) => Ok(status),
        _ => Err(ServerError::Protocol),
    }
}

/// Reserve one exact operation before allowing enrollment. Cancellation permanently fences it.
pub fn prepare_direct(
    identity: &str,
    policy: NetworkPolicy,
    request: SetupRequest,
) -> Result<(), ServerError> {
    match manage(
        identity,
        SETUP,
        Management::PrepareSetup {
            generation: request.generation,
            pin: request.pin,
            expected_policy: request.expected_policy,
            expected_enabled: request.expected_enabled,
            endpoint: request.endpoint,
            policy,
            operation: request.operation,
            public_key: request.public_key,
            name: request.name,
        },
    )? {
        ManagementReply::SetupPrepared => Ok(()),
        _ => Err(ServerError::Protocol),
    }
}

/// Authorize an exact Network Access policy and client public key through owning-account IPC.
/// The server atomically binds the operation to the policy, key, name and assigned identities.
pub fn setup_direct(
    identity: &str,
    policy: NetworkPolicy,
    request: SetupRequest,
) -> Result<SetupReply, ServerError> {
    let endpoint = crate::direct::Endpoint::from_url(&request.endpoint)?.url();
    let paired = match manage(
        identity,
        SETUP,
        Management::Setup {
            generation: request.generation,
            pin: request.pin,
            expected_policy: request.expected_policy,
            expected_enabled: request.expected_enabled,
            endpoint: request.endpoint,
            policy,
            operation: request.operation,
            public_key: request.public_key,
            name: request.name,
        },
    )? {
        ManagementReply::Setup(paired) => paired,
        _ => return Err(ServerError::Protocol),
    };
    let status = direct_status(identity)?;
    if status.pin != paired.pin {
        return Err(DirectError::PinMismatch.into());
    }
    Ok(SetupReply {
        schema: 1,
        endpoint,
        paired,
        server_name: status.server_name,
    })
}

/// Revoke only the pairing allocated by this setup operation, including after a lost reply.
pub fn cancel_direct(
    identity: &str,
    request: SetupCancellation,
) -> Result<SetupCancellationReply, ServerError> {
    match manage(
        identity,
        SETUP,
        Management::CancelSetup {
            pin: request.pin.clone(),
            operation: request.operation,
        },
    )? {
        ManagementReply::SetupCancelled { pin, operation }
            if pin == request.pin && operation == request.operation =>
        {
            Ok(SetupCancellationReply {
                cancelled: true,
                pin,
                operation,
            })
        }
        _ => Err(ServerError::Protocol),
    }
}

/// The client name setup connects under.
const SETUP: &str = "Server setup";

fn manage(identity: &str, name: &str, request: Management) -> Result<ManagementReply, ServerError> {
    let paths = host::Paths::for_identity(identity)?;
    // Remote setup observes this reply for 15 s, so it never waits out a long operation.
    crate::service::check_admission(&paths, crate::service::OPERATION_GRACE, None)?;
    let server = Directory::open(&paths.data, false)?.read()?.server;
    let client = protocol::random()?;
    let mut stream = host::connect(&paths)?;
    protocol::admit_client(&mut stream, (&client, &name.into()), &server, identity)?;
    let link = host::link(stream, Arc::new(|_| {}))?;
    let reply = link
        .request(crate::protocol::Operation::Direct(request))?
        .wait();
    link.close();
    match reply? {
        crate::protocol::ReplyBody::Direct(reply) => reply.map_err(Into::into),
        _ => Err(ServerError::Protocol),
    }
}

/// Bounded account discovery. The helper declares its protocol; final admission selects the
/// protocol major with the running server before any operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountDiscovery {
    pub schema: u32,
    pub account: u32,
    pub ssh_local_address: Option<std::net::IpAddr>,
    pub build: String,
    pub server: Option<[u8; 32]>,
    pub installed: bool,
    pub enabled: bool,
    pub running: bool,
    pub stopped: bool,
    pub start_observation: Option<crate::service::SetupStartObservation>,
    pub protocol_majors: Vec<u16>,
    pub target: String,
    pub version: String,
    pub commit: String,
}

/// Discover the account's existing state without provisioning or starting a server.
pub fn discover(identity: &str) -> Result<AccountDiscovery, ServerError> {
    let paths = host::Paths::for_identity(identity)?;
    let server = match Directory::open(&paths.data, false).and_then(|d| d.read()) {
        Ok(record) => Some(record.server),
        Err(ServerError::NotSetUp) => None,
        Err(error) => return Err(error),
    };
    let service = crate::service::Service::for_identity(identity)?;
    let status = service.passive_status()?;
    let start_observation = service.setup_start_observation()?;
    Ok(AccountDiscovery {
        schema: 1,
        // SAFETY: geteuid has no preconditions.
        account: unsafe { libc::geteuid() },
        ssh_local_address: std::env::var("SSH_CONNECTION")
            .ok()
            .and_then(|value| ssh_local_address(&value)),
        build: identity.into(),
        server,
        installed: status.installed,
        enabled: status.enabled,
        running: status.running,
        stopped: status.stopped,
        start_observation,
        protocol_majors: vec![protocol::MAJOR],
        target: String::new(),
        version: String::new(),
        commit: String::new(),
    })
}

fn ssh_local_address(connection: &str) -> Option<std::net::IpAddr> {
    if connection.len() > 256 {
        return None;
    }
    let fields: Vec<_> = connection.split_ascii_whitespace().collect();
    let [client, client_port, server, server_port] = fields.as_slice() else {
        return None;
    };
    client.parse::<std::net::IpAddr>().ok()?;
    if client_port.parse::<u16>().ok()? == 0 || server_port.parse::<u16>().ok()? == 0 {
        return None;
    }
    server.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ssh_socket_metadata_accepts_only_the_complete_connection_tuple() {
        assert_eq!(
            ssh_local_address("192.0.2.2 50312 192.0.2.8 22"),
            Some("192.0.2.8".parse().unwrap())
        );
        for value in [
            "192.0.2.8",
            "host 50312 192.0.2.8 22",
            "192.0.2.2 0 192.0.2.8 22",
            "192.0.2.2 50312 host 22",
            "192.0.2.2 50312 192.0.2.8 22 extra",
        ] {
            assert_eq!(ssh_local_address(value), None);
        }
    }
}
