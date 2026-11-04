//! Temporary account-authorized server provisioning.
use crate::direct::{
    DirectError, DirectStatus, MachinePin, Management, ManagementReply, NetworkPolicy, PairedDevice,
};
use crate::pane::control::ClientName;
use crate::{CoreError, host, protocol, storage::Directory};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub mod preflight;

/// Why Add Machine refuses a computer. Each is one sentence in the desktop. Every refusal but
/// `UdpBlocked` comes before anything is uploaded. SSH preflight reports the first that applies,
/// in declaration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "refusal", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetupRefusal {
    /// A Mac with an Intel processor. SpaceTerm Core runs on Apple silicon only.
    IntelMac,
    /// Linux on an ARM processor. SpaceTerm Core runs on x86_64 Linux only.
    ArmLinux,
    /// Neither macOS nor Linux.
    OtherSystem,
    /// macOS older than [`MINIMUM_MACOS`]. `found` is its (major, minor), `None` when unknown.
    MacosTooOld { found: Option<(u16, u16)> },
    /// glibc older than [`MINIMUM_GLIBC`]. `found` is its (major, minor), `None` when the
    /// computer has no glibc.
    GlibcTooOld { found: Option<(u16, u16)> },
    /// The SSH account is root. SpaceTerm Core runs under an ordinary account.
    RootAccount,
    /// Linux with no systemd user manager for the account.
    NoUserManager,
    /// A Mac with nobody logged in, so there is no Aqua session to start SpaceTerm Core in.
    /// `filevault` is true when FileVault is on. Automatic login is then unavailable, so the
    /// desktop suggests it only when this is false.
    NobodyLoggedIn { filevault: bool },
    /// The user declined to let SpaceTerm Core keep running after logout on a remote Linux
    /// machine. Remote Linux requires linger.
    LingerDeclined,
    /// Network Access was enabled but no QUIC packet reached SpaceTerm Core at `port`. Setup
    /// turned Network Access off again. Try Again turns it on.
    UdpBlocked { port: u16 },
}

/// The oldest macOS release SpaceTerm Core runs on, as (major, minor).
pub const MINIMUM_MACOS: (u16, u16) = (26, 0);
/// The oldest glibc SpaceTerm Core runs on, as (major, minor).
pub const MINIMUM_GLIBC: (u16, u16) = (2, 35);

/// One exact enrollment authorized by the owning account during Add Machine.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    /// Encoded big-endian in operation[..8]; the remaining 24 bytes are client randomness.
    pub generation: u64,
    pub pin: MachinePin,
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
    pub pin: MachinePin,
    pub operation: [u8; 32],
    /// The Network Access to restore after the cancellation, when setup changed it. Add Machine
    /// restores it when the enrolled device could not reach SpaceTerm Core over UDP.
    pub restore: Option<NetworkRestore>,
}

/// Network Access as it was before setup changed it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRestore {
    pub policy: Option<NetworkPolicy>,
    pub enabled: bool,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupCancellationReply {
    pub cancelled: bool,
    pub pin: MachinePin,
    pub operation: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupReply {
    pub schema: u8,
    pub endpoint: String,
    pub paired: PairedDevice,
    pub machine_name: String,
}

/// Observe the running server's pin before retaining a pending enrollment on the client.
pub fn direct_status(identity: &str) -> Result<DirectStatus, CoreError> {
    match manage(identity, SETUP, Management::Status)? {
        ManagementReply::Status(status) => Ok(status),
        _ => Err(CoreError::Protocol),
    }
}

/// Reserve one exact operation before allowing enrollment. Cancellation permanently fences it.
pub fn prepare_direct(
    identity: &str,
    policy: NetworkPolicy,
    request: SetupRequest,
) -> Result<(), CoreError> {
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
        _ => Err(CoreError::Protocol),
    }
}

/// Authorize an exact Network Access policy and client public key through owning-account IPC.
/// The server atomically binds the operation to the policy, key, name and assigned identities.
pub fn setup_direct(
    identity: &str,
    policy: NetworkPolicy,
    request: SetupRequest,
) -> Result<SetupReply, CoreError> {
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
        _ => return Err(CoreError::Protocol),
    };
    let status = direct_status(identity)?;
    if status.pin != paired.pin {
        return Err(DirectError::PinMismatch.into());
    }
    Ok(SetupReply {
        schema: 1,
        endpoint,
        paired,
        machine_name: status.machine_name,
    })
}

/// Revoke only the pairing allocated by this setup operation, including after a lost reply, then
/// restore Network Access when the request names how it was.
pub fn cancel_direct(
    identity: &str,
    request: SetupCancellation,
) -> Result<SetupCancellationReply, CoreError> {
    let reply = cancel_setup(identity, &request)?;
    if let Some(restore) = request.restore {
        // An earlier policy is kept as the configured one even while access is off.
        match manage(
            identity,
            SETUP,
            Management::SetPolicy {
                policy: restore.policy,
                enabled: restore.enabled,
            },
        )? {
            ManagementReply::Status(status) if status.pin == request.pin => {}
            ManagementReply::Status(_) => return Err(DirectError::PinMismatch.into()),
            _ => return Err(CoreError::Protocol),
        }
    }
    Ok(reply)
}

fn cancel_setup(
    identity: &str,
    request: &SetupCancellation,
) -> Result<SetupCancellationReply, CoreError> {
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
        _ => Err(CoreError::Protocol),
    }
}

/// The client name setup connects under.
const SETUP: &str = "Machine setup";

fn manage(identity: &str, name: &str, request: Management) -> Result<ManagementReply, CoreError> {
    let paths = host::Paths::for_identity(identity)?;
    // Remote setup observes this reply for 15 s, so it never waits out a long operation.
    crate::service::check_admission(&paths, crate::service::OPERATION_GRACE, None)?;
    let core = Directory::open(&paths.data, false)?.read()?.core;
    let client = protocol::random()?;
    let mut stream = host::connect(&paths)?;
    protocol::admit_client(&mut stream, (&client, &name.into()), &core, identity)?;
    let link = host::link(stream, Arc::new(|_| {}))?;
    let reply = link
        .request(crate::protocol::Operation::Direct(request))?
        .wait();
    link.close();
    match reply? {
        crate::protocol::ReplyBody::Direct(reply) => reply.map_err(Into::into),
        _ => Err(CoreError::Protocol),
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
    pub core: Option<[u8; 32]>,
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
pub fn discover(identity: &str) -> Result<AccountDiscovery, CoreError> {
    let paths = host::Paths::for_identity(identity)?;
    let core = match Directory::open(&paths.data, false).and_then(|d| d.read()) {
        Ok(record) => Some(record.core),
        Err(CoreError::NotSetUp) => None,
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
        core,
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
    let [client, client_port, core, core_port] = fields.as_slice() else {
        return None;
    };
    client.parse::<std::net::IpAddr>().ok()?;
    if client_port.parse::<u16>().ok()? == 0 || core_port.parse::<u16>().ok()? == 0 {
        return None;
    }
    core.parse().ok()
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
