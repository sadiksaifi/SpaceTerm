//! Deliberately enabled WSS access, exact device enrollment and restricted revocation.
//! Native credential access and connection persistence belong to the calling client.
use crate::{ServerError, pane::control::ClientName};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
pub use wire::{Management, ManagementReply};
pub const DEFAULT_PORT: u16 = 9231;

mod client;
mod crypto;
pub(crate) mod host;
mod storage;
#[cfg(test)]
pub(crate) mod tests;
mod wire;
mod wss;

pub type PairingId = [u8; 32];
pub type ClaimId = [u8; 32];
pub type OperationId = [u8; 32];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerPin {
    pub build: String,
    pub server: [u8; 32],
    /// Raw Ed25519 enrollment/authentication public key.
    pub public_key: [u8; 32],
    /// SHA-256 of the server's exact retained TLS certificate.
    pub certificate: [u8; 32],
}
impl ServerPin {
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_key)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedDevice {
    pub pin: ServerPin,
    pub pairing: PairingId,
    /// Assigned at enrollment and inseparable from this pairing's credential.
    pub client: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkPolicy {
    pub bind: SocketAddr,
}
impl NetworkPolicy {
    pub fn new(bind: SocketAddr) -> Result<Self, DirectError> {
        if bind.port() == 0 || bind.ip().is_multicast() || bind.ip().is_unspecified() {
            return Err(DirectError::InvalidAddress);
        }
        Ok(Self { bind })
    }
    pub fn parse(address: &str) -> Result<Self, DirectError> {
        if let Ok(bind) = address.parse() {
            return Self::new(bind);
        }
        let ip: std::net::IpAddr = address.parse().map_err(|_| DirectError::InvalidAddress)?;
        Self::new(SocketAddr::new(ip, DEFAULT_PORT))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectStatus {
    pub pin: ServerPin,
    pub policy: Option<NetworkPolicy>,
    pub server_name: String,
    pub listener: ListenerState,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ListenerState {
    Off,
    Listening { addr: SocketAddr },
    Failed(DirectError),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingClaim {
    pub id: ClaimId,
    pub server: [u8; 32],
    pub device_public_key: [u8; 32],
    pub device_name: ClientName,
    pub expires_at_unix_seconds: u64,
}
impl PairingClaim {
    pub fn device_fingerprint(&self) -> String {
        fingerprint(&self.device_public_key)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceInfo {
    pub pairing: PairingId,
    pub client: [u8; 32],
    pub name: ClientName,
    pub public_key: [u8; 32],
}
impl DeviceInfo {
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_key)
    }
}
fn fingerprint(public: &[u8; 32]) -> String {
    crypto::hash(public)
        .chunks(4)
        .map(|group| group.iter().map(|b| format!("{b:02X}")).collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevocationReceipt {
    pub server: [u8; 32],
    pub operation: OperationId,
    pub target: PairingId,
}

/// Bounded classifications only. No endpoints, invitations, credentials or native errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, thiserror::Error)]
pub enum DirectError {
    #[error("The direct connection did not complete.")]
    Transport,
    #[error("Direct connection storage is unavailable.")]
    StorageUnavailable,
    #[error("This connection's native device credential is unavailable.")]
    CredentialUnavailable,
    #[error("Resume this connection to authorize native credential access.")]
    InteractionRequired,
    #[error("This invitation is invalid or has already been used.")]
    InvalidInvitation,
    #[error("This invitation or device claim has expired.")]
    Expired,
    #[error("The server owner rejected this device claim.")]
    Rejected,
    #[error("The device claim changed. Review the current claim before approving it.")]
    ChangedClaim,
    #[error("The server or device proof did not match this connection.")]
    AuthenticationRefused,
    #[error("The server no longer matches this connection's retained trust pin.")]
    PinMismatch,
    #[error("This device has been unpaired.")]
    Revoked,
    #[error("The client and server have no compatible protocol version.")]
    IncompatibleProtocol,
    #[error("Direct connections have reached their bounded capacity.")]
    Capacity,
    #[error("Enable direct network access explicitly before inviting a device.")]
    Disabled,
    #[error("This direct connection request is invalid.")]
    InvalidRequest,
    #[error("Enter a concrete IP address and a nonzero TCP port.")]
    InvalidAddress,
    #[error("This device claim has already been decided.")]
    AlreadyDecided,
    #[error("This proof belongs to a different device claim.")]
    Substituted,
    #[error("This claim or connection names another server.")]
    WrongServer,
}

impl From<ServerError> for DirectError {
    fn from(error: ServerError) -> Self {
        match error {
            ServerError::Direct(error) => error,
            ServerError::StorageUnavailable
            | ServerError::UnsafeStorage
            | ServerError::IncompatibleState => Self::StorageUnavailable,
            ServerError::IncompatibleProtocol | ServerError::MissingRequiredCapability => {
                Self::IncompatibleProtocol
            }
            ServerError::AuthenticationRefused => Self::AuthenticationRefused,
            ServerError::Capacity => Self::Capacity,
            _ => Self::Transport,
        }
    }
}
impl From<DirectError> for ServerError {
    fn from(error: DirectError) -> Self {
        Self::Direct(error)
    }
}

/// A private Ed25519 PKCS#8 key. The caller stores these bytes only in native storage.
pub struct DeviceCredential(zeroize::Zeroizing<Vec<u8>>);
impl DeviceCredential {
    pub fn generate() -> Result<Self, DirectError> {
        crypto::generate()
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DirectError> {
        crypto::credential(bytes)
    }
    pub fn expose_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn fingerprint(&self) -> String {
        crypto::public(&self.0)
            .map(|key| fingerprint(&key))
            .unwrap_or_default()
    }
}

/// Transient invitation. Never serialize it into retained client state or diagnostics.
pub struct Invitation {
    payload: wire::Invite,
    text: zeroize::Zeroizing<String>,
}
impl Invitation {
    pub fn parse(text: &str) -> Result<Self, DirectError> {
        client::parse_invitation(text)
    }
    pub fn endpoint(&self) -> &str {
        &self.payload.endpoint
    }
    pub fn server_pin(&self) -> &ServerPin {
        &self.payload.pin
    }
    pub fn server_name(&self) -> &str {
        &self.payload.server_name
    }
    pub fn id(&self) -> ClaimId {
        self.payload.id
    }
    pub fn expires_at_unix_seconds(&self) -> u64 {
        self.payload.expires
    }
    /// For transient display and explicit Copy only.
    pub fn display_text(&self) -> zeroize::Zeroizing<String> {
        self.text.clone()
    }
}

/// The caller supplies native credential policy; construction and cloning open nothing.
#[cfg(feature = "client")]
pub trait DirectConnector: Send + Sync {
    fn connect(&self) -> Result<DirectConnection, DirectError>;
}

/// Authenticated WSS carrier. Drop cancels its one socket owner and closes its private bridge.
pub struct DirectConnection {
    bridge: wss::Bridge,
    #[cfg_attr(
        not(feature = "client"),
        allow(
            dead_code,
            reason = "desktop admission checks the credential-bound pairing"
        )
    )]
    paired: PairedDevice,
}
impl DirectConnection {
    /// Connects to the literal IP endpoint advertised by an explicitly enabled policy.
    /// TLS, HTTP and device admission are bounded; this operation never runs a DNS resolver.
    pub fn connect(
        endpoint: &str,
        paired: &PairedDevice,
        credential: &DeviceCredential,
    ) -> Result<Self, DirectError> {
        client::connect(endpoint, paired, credential)
    }
}

/// Transient enrollment. Calling finish observes only this original exact claim.
pub struct Enrollment {
    endpoint: String,
    pin: ServerPin,
    claim: ClaimId,
    credential: DeviceCredential,
    prepared: Option<(wire::Invite, ClientName)>,
}
impl Enrollment {
    /// Reconstructs only an original claim for observation or cancellation. Opens nothing.
    pub fn resume(
        endpoint: &str,
        pin: ServerPin,
        claim: ClaimId,
        credential: &DeviceCredential,
    ) -> Result<Self, DirectError> {
        wss::validate_endpoint(endpoint)?;
        if claim == [0; 32]
            || pin.server == [0; 32]
            || pin.public_key == [0; 32]
            || pin.certificate == [0; 32]
            || !client::valid_build(&pin.build)
        {
            return Err(DirectError::InvalidRequest);
        }
        Ok(Self {
            endpoint: endpoint.to_owned(),
            pin,
            claim,
            credential: DeviceCredential::from_bytes(credential.expose_bytes())?,
            prepared: None,
        })
    }
    /// Preallocates the exact claim without opening a socket. Retain its ID, endpoint,
    /// server pin and exact native credential item before calling [`Self::submit`].
    /// The invitation remains transient and is zeroized when this value is dropped.
    pub fn prepare(
        invitation: &Invitation,
        name: ClientName,
        credential: &DeviceCredential,
    ) -> Result<Self, DirectError> {
        client::prepare(invitation, name, credential)
    }
    /// Submits the prepared original. Repeating the same claim and name is idempotent.
    /// A resumed cleanup has no invitation and cannot submit a replacement claim.
    pub fn submit(&mut self) -> Result<(), DirectError> {
        client::submit(self)
    }
    /// Prepares and submits an ephemeral enrollment. Durable callers must use
    /// [`Self::prepare`] and retain recovery metadata before [`Self::submit`].
    pub fn claim(
        invitation: &Invitation,
        name: ClientName,
        credential: &DeviceCredential,
    ) -> Result<Self, DirectError> {
        let mut enrollment = Self::prepare(invitation, name, credential)?;
        enrollment.submit()?;
        Ok(enrollment)
    }
    pub fn claim_id(&self) -> ClaimId {
        self.claim
    }
    pub fn finish(&mut self) -> Result<Option<PairedDevice>, DirectError> {
        client::finish(self)
    }
    /// Cancels this exact proved claim; if approval won, revokes only its enrolled pairing.
    pub fn cancel(&mut self) -> Result<Option<RevocationReceipt>, DirectError> {
        client::cancel(self)
    }
}

/// Resolves only this exact revocation. A retired actor can confirm its own revoked pairing
/// without another effect or ledger entry, but cannot authenticate a terminal or select a
/// different target. A retained admitted original also returns its exact receipt.
pub fn revoke(
    endpoint: &str,
    actor: &PairedDevice,
    credential: &DeviceCredential,
    operation: OperationId,
    target: PairingId,
) -> Result<RevocationReceipt, DirectError> {
    client::revoke(endpoint, actor, credential, operation, target)
}
