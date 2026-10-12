//! Additive pairing projections and operation payloads.
//!
//! These records contain comparison facts, public keys and signed public receipts.
//! They contain no credential seeds, bearer authorization, native credential
//! locators for server use, host authority, process handles or terminal content.
//! Structural validation never authenticates a caller. The actor checks current
//! authority, the ledger checks stream purpose/capacity, and the store compares
//! expected versions and validates signed bytes before returning recovered state.

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{ClientRecord, PairingRecord, PairingState, PendingSelfRevocation, RevocationReceipt};
use crate::authority::{FieldVersion, ServerEpoch, StoreRevision};
use crate::identity::{
    ClientId, DeviceId, IdentityHighWater, OperationStreamId, PairingId, ServerId,
};
use crate::operations::{AssessmentRef, OperationBinding, OperationId, RequestDigest};

/// Admission ceilings, not promises of simultaneous ledger capacity.
pub const MAX_ACTIVE_PAIRINGS: usize = 64;
/// Each activated Client owns a distinct dedicated self-revocation stream.
pub const MAX_CLIENTS_PER_PAIRING: usize = 16;
/// Includes revoked lifetimes and abandoned prospective pairing identities.
pub const MAX_PAIRING_LIFETIMES: usize = 4096;
/// Outstanding unconsumed authorizations only.
pub const MAX_OUTSTANDING_ENROLLMENTS: usize = 32;
/// Canonical signed receipt body plus its 64-byte Ed25519 signature.
pub const MAX_SIGNED_PAIRING_RECEIPT_BYTES: usize = 2048;
/// Pairing management requests have this additional admission ceiling.
pub const MAX_PAIRING_MANAGEMENT_BYTES: usize = 8192;
/// Each Client reserves one ledger slot and the ledger's full reservation unit.
pub const SELF_REVOCATION_RESERVED_BYTES: usize = 64 * 1024;
/// Client journal admission ceiling for pending restricted tasks.
pub const MAX_PENDING_REVOCATIONS: usize = 1024;
/// Decode admission ceiling for the pairing portion of the client journal.
pub const MAX_LOCAL_PAIRING_DECODED_BYTES: usize = 8 * 1024 * 1024;
/// Authorization expiry comparison facts use milliseconds, not a live clock.
pub const MAX_ENROLLMENT_LIFETIME_MILLIS: u64 = 5 * 60 * 1000;

/// Content-free structural failures. These do not describe native failures.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PairingContractError {
    #[error("pairing record has inconsistent identities")]
    WrongBinding,
    #[error("pairing record is invalid")]
    Invalid,
    #[error("pairing record exceeds its limit")]
    LimitExceeded,
}

fn require(valid: bool) -> Result<(), PairingContractError> {
    if valid {
        Ok(())
    } else {
        Err(PairingContractError::WrongBinding)
    }
}
fn limit(count: usize, maximum: usize) -> Result<(), PairingContractError> {
    if count <= maximum {
        Ok(())
    } else {
        Err(PairingContractError::LimitExceeded)
    }
}
fn nonzero(bytes: &[u8]) -> Result<(), PairingContractError> {
    if bytes.iter().any(|byte| *byte != 0) {
        Ok(())
    } else {
        Err(PairingContractError::Invalid)
    }
}
fn stream_fits(stream: OperationStreamId, marks: IdentityHighWater) -> bool {
    stream.key().local() <= marks.operation_stream
        && stream.pairing().local() <= marks.pairing
        && stream.client().local() <= marks.client
}

// A checked record exposes only an immutable parts borrow. Deserialization uses
// the same constructor. Producers must bound bytes/counts before serde allocation.
macro_rules! checked_record {
    ($name:ident, $parts:ident, $parts_name:literal, $doc:literal, $check:expr) => {
        #[doc = $doc]
        #[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(try_from = $parts_name, into = $parts_name)]
        pub struct $name($parts);
        impl $name {
            /// Checks structural invariants without granting authorization.
            pub fn new(parts: $parts) -> Result<Self, PairingContractError> {
                ($check)(&parts)?;
                Ok(Self(parts))
            }
            /// Returns immutable validated comparison facts.
            pub fn parts(&self) -> &$parts {
                &self.0
            }
            /// Rechecks structural invariants at a recovery boundary.
            pub fn validate(&self) -> Result<(), PairingContractError> {
                ($check)(&self.0)
            }
        }
        impl TryFrom<$parts> for $name {
            type Error = PairingContractError;
            fn try_from(parts: $parts) -> Result<Self, Self::Error> {
                Self::new(parts)
            }
        }
        impl From<$name> for $parts {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($name), "(..)"))
            }
        }
    };
}

/// Exact target of an enrollment or revocation lifetime.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingTargetParts {
    pub server: ServerId,
    pub device: DeviceId,
    pub pairing: PairingId,
}
checked_record!(
    PairingTarget,
    PairingTargetParts,
    "PairingTargetParts",
    "Exact original pairing target. A newer pairing of the Device is different.",
    |p: &PairingTargetParts| {
        nonzero(&p.server.bytes())?;
        nonzero(&p.device.bytes())?;
        require(p.pairing.server() == p.server)
    }
);

/// The two public-key roles cannot be collapsed by a codec or storage adapter.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingKeysParts {
    pub device_identity_public_key: [u8; 32],
    pub pairing_authentication_public_key: [u8; 32],
}
checked_record!(
    PairingKeys,
    PairingKeysParts,
    "PairingKeysParts",
    "Stable device-identity key and separate per-pairing authentication key.",
    |p: &PairingKeysParts| {
        nonzero(&p.device_identity_public_key)?;
        nonzero(&p.pairing_authentication_public_key)?;
        require(p.device_identity_public_key != p.pairing_authentication_public_key)
    }
);

/// Immutable stream bindings activated together for one Client.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingClientStreamsParts {
    pub client: ClientRecord,
    pub ordinary: OperationStreamId,
    pub self_revocation: OperationStreamId,
}
checked_record!(
    PairingClientStreams,
    PairingClientStreamsParts,
    "PairingClientStreamsParts",
    "One Client's ordinary stream and dedicated self-revocation stream.",
    |p: &PairingClientStreamsParts| {
        require(
            p.client.id.server() == p.client.pairing.server()
                && p.ordinary.client() == p.client.id
                && p.self_revocation.client() == p.client.id
                && p.ordinary.pairing() == p.client.pairing
                && p.self_revocation.pairing() == p.client.pairing
                && p.ordinary.key() != p.self_revocation.key(),
        )
    }
);
impl PairingClientStreams {
    /// Checks all referenced durable allocation marks.
    pub fn fits_marks(&self, marks: IdentityHighWater) -> bool {
        stream_fits(self.0.ordinary, marks) && stream_fits(self.0.self_revocation, marks)
    }
}

/// Public receipt claims. Signatures never bind a route, epoch or expiration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PairingReceiptBody {
    Enrollment {
        target: PairingTarget,
        keys: PairingKeys,
        client: PairingClientStreams,
        enrollment: OperationBinding,
        revision: StoreRevision,
    },
    ClientBinding {
        target: PairingTarget,
        keys: PairingKeys,
        client: PairingClientStreams,
        operation: OperationBinding,
        revision: StoreRevision,
    },
    Revocation {
        receipt: RevocationReceipt,
        authentication_key_digest: RequestDigest,
        revocation_event: [u8; 16],
    },
}
impl PairingReceiptBody {
    /// Returns the original Server identity.
    pub fn server(&self) -> ServerId {
        match self {
            Self::Enrollment { target, .. } | Self::ClientBinding { target, .. } => target.0.server,
            Self::Revocation { receipt, .. } => receipt.server,
        }
    }
    /// Returns the receipt's historical durable revision.
    pub fn revision(&self) -> StoreRevision {
        match self {
            Self::Enrollment { revision, .. } | Self::ClientBinding { revision, .. } => *revision,
            Self::Revocation { receipt, .. } => receipt.revision,
        }
    }
    /// Checks identity agreement, not cryptography or current authorization.
    pub fn validate(&self) -> Result<(), PairingContractError> {
        require(self.revision().get() != 0)?;
        match self {
            Self::Enrollment {
                target,
                keys,
                client,
                enrollment,
                ..
            } => {
                target.validate()?;
                keys.validate()?;
                client.validate()?;
                require(
                    client.0.client.pairing == target.0.pairing
                        && enrollment.id.stream().pairing() == target.0.pairing
                        && enrollment.id.stream().client() == client.0.client.id
                        && enrollment.id.stream().key() != client.0.ordinary.key()
                        && enrollment.id.stream().key() != client.0.self_revocation.key(),
                )
            }
            Self::ClientBinding {
                target,
                keys,
                client,
                operation,
                ..
            } => {
                target.validate()?;
                keys.validate()?;
                client.validate()?;
                require(
                    client.0.client.pairing == target.0.pairing
                        && operation.id.stream().pairing() == target.0.pairing,
                )
            }
            Self::Revocation {
                receipt,
                revocation_event,
                ..
            } => {
                nonzero(revocation_event)?;
                nonzero(&receipt.server.bytes())?;
                nonzero(&receipt.device.bytes())?;
                require(
                    receipt.pairing.server() == receipt.server
                        && receipt.operation.stream().server() == receipt.server,
                )
            }
        }
    }
    /// Checks historical identities without requiring live authorization.
    pub fn fits_marks(&self, marks: IdentityHighWater) -> bool {
        match self {
            Self::Enrollment {
                target,
                client,
                enrollment,
                ..
            } => {
                target.0.pairing.local() <= marks.pairing
                    && client.fits_marks(marks)
                    && stream_fits(enrollment.id.stream(), marks)
            }
            Self::ClientBinding {
                target,
                client,
                operation,
                ..
            } => {
                target.0.pairing.local() <= marks.pairing
                    && client.fits_marks(marks)
                    && stream_fits(operation.id.stream(), marks)
            }
            Self::Revocation { receipt, .. } => {
                receipt.pairing.local() <= marks.pairing
                    && stream_fits(receipt.operation.stream(), marks)
            }
        }
    }
}

/// Exact received canonical bytes are retained separately from decoded claims.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct SignedPairingReceiptParts {
    pub schema: u16,
    pub claims: PairingReceiptBody,
    pub canonical_body: Vec<u8>,
    pub signature: Vec<u8>,
}
checked_record!(
    SignedPairingReceipt,
    SignedPairingReceiptParts,
    "SignedPairingReceiptParts",
    "Bounded signed receipt data. Construction does not establish verification.",
    |p: &SignedPairingReceiptParts| {
        require(p.schema == 1 && !p.canonical_body.is_empty() && p.signature.len() == 64)?;
        limit(
            p.canonical_body
                .len()
                .checked_add(p.signature.len())
                .ok_or(PairingContractError::LimitExceeded)?,
            MAX_SIGNED_PAIRING_RECEIPT_BYTES,
        )?;
        p.claims.validate()
    }
);

/// Injected pure receipt codec/verification boundary, implemented by pairing_adapter.
///
/// The implementation must preflight and decode canonical_body, compare every
/// decoded claim with claims, check schema/domain, and verify the Ed25519 signature
/// against the original pinned Server key over the exact received bytes. It must
/// recompute RevocationReceipt.receipt_digest from those bytes. It performs no I/O,
/// reads no clock, changes no authorization and contains no plaintext credentials.
pub trait PairingReceiptValidator: Send + Sync {
    fn validate_receipt(&self, receipt: &SignedPairingReceipt) -> Result<(), PairingContractError>;
}

/// Additive durable authorization projection around the unchanged Wave 1 record.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingAuthorizationParts {
    pub record: PairingRecord,
    pub authorization_version: FieldVersion,
    pub keys: PairingKeys,
    pub clients: Vec<PairingClientStreams>,
    pub revocation_receipt: Option<SignedPairingReceipt>,
}
checked_record!(
    PairingAuthorization,
    PairingAuthorizationParts,
    "PairingAuthorizationParts",
    "Immutable lifetime, public-key roles and per-Client stream bindings.",
    |p: &PairingAuthorizationParts| {
        p.keys.validate()?;
        nonzero(&p.record.device.bytes())?;
        require(p.authorization_version.get() != 0 && !p.clients.is_empty())?;
        limit(p.clients.len(), MAX_CLIENTS_PER_PAIRING)?;
        let mut clients = BTreeSet::new();
        let mut streams = BTreeSet::new();
        for binding in &p.clients {
            binding.validate()?;
            require(
                binding.0.client.pairing == p.record.id
                    && clients.insert(binding.0.client.id)
                    && streams.insert(binding.0.ordinary.key())
                    && streams.insert(binding.0.self_revocation.key()),
            )?;
        }
        require(
            p.clients
                .windows(2)
                .all(|pair| pair[0].0.client.id < pair[1].0.client.id),
        )?;
        match (p.record.state, &p.revocation_receipt) {
            (PairingState::Authorized, None) => Ok(()),
            (PairingState::Revoked { revision }, Some(proof)) => {
                proof.validate()?;
                match &proof.0.claims {
                    PairingReceiptBody::Revocation { receipt, .. } => require(
                        receipt.server == p.record.id.server()
                            && receipt.pairing == p.record.id
                            && receipt.device == p.record.device
                            && receipt.revision == revision,
                    ),
                    _ => Err(PairingContractError::WrongBinding),
                }
            }
            _ => Err(PairingContractError::WrongBinding),
        }
    }
);

/// Opaque single-use ticket identity. It is not its bearer secret.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnrollmentTicketParts {
    pub server: ServerId,
    pub nonce: [u8; 16],
}
checked_record!(
    EnrollmentTicketId,
    EnrollmentTicketParts,
    "EnrollmentTicketParts",
    "Server-bound random ticket comparison identity, never enrollment authority.",
    |p: &EnrollmentTicketParts| {
        nonzero(&p.server.bytes())?;
        nonzero(&p.nonce)
    }
);

/// Public proposal used before prospective identities have been reserved.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnrollmentProposalParts {
    pub server: ServerId,
    pub device: DeviceId,
    pub keys: PairingKeys,
    pub claim_nonce: [u8; 32],
}
checked_record!(
    EnrollmentProposal,
    EnrollmentProposalParts,
    "EnrollmentProposalParts",
    "Public enrollment proposal. Proofs and bearer authorization remain transient.",
    |p: &EnrollmentProposalParts| {
        nonzero(&p.server.bytes())?;
        nonzero(&p.device.bytes())?;
        nonzero(&p.claim_nonce)?;
        p.keys.validate()
    }
);

/// Issuer comparison data, never an EnrollmentAuthority capability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EnrollmentIssuer {
    Paired {
        operation: OperationBinding,
        authorization_version: FieldVersion,
    },
    AccountOwner {
        /// Exact NativeRequestId::as_bytes(), not a generated surrogate identity.
        native_request: [u8; 16],
        native_request_digest: RequestDigest,
    },
}

/// Terminal reservation phases; invalidation cannot restore a consumed ticket.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EnrollmentReservationKind {
    Reserved,
    Consumed,
    Invalidated,
}
/// Consumption retains the exact original result for historical lookup.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EnrollmentReservationState {
    Reserved,
    Consumed {
        enrollment: OperationBinding,
        receipt: Box<SignedPairingReceipt>,
    },
    Invalidated,
}
impl EnrollmentReservationState {
    /// Returns comparison phase without granting a resumed authorization.
    pub fn kind(&self) -> EnrollmentReservationKind {
        match self {
            Self::Reserved => EnrollmentReservationKind::Reserved,
            Self::Consumed { .. } => EnrollmentReservationKind::Consumed,
            Self::Invalidated => EnrollmentReservationKind::Invalidated,
        }
    }
}

/// Prospective identities and enrollment stream become durable before submission.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnrollmentReservationParts {
    pub ticket: EnrollmentTicketId,
    pub target: PairingTarget,
    pub keys: PairingKeys,
    pub initial_client: ClientId,
    pub enrollment_stream: OperationStreamId,
    pub claim_digest: RequestDigest,
    pub authorization_digest: RequestDigest,
    pub enrollment_request_digest: RequestDigest,
    pub issuer: EnrollmentIssuer,
    pub issued_epoch: ServerEpoch,
    pub issued_revision: StoreRevision,
    pub issued_at_millis: u64,
    pub expires_at_millis: u64,
    pub state: EnrollmentReservationState,
}
checked_record!(
    EnrollmentReservation,
    EnrollmentReservationParts,
    "EnrollmentReservationParts",
    "Durable prospective identity reservation. No reserved identity authenticates ordinarily.",
    |p: &EnrollmentReservationParts| {
        p.ticket.validate()?;
        p.target.validate()?;
        p.keys.validate()?;
        require(
            p.ticket.0.server == p.target.0.server
                && p.initial_client.server() == p.target.0.server
                && p.enrollment_stream.pairing() == p.target.0.pairing
                && p.enrollment_stream.client() == p.initial_client
                && p.issued_epoch.get() != 0
                && p.issued_revision.get() != 0,
        )?;
        let lifetime = p
            .expires_at_millis
            .checked_sub(p.issued_at_millis)
            .ok_or(PairingContractError::Invalid)?;
        require(lifetime != 0 && lifetime <= MAX_ENROLLMENT_LIFETIME_MILLIS)?;
        match &p.issuer {
            EnrollmentIssuer::Paired {
                operation,
                authorization_version,
            } => require(
                operation.id.stream().server() == p.target.0.server
                    && authorization_version.get() != 0,
            )?,
            EnrollmentIssuer::AccountOwner { native_request, .. } => nonzero(native_request)?,
        }
        if let EnrollmentReservationState::Consumed {
            enrollment,
            receipt,
        } = &p.state
        {
            receipt.validate()?;
            require(
                enrollment.id.stream() == p.enrollment_stream
                    && enrollment.digest == p.enrollment_request_digest,
            )?;
            match &receipt.0.claims {
                PairingReceiptBody::Enrollment {
                    target,
                    keys,
                    client,
                    enrollment: original,
                    revision,
                } => {
                    require(
                        target == &p.target
                            && keys == &p.keys
                            && client.0.client.id == p.initial_client
                            && original == enrollment
                            && *revision >= p.issued_revision,
                    )?;
                }
                _ => return Err(PairingContractError::WrongBinding),
            }
        }
        Ok(())
    }
);

/// Exact assessed lifetime and immutable Client set, without Session retirement.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnpairAssessmentParts {
    pub reference: AssessmentRef,
    pub target: PairingTarget,
    pub authorization_version: FieldVersion,
    pub clients: Vec<ClientRecord>,
}
checked_record!(
    UnpairAssessment,
    UnpairAssessmentParts,
    "UnpairAssessmentParts",
    "Comparison-only unpair assessment. The actor revalidates it before commit.",
    |p: &UnpairAssessmentParts| {
        p.target.validate()?;
        require(
            p.reference.operation.stream().server() == p.target.0.server
                && p.authorization_version.get() != 0
                && !p.clients.is_empty(),
        )?;
        limit(p.clients.len(), MAX_CLIENTS_PER_PAIRING)?;
        require(
            p.clients.iter().all(|client| {
                client.pairing == p.target.0.pairing && client.id.server() == p.target.0.server
            }) && p.clients.windows(2).all(|pair| pair[0].id < pair[1].id),
        )
    }
);

/// Domain pairing requests retained and encoded by the one operation ledger.
/// Bearer authorization, fingerprint-confirmation capabilities and signature
/// proofs are verified at the restricted admission boundary, never retained here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PairingCommand {
    IssueDirectAuthorization {
        proposal: EnrollmentProposal,
    },
    Enroll {
        ticket: EnrollmentTicketId,
        claim_digest: RequestDigest,
    },
    AssessUnpair {
        target: PairingTarget,
    },
    UnpairConfirmed {
        assessment: UnpairAssessment,
    },
    SelfRevoke {
        target: PairingTarget,
    },
    BindAdditionalClient {
        target: PairingTarget,
        expected_authorization_version: FieldVersion,
        claim_digest: RequestDigest,
    },
}
impl PairingCommand {
    /// Returns the explicit owning Server.
    pub fn server(&self) -> ServerId {
        match self {
            Self::IssueDirectAuthorization { proposal } => proposal.0.server,
            Self::Enroll { ticket, .. } => ticket.0.server,
            Self::AssessUnpair { target }
            | Self::SelfRevoke { target }
            | Self::BindAdditionalClient { target, .. } => target.0.server,
            Self::UnpairConfirmed { assessment } => assessment.0.target.0.server,
        }
    }
    /// Structural scope only; immutable ledger purpose is checked separately.
    pub fn validate_scope(&self, stream: OperationStreamId) -> Result<(), PairingContractError> {
        require(self.server() == stream.server())?;
        match self {
            Self::IssueDirectAuthorization { proposal } => proposal.validate(),
            Self::Enroll { ticket, .. } => ticket.validate(),
            Self::AssessUnpair { target } => target.validate(),
            Self::UnpairConfirmed { assessment } => {
                assessment.validate()?;
                require(assessment.0.reference.operation.stream() == stream)
            }
            Self::SelfRevoke { target } => {
                target.validate()?;
                require(target.0.pairing == stream.pairing())
            }
            Self::BindAdditionalClient {
                target,
                expected_authorization_version,
                ..
            } => {
                target.validate()?;
                require(
                    target.0.pairing == stream.pairing()
                        && expected_authorization_version.get() != 0,
                )
            }
        }
    }
}

/// Classified public pairing failures, without native text or sensitive values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PairingFailure {
    InvalidRequest,
    LimitExceeded,
    IdentityMismatch,
    FingerprintMismatch,
    AuthorizationRequired,
    AuthorizationDenied,
    AuthorizationUnavailable,
    PairingRevoked,
    ClientNotBound,
    StaleAssessment,
    AuthorizationExpired,
    AuthorizationConsumed,
    RequestIdentityReused,
    Offline,
    RouteChanged,
    TimedOut,
    Cancelled,
    Incompatible,
    UnsupportedTarget,
    ServiceManagerUnavailable,
    ExplicitStartRequired,
    CredentialUnavailable,
    CredentialIntegrity,
    StorageUnavailable,
    UnsafeStorage,
    UnknownSchema,
    InvalidDurableState,
    RecoveryRequired,
    ReceiptInvalid,
    RevocationUnconfirmed,
}

/// Compact results contain no recoverable bearer authorization.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PairingOutcome {
    AuthorizationReserved {
        ticket: EnrollmentTicketId,
        enrollment: OperationId,
        claim_digest: RequestDigest,
    },
    Enrolled(SignedPairingReceipt),
    ClientBound(SignedPairingReceipt),
    UnpairAssessed(UnpairAssessment),
    Revoked(SignedPairingReceipt),
    AlreadyRevoked(SignedPairingReceipt),
    Failed(PairingFailure),
}
impl PairingOutcome {
    /// Checks historical scope without interpreting a receipt as current access.
    pub fn validate_scope(&self, stream: OperationStreamId) -> Result<(), PairingContractError> {
        match self {
            Self::AuthorizationReserved {
                ticket, enrollment, ..
            } => {
                ticket.validate()?;
                require(
                    ticket.0.server == stream.server()
                        && enrollment.stream().server() == stream.server(),
                )
            }
            Self::Enrolled(proof) => {
                proof.validate()?;
                match &proof.0.claims {
                    PairingReceiptBody::Enrollment { enrollment, .. } => {
                        require(enrollment.id.stream() == stream)
                    }
                    _ => Err(PairingContractError::WrongBinding),
                }
            }
            Self::ClientBound(proof) => {
                proof.validate()?;
                match &proof.0.claims {
                    PairingReceiptBody::ClientBinding { operation, .. } => {
                        require(operation.id.stream() == stream)
                    }
                    _ => Err(PairingContractError::WrongBinding),
                }
            }
            Self::UnpairAssessed(assessment) => {
                assessment.validate()?;
                require(assessment.0.reference.operation.stream() == stream)
            }
            Self::Revoked(proof) | Self::AlreadyRevoked(proof) => {
                proof.validate()?;
                match &proof.0.claims {
                    PairingReceiptBody::Revocation { receipt, .. } => {
                        require(receipt.server == stream.server())
                    }
                    _ => Err(PairingContractError::WrongBinding),
                }
            }
            Self::Failed(_) => Ok(()),
        }
    }
    /// Checks all retained identities against durable high-water facts.
    pub fn fits_marks(&self, marks: IdentityHighWater) -> bool {
        match self {
            Self::AuthorizationReserved { enrollment, .. } => {
                stream_fits(enrollment.stream(), marks)
            }
            Self::Enrolled(proof)
            | Self::ClientBound(proof)
            | Self::Revoked(proof)
            | Self::AlreadyRevoked(proof) => proof.0.claims.fits_marks(marks),
            Self::UnpairAssessed(assessment) => {
                assessment.0.target.0.pairing.local() <= marks.pairing
                    && stream_fits(assessment.0.reference.operation.stream(), marks)
                    && assessment
                        .0
                        .clients
                        .iter()
                        .all(|client| client.id.local() <= marks.client)
            }
            Self::Failed(_) => true,
        }
    }
}

/// Typed pairing mutations merged into the one durable server transaction.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PairingWrite {
    PutAuthorization {
        expected_authorization_version: Option<FieldVersion>,
        value: PairingAuthorization,
    },
    PutEnrollmentReservation {
        expected_kind: Option<EnrollmentReservationKind>,
        value: EnrollmentReservation,
    },
}
/// Checked bounded write collection. Domain transitions remain policy-owned.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingWritesParts {
    pub writes: Vec<PairingWrite>,
}
checked_record!(
    PairingWrites,
    PairingWritesParts,
    "PairingWritesParts",
    "Pairing write slot. Store version/state comparisons occur in the physical transaction.",
    |p: &PairingWritesParts| {
        limit(p.writes.len(), MAX_PAIRING_LIFETIMES)?;
        let mut pairings = BTreeSet::new();
        let mut tickets = BTreeSet::new();
        for write in &p.writes {
            match write {
                PairingWrite::PutAuthorization {
                    expected_authorization_version,
                    value,
                } => {
                    value.validate()?;
                    require(pairings.insert(value.0.record.id))?;
                    match expected_authorization_version {
                        None => require(
                            value.0.authorization_version.get() == 1
                                && value.0.record.state == PairingState::Authorized,
                        )?,
                        Some(previous) => require(
                            previous.checked_successor() == Some(value.0.authorization_version),
                        )?,
                    }
                }
                PairingWrite::PutEnrollmentReservation {
                    expected_kind,
                    value,
                } => {
                    value.validate()?;
                    require(tickets.insert((value.0.ticket.0.server, value.0.ticket.0.nonce)))?;
                    if expected_kind.is_none() {
                        require(value.0.state.kind() == EnrollmentReservationKind::Reserved)?;
                    }
                }
            }
        }
        Ok(())
    }
);
impl Default for PairingWrites {
    fn default() -> Self {
        Self(PairingWritesParts { writes: Vec::new() })
    }
}
impl PairingWrites {
    /// Validates scope, revisions and allocation facts before native commit admission.
    pub fn validate_for(
        &self,
        server: ServerId,
        revision: StoreRevision,
        marks: IdentityHighWater,
    ) -> Result<(), PairingContractError> {
        self.validate()?;
        for write in &self.0.writes {
            match write {
                PairingWrite::PutAuthorization { value, .. } => {
                    require(
                        value.0.record.id.server() == server
                            && value.0.record.id.local() <= marks.pairing
                            && value
                                .0
                                .clients
                                .iter()
                                .all(|client| client.fits_marks(marks)),
                    )?;
                    if let Some(proof) = &value.0.revocation_receipt {
                        require(
                            proof.0.claims.revision() <= revision
                                && proof.0.claims.fits_marks(marks),
                        )?;
                    }
                }
                PairingWrite::PutEnrollmentReservation { value, .. } => {
                    require(
                        value.0.target.0.server == server
                            && value.0.target.0.pairing.local() <= marks.pairing
                            && value.0.initial_client.local() <= marks.client
                            && stream_fits(value.0.enrollment_stream, marks)
                            && value.0.issued_revision <= revision,
                    )?;
                    if let EnrollmentIssuer::Paired { operation, .. } = &value.0.issuer {
                        require(stream_fits(operation.id.stream(), marks))?;
                    }
                    if let EnrollmentReservationState::Consumed { receipt, .. } = &value.0.state {
                        require(
                            receipt.0.claims.revision() <= revision
                                && receipt.0.claims.fits_marks(marks),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
    /// Requires exact-byte receipt verification before the authoritative store commit.
    pub fn validate_receipts(
        &self,
        validator: &dyn PairingReceiptValidator,
    ) -> Result<(), PairingContractError> {
        for write in &self.0.writes {
            match write {
                PairingWrite::PutAuthorization { value, .. } => {
                    if let Some(receipt) = &value.0.revocation_receipt {
                        validator.validate_receipt(receipt)?;
                    }
                }
                PairingWrite::PutEnrollmentReservation { value, .. } => {
                    if let EnrollmentReservationState::Consumed { receipt, .. } = &value.0.state {
                        validator.validate_receipt(receipt)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// All server pairing facts recovered through OrganizationStore, never a downcast.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingRecoveryParts {
    pub authorizations: Vec<PairingAuthorization>,
    pub enrollments: Vec<EnrollmentReservation>,
}
checked_record!(
    PairingRecovery,
    PairingRecoveryParts,
    "PairingRecoveryParts",
    "Bounded complete pairing recovery projection; an empty projection grants no access.",
    |p: &PairingRecoveryParts| {
        limit(p.authorizations.len(), MAX_PAIRING_LIFETIMES)?;
        limit(p.enrollments.len(), MAX_PAIRING_LIFETIMES)?;
        let mut pairings = BTreeSet::new();
        let mut lifetimes = BTreeSet::new();
        let mut active_devices = BTreeSet::new();
        let mut clients = BTreeSet::new();
        let mut streams = BTreeSet::new();
        let mut tickets = BTreeSet::new();
        let mut prospective = BTreeSet::new();
        for authorization in &p.authorizations {
            authorization.validate()?;
            let a = &authorization.0;
            require(pairings.insert(a.record.id))?;
            lifetimes.insert(a.record.id);
            if a.record.state == PairingState::Authorized {
                require(active_devices.insert((a.record.id.server(), a.record.device)))?;
            }
            for client in &a.clients {
                require(
                    clients.insert(client.0.client.id)
                        && streams.insert(client.0.ordinary.key())
                        && streams.insert(client.0.self_revocation.key()),
                )?;
            }
        }
        limit(active_devices.len(), MAX_ACTIVE_PAIRINGS)?;
        let mut outstanding = 0usize;
        for enrollment in &p.enrollments {
            enrollment.validate()?;
            let e = &enrollment.0;
            require(
                tickets.insert((e.ticket.0.server, e.ticket.0.nonce))
                    && prospective.insert(e.target.0.pairing)
                    && streams.insert(e.enrollment_stream.key()),
            )?;
            lifetimes.insert(e.target.0.pairing);
            if let EnrollmentReservationState::Consumed { receipt, .. } = &e.state {
                let authorization = p
                    .authorizations
                    .iter()
                    .find(|a| a.0.record.id == e.target.0.pairing)
                    .ok_or(PairingContractError::WrongBinding)?;
                require(
                    authorization.0.record.device == e.target.0.device
                        && authorization.0.keys == e.keys,
                )?;
                if let PairingReceiptBody::Enrollment { client, .. } = &receipt.0.claims {
                    require(authorization.0.clients.contains(client))?;
                }
            } else {
                require(
                    !pairings.contains(&e.target.0.pairing) && clients.insert(e.initial_client),
                )?;
            }
            if e.state.kind() == EnrollmentReservationKind::Reserved {
                outstanding += 1;
            }
        }
        limit(outstanding, MAX_OUTSTANDING_ENROLLMENTS)?;
        limit(lifetimes.len(), MAX_PAIRING_LIFETIMES)?;
        limit(streams.len(), 4096)?;
        Ok(())
    }
);
impl Default for PairingRecovery {
    fn default() -> Self {
        Self(PairingRecoveryParts {
            authorizations: Vec::new(),
            enrollments: Vec::new(),
        })
    }
}
impl PairingRecovery {
    /// Structural recovery validation with the enclosing store's exact comparison facts.
    pub fn validate_for(
        &self,
        server: ServerId,
        revision: StoreRevision,
        marks: IdentityHighWater,
    ) -> Result<(), PairingContractError> {
        self.validate()?;
        for authorization in &self.0.authorizations {
            let a = &authorization.0;
            require(
                a.record.id.server() == server
                    && a.record.id.local() <= marks.pairing
                    && a.clients.iter().all(|client| client.fits_marks(marks)),
            )?;
            if let Some(proof) = &a.revocation_receipt {
                require(proof.0.claims.revision() <= revision && proof.0.claims.fits_marks(marks))?;
            }
        }
        for enrollment in &self.0.enrollments {
            let e = &enrollment.0;
            require(
                e.target.0.server == server
                    && e.target.0.pairing.local() <= marks.pairing
                    && e.initial_client.local() <= marks.client
                    && stream_fits(e.enrollment_stream, marks)
                    && e.issued_revision <= revision,
            )?;
            if let EnrollmentIssuer::Paired { operation, .. } = &e.issuer {
                require(stream_fits(operation.id.stream(), marks))?;
            }
            if let EnrollmentReservationState::Consumed { receipt, .. } = &e.state {
                require(
                    receipt.0.claims.revision() <= revision && receipt.0.claims.fits_marks(marks),
                )?;
            }
        }
        Ok(())
    }
    /// Required before trait recovery exposes signed facts as validated state.
    pub fn validate_receipts(
        &self,
        validator: &dyn PairingReceiptValidator,
    ) -> Result<(), PairingContractError> {
        for authorization in &self.0.authorizations {
            if let Some(receipt) = &authorization.0.revocation_receipt {
                validator.validate_receipt(receipt)?;
            }
        }
        for enrollment in &self.0.enrollments {
            if let EnrollmentReservationState::Consumed { receipt, .. } = &enrollment.0.state {
                validator.validate_receipt(receipt)?;
            }
        }
        Ok(())
    }
}

/// Local pairing incarnation; never inferred from a later saved connection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalPairingGeneration(NonZeroU64);
impl LocalPairingGeneration {
    /// Rejects zero; the one client journal advances generations without reuse.
    pub fn new(value: u64) -> Result<Self, PairingContractError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(PairingContractError::Invalid)
    }
    /// Returns the exact retained comparison generation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
    /// Exhaustion rejects a new local binding before credential creation.
    pub fn checked_successor(self) -> Option<Self> {
        self.get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
    }
}
/// Exact journal suppression and attachment-cleanup target.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalPairingKeyParts {
    pub server: ServerId,
    pub pairing: PairingId,
    pub generation: LocalPairingGeneration,
}
checked_record!(
    LocalPairingKey,
    LocalPairingKeyParts,
    "LocalPairingKeyParts",
    "Exact original saved pairing incarnation, not a server-global removal target.",
    |p: &LocalPairingKeyParts| require(p.pairing.server() == p.server)
);

/// Canonical task payload shared by pairing and client-core's journal/scheduler.
/// Phase, notice visibility and retry metadata are separate client journal records.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct PendingRevocationTaskParts {
    pub original: PendingSelfRevocation,
    pub local_pairing: LocalPairingKey,
    pub pinned_server_public_key: [u8; 32],
    pub request_digest: RequestDigest,
}
checked_record!(
    PendingRevocationTask,
    PendingRevocationTaskParts,
    "PendingRevocationTaskParts",
    "Restricted original-pairing task. It cannot install, Start, update or enroll.",
    |p: &PendingRevocationTaskParts| {
        p.local_pairing.validate()?;
        nonzero(&p.pinned_server_public_key)?;
        require(
            p.original.server() == p.local_pairing.0.server
                && p.original.pairing() == p.local_pairing.0.pairing
                && !p.original.notice_dismissed(),
        )
    }
);
