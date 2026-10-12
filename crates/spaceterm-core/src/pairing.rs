//! Pairing and restricted pending self-revocation data, without credentials or
//! native authority. Protocol/native composition authenticates signed receipts.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::authority::StoreRevision;
use crate::identity::{ClientId, DeviceId, PairingId, ServerId};
use crate::operations::{OperationId, RequestDigest};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PairingState {
    Authorized,
    Revoked { revision: StoreRevision },
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingRecord {
    pub id: PairingId,
    pub device: DeviceId,
    pub state: PairingState,
}
/// A companion and desktop Client on the same device receive different IDs.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClientRecord {
    pub id: ClientId,
    pub pairing: PairingId,
}

/// Opaque native-store locator, never key bytes, a path or a plaintext fallback.
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub struct CredentialReference([u8; 16]);
impl CredentialReference {
    pub const fn new(opaque: [u8; 16]) -> Self {
        Self(opaque)
    }
    pub const fn opaque(self) -> [u8; 16] {
        self.0
    }
}
impl std::fmt::Debug for CredentialReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialReference([redacted])")
    }
}
/// Identifies the original retained route in client-native storage. Changing a
/// saved destination cannot redirect this task to another server or pairing.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevocationRouteReference([u8; 16]);
impl RevocationRouteReference {
    pub const fn new(opaque: [u8; 16]) -> Self {
        Self(opaque)
    }
    pub const fn opaque(self) -> [u8; 16] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RevocationError {
    #[error("pending revocation has inconsistent original identities")]
    WrongBinding,
}

/// This record must become durable before ordinary local use, connections and
/// attachments are removed. Its native credential reference is restricted to
/// revocation/status. It never permits Start, update, restart or enrollment.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "PendingParts", into = "PendingParts")]
pub struct PendingSelfRevocation {
    original_server: ServerId,
    original_pairing: PairingId,
    device: DeviceId,
    client: ClientId,
    operation: OperationId,
    route: RevocationRouteReference,
    credential: CredentialReference,
    notice_dismissed: bool,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct PendingParts {
    pub original_server: ServerId,
    pub original_pairing: PairingId,
    pub device: DeviceId,
    pub client: ClientId,
    pub operation: OperationId,
    pub route: RevocationRouteReference,
    pub credential: CredentialReference,
    pub notice_dismissed: bool,
}
impl PendingSelfRevocation {
    pub fn new(parts: PendingParts) -> Result<Self, RevocationError> {
        if parts.original_pairing.server() != parts.original_server
            || parts.client.server() != parts.original_server
            || parts.operation.stream().server() != parts.original_server
            || parts.operation.stream().pairing() != parts.original_pairing
            || parts.operation.stream().client() != parts.client
        {
            return Err(RevocationError::WrongBinding);
        }
        Ok(Self {
            original_server: parts.original_server,
            original_pairing: parts.original_pairing,
            device: parts.device,
            client: parts.client,
            operation: parts.operation,
            route: parts.route,
            credential: parts.credential,
            notice_dismissed: parts.notice_dismissed,
        })
    }
    pub const fn server(self) -> ServerId {
        self.original_server
    }
    pub const fn pairing(self) -> PairingId {
        self.original_pairing
    }
    pub const fn device(self) -> DeviceId {
        self.device
    }
    pub const fn client(self) -> ClientId {
        self.client
    }
    pub const fn operation(self) -> OperationId {
        self.operation
    }
    pub const fn route(self) -> RevocationRouteReference {
        self.route
    }
    pub const fn credential(self) -> CredentialReference {
        self.credential
    }
    pub const fn notice_dismissed(self) -> bool {
        self.notice_dismissed
    }

    /// Compare only after protocol verifies the receipt's signature. Cleanup may
    /// remove this original pairing's retained material, never a newer pairing,
    /// another server's material, attachments or a shared device identity key.
    pub fn matches_receipt(self, receipt: &RevocationReceipt) -> bool {
        receipt.server == self.original_server
            && receipt.pairing == self.original_pairing
            && receipt.device == self.device
            && receipt.operation == self.operation
    }
}
impl TryFrom<PendingParts> for PendingSelfRevocation {
    type Error = RevocationError;
    fn try_from(value: PendingParts) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<PendingSelfRevocation> for PendingParts {
    fn from(value: PendingSelfRevocation) -> Self {
        Self {
            original_server: value.original_server,
            original_pairing: value.original_pairing,
            device: value.device,
            client: value.client,
            operation: value.operation,
            route: value.route,
            credential: value.credential,
            notice_dismissed: value.notice_dismissed,
        }
    }
}
/// Receipt claims. The wire layer separately carries and verifies the signature,
/// including on the restricted status route after ordinary authorization ends.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevocationReceipt {
    pub server: ServerId,
    pub pairing: PairingId,
    pub device: DeviceId,
    pub operation: OperationId,
    pub revision: StoreRevision,
    pub receipt_digest: RequestDigest,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PendingRevocationState {
    AwaitingConfirmation(PendingSelfRevocation),
    Unconfirmed {
        task: PendingSelfRevocation,
        failure: RevocationFailure,
    },
    Confirmed(RevocationReceipt),
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RevocationFailure {
    Unreachable,
    AuthorizationDenied,
    Incompatible,
    StorageUnavailable,
}
