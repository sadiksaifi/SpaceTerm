//! Paired QUIC authentication, enrollment and revocation.
use crate::{CoreError, pane::control::ClientName};

use crate::connection::path::NetworkPath;
use serde::Serialize;
pub use spaceterm_protocol::direct::*;

#[doc(hidden)]
pub mod client;
#[doc(hidden)]
pub mod crypto;
mod endpoint;
pub use endpoint::Endpoint;
#[doc(hidden)]
pub mod quic;

pub const MAX_CONNECTION_TIME: u64 = 253_402_300_799;

impl From<CoreError> for DirectError {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::Direct(error) => error,
            CoreError::StorageUnavailable
            | CoreError::UnsafeStorage
            | CoreError::IncompatibleState
            | CoreError::DataResetRequired => Self::StorageUnavailable,
            CoreError::IncompatibleProtocol(_) => Self::IncompatibleProtocol,
            CoreError::AuthenticationRefused => Self::AuthenticationRefused,
            CoreError::Capacity => Self::Capacity,
            _ => Self::Transport,
        }
    }
}
impl From<DirectError> for CoreError {
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
    #[doc(hidden)]
    pub payload: wire::Invite,
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
        let port = default_port(&self.payload.pin.build);
        self.payload.endpoint = Endpoint::parse(address, port)?.url();
        client::invitation(self.payload.clone())
    }
    pub fn machine_pin(&self) -> &MachinePin {
        &self.payload.pin
    }
    pub fn machine_name(&self) -> &str {
        &self.payload.machine_name
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
pub trait DirectConnector: Send + Sync {
    fn connect(&self) -> Result<DirectConnection, DirectError>;
    /// Connects by `deadline`, the deadline of the request that needs the connection. A
    /// connector that bounds its dials by a deadline of its own replaces it with this one, so a
    /// dial that starts after a person answered a prompt follows the budget their answer began.
    fn connect_by(&self, deadline: std::time::Instant) -> Result<DirectConnection, DirectError> {
        let _ = deadline;
        self.connect()
    }
}

/// An authenticated QUIC connection before admission. Admission turns it into the connection's
/// carrier; dropping it closes the connection.
pub struct DirectConnection {
    stream: Box<crate::connection::quic::QuicStream>,
    paired: PairedDevice,
    path: Option<NetworkPath>,
}
impl DirectConnection {
    /// Uses the supplied route-change notifications once the work carrier is running.
    pub fn with_path(mut self, path: NetworkPath) -> Self {
        self.path = Some(path);
        self
    }

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
    pin: MachinePin,
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
        pin: MachinePin,
        claim: ClaimId,
        credential: &DeviceCredential,
    ) -> Result<Self, DirectError> {
        Endpoint::from_url(endpoint)?;
        if claim == [0; 32]
            || pin.core == [0; 32]
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
        if receipt.core != self.pin.core
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
