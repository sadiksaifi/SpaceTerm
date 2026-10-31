//! Deliberately enabled QUIC access, exact device enrollment and restricted revocation.
//! Native credential access and connection persistence belong to the calling client.
use crate::{ServerError, pane::control::ClientName};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
pub use wire::{Management, ManagementReply};
pub const DEFAULT_PORT: u16 = 9231;

mod client;
mod crypto;
mod endpoint;
pub use endpoint::Endpoint;
pub(crate) mod host;
mod network;
mod quic;
mod storage;
pub(crate) use storage::observe;
#[cfg(test)]
pub(crate) mod tests;
mod wire;

pub(super) fn setup_generation(operation: &[u8; 32]) -> u64 {
    let mut generation = [0; 8];
    generation.copy_from_slice(&operation[..8]);
    u64::from_be_bytes(generation)
}

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
    pub setup_generation: u64,
    pub pin: ServerPin,
    pub policy: Option<NetworkPolicy>,
    pub enabled: bool,
    pub server_name: String,
    pub listener: ListenerState,
    /// Fresh observations from the machine that owns this server.
    pub network_addresses: Result<NetworkAddresses, DirectError>,
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
    /// Waits to bind `addr` until the connections of the previous address end, within seconds.
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
    pub server: [u8; 32],
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
    #[error("The server owner rejected this claim.")]
    Rejected,
    #[error("The claim changed. Review it before approving.")]
    ChangedClaim,
    #[error("Network access changed. Review it before continuing setup.")]
    PolicyChanged,
    #[error("The server or device did not match this connection.")]
    AuthenticationRefused,
    #[error("The server no longer matches this connection.")]
    PinMismatch,
    #[error("This device has been unpaired.")]
    Revoked,
    #[error("The client and server have no compatible protocol version.")]
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
    #[error("The server couldn\u{2019}t listen at this address and port.")]
    AddressUnavailable,
    #[error("This claim has already been decided.")]
    AlreadyDecided,
    #[error("This proof belongs to a different claim.")]
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
    /// The public enrollment key. The private credential stays on its client device.
    pub fn public_key(&self) -> Result<[u8; 32], DirectError> {
        crypto::public(&self.0)
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
    /// Connect through a different authority while retaining the invitation's pinned identity.
    pub fn with_address(mut self, address: &str) -> Result<Self, DirectError> {
        self.payload.endpoint = Endpoint::parse(address)?.url();
        client::invitation(self.payload.clone())
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
    /// Server-issued lifetime for a local monotonic display or wait deadline.
    /// The server alone decides actual expiry; this is not remaining validity.
    pub fn lifetime(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.payload.valid_for_seconds)
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

/// An authenticated QUIC connection before admission. Admission turns it into the connection's
/// carrier; dropping it closes the connection.
pub struct DirectConnection {
    #[cfg_attr(
        not(any(feature = "client", test)),
        allow(dead_code, reason = "the desktop admits and carries the connection")
    )]
    stream: Box<crate::connection::quic::QuicStream>,
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
    /// Resolves the retained DNS name afresh and admits the server using pinned trust.
    pub fn connect(
        endpoint: &str,
        paired: &PairedDevice,
        credential: &DeviceCredential,
    ) -> Result<Self, DirectError> {
        client::connect(endpoint, paired, credential, None)
    }
    pub fn connect_until(
        endpoint: &str,
        paired: &PairedDevice,
        credential: &DeviceCredential,
        deadline: std::time::Instant,
    ) -> Result<Self, DirectError> {
        client::connect(endpoint, paired, credential, Some(deadline))
    }
}

/// Transient enrollment. Calling finish observes only this original exact claim.
pub struct Enrollment {
    endpoint: String,
    pin: ServerPin,
    claim: ClaimId,
    credential: DeviceCredential,
    prepared: Option<(wire::Invite, ClientName)>,
    admitted: Option<PairingClaim>,
    exchanges: Vec<[u8; 32]>,
    deadline: Option<std::time::Instant>,
}
impl Enrollment {
    /// Reconstructs only an original claim for observation or cancellation. Opens nothing.
    pub fn resume(
        endpoint: &str,
        pin: ServerPin,
        claim: ClaimId,
        credential: &DeviceCredential,
    ) -> Result<Self, DirectError> {
        Endpoint::from_url(endpoint)?;
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
            admitted: None,
            exchanges: Vec::new(),
            deadline: None,
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
    /// Bound every transport exchange by the caller's monotonic deadline.
    pub fn set_deadline(&mut self, deadline: std::time::Instant) {
        self.deadline = Some(deadline);
    }
    /// Submits the prepared original. Repeating the same claim and name is idempotent.
    /// A resumed cleanup has no invitation and cannot submit a replacement claim.
    pub fn submit(&mut self) -> Result<ComparisonCode, DirectError> {
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
    /// Available only after a pinned server confirms this exact authenticated claim.
    pub fn comparison_code(&self) -> Option<ComparisonCode> {
        self.admitted.as_ref().map(PairingClaim::comparison_code)
    }
    pub fn finish(&mut self) -> Result<Option<PairedDevice>, DirectError> {
        client::finish(self)
    }
    /// Cancels this exact proved claim; if approval won, revokes only its enrolled pairing.
    pub fn validate_cancellation(&self, receipt: &RevocationReceipt) -> Result<(), DirectError> {
        let operation = crypto::hash(&wire::transcript(
            "cancel",
            &self.pin,
            &self.claim,
            &receipt.target,
        )?);
        if receipt.server != self.pin.server
            || receipt.target == [0; 32]
            || receipt.operation != operation
        {
            return Err(DirectError::Substituted);
        }
        Ok(())
    }
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
    client::revoke(endpoint, actor, credential, operation, target, None)
}

/// Restricted revocation bounded by the command's deadline.
pub fn revoke_until(
    endpoint: &str,
    actor: &PairedDevice,
    credential: &DeviceCredential,
    operation: OperationId,
    target: PairingId,
    deadline: std::time::Instant,
) -> Result<RevocationReceipt, DirectError> {
    client::revoke(
        endpoint,
        actor,
        credential,
        operation,
        target,
        Some(deadline),
    )
}
