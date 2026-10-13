//! Pairing-owned pure codecs. Native authorization and enrollment/revocation
//! orchestration are not supplied by this declaration freeze.

use crate::ProtocolError;
use crate::authentication::Principal;
use crate::limits::NegotiatedLimits;
use crate::wire::{common, pairing};

/// The pairing owner validates exact target, operation/digest, direction,
/// receipt bytes and restricted purpose. No ordinary connection is returned.
pub trait RestrictedExtensionCodec: Send + Sync {
    fn encode_restricted_extension(
        &self,
        payload: &pairing::RestrictedExtension,
        principal: Principal,
        direction: common::Direction,
        limits: NegotiatedLimits,
    ) -> Result<Vec<u8>, ProtocolError>;
    fn decode_restricted_extension(
        &self,
        bytes: &[u8],
        principal: Principal,
        direction: common::Direction,
        limits: NegotiatedLimits,
    ) -> Result<pairing::RestrictedExtension, ProtocolError>;
}

use crate::authentication::{self, ServerPin, SigningKey};
use crate::{
    negotiation::{Negotiator, SUPPORTED_MAJORS, validate_advertisement},
    preflight,
};
use prost::Message;
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use spaceterm_core::{
    authority::StoreRevision,
    identity::*,
    operations::{OperationBinding, OperationId, OperationSequence, RequestDigest},
    pairing::*,
};
use std::sync::Arc;
use zeroize::{Zeroize, Zeroizing};

fn shape<T>(value: Result<T, PairingContractError>) -> Result<T, ProtocolError> {
    value.map_err(|_| ProtocolError::Malformed)
}
fn needed<T>(value: Option<T>) -> Result<T, ProtocolError> {
    value.ok_or(ProtocolError::Malformed)
}
fn array<const N: usize>(bytes: &[u8]) -> Result<[u8; N], ProtocolError> {
    bytes.try_into().map_err(|_| ProtocolError::Malformed)
}
fn hash(bytes: &[u8]) -> RequestDigest {
    let mut result = [0; 32];
    result.copy_from_slice(digest::digest(&digest::SHA256, bytes).as_ref());
    RequestDigest::from_bytes(result)
}
fn ident(bytes: [u8; 16]) -> common::Identity {
    authentication::identity(bytes)
}
fn local_ident(local: u64) -> common::Identity {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&local.to_be_bytes());
    ident(bytes)
}
fn local(value: Option<&common::Identity>) -> Result<u64, ProtocolError> {
    let bytes = authentication::get_identity(value)?;
    if bytes[..8] != [0; 8] {
        return Err(ProtocolError::Malformed);
    }
    Ok(u64::from_be_bytes(array(&bytes[8..])?))
}
fn entity(server: ServerId, local: u64) -> common::Entity {
    common::Entity {
        server: Some(ident(server.bytes())),
        local_id: local,
    }
}
fn entity_local(value: Option<&common::Entity>, server: ServerId) -> Result<u64, ProtocolError> {
    let v = value.ok_or(ProtocolError::Malformed)?;
    if authentication::get_identity(v.server.as_ref())? != server.bytes() || v.local_id == 0 {
        return Err(ProtocolError::Malformed);
    }
    Ok(v.local_id)
}
pub fn operation_to_wire(id: OperationId) -> common::OperationId {
    common::OperationId {
        server: Some(ident(id.stream().server().bytes())),
        pairing: Some(local_ident(id.stream().pairing().local())),
        client: Some(local_ident(id.stream().client().local())),
        stream: Some(local_ident(id.stream().key().local())),
        sequence: id.sequence().get(),
    }
}
pub fn operation_from_wire(v: &common::OperationId) -> Result<OperationId, ProtocolError> {
    let server = ServerId::new(authentication::get_identity(v.server.as_ref())?);
    let pairing =
        PairingId::new(server, local(v.pairing.as_ref())?).map_err(|_| ProtocolError::Malformed)?;
    let client =
        ClientId::new(server, local(v.client.as_ref())?).map_err(|_| ProtocolError::Malformed)?;
    let key = OperationStreamKey::new(server, local(v.stream.as_ref())?)
        .map_err(|_| ProtocolError::Malformed)?;
    Ok(OperationId::new(
        OperationStreamId::new(key, pairing, client).map_err(|_| ProtocolError::Malformed)?,
        OperationSequence::new(v.sequence).map_err(|_| ProtocolError::Malformed)?,
    ))
}
pub fn target_to_wire(target: &PairingTarget) -> pairing::Target {
    let p = target.parts();
    pairing::Target {
        server: Some(ident(p.server.bytes())),
        device: Some(ident(p.device.bytes())),
        pairing: Some(entity(p.server, p.pairing.local())),
    }
}
pub fn target_from_wire(v: &pairing::Target) -> Result<PairingTarget, ProtocolError> {
    let server = ServerId::new(authentication::get_identity(v.server.as_ref())?);
    shape(PairingTarget::new(PairingTargetParts {
        server,
        device: DeviceId::new(authentication::get_identity(v.device.as_ref())?),
        pairing: PairingId::new(server, entity_local(v.pairing.as_ref(), server)?)
            .map_err(|_| ProtocolError::Malformed)?,
    }))
}
fn keys_to_wire(keys: &PairingKeys) -> pairing::Keys {
    pairing::Keys {
        device_identity_public_key: keys.parts().device_identity_public_key.to_vec(),
        pairing_authentication_public_key: keys.parts().pairing_authentication_public_key.to_vec(),
    }
}
fn keys_from_wire(v: &pairing::Keys) -> Result<PairingKeys, ProtocolError> {
    shape(PairingKeys::new(PairingKeysParts {
        device_identity_public_key: array(&v.device_identity_public_key)?,
        pairing_authentication_public_key: array(&v.pairing_authentication_public_key)?,
    }))
}
pub fn proposal_to_wire(proposal: &EnrollmentProposal) -> pairing::Proposal {
    let p = proposal.parts();
    pairing::Proposal {
        server: Some(ident(p.server.bytes())),
        device: Some(ident(p.device.bytes())),
        keys: Some(keys_to_wire(&p.keys)),
        claim_nonce: p.claim_nonce.to_vec(),
    }
}
/// An authenticated issuer can propose another device without possessing its
/// keys. Possession is verified by both restricted enrollment proofs, not here.
pub fn proposal_from_wire(wire: &pairing::Proposal) -> Result<EnrollmentProposal, ProtocolError> {
    if wire.encoded_len() > MAX_PAIRING_MANAGEMENT_BYTES {
        return Err(ProtocolError::LimitExceeded);
    }
    shape(EnrollmentProposal::new(EnrollmentProposalParts {
        server: ServerId::new(authentication::get_identity(wire.server.as_ref())?),
        device: DeviceId::new(authentication::get_identity(wire.device.as_ref())?),
        keys: keys_from_wire(needed(wire.keys.as_ref())?)?,
        claim_nonce: array(&wire.claim_nonce)?,
    }))
}
pub fn enrollment_proposal_digest(
    proposal: &EnrollmentProposal,
) -> Result<RequestDigest, ProtocolError> {
    shape(proposal.validate())?;
    Ok(hash(&proposal_to_wire(proposal).encode_to_vec()))
}
fn stream_to_wire(s: OperationStreamId) -> common::OperationStream {
    common::OperationStream {
        key: Some(entity(s.server(), s.key().local())),
        pairing: Some(entity(s.server(), s.pairing().local())),
        client: Some(entity(s.server(), s.client().local())),
    }
}
fn stream_from_wire(
    v: &common::OperationStream,
    server: ServerId,
) -> Result<OperationStreamId, ProtocolError> {
    OperationStreamId::new(
        OperationStreamKey::new(server, entity_local(v.key.as_ref(), server)?)
            .map_err(|_| ProtocolError::Malformed)?,
        PairingId::new(server, entity_local(v.pairing.as_ref(), server)?)
            .map_err(|_| ProtocolError::Malformed)?,
        ClientId::new(server, entity_local(v.client.as_ref(), server)?)
            .map_err(|_| ProtocolError::Malformed)?,
    )
    .map_err(|_| ProtocolError::Malformed)
}
fn binding_to_wire(c: &PairingClientStreams) -> pairing::Binding {
    let c = c.parts();
    pairing::Binding {
        client: Some(entity(c.client.id.server(), c.client.id.local())),
        pairing: Some(entity(c.client.pairing.server(), c.client.pairing.local())),
        ordinary_stream: Some(stream_to_wire(c.ordinary)),
        self_revocation_stream: Some(stream_to_wire(c.self_revocation)),
    }
}
fn binding_from_wire(
    v: &pairing::Binding,
    server: ServerId,
) -> Result<PairingClientStreams, ProtocolError> {
    shape(PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: ClientId::new(server, entity_local(v.client.as_ref(), server)?)
                .map_err(|_| ProtocolError::Malformed)?,
            pairing: PairingId::new(server, entity_local(v.pairing.as_ref(), server)?)
                .map_err(|_| ProtocolError::Malformed)?,
        },
        ordinary: stream_from_wire(needed(v.ordinary_stream.as_ref())?, server)?,
        self_revocation: stream_from_wire(needed(v.self_revocation_stream.as_ref())?, server)?,
    }))
}
fn ticket_to_wire(t: &EnrollmentTicketId) -> pairing::Ticket {
    pairing::Ticket {
        server: Some(ident(t.parts().server.bytes())),
        ticket_nonce: t.parts().nonce.to_vec(),
    }
}
fn ticket_from_wire(t: &pairing::Ticket) -> Result<EnrollmentTicketId, ProtocolError> {
    shape(EnrollmentTicketId::new(EnrollmentTicketParts {
        server: ServerId::new(authentication::get_identity(t.server.as_ref())?),
        nonce: array(&t.ticket_nonce)?,
    }))
}
/// Public comparison facts delivered to a prospective Client. This record contains
/// no issuer, epoch, expiry, authorization digest or durable reservation state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicEnrollmentReservation(PublicEnrollmentReservationParts);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicEnrollmentReservationParts {
    pub ticket: EnrollmentTicketId,
    pub target: PairingTarget,
    pub keys: PairingKeys,
    pub initial_client: ClientId,
    pub enrollment_stream: OperationStreamId,
    pub claim_digest: RequestDigest,
    pub enrollment_request_digest: RequestDigest,
}
impl PublicEnrollmentReservation {
    pub fn parts(&self) -> &PublicEnrollmentReservationParts {
        &self.0
    }
    /// Decodes only the frozen public wire record, never a durable server record.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let wire = bound_decode(
            preflight::SPACETERM_PAIRING_RESERVATION,
            bytes,
            MAX_PAIRING_MANAGEMENT_BYTES,
            NegotiatedLimits::default(),
        )?;
        Self::from_wire(&wire)
    }
    pub fn from_wire(wire: &pairing::Reservation) -> Result<Self, ProtocolError> {
        let target = target_from_wire(needed(wire.target.as_ref())?)?;
        let server = target.parts().server;
        let ticket = ticket_from_wire(needed(wire.ticket.as_ref())?)?;
        let initial_client =
            ClientId::new(server, entity_local(wire.initial_client.as_ref(), server)?)
                .map_err(|_| ProtocolError::Malformed)?;
        let operation = operation_from_wire(needed(wire.enrollment_operation.as_ref())?)?;
        if ticket.parts().server != server
            || operation.stream().server() != server
            || operation.stream().pairing() != target.parts().pairing
            || operation.stream().client() != initial_client
            || operation.sequence().get() != 1
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(Self(PublicEnrollmentReservationParts {
            ticket,
            target,
            keys: keys_from_wire(needed(wire.keys.as_ref())?)?,
            initial_client,
            enrollment_stream: operation.stream(),
            claim_digest: RequestDigest::from_bytes(array(&wire.claim_digest)?),
            enrollment_request_digest: RequestDigest::from_bytes(array(
                &wire.enrollment_request_digest,
            )?),
        }))
    }
    fn from_reservation(r: &EnrollmentReservation) -> Self {
        let p = r.parts();
        Self(PublicEnrollmentReservationParts {
            ticket: p.ticket.clone(),
            target: p.target.clone(),
            keys: p.keys.clone(),
            initial_client: p.initial_client,
            enrollment_stream: p.enrollment_stream,
            claim_digest: p.claim_digest,
            enrollment_request_digest: p.enrollment_request_digest,
        })
    }
    pub fn to_wire(&self) -> pairing::Reservation {
        let p = self.parts();
        pairing::Reservation {
            ticket: Some(ticket_to_wire(&p.ticket)),
            target: Some(target_to_wire(&p.target)),
            keys: Some(keys_to_wire(&p.keys)),
            initial_client: Some(entity(p.initial_client.server(), p.initial_client.local())),
            enrollment_operation: Some(common::OperationId {
                server: Some(ident(p.enrollment_stream.server().bytes())),
                pairing: Some(local_ident(p.enrollment_stream.pairing().local())),
                client: Some(local_ident(p.enrollment_stream.client().local())),
                stream: Some(local_ident(p.enrollment_stream.key().local())),
                sequence: 1,
            }),
            claim_digest: p.claim_digest.bytes().to_vec(),
            enrollment_request_digest: p.enrollment_request_digest.bytes().to_vec(),
        }
    }
}
pub fn reservation_to_wire(r: &EnrollmentReservation) -> pairing::Reservation {
    PublicEnrollmentReservation::from_reservation(r).to_wire()
}
fn exact_reservation(
    v: Option<&pairing::Reservation>,
    r: &pairing::Reservation,
) -> Result<(), ProtocolError> {
    if needed(v)? != r {
        return Err(ProtocolError::PairingDenied);
    }
    Ok(())
}
fn bound_decode<T: Message + Default>(
    root: usize,
    bytes: &[u8],
    ceiling: usize,
    limits: NegotiatedLimits,
) -> Result<T, ProtocolError> {
    if bytes.len() > ceiling {
        return Err(ProtocolError::LimitExceeded);
    }
    preflight::check(root, bytes, limits)?;
    T::decode(bytes).map_err(|_| ProtocolError::Malformed)
}

/// Exact-byte receipt signing/verification against one original Server pin.
#[derive(Clone, Copy)]
pub struct ReceiptCodec {
    pin: ServerPin,
}
impl ReceiptCodec {
    pub const fn new(pin: ServerPin) -> Self {
        Self { pin }
    }
    pub const fn pin(&self) -> ServerPin {
        self.pin
    }
    pub fn sign(
        &self,
        mut claims: PairingReceiptBody,
        signer: &dyn SigningKey,
    ) -> Result<SignedPairingReceipt, ProtocolError> {
        shape(claims.validate())?;
        if claims.server().bytes() != self.pin.identity
            || signer.public_key() != self.pin.public_key
        {
            return Err(ProtocolError::Authentication);
        }
        let bytes = receipt_body_to_wire(&claims).encode_to_vec();
        if bytes.len() + 64 > MAX_SIGNED_PAIRING_RECEIPT_BYTES {
            return Err(ProtocolError::LimitExceeded);
        }
        if let PairingReceiptBody::Revocation { receipt, .. } = &mut claims {
            receipt.receipt_digest = hash(&bytes);
        }
        let signature = authentication::sign(signer, b"pairing-receipt-v1", &[&bytes])?;
        shape(SignedPairingReceipt::new(SignedPairingReceiptParts {
            schema: 1,
            claims,
            canonical_body: bytes,
            signature: signature.to_vec(),
        }))
    }
    pub fn verify(
        &self,
        receipt: &SignedPairingReceipt,
    ) -> Result<PairingReceiptBody, ProtocolError> {
        shape(receipt.validate())?;
        let p = receipt.parts();
        // Never verify a reconstructed message. Canonical shape is checked only
        // after authenticating the exact received bounded bytes.
        authentication::verify(
            self.pin.public_key,
            b"pairing-receipt-v1",
            &[&p.canonical_body],
            &p.signature,
        )?;
        let wire: pairing::ReceiptBody = bound_decode(
            preflight::SPACETERM_PAIRING_RECEIPTBODY,
            &p.canonical_body,
            MAX_SIGNED_PAIRING_RECEIPT_BYTES - 64,
            NegotiatedLimits::default(),
        )?;
        if wire.encode_to_vec() != p.canonical_body {
            return Err(ProtocolError::Malformed);
        }
        let claims = receipt_body_from_wire(wire, hash(&p.canonical_body))?;
        if claims != p.claims || claims.server().bytes() != self.pin.identity {
            return Err(ProtocolError::Authentication);
        }
        Ok(claims)
    }
    pub fn from_wire(
        &self,
        wire: &pairing::SignedReceipt,
    ) -> Result<SignedPairingReceipt, ProtocolError> {
        if wire.canonical_body.len() + wire.signature.len() > MAX_SIGNED_PAIRING_RECEIPT_BYTES
            || wire.signature.len() != 64
        {
            return Err(ProtocolError::LimitExceeded);
        }
        authentication::verify(
            self.pin.public_key,
            b"pairing-receipt-v1",
            &[&wire.canonical_body],
            &wire.signature,
        )?;
        let body = bound_decode(
            preflight::SPACETERM_PAIRING_RECEIPTBODY,
            &wire.canonical_body,
            MAX_SIGNED_PAIRING_RECEIPT_BYTES - 64,
            NegotiatedLimits::default(),
        )?;
        let claims = receipt_body_from_wire(body, hash(&wire.canonical_body))?;
        let receipt = shape(SignedPairingReceipt::new(SignedPairingReceiptParts {
            schema: 1,
            claims,
            canonical_body: wire.canonical_body.clone(),
            signature: wire.signature.clone(),
        }))?;
        self.verify(&receipt)?;
        Ok(receipt)
    }
}
impl PairingReceiptValidator for ReceiptCodec {
    fn validate_receipt(&self, receipt: &SignedPairingReceipt) -> Result<(), PairingContractError> {
        self.verify(receipt)
            .map(|_| ())
            .map_err(|_| PairingContractError::Invalid)
    }
}
pub fn receipt_to_wire(receipt: &SignedPairingReceipt) -> pairing::SignedReceipt {
    pairing::SignedReceipt {
        canonical_body: receipt.parts().canonical_body.clone(),
        signature: receipt.parts().signature.clone(),
    }
}
fn receipt_body_to_wire(body: &PairingReceiptBody) -> pairing::ReceiptBody {
    use pairing::receipt_body::Kind;
    let kind = match body {
        PairingReceiptBody::Enrollment {
            target,
            keys,
            client,
            enrollment,
            revision,
        } => Kind::Enrollment(pairing::EnrollmentReceiptBody {
            target: Some(target_to_wire(target)),
            keys: Some(keys_to_wire(keys)),
            client: Some(binding_to_wire(client)),
            enrollment_operation: Some(operation_to_wire(enrollment.id)),
            request_digest: enrollment.digest.bytes().to_vec(),
            store_revision: revision.get(),
        }),
        PairingReceiptBody::ClientBinding {
            target,
            keys,
            client,
            operation,
            revision,
        } => Kind::ClientBinding(pairing::ClientBindingReceiptBody {
            target: Some(target_to_wire(target)),
            keys: Some(keys_to_wire(keys)),
            client: Some(binding_to_wire(client)),
            operation: Some(operation_to_wire(operation.id)),
            request_digest: operation.digest.bytes().to_vec(),
            store_revision: revision.get(),
        }),
        PairingReceiptBody::Revocation {
            receipt,
            authentication_key_digest,
            revocation_event,
        } => Kind::Revocation(pairing::RevocationReceiptBody {
            target: Some(pairing::Target {
                server: Some(ident(receipt.server.bytes())),
                device: Some(ident(receipt.device.bytes())),
                pairing: Some(entity(receipt.server, receipt.pairing.local())),
            }),
            authentication_key_digest: authentication_key_digest.bytes().to_vec(),
            revocation_event: revocation_event.to_vec(),
            causing_operation: Some(operation_to_wire(receipt.operation)),
            store_revision: receipt.revision.get(),
        }),
    };
    pairing::ReceiptBody {
        schema: 1,
        kind: Some(kind),
    }
}
fn receipt_body_from_wire(
    body: pairing::ReceiptBody,
    receipt_digest: RequestDigest,
) -> Result<PairingReceiptBody, ProtocolError> {
    if body.schema != 1 {
        return Err(ProtocolError::Malformed);
    }
    use pairing::receipt_body::Kind;
    let result = match needed(body.kind)? {
        Kind::Enrollment(v) => {
            let target = target_from_wire(needed(v.target.as_ref())?)?;
            PairingReceiptBody::Enrollment {
                keys: keys_from_wire(needed(v.keys.as_ref())?)?,
                client: binding_from_wire(needed(v.client.as_ref())?, target.parts().server)?,
                target,
                enrollment: OperationBinding {
                    id: operation_from_wire(needed(v.enrollment_operation.as_ref())?)?,
                    digest: RequestDigest::from_bytes(array(&v.request_digest)?),
                },
                revision: StoreRevision::new(v.store_revision),
            }
        }
        Kind::ClientBinding(v) => {
            let target = target_from_wire(needed(v.target.as_ref())?)?;
            PairingReceiptBody::ClientBinding {
                keys: keys_from_wire(needed(v.keys.as_ref())?)?,
                client: binding_from_wire(needed(v.client.as_ref())?, target.parts().server)?,
                target,
                operation: OperationBinding {
                    id: operation_from_wire(needed(v.operation.as_ref())?)?,
                    digest: RequestDigest::from_bytes(array(&v.request_digest)?),
                },
                revision: StoreRevision::new(v.store_revision),
            }
        }
        Kind::Revocation(v) => {
            let target = target_from_wire(needed(v.target.as_ref())?)?;
            PairingReceiptBody::Revocation {
                receipt: RevocationReceipt {
                    server: target.parts().server,
                    pairing: target.parts().pairing,
                    device: target.parts().device,
                    operation: operation_from_wire(needed(v.causing_operation.as_ref())?)?,
                    revision: StoreRevision::new(v.store_revision),
                    receipt_digest,
                },
                authentication_key_digest: RequestDigest::from_bytes(array(
                    &v.authentication_key_digest,
                )?),
                revocation_event: array(&v.revocation_event)?,
            }
        }
    };
    shape(result.validate())?;
    Ok(result)
}

/// Current and historical reservations are lookup-only. This port cannot enroll,
/// register an ordinary stream, or reactivate a pairing.
pub trait EnrollmentLookup {
    fn lookup(&self, ticket: &EnrollmentTicketId) -> Result<EnrollmentReservation, ProtocolError>;
}
fn random_nonce() -> Result<[u8; 32], ProtocolError> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| ProtocolError::RandomUnavailable)?;
    Ok(bytes)
}
fn majors(values: &[u32]) -> Result<(), ProtocolError> {
    validate_advertisement(values)?;
    if values.iter().any(|v| !SUPPORTED_MAJORS.contains(v)) {
        return Err(ProtocolError::Incompatible);
    }
    Ok(())
}
fn exchange(kind: pairing::enrollment_exchange::Kind) -> Result<Vec<u8>, ProtocolError> {
    let mut message = pairing::EnrollmentExchange { kind: Some(kind) };
    let mut bytes = message.encode_to_vec();
    if let Some(pairing::enrollment_exchange::Kind::Envelope(ref mut signed)) = message.kind {
        signed.body.zeroize();
    }
    if bytes.len() > MAX_PAIRING_MANAGEMENT_BYTES {
        bytes.zeroize();
        return Err(ProtocolError::LimitExceeded);
    }
    Ok(bytes)
}
/// Purpose discrimination before authentication. This grants no authority; the
/// enrollment handshake still verifies the exact reserved claim and both keys.
pub fn is_enrollment_hello(bytes: &[u8]) -> bool {
    matches!(
        read_exchange(bytes),
        Ok(pairing::enrollment_exchange::Kind::Hello(_))
    )
}
fn read_exchange(bytes: &[u8]) -> Result<pairing::enrollment_exchange::Kind, ProtocolError> {
    let message: pairing::EnrollmentExchange = bound_decode(
        preflight::SPACETERM_PAIRING_ENROLLMENTEXCHANGE,
        bytes,
        MAX_PAIRING_MANAGEMENT_BYTES,
        NegotiatedLimits::default(),
    )?;
    needed(message.kind)
}
fn signed(
    signer: &dyn SigningKey,
    domain: &[u8],
    parts: &[&[u8]],
    body: Vec<u8>,
) -> Result<common::Signed, ProtocolError> {
    let signature = authentication::sign(signer, domain, parts)?;
    Ok(common::Signed {
        body,
        signature: signature.to_vec(),
    })
}

pub struct EnrollmentClient {
    pin: ServerPin,
    reservation: PublicEnrollmentReservation,
    majors: Vec<u32>,
    device: Arc<dyn SigningKey>,
    auth: Arc<dyn SigningKey>,
    hello: Vec<u8>,
}
pub struct EnrollmentAwaitingReady {
    client: EnrollmentClient,
    reply: Vec<u8>,
    binding: RequestDigest,
    major: u32,
}
pub struct EnrollmentServer {
    server: [u8; 16],
    reservation: EnrollmentReservation,
    signer: Arc<dyn SigningKey>,
    hello: Vec<u8>,
    reply: Vec<u8>,
    binding: RequestDigest,
    major: u32,
}
impl EnrollmentClient {
    pub fn begin(
        pin: ServerPin,
        reservation: PublicEnrollmentReservation,
        versions: Vec<u32>,
        device: Arc<dyn SigningKey>,
        auth: Arc<dyn SigningKey>,
    ) -> Result<(Self, Vec<u8>), ProtocolError> {
        majors(&versions)?;
        if reservation.parts().target.parts().server.bytes() != pin.identity
            || device.public_key() != reservation.parts().keys.parts().device_identity_public_key
            || auth.public_key()
                != reservation
                    .parts()
                    .keys
                    .parts()
                    .pairing_authentication_public_key
        {
            return Err(ProtocolError::Authentication);
        }
        let hello = pairing::EnrollmentHello {
            reservation: Some(reservation.to_wire()),
            nonce: random_nonce()?.to_vec(),
            majors: versions.clone(),
        }
        .encode_to_vec();
        let outbound = exchange(pairing::enrollment_exchange::Kind::Hello(
            pairing::EnrollmentHello::decode(hello.as_slice())
                .map_err(|_| ProtocolError::Malformed)?,
        ))?;
        Ok((
            Self {
                pin,
                reservation,
                majors: versions,
                device,
                auth,
                hello: outbound.clone(),
            },
            outbound,
        ))
    }
    pub fn receive_server(
        self,
        bytes: &[u8],
    ) -> Result<(EnrollmentAwaitingReady, Vec<u8>), ProtocolError> {
        let reply = match read_exchange(bytes)? {
            pairing::enrollment_exchange::Kind::ServerHello(reply) => reply,
            _ => return Err(ProtocolError::UnexpectedMessage),
        };
        authentication::verify(
            self.pin.public_key,
            b"enrollment-server-hello-v1",
            &[&self.hello, &reply.body],
            &reply.signature,
        )?;
        let v: pairing::EnrollmentServerHello = bound_decode(
            preflight::SPACETERM_PAIRING_ENROLLMENTSERVERHELLO,
            &reply.body,
            MAX_PAIRING_MANAGEMENT_BYTES,
            NegotiatedLimits::default(),
        )?;
        if authentication::get_identity(v.server.as_ref())? != self.pin.identity
            || array::<32>(&v.public_key)? != self.pin.public_key
            || v.purpose != 3
        {
            return Err(ProtocolError::Authentication);
        }
        exact_reservation(v.reservation.as_ref(), &self.reservation.to_wire())?;
        Negotiator::verify(&self.majors, &v.majors, v.selected_major)?;
        let binding = enrollment_binding(&self.hello, &reply.body);
        let proof = pairing::EnrollmentProof {
            reservation: Some(self.reservation.to_wire()),
            transcript: binding.bytes().to_vec(),
            device_identity_proof: authentication::sign(
                self.device.as_ref(),
                b"enrollment-device-proof-v1",
                &[&self.hello, &reply.body],
            )?
            .to_vec(),
            pairing_authentication_proof: authentication::sign(
                self.auth.as_ref(),
                b"enrollment-auth-proof-v1",
                &[&self.hello, &reply.body],
            )?
            .to_vec(),
        };
        Ok((
            EnrollmentAwaitingReady {
                client: self,
                reply: reply.body,
                binding,
                major: v.selected_major,
            },
            exchange(pairing::enrollment_exchange::Kind::Proof(proof))?,
        ))
    }
}
fn enrollment_binding(hello: &[u8], reply: &[u8]) -> RequestDigest {
    let mut bytes = b"SpaceTerm enrollment binding v1\0".to_vec();
    for part in [hello, reply] {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part);
    }
    hash(&bytes)
}
impl EnrollmentServer {
    pub fn accept(
        server: [u8; 16],
        versions: Vec<u32>,
        signer: Arc<dyn SigningKey>,
        lookup: &dyn EnrollmentLookup,
        bytes: &[u8],
    ) -> Result<(Self, Vec<u8>), ProtocolError> {
        majors(&versions)?;
        let hello = match read_exchange(bytes)? {
            pairing::enrollment_exchange::Kind::Hello(v) => v,
            _ => return Err(ProtocolError::UnexpectedMessage),
        };
        let wire = needed(hello.reservation.as_ref())?;
        let reservation = lookup.lookup(&ticket_from_wire(needed(wire.ticket.as_ref())?)?)?;
        if reservation.parts().target.parts().server.bytes() != server {
            return Err(ProtocolError::PairingDenied);
        }
        exact_reservation(Some(wire), &reservation_to_wire(&reservation))?;
        let major = Negotiator::select(&hello.majors, &versions)?;
        let hello = bytes.to_vec();
        let reply = pairing::EnrollmentServerHello {
            server: Some(ident(server)),
            public_key: signer.public_key().to_vec(),
            nonce: random_nonce()?.to_vec(),
            majors: versions,
            selected_major: major,
            reservation: Some(reservation_to_wire(&reservation)),
            purpose: 3,
        }
        .encode_to_vec();
        let outbound = exchange(pairing::enrollment_exchange::Kind::ServerHello(signed(
            signer.as_ref(),
            b"enrollment-server-hello-v1",
            &[&hello, &reply],
            reply.clone(),
        )?))?;
        let binding = enrollment_binding(&hello, &reply);
        Ok((
            Self {
                server,
                reservation,
                signer,
                hello,
                reply,
                binding,
                major,
            },
            outbound,
        ))
    }
    pub fn receive_proof(
        self,
        lookup: &dyn EnrollmentLookup,
        bytes: &[u8],
    ) -> Result<(EnrollmentConnection, Vec<u8>), ProtocolError> {
        let proof = match read_exchange(bytes)? {
            pairing::enrollment_exchange::Kind::Proof(v) => v,
            _ => return Err(ProtocolError::UnexpectedMessage),
        };
        exact_reservation(
            proof.reservation.as_ref(),
            &reservation_to_wire(&self.reservation),
        )?;
        if proof.transcript != self.binding.bytes() {
            return Err(ProtocolError::Authentication);
        }
        let keys = self.reservation.parts().keys.parts();
        authentication::verify(
            keys.device_identity_public_key,
            b"enrollment-device-proof-v1",
            &[&self.hello, &self.reply],
            &proof.device_identity_proof,
        )?;
        authentication::verify(
            keys.pairing_authentication_public_key,
            b"enrollment-auth-proof-v1",
            &[&self.hello, &self.reply],
            &proof.pairing_authentication_proof,
        )?;
        let current = lookup.lookup(&self.reservation.parts().ticket)?;
        exact_reservation(
            Some(&reservation_to_wire(&current)),
            &reservation_to_wire(&self.reservation),
        )?;
        let ready = pairing::EnrollmentReady {
            reservation: Some(reservation_to_wire(&self.reservation)),
            transcript: self.binding.bytes().to_vec(),
            purpose: 3,
        }
        .encode_to_vec();
        let outbound = exchange(pairing::enrollment_exchange::Kind::Ready(signed(
            self.signer.as_ref(),
            b"enrollment-ready-v1",
            &[&self.hello, &self.reply, &ready],
            ready.clone(),
        )?))?;
        let current = lookup.lookup(&self.reservation.parts().ticket)?;
        exact_reservation(
            Some(&reservation_to_wire(&current)),
            &reservation_to_wire(&self.reservation),
        )?;
        let peer_key = keys.pairing_authentication_public_key;
        let pin = ServerPin {
            identity: self.server,
            public_key: self.signer.public_key(),
        };
        Ok((
            EnrollmentConnection::new(
                pin,
                PublicEnrollmentReservation::from_reservation(&self.reservation),
                self.signer,
                peer_key,
                self.binding,
                self.major,
                common::Direction::ServerToClient,
            )
            .with_server_reservation(self.reservation),
            outbound,
        ))
    }
}
impl EnrollmentAwaitingReady {
    pub fn receive_ready(self, bytes: &[u8]) -> Result<EnrollmentConnection, ProtocolError> {
        let ready = match read_exchange(bytes)? {
            pairing::enrollment_exchange::Kind::Ready(v) => v,
            _ => return Err(ProtocolError::UnexpectedMessage),
        };
        authentication::verify(
            self.client.pin.public_key,
            b"enrollment-ready-v1",
            &[&self.client.hello, &self.reply, &ready.body],
            &ready.signature,
        )?;
        let v: pairing::EnrollmentReady = bound_decode(
            preflight::SPACETERM_PAIRING_ENROLLMENTREADY,
            &ready.body,
            MAX_PAIRING_MANAGEMENT_BYTES,
            NegotiatedLimits::default(),
        )?;
        exact_reservation(v.reservation.as_ref(), &self.client.reservation.to_wire())?;
        if v.purpose != 3 || v.transcript != self.binding.bytes() {
            return Err(ProtocolError::Authentication);
        }
        Ok(EnrollmentConnection::new(
            self.client.pin,
            self.client.reservation,
            self.client.auth,
            self.client.pin.public_key,
            self.binding,
            self.major,
            common::Direction::ClientToServer,
        ))
    }
}

/// Purpose-restricted channel. There is deliberately no ordinary connection or
/// conversion to Established. Possession authorizes only this original claim.
pub struct EnrollmentConnection {
    pin: ServerPin,
    reservation: PublicEnrollmentReservation,
    server_reservation: Option<EnrollmentReservation>,
    signer: Arc<dyn SigningKey>,
    peer_key: [u8; 32],
    binding: RequestDigest,
    major: u32,
    direction: common::Direction,
    sent: u64,
    received: u64,
    failed: bool,
}
impl EnrollmentConnection {
    fn new(
        pin: ServerPin,
        reservation: PublicEnrollmentReservation,
        signer: Arc<dyn SigningKey>,
        peer_key: [u8; 32],
        binding: RequestDigest,
        major: u32,
        direction: common::Direction,
    ) -> Self {
        Self {
            pin,
            reservation,
            server_reservation: None,
            signer,
            peer_key,
            binding,
            major,
            direction,
            sent: 0,
            received: 0,
            failed: false,
        }
    }
    fn with_server_reservation(mut self, reservation: EnrollmentReservation) -> Self {
        self.server_reservation = Some(reservation);
        self
    }
    pub const fn major(&self) -> u32 {
        self.major
    }
    pub fn reservation(&self) -> &PublicEnrollmentReservation {
        &self.reservation
    }
    pub fn lookup_message(&mut self) -> Result<Vec<u8>, ProtocolError> {
        self.encode(&pairing::EnrollmentMessage {
            kind: Some(pairing::enrollment_message::Kind::Lookup(
                pairing::EnrollmentLookup {
                    reservation: Some(self.reservation.to_wire()),
                },
            )),
        })
    }
    pub fn encode(
        &mut self,
        message: &pairing::EnrollmentMessage,
    ) -> Result<Vec<u8>, ProtocolError> {
        if self.failed {
            return Err(ProtocolError::Sequence);
        }
        if message.encoded_len() > MAX_PAIRING_MANAGEMENT_BYTES {
            return Err(ProtocolError::LimitExceeded);
        }
        let payload = Zeroizing::new(message.encode_to_vec());
        preflight::check(
            preflight::SPACETERM_PAIRING_ENROLLMENTMESSAGE,
            &payload,
            NegotiatedLimits::default(),
        )?;
        self.validate(message, self.direction)?;
        let sequence = self.sent.checked_add(1).ok_or(ProtocolError::Exhausted)?;
        let mut envelope = common::AuthenticatedEnvelope {
            direction: self.direction as i32,
            major: self.major,
            binding: self.binding.bytes().to_vec(),
            sequence,
            payload: payload.to_vec(),
            purpose: 3,
        };
        let body = Zeroizing::new(envelope.encode_to_vec());
        envelope.payload.zeroize();
        let signature =
            authentication::sign(self.signer.as_ref(), b"authenticated-envelope", &[&body])?;
        let envelope = common::Signed {
            body: body.to_vec(),
            signature: signature.to_vec(),
        };
        let bytes = exchange(pairing::enrollment_exchange::Kind::Envelope(envelope))?;
        self.sent = sequence;
        Ok(bytes)
    }
    pub fn decode(&mut self, bytes: &[u8]) -> Result<pairing::EnrollmentMessage, ProtocolError> {
        if self.failed {
            return Err(ProtocolError::Sequence);
        }
        let result = (|| {
            let mut signed = match read_exchange(bytes)? {
                pairing::enrollment_exchange::Kind::Envelope(v) => v,
                _ => return Err(ProtocolError::UnexpectedMessage),
            };
            let signed_body = Zeroizing::new(std::mem::take(&mut signed.body));
            authentication::verify(
                self.peer_key,
                b"authenticated-envelope",
                &[&signed_body],
                &signed.signature,
            )?;
            let mut envelope: common::AuthenticatedEnvelope = bound_decode(
                preflight::SPACETERM_COMMON_AUTHENTICATEDENVELOPE,
                &signed_body,
                MAX_PAIRING_MANAGEMENT_BYTES,
                NegotiatedLimits::default(),
            )?;
            let payload = Zeroizing::new(std::mem::take(&mut envelope.payload));
            let opposite = if self.direction == common::Direction::ClientToServer {
                common::Direction::ServerToClient
            } else {
                common::Direction::ClientToServer
            };
            let next = self
                .received
                .checked_add(1)
                .ok_or(ProtocolError::Exhausted)?;
            if envelope.direction != opposite as i32
                || envelope.major != self.major
                || envelope.binding != self.binding.bytes()
                || envelope.purpose != 3
            {
                return Err(ProtocolError::Authentication);
            }
            if envelope.sequence != next {
                return Err(ProtocolError::Sequence);
            }
            let mut message = bound_decode(
                preflight::SPACETERM_PAIRING_ENROLLMENTMESSAGE,
                &payload,
                MAX_PAIRING_MANAGEMENT_BYTES,
                NegotiatedLimits::default(),
            )?;
            if let Err(error) = self.validate(&message, opposite) {
                scrub_submission(&mut message);
                return Err(error);
            }
            self.received = next;
            Ok(message)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn validate(
        &self,
        message: &pairing::EnrollmentMessage,
        direction: common::Direction,
    ) -> Result<(), ProtocolError> {
        use pairing::enrollment_message::Kind;
        let e = self.reservation.parts();
        let check_operation =
            |v: Option<&common::OperationId>, digest: &[u8]| -> Result<(), ProtocolError> {
                let op = operation_from_wire(needed(v)?)?;
                if op.stream() != e.enrollment_stream
                    || op.sequence().get() != 1
                    || digest != e.enrollment_request_digest.bytes()
                {
                    return Err(ProtocolError::PairingDenied);
                }
                Ok(())
            };
        match (&message.kind, direction) {
            (Some(Kind::Lookup(v)), common::Direction::ClientToServer) => {
                exact_reservation(v.reservation.as_ref(), &self.reservation.to_wire())
            }
            (Some(Kind::Submit(v)), common::Direction::ClientToServer) => {
                exact_reservation(v.reservation.as_ref(), &self.reservation.to_wire())?;
                if v.single_use_authorization.len() != 32 {
                    return Err(ProtocolError::Malformed);
                }
                let bytes = self.reservation.to_wire().encode_to_vec();
                authentication::verify(
                    e.keys.parts().device_identity_public_key,
                    b"enrollment-claim-v1",
                    &[&bytes],
                    &v.device_identity_proof,
                )?;
                authentication::verify(
                    e.keys.parts().pairing_authentication_public_key,
                    b"enrollment-claim-v1",
                    &[&bytes],
                    &v.pairing_authentication_proof,
                )
            }
            (Some(Kind::Acknowledgement(v)), common::Direction::ClientToServer) => {
                check_operation(v.operation.as_ref(), &v.request_digest)?;
                if v.acknowledged_through != 1 {
                    return Err(ProtocolError::Malformed);
                }
                Ok(())
            }
            (Some(Kind::Result(v)), common::Direction::ServerToClient) => {
                check_operation(v.operation.as_ref(), &v.request_digest)?;
                match (&v.receipt, &v.failure) {
                    (Some(proof), None) => {
                        let receipt = ReceiptCodec::new(self.pin).from_wire(proof)?;
                        match &receipt.parts().claims {
                            PairingReceiptBody::Enrollment {
                                target,
                                keys,
                                client,
                                enrollment,
                                ..
                            } if target == &e.target
                                && keys == &e.keys
                                && client.parts().client.id == e.initial_client
                                && enrollment.id.stream() == e.enrollment_stream
                                && enrollment.digest == e.enrollment_request_digest =>
                            {
                                Ok(())
                            }
                            _ => Err(ProtocolError::PairingDenied),
                        }
                    }
                    (None, Some(_)) => Ok(()),
                    _ => Err(ProtocolError::Malformed),
                }
            }
            _ => Err(ProtocolError::UnexpectedMessage),
        }
    }
}

/// Pairing-owned codec used by the existing restricted connection type state.
pub struct PairingRestrictedCodec {
    receipts: ReceiptCodec,
}
impl PairingRestrictedCodec {
    pub const fn new(pin: ServerPin) -> Self {
        Self {
            receipts: ReceiptCodec::new(pin),
        }
    }
    fn validate(
        &self,
        payload: &pairing::RestrictedExtension,
        principal: Principal,
        direction: common::Direction,
    ) -> Result<(), ProtocolError> {
        use pairing::restricted_extension::Kind;
        let (op, digest, target) = match (&payload.kind, direction) {
            (Some(Kind::Revoke(v)), common::Direction::ClientToServer) => (
                v.operation.as_ref(),
                v.request_digest.as_slice(),
                v.target.as_ref(),
            ),
            (Some(Kind::Status(v)), common::Direction::ClientToServer) => (
                v.operation.as_ref(),
                v.request_digest.as_slice(),
                v.target.as_ref(),
            ),
            (Some(Kind::Acknowledgement(v)), common::Direction::ClientToServer) => {
                if v.acknowledged_through != 1 {
                    return Err(ProtocolError::Malformed);
                }
                (
                    v.operation.as_ref(),
                    v.request_digest.as_slice(),
                    v.target.as_ref(),
                )
            }
            (Some(Kind::Result(v)), common::Direction::ServerToClient) => {
                let target = target_from_wire(needed(v.target.as_ref())?)?;
                match (v.disposition, &v.receipt) {
                    (3, None) => (),
                    (1 | 2, Some(proof)) => {
                        let r = self.receipts.from_wire(proof)?;
                        match &r.parts().claims {
                            PairingReceiptBody::Revocation { receipt, .. }
                                if receipt.server == target.parts().server
                                    && receipt.pairing == target.parts().pairing
                                    && receipt.device == target.parts().device
                                    && (v.disposition == 2
                                        || receipt.operation
                                            == operation_from_wire(needed(
                                                v.request_operation.as_ref(),
                                            )?)?) => {}
                            _ => return Err(ProtocolError::PairingDenied),
                        }
                    }
                    _ => return Err(ProtocolError::Malformed),
                }
                (
                    v.request_operation.as_ref(),
                    v.request_digest.as_slice(),
                    v.target.as_ref(),
                )
            }
            (Some(Kind::Failure(_)), common::Direction::ServerToClient) => return Ok(()),
            _ => return Err(ProtocolError::UnexpectedMessage),
        };
        let op = operation_from_wire(needed(op)?)?;
        let target = target_from_wire(needed(target)?)?;
        if digest.len() != 32
            || op.sequence().get() != 1
            || target.parts().server.bytes() != principal.server
            || target.parts().device.bytes() != principal.device
            || local_ident(target.parts().pairing.local()).value != principal.pairing
            || local_ident(op.stream().client().local()).value != principal.client
            || op.stream().pairing() != target.parts().pairing
            || op.stream().server() != target.parts().server
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(())
    }
}
impl RestrictedExtensionCodec for PairingRestrictedCodec {
    fn encode_restricted_extension(
        &self,
        payload: &pairing::RestrictedExtension,
        principal: Principal,
        direction: common::Direction,
        limits: NegotiatedLimits,
    ) -> Result<Vec<u8>, ProtocolError> {
        if payload.encoded_len() > limits.management_bytes.min(MAX_PAIRING_MANAGEMENT_BYTES) {
            return Err(ProtocolError::LimitExceeded);
        }
        self.validate(payload, principal, direction)?;
        let bytes = payload.encode_to_vec();
        preflight::check(
            preflight::SPACETERM_PAIRING_RESTRICTEDEXTENSION,
            &bytes,
            limits,
        )?;
        Ok(bytes)
    }
    fn decode_restricted_extension(
        &self,
        bytes: &[u8],
        principal: Principal,
        direction: common::Direction,
        limits: NegotiatedLimits,
    ) -> Result<pairing::RestrictedExtension, ProtocolError> {
        let payload = bound_decode(
            preflight::SPACETERM_PAIRING_RESTRICTEDEXTENSION,
            bytes,
            limits.management_bytes.min(MAX_PAIRING_MANAGEMENT_BYTES),
            limits,
        )?;
        self.validate(&payload, principal, direction)?;
        Ok(payload)
    }
}

/// Transient bearer delivery. It has no Clone, Debug or serialization and is
/// never retained in an Outcome. The receiving adapter must zeroize its buffers.
pub struct EnrollmentAuthorization(zeroize::Zeroizing<[u8; 32]>);
impl EnrollmentAuthorization {
    pub fn generate() -> Result<Self, ProtocolError> {
        Ok(Self(zeroize::Zeroizing::new(random_nonce()?)))
    }
    /// Encodes the transient frozen delivery. The caller owns its bounded secret
    /// bytes until transport completion; they are never a compact ledger outcome.
    pub fn delivery_message(
        &self,
        reservation: &EnrollmentReservation,
    ) -> Result<Zeroizing<Vec<u8>>, ProtocolError> {
        let mut delivery = pairing::AuthorizationDelivery {
            reservation: Some(reservation_to_wire(reservation)),
            single_use_authorization: self.0.to_vec(),
        };
        let bytes = Zeroizing::new(delivery.encode_to_vec());
        delivery.single_use_authorization.zeroize();
        if bytes.len() > MAX_PAIRING_MANAGEMENT_BYTES {
            return Err(ProtocolError::LimitExceeded);
        }
        Ok(bytes)
    }
    /// Takes ownership of the received secret frame, including on decode failure.
    /// Decode the bearer directly into its zeroizing owner, without a prost Vec
    /// copy that could be dropped unprotected after a later shape failure.
    pub fn from_delivery(
        bytes: Zeroizing<Vec<u8>>,
    ) -> Result<(PublicEnrollmentReservation, Self), ProtocolError> {
        if bytes.len() > MAX_PAIRING_MANAGEMENT_BYTES {
            return Err(ProtocolError::LimitExceeded);
        }
        preflight::check(
            preflight::SPACETERM_PAIRING_AUTHORIZATIONDELIVERY,
            &bytes,
            NegotiatedLimits::default(),
        )?;
        let mut input = bytes.as_slice();
        let mut reservation = None;
        let mut authorization = None;
        while !input.is_empty() {
            let (field, wire) =
                prost::encoding::decode_key(&mut input).map_err(|_| ProtocolError::Malformed)?;
            if wire != prost::encoding::WireType::LengthDelimited {
                return Err(ProtocolError::Malformed);
            }
            let length =
                prost::encoding::decode_varint(&mut input).map_err(|_| ProtocolError::Malformed)?;
            let length = usize::try_from(length).map_err(|_| ProtocolError::LimitExceeded)?;
            let value = input.get(..length).ok_or(ProtocolError::Malformed)?;
            input = &input[length..];
            match field {
                1 => reservation = Some(PublicEnrollmentReservation::decode(value)?),
                2 if value.len() == 32 => {
                    let mut secret = Zeroizing::new([0; 32]);
                    secret.copy_from_slice(value);
                    authorization = Some(Self(secret));
                }
                _ => return Err(ProtocolError::Malformed),
            }
        }
        Ok((needed(reservation)?, needed(authorization)?))
    }
    pub fn digest(&self) -> RequestDigest {
        hash(self.0.as_slice())
    }
    pub fn with_secret<T>(&self, send: impl FnOnce(&[u8; 32]) -> T) -> T {
        send(&self.0)
    }
}

/// Verified public proposal; this is proof of key possession, not issuance authority.
pub struct VerifiedDeviceClaim {
    proposal: EnrollmentProposal,
    digest: RequestDigest,
}
impl VerifiedDeviceClaim {
    pub fn verify(
        proposal: EnrollmentProposal,
        device_proof: &[u8],
        auth_proof: &[u8],
    ) -> Result<Self, ProtocolError> {
        shape(proposal.validate())?;
        let p = proposal.parts();
        let wire = pairing::Proposal {
            server: Some(ident(p.server.bytes())),
            device: Some(ident(p.device.bytes())),
            keys: Some(keys_to_wire(&p.keys)),
            claim_nonce: p.claim_nonce.to_vec(),
        };
        let bytes = wire.encode_to_vec();
        authentication::verify(
            p.keys.parts().device_identity_public_key,
            b"enrollment-proposal-v1",
            &[&bytes],
            device_proof,
        )?;
        authentication::verify(
            p.keys.parts().pairing_authentication_public_key,
            b"enrollment-proposal-v1",
            &[&bytes],
            auth_proof,
        )?;
        Ok(Self {
            proposal,
            digest: hash(&bytes),
        })
    }
    pub fn proposal(&self) -> &EnrollmentProposal {
        &self.proposal
    }
    pub const fn digest(&self) -> RequestDigest {
        self.digest
    }
    pub fn proofs(
        proposal: &EnrollmentProposal,
        device: &dyn SigningKey,
        auth: &dyn SigningKey,
    ) -> Result<([u8; 64], [u8; 64]), ProtocolError> {
        let p = proposal.parts();
        if device.public_key() != p.keys.parts().device_identity_public_key
            || auth.public_key() != p.keys.parts().pairing_authentication_public_key
        {
            return Err(ProtocolError::Authentication);
        }
        let bytes = pairing::Proposal {
            server: Some(ident(p.server.bytes())),
            device: Some(ident(p.device.bytes())),
            keys: Some(keys_to_wire(&p.keys)),
            claim_nonce: p.claim_nonce.to_vec(),
        }
        .encode_to_vec();
        Ok((
            authentication::sign(device, b"enrollment-proposal-v1", &[&bytes])?,
            authentication::sign(auth, b"enrollment-proposal-v1", &[&bytes])?,
        ))
    }
}

/// Produced only by the server-side enrollment channel after exact reservation
/// and both key proofs. Contains only comparison digests, never the bearer.
pub struct VerifiedEnrollmentSubmission {
    reservation: EnrollmentReservation,
    authorization_digest: RequestDigest,
}
impl VerifiedEnrollmentSubmission {
    pub fn reservation(&self) -> &EnrollmentReservation {
        &self.reservation
    }
    pub const fn authorization_digest(&self) -> RequestDigest {
        self.authorization_digest
    }
}
/// Comparison reference available only after verifying an enrollment-purpose
/// request. Possession of this reference cannot construct an ordinary principal.
pub struct VerifiedEnrollmentReference(EnrollmentReservation);
impl VerifiedEnrollmentReference {
    pub fn reservation(&self) -> &EnrollmentReservation {
        &self.0
    }
}
pub enum VerifiedEnrollmentRequest {
    Submit(VerifiedEnrollmentSubmission),
    Lookup(VerifiedEnrollmentReference),
    Acknowledgement(VerifiedEnrollmentReference),
}
impl EnrollmentConnection {
    pub fn submission_message(
        &mut self,
        authorization: &EnrollmentAuthorization,
        device: &dyn SigningKey,
    ) -> Result<Zeroizing<Vec<u8>>, ProtocolError> {
        let bytes = self.reservation.to_wire().encode_to_vec();
        let device_identity_proof =
            authentication::sign(device, b"enrollment-claim-v1", &[&bytes])?.to_vec();
        let pairing_authentication_proof =
            authentication::sign(self.signer.as_ref(), b"enrollment-claim-v1", &[&bytes])?.to_vec();
        let mut message = pairing::EnrollmentMessage {
            kind: Some(pairing::enrollment_message::Kind::Submit(
                pairing::EnrollmentSubmission {
                    reservation: Some(self.reservation.to_wire()),
                    single_use_authorization: authorization.with_secret(|s| s.to_vec()),
                    device_identity_proof,
                    pairing_authentication_proof,
                },
            )),
        };
        let result = self.encode(&message).map(Zeroizing::new);
        scrub_submission(&mut message);
        result
    }
    /// Verify the envelope and purpose once, then consume its checked request.
    /// No raw Submit bytes or bearer secret escape this type-state boundary.
    pub fn receive_request(
        &mut self,
        bytes: &[u8],
    ) -> Result<VerifiedEnrollmentRequest, ProtocolError> {
        let reservation = self
            .server_reservation
            .clone()
            .ok_or(ProtocolError::Authentication)?;
        let message = self.decode(bytes)?;
        match message.kind {
            Some(pairing::enrollment_message::Kind::Submit(mut value)) => {
                let secret = Zeroizing::new(std::mem::take(&mut value.single_use_authorization));
                Ok(VerifiedEnrollmentRequest::Submit(
                    VerifiedEnrollmentSubmission {
                        reservation,
                        authorization_digest: hash(&secret),
                    },
                ))
            }
            Some(pairing::enrollment_message::Kind::Lookup(_)) => Ok(
                VerifiedEnrollmentRequest::Lookup(VerifiedEnrollmentReference(reservation)),
            ),
            Some(pairing::enrollment_message::Kind::Acknowledgement(_)) => {
                Ok(VerifiedEnrollmentRequest::Acknowledgement(
                    VerifiedEnrollmentReference(reservation),
                ))
            }
            _ => Err(ProtocolError::UnexpectedMessage),
        }
    }
    pub fn receive_submission(
        &mut self,
        bytes: &[u8],
    ) -> Result<VerifiedEnrollmentSubmission, ProtocolError> {
        match self.receive_request(bytes)? {
            VerifiedEnrollmentRequest::Submit(submission) => Ok(submission),
            _ => Err(ProtocolError::UnexpectedMessage),
        }
    }
}

/// A binding proof cannot be reused for a new operation, target or version.
pub fn client_binding_claim(
    target: &PairingTarget,
    version: spaceterm_core::authority::FieldVersion,
    operation: OperationId,
) -> Result<(RequestDigest, Vec<u8>), ProtocolError> {
    shape(target.validate())?;
    if version.get() == 0 || operation.stream().pairing() != target.parts().pairing {
        return Err(ProtocolError::PairingDenied);
    }
    let mut bytes = b"SpaceTerm additional Client claim v1\0".to_vec();
    let target = target_to_wire(target).encode_to_vec();
    let op = operation_to_wire(operation).encode_to_vec();
    for part in [
        target.as_slice(),
        version.get().to_be_bytes().as_slice(),
        op.as_slice(),
    ] {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part);
    }
    Ok((hash(&bytes), bytes))
}
pub fn sign_client_binding_claim(
    target: &PairingTarget,
    version: spaceterm_core::authority::FieldVersion,
    operation: OperationId,
    device: &dyn SigningKey,
    auth: &dyn SigningKey,
) -> Result<([u8; 64], [u8; 64]), ProtocolError> {
    let (_, bytes) = client_binding_claim(target, version, operation)?;
    Ok((
        authentication::sign(device, b"client-binding-claim-v1", &[&bytes])?,
        authentication::sign(auth, b"client-binding-claim-v1", &[&bytes])?,
    ))
}
pub fn verify_client_binding_claim(
    target: &PairingTarget,
    version: spaceterm_core::authority::FieldVersion,
    operation: OperationId,
    keys: &PairingKeys,
    claim: RequestDigest,
    device_proof: &[u8],
    auth_proof: &[u8],
) -> Result<(), ProtocolError> {
    let (digest, bytes) = client_binding_claim(target, version, operation)?;
    if digest != claim {
        return Err(ProtocolError::Authentication);
    }
    authentication::verify(
        keys.parts().device_identity_public_key,
        b"client-binding-claim-v1",
        &[&bytes],
        device_proof,
    )?;
    authentication::verify(
        keys.parts().pairing_authentication_public_key,
        b"client-binding-claim-v1",
        &[&bytes],
        auth_proof,
    )
}

fn scrub_submission(message: &mut pairing::EnrollmentMessage) {
    if let Some(pairing::enrollment_message::Kind::Submit(ref mut v)) = message.kind {
        v.single_use_authorization.zeroize();
    }
}
