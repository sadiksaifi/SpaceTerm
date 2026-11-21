//! Pairing identities, Network Access facts and management operations.
use crate::pane::control::ClientName;
use serde::{Deserialize, Serialize};
use sha2_const_stable::Sha256;
use std::net::{IpAddr, SocketAddr};

pub mod wire;
pub use wire::{Management, ManagementReply};

/// The UDP port a build identity's Network Access uses when no port is given: Release 9231,
/// Preflight 9232 and Development 9233. One account can then run every identity at once.
pub fn default_port(identity: &str) -> u16 {
    match identity {
        "spaceterm-preflight" => 9232,
        "spaceterm-development" => 9233,
        _ => 9231,
    }
}

pub type PairingId = [u8; 32];
pub type ClaimId = [u8; 32];
pub type OperationId = [u8; 32];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachinePin {
    pub build: String,
    pub core: [u8; 32],
    /// Raw Ed25519 enrollment/authentication public key.
    pub public_key: [u8; 32],
    /// SHA-256 of the server's exact retained TLS certificate.
    pub certificate: [u8; 32],
}
impl MachinePin {
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_key)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedDevice {
    pub pin: MachinePin,
    pub pairing: PairingId,
    /// Assigned at enrollment and inseparable from this pairing's credential.
    pub client: [u8; 32],
}

/// Where the server listens for paired connections. Reading one validates it and keeps every
/// interface as `0.0.0.0`, as [`NetworkPolicy::new`] does.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PolicyFields")]
pub struct NetworkPolicy {
    /// One address of the server's machine, or `0.0.0.0` for every interface.
    pub bind: SocketAddr,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFields {
    bind: SocketAddr,
}
impl TryFrom<PolicyFields> for NetworkPolicy {
    type Error = DirectError;
    fn try_from(fields: PolicyFields) -> Result<Self, DirectError> {
        Self::new(fields.bind)
    }
}
impl NetworkPolicy {
    /// An unspecified address, `0.0.0.0` or `[::]`, listens on every interface for IPv4 and
    /// IPv6, and is kept as `0.0.0.0`.
    pub fn new(bind: SocketAddr) -> Result<Self, DirectError> {
        if bind.port() == 0 || bind.ip().is_multicast() {
            return Err(DirectError::InvalidAddress);
        }
        if bind.ip().is_unspecified() {
            return Self::every_interface(bind.port());
        }
        Ok(Self { bind })
    }
    /// Every interface at `port`, for IPv4 and IPv6.
    pub fn every_interface(port: u16) -> Result<Self, DirectError> {
        if port == 0 {
            return Err(DirectError::InvalidAddress);
        }
        Ok(Self {
            bind: SocketAddr::new(std::net::Ipv4Addr::UNSPECIFIED.into(), port),
        })
    }
    pub fn is_every_interface(&self) -> bool {
        self.bind.ip().is_unspecified()
    }
    /// Whether a listener at this policy also takes what one at `address` takes: the same
    /// address, or every interface at the same port.
    pub fn covers(&self, address: SocketAddr) -> bool {
        self.bind.port() == address.port() && (self.is_every_interface() || self.bind == address)
    }
    /// An `IP` or `IP:PORT`. `default_port` applies when no port is given.
    pub fn parse(address: &str, default_port: u16) -> Result<Self, DirectError> {
        if let Ok(bind) = address.parse() {
            return Self::new(bind);
        }
        let ip: std::net::IpAddr = address.parse().map_err(|_| DirectError::InvalidAddress)?;
        Self::new(SocketAddr::new(ip, default_port))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectStatus {
    pub setup_generation: u64,
    pub pin: MachinePin,
    pub policy: Option<NetworkPolicy>,
    pub enabled: bool,
    pub machine_name: String,
    pub listener: ListenerState,
    /// Fresh observations from the machine that owns this server.
    pub network_addresses: Result<NetworkAddresses, DirectError>,
}

/// How to put Network Access back after an SSH setup changed it: `installed` is the policy that
/// setup turned on, and `policy` and `enabled` are Network Access as it was before. A setup
/// cancellation puts it back only while Network Access is still what this setup installed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRestore {
    pub installed: NetworkPolicy,
    pub policy: Option<NetworkPolicy>,
    pub enabled: bool,
}
impl NetworkRestore {
    /// Puts Network Access back to `before`, read before setup turned on `installed`.
    pub fn new(installed: NetworkPolicy, before: &DirectStatus) -> Self {
        Self {
            installed,
            policy: before.policy.clone(),
            enabled: before.enabled,
        }
    }
}

/// What a setup cancellation did with the Network Access that the setup changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoredAccess {
    /// Put back. Network Access is off.
    Off,
    /// Put back. Network Access is on at the address it had before setup.
    On,
    /// Network Access changed after setup changed it, so the newer choice stays.
    Kept,
    /// Another device paired, or another setup is pending, over the Network Access this setup
    /// turned on, so it stays on.
    InUse,
    /// Putting it back failed. Network Access stays on as setup turned it on.
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkAddresses {
    /// Sorted concrete addresses on active interfaces. Empty means none are usable.
    pub available: Vec<IpAddr>,
    /// A routing-table source address, present only when it is also available.
    pub suggested: Option<IpAddr>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ListenerState {
    Off,
    Listening {
        addr: SocketAddr,
    },
    /// Waits to bind `addr`: until the connections of the previous address end, within seconds,
    /// or until the address is on this computer, without a limit.
    Starting {
        addr: SocketAddr,
    },
    Failed(DirectError),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingClaim {
    pub id: ClaimId,
    /// The exact invitation consumed by this claim, independent of its expiry.
    pub invitation_id: ClaimId,
    pub core: [u8; 32],
    pub device_public_key: [u8; 32],
    pub device_name: ClientName,
    pub expires_at_unix_seconds: u64,
    /// Hash of the first verified server challenge and signed device claim.
    pub exchange_hash: [u8; 32],
}
impl PairingClaim {
    pub fn device_fingerprint(&self) -> String {
        fingerprint(&self.device_public_key)
    }
    /// Display on both devices before approving this exact claim.
    pub fn comparison_code(&self) -> ComparisonCode {
        ComparisonCode(
            u32::from_be_bytes([
                self.exchange_hash[0],
                self.exchange_hash[1],
                self.exchange_hash[2],
                self.exchange_hash[3],
            ]) % 1_000_000,
        )
    }
}

/// A display-only comparison of an authenticated exchange. Never an admission proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ComparisonCode(u32);

impl std::fmt::Display for ComparisonCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:06}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceInfo {
    pub pairing: PairingId,
    pub client: [u8; 32],
    pub name: ClientName,
    pub public_key: [u8; 32],
    /// Last fully admitted connection, in Unix seconds through year 9999.
    /// `None` means no connection time has been observed.
    pub last_connected_at_unix_seconds: Option<u64>,
}
impl DeviceInfo {
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_key)
    }
}
/// The display fingerprint of a raw Ed25519 public key.
pub fn fingerprint(public: &[u8; 32]) -> String {
    Sha256::new()
        .update(public)
        .finalize()
        .chunks(4)
        .map(|group| group.iter().map(|b| format!("{b:02X}")).collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevocationReceipt {
    pub core: [u8; 32],
    pub operation: OperationId,
    pub target: PairingId,
}

/// Bounded classifications only. No endpoints, invitations, credentials or native errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, thiserror::Error)]
pub enum DirectError {
    #[error("The connection did not complete.")]
    Transport,
    #[error("Connection storage is unavailable.")]
    StorageUnavailable,
    #[error("This connection's device credential is unavailable.")]
    CredentialUnavailable,
    #[error("Resume this connection to authorize credential access.")]
    InteractionRequired,
    #[error("This invitation is invalid or used.")]
    InvalidInvitation,
    #[error("This invitation or claim has expired.")]
    Expired,
    #[error("The machine\u{2019}s owner rejected this claim.")]
    Rejected,
    #[error("The claim changed. Review it before approving.")]
    ChangedClaim,
    #[error("Network access changed. Review it before continuing setup.")]
    PolicyChanged,
    #[error("The machine or device did not match this connection.")]
    AuthenticationRefused,
    #[error("The machine no longer matches this connection.")]
    PinMismatch,
    #[error("This device has been unpaired.")]
    Revoked,
    #[error("SpaceTerm and SpaceTerm Core have no compatible protocol version.")]
    IncompatibleProtocol,
    #[error("Direct connections have reached capacity.")]
    Capacity,
    #[error("Enable direct network access before inviting a device.")]
    Disabled,
    #[error("This connection request is invalid.")]
    InvalidRequest,
    #[error("Enter a DNS name or IP address and a nonzero UDP port.")]
    InvalidAddress,
    /// The server couldn't listen at the address and port, for example because another app
    /// holds them. Nothing changed.
    #[error("SpaceTerm Core couldn\u{2019}t listen at this address and port.")]
    AddressUnavailable,
    /// Another socket holds the UDP port, often another build identity's SpaceTerm Core or
    /// another app. Nothing changed.
    #[error("Another app is using this UDP port.")]
    PortInUse,
    /// The invitation or Machine belongs to another build identity, such as a Preflight Core
    /// for a Release desktop. Nothing was claimed.
    #[error("This invitation is for another edition of SpaceTerm.")]
    IdentityMismatch,
    #[error("This claim has already been decided.")]
    AlreadyDecided,
    #[error("This proof belongs to a different claim.")]
    Substituted,
    #[error("This claim or connection names another machine.")]
    WrongMachine,
}
