//! Pairing relational projection. Public-key roles, immutable Client bindings,
//! prospective identities, single-use enrollment and signed bytes share the one
//! organization transaction. Pairing policy and cryptography remain producer-owned.

use super::{
    StoreLimits,
    auxiliary::*,
    codec,
    limits::{Collection, SizeBudget},
};
use crate::storage::Transaction;
use rusqlite::params;
use spaceterm_core::authority::{FieldVersion, ServerEpoch, StoreRevision};
use spaceterm_core::identity::*;
use spaceterm_core::operation_ledger_contracts::LedgerWrite;
use spaceterm_core::operations::OperationBinding;
use spaceterm_core::pairing::*;
use spaceterm_core::storage::{OperationWrite, RecoveredState, StoreFailure, TransactionParts};
use std::collections::{BTreeMap, BTreeSet};

/// Required pairing-owner reader for exact signed bytes. No host receipt format
/// or permissive decoder is installed. Implementations must bound decoding, verify
/// the original pinned Server key, and implement PairingReceiptValidator's full
/// exact-byte/claim/digest contract. The host compares the returned envelope again.
pub trait PairingReceiptReader: PairingReceiptValidator {
    /// Decodes and verifies one retained exact envelope. Unknown formats are
    /// UnknownSchema; malformed bytes or invalid signatures are InvalidDurableState.
    fn read_receipt(
        &self,
        schema: u16,
        body: &[u8],
        signature: &[u8],
    ) -> Result<SignedPairingReceipt, StoreFailure>;
}

/// Reads the pairing protocol's retained receipt format against the original
/// native Server pin. It owns no signer or credential and cannot issue receipts.
pub struct SignedReceiptReader(spaceterm_protocol::pairing_adapter::ReceiptCodec);
impl SignedReceiptReader {
    /// Binds verification to the retained server identity and public key.
    pub fn new(
        pin: spaceterm_protocol::authentication::ServerPin,
    ) -> Result<Self, spaceterm_core::storage::StoreFailure> {
        if pin.identity == [0; 16] || pin.public_key == [0; 32] {
            return Err(spaceterm_core::storage::StoreFailure::IdentityMismatch);
        }
        Ok(Self(
            spaceterm_protocol::pairing_adapter::ReceiptCodec::new(pin),
        ))
    }
}
impl PairingReceiptValidator for SignedReceiptReader {
    fn validate_receipt(&self, receipt: &SignedPairingReceipt) -> Result<(), PairingContractError> {
        self.0.validate_receipt(receipt)
    }
}
impl PairingReceiptReader for SignedReceiptReader {
    fn read_receipt(
        &self,
        schema: u16,
        body: &[u8],
        signature: &[u8],
    ) -> Result<SignedPairingReceipt, spaceterm_core::storage::StoreFailure> {
        use spaceterm_core::storage::StoreFailure;
        if schema != 1 {
            return Err(StoreFailure::UnknownSchema);
        }
        // Check before copying retained bytes into the bounded protocol envelope.
        if signature.len() != 64 || body.len() > MAX_SIGNED_PAIRING_RECEIPT_BYTES - 64 {
            return Err(StoreFailure::InvalidDurableState);
        }
        self.0
            .from_wire(&spaceterm_protocol::wire::pairing::SignedReceipt {
                canonical_body: body.to_vec(),
                signature: signature.to_vec(),
            })
            .map_err(|_| StoreFailure::InvalidDurableState)
    }
}

fn receipt_binding(
    value: &SignedPairingReceipt,
) -> (spaceterm_core::operations::OperationId, PairingId, i64) {
    match &value.parts().claims {
        PairingReceiptBody::Enrollment {
            target, enrollment, ..
        } => (enrollment.id, target.parts().pairing, 0),
        PairingReceiptBody::ClientBinding {
            target, operation, ..
        } => (operation.id, target.parts().pairing, 1),
        PairingReceiptBody::Revocation { receipt, .. } => (receipt.operation, receipt.pairing, 2),
    }
}
fn write_receipt(
    sql: &Transaction<'_>,
    value: &SignedPairingReceipt,
    reader: &dyn PairingReceiptReader,
) -> Result<usize, StoreFailure> {
    reader
        .validate_receipt(value)
        .map_err(|_| StoreFailure::InvalidTransaction)?;
    let p = value.parts();
    let (operation, pairing, kind) = receipt_binding(value);
    exact(
        sql,
        "INSERT INTO pairing_receipts VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            operation.stream().key().local().to_be_bytes().as_slice(),
            operation.sequence().get().to_be_bytes().as_slice(),
            pairing.local().to_be_bytes().as_slice(),
            kind,
            p.schema,
            p.canonical_body,
            p.signature
        ],
        1,
    )
}
fn charge(budget: &mut SizeBudget, bytes: usize) -> Result<(), StoreFailure> {
    budget
        .charge(bytes)
        .map_err(|_| StoreFailure::InvalidTransaction)
}

pub(super) fn preflight(
    write: &PairingWrite,
    budget: &mut SizeBudget,
) -> Result<usize, StoreFailure> {
    let (bytes, rows) = match write {
        PairingWrite::PutAuthorization { value, .. } => {
            let p = value.parts();
            value
                .validate()
                .map_err(|_| StoreFailure::InvalidTransaction)?;
            (
                p.clients
                    .len()
                    .checked_mul(1024)
                    .and_then(|n| n.checked_add(16_384))
                    .ok_or(StoreFailure::InvalidTransaction)?,
                p.clients.len() + 3,
            )
        }
        PairingWrite::PutEnrollmentReservation { value, .. } => {
            value
                .validate()
                .map_err(|_| StoreFailure::InvalidTransaction)?;
            (16_384, 2)
        }
    };
    charge(budget, bytes)?;
    Ok(rows)
}

pub(super) fn check_duplicates(writes: &[OperationWrite]) -> Result<(), StoreFailure> {
    let mut pairings = BTreeSet::new();
    let mut tickets = BTreeSet::new();
    for write in writes {
        if let OperationWrite::Pairing(write) = write {
            let unique = match write.as_ref() {
                PairingWrite::PutAuthorization { value, .. } => {
                    pairings.insert(value.parts().record.id)
                }
                PairingWrite::PutEnrollmentReservation { value, .. } => {
                    tickets.insert(value.parts().ticket.parts().nonce)
                }
            };
            if !unique {
                return Err(StoreFailure::InvalidTransaction);
            }
        }
    }
    Ok(())
}

fn reserved_activation(
    parts: &TransactionParts,
    previous: &PairingRecovery,
    value: &PairingAuthorization,
) -> bool {
    previous.parts().enrollments.iter().any(|reservation| {
        let e = reservation.parts();
        e.state.kind() == EnrollmentReservationKind::Reserved
            && e.target.parts().pairing == value.parts().record.id
            && e.target.parts().device == value.parts().record.device
            && e.keys == value.parts().keys
            && parts.operation_writes.iter().any(|w| matches!(w, OperationWrite::Pairing(w) if matches!(w.as_ref(), PairingWrite::PutEnrollmentReservation { expected_kind: Some(EnrollmentReservationKind::Reserved), value: next } if next.parts().ticket == e.ticket && next.parts().state.kind() == EnrollmentReservationKind::Consumed)))
    })
}
fn expected_enrollment(a: &EnrollmentReservationParts, b: &EnrollmentReservationParts) -> bool {
    a.ticket == b.ticket
        && a.target == b.target
        && a.keys == b.keys
        && a.initial_client == b.initial_client
        && a.enrollment_stream == b.enrollment_stream
        && a.claim_digest == b.claim_digest
        && a.authorization_digest == b.authorization_digest
        && a.enrollment_request_digest == b.enrollment_request_digest
        && a.issuer == b.issuer
        && a.issued_epoch == b.issued_epoch
        && a.issued_revision == b.issued_revision
        && a.issued_at_millis == b.issued_at_millis
        && a.expires_at_millis == b.expires_at_millis
}
fn completion(
    parts: &TransactionParts,
    id: spaceterm_core::operations::OperationId,
) -> Result<OperationBinding, StoreFailure> {
    parts
        .operation_writes
        .iter()
        .find_map(|w| match w {
            OperationWrite::Ledger(v) => match v.as_ref() {
                LedgerWrite::CompleteOperation { record, .. }
                    if record.binding.id == id
                        && record.completed_at == parts.resulting_store_revision =>
                {
                    Some(record.binding)
                }
                _ => None,
            },
            _ => None,
        })
        .ok_or(StoreFailure::InvalidTransaction)
}

pub(super) fn apply(
    sql: &Transaction<'_>,
    write: &PairingWrite,
    parts: &TransactionParts,
    previous: &RecoveredState,
    receipts: &dyn PairingReceiptReader,
) -> Result<usize, StoreFailure> {
    let previous = previous
        .pairing
        .as_ref()
        .ok_or(StoreFailure::RecoveryRequired)?;
    match write {
        PairingWrite::PutAuthorization {
            expected_authorization_version,
            value,
        } => {
            let p = value.parts();
            let old = previous
                .parts()
                .authorizations
                .iter()
                .find(|v| v.parts().record.id == p.record.id);
            let key = p.record.id.local().to_be_bytes();
            let state = match p.record.state {
                PairingState::Authorized => 0,
                PairingState::Revoked { .. } => 1,
            };
            let revoked = match p.record.state {
                PairingState::Authorized => None,
                PairingState::Revoked { revision } => Some(revision.get().to_be_bytes()),
            };
            if p.record.id.server() != parts.server
                || p.record.id.local() > parts.resulting_high_water.pairing
                || p.clients
                    .iter()
                    .any(|c| !c.fits_marks(parts.resulting_high_water))
            {
                return Err(StoreFailure::InvalidTransaction);
            }
            let mut rows = 0;
            if let Some(old) = old {
                let o = old.parts();
                if *expected_authorization_version != Some(o.authorization_version)
                    || o.authorization_version.checked_successor() != Some(p.authorization_version)
                    || o.record.device != p.record.device
                    || o.keys != p.keys
                    || o.record.state != PairingState::Authorized
                    || o.clients.iter().any(|c| !p.clients.contains(c))
                {
                    return Err(StoreFailure::InvalidTransaction);
                }
                rows += exact(
                    sql,
                    "UPDATE pairings SET authorization_version = ?2, state = ?3, revoked_revision = ?4 WHERE id = ?1 AND authorization_version = ?5 AND state = 0",
                    params![
                        key.as_slice(),
                        p.authorization_version.get().to_be_bytes().as_slice(),
                        state,
                        revoked.as_ref().map(|v| v.as_slice()),
                        o.authorization_version.get().to_be_bytes().as_slice()
                    ],
                    1,
                )?;
            } else {
                let activation = reserved_activation(parts, previous, value);
                if expected_authorization_version.is_some()
                    || p.authorization_version.get() != 1
                    || state != 0
                    || (p.record.id.local() <= parts.expected_high_water.pairing && !activation)
                {
                    return Err(StoreFailure::InvalidTransaction);
                }
                rows += exact(
                    sql,
                    "INSERT INTO pairings VALUES (?1, ?2, ?3, ?4, ?5, 0, NULL)",
                    params![
                        key.as_slice(),
                        p.record.device.bytes().as_slice(),
                        p.keys.parts().device_identity_public_key.as_slice(),
                        p.keys.parts().pairing_authentication_public_key.as_slice(),
                        p.authorization_version.get().to_be_bytes().as_slice()
                    ],
                    1,
                )?;
            }
            for client in &p.clients {
                if old.is_some_and(|o| o.parts().clients.contains(client)) {
                    continue;
                }
                let c = client.parts();
                let reserved = previous.parts().enrollments.iter().any(|e| {
                    e.parts().target.parts().pairing == p.record.id
                        && e.parts().initial_client == c.client.id
                        && e.parts().state.kind() == EnrollmentReservationKind::Reserved
                });
                if c.client.id.local() <= parts.expected_high_water.client && !reserved {
                    return Err(StoreFailure::InvalidTransaction);
                }
                rows += exact(
                    sql,
                    "INSERT INTO paired_clients VALUES (?1, ?2, ?3, ?4)",
                    params![
                        c.client.id.local().to_be_bytes().as_slice(),
                        key.as_slice(),
                        c.ordinary.key().local().to_be_bytes().as_slice(),
                        c.self_revocation.key().local().to_be_bytes().as_slice()
                    ],
                    1,
                )?;
            }
            if let Some(proof) = &p.revocation_receipt {
                if proof.parts().claims.revision() != parts.resulting_store_revision {
                    return Err(StoreFailure::InvalidTransaction);
                }
                let b = completion(parts, receipt_binding(proof).0)?;
                rows += write_receipt(sql, proof, receipts)?;
                rows += exact(
                    sql,
                    "INSERT INTO revocation_receipts VALUES (?1, ?2, ?3, ?4)",
                    params![
                        key.as_slice(),
                        b.id.stream().key().local().to_be_bytes().as_slice(),
                        b.id.sequence().get().to_be_bytes().as_slice(),
                        b.digest.bytes().as_slice()
                    ],
                    1,
                )?;
            }
            Ok(rows)
        }
        PairingWrite::PutEnrollmentReservation {
            expected_kind,
            value,
        } => {
            let p = value.parts();
            let old = previous
                .parts()
                .enrollments
                .iter()
                .find(|v| v.parts().ticket == p.ticket);
            let state = match p.state.kind() {
                EnrollmentReservationKind::Reserved => 0,
                EnrollmentReservationKind::Consumed => 1,
                EnrollmentReservationKind::Invalidated => 2,
            };
            let mut rows = 0;
            let (sequence, digest) = if let EnrollmentReservationState::Consumed {
                enrollment,
                receipt,
            } = &p.state
            {
                if completion(parts, enrollment.id)? != *enrollment
                    || receipt.parts().claims.revision() != parts.resulting_store_revision
                {
                    return Err(StoreFailure::InvalidTransaction);
                }
                rows += write_receipt(sql, receipt, receipts)?;
                (
                    Some(enrollment.id.sequence().get().to_be_bytes()),
                    Some(enrollment.digest.bytes()),
                )
            } else {
                (None, None)
            };
            if let Some(old) = old {
                let o = old.parts();
                if *expected_kind != Some(EnrollmentReservationKind::Reserved)
                    || o.state.kind() != EnrollmentReservationKind::Reserved
                    || state == 0
                    || !expected_enrollment(o, p)
                {
                    return Err(StoreFailure::InvalidTransaction);
                }
                rows += exact(
                    sql,
                    "UPDATE enrollment_authorizations SET state = ?2, consumed_sequence = ?3, consumed_digest = ?4 WHERE ticket_nonce = ?1 AND state = 0",
                    params![
                        p.ticket.parts().nonce.as_slice(),
                        state,
                        sequence.as_ref().map(|v| v.as_slice()),
                        digest.as_ref().map(|v| v.as_slice())
                    ],
                    1,
                )?;
            } else {
                if expected_kind.is_some()
                    || state != 0
                    || p.target.parts().pairing.local() <= parts.expected_high_water.pairing
                    || p.initial_client.local() <= parts.expected_high_water.client
                    || p.enrollment_stream.key().local()
                        <= parts.expected_high_water.operation_stream
                    || p.issued_revision != parts.resulting_store_revision
                    || p.issued_epoch != parts.resulting_epoch
                {
                    return Err(StoreFailure::InvalidTransaction);
                }
                let issuer = codec::encode_enrollment_issuer(&p.issuer).map_err(codec_error)?;
                rows += exact(
                    sql,
                    "INSERT INTO enrollment_authorizations VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0, NULL, NULL)",
                    params![
                        p.ticket.parts().nonce.as_slice(),
                        p.target.parts().pairing.local().to_be_bytes().as_slice(),
                        p.target.parts().device.bytes().as_slice(),
                        p.keys.parts().device_identity_public_key.as_slice(),
                        p.keys.parts().pairing_authentication_public_key.as_slice(),
                        p.initial_client.local().to_be_bytes().as_slice(),
                        p.enrollment_stream.key().local().to_be_bytes().as_slice(),
                        p.claim_digest.bytes().as_slice(),
                        p.authorization_digest.bytes().as_slice(),
                        p.enrollment_request_digest.bytes().as_slice(),
                        issuer,
                        p.issued_epoch.get().to_be_bytes().as_slice(),
                        p.issued_revision.get().to_be_bytes().as_slice(),
                        p.issued_at_millis.to_be_bytes().as_slice(),
                        p.expires_at_millis.to_be_bytes().as_slice()
                    ],
                    1,
                )?;
            }
            Ok(rows)
        }
    }
}

pub(super) fn recover(
    sql: &Transaction<'_>,
    recovered: &mut RecoveredState,
    limits: &StoreLimits,
    budget: &mut SizeBudget,
    reader: &dyn PairingReceiptReader,
) -> Result<(), StoreFailure> {
    let server = recovered.organization.server;
    let authorizations = inventory(sql, "pairings", "0", Collection::Pairings, limits, budget)?;
    let clients = inventory(
        sql,
        "paired_clients",
        "0",
        Collection::Clients,
        limits,
        budget,
    )?;
    let enrollments = inventory(
        sql,
        "enrollment_authorizations",
        "length(issuer)",
        Collection::Enrollments,
        limits,
        budget,
    )?;
    let receipts = inventory(
        sql,
        "pairing_receipts",
        "length(canonical_body) + length(signature)",
        Collection::Receipts,
        limits,
        budget,
    )?;
    let revocations = inventory(
        sql,
        "revocation_receipts",
        "0",
        Collection::Receipts,
        limits,
        budget,
    )?;
    invalid(budget.charge_rows(receipts, 16_384))?;
    let proofs = scan_operations(
        sql,
        "SELECT stream_key, sequence, pairing_id, receipt_kind, format_version, canonical_body, signature FROM pairing_receipts WHERE (stream_key, sequence) > (?1, ?2) ORDER BY stream_key, sequence LIMIT 1",
        receipts,
        |row| {
            let schema = invalid(row.get(4))?;
            let body = blob(row, 5)?;
            let signature = blob(row, 6)?;
            if body.len() > 1984 || signature.len() != 64 {
                return Err(StoreFailure::InvalidDurableState);
            }
            let proof = reader.read_receipt(schema, body, signature)?;
            reader
                .validate_receipt(&proof)
                .map_err(|_| StoreFailure::InvalidDurableState)?;
            let p = proof.parts();
            let (operation, target, kind) = receipt_binding(&proof);
            if p.schema != schema
                || p.canonical_body != body
                || p.signature != signature
                || operation.stream().server() != server
                || operation.stream().key().local() != counter(row, 0)?
                || operation.sequence().get() != counter(row, 1)?
                || target.local() != counter(row, 2)?
                || kind != invalid(row.get::<_, i64>(3))?
            {
                return Err(StoreFailure::InvalidDurableState);
            }
            Ok(proof)
        },
    )?;
    let bindings = scan::<8, _>(
        sql,
        "SELECT id, pairing_id, ordinary_stream, self_revocation_stream FROM paired_clients WHERE id > ?1 ORDER BY id LIMIT 1",
        clients,
        |row| {
            let id = invalid(ClientId::new(server, counter(row, 0)?))?;
            let pairing = invalid(PairingId::new(server, counter(row, 1)?))?;
            invalid(PairingClientStreams::new(PairingClientStreamsParts {
                client: ClientRecord { id, pairing },
                ordinary: invalid(OperationStreamId::new(
                    invalid(OperationStreamKey::new(server, counter(row, 2)?))?,
                    pairing,
                    id,
                ))?,
                self_revocation: invalid(OperationStreamId::new(
                    invalid(OperationStreamKey::new(server, counter(row, 3)?))?,
                    pairing,
                    id,
                ))?,
            }))
        },
    )?;
    let mut by_pairing: BTreeMap<PairingId, Vec<PairingClientStreams>> = BTreeMap::new();
    for value in bindings {
        let group = by_pairing.entry(value.parts().client.pairing).or_default();
        if group.len() == MAX_CLIENTS_PER_PAIRING {
            return Err(StoreFailure::InvalidDurableState);
        }
        group
            .try_reserve(1)
            .map_err(|_| StoreFailure::Unavailable)?;
        group.push(value);
    }
    let mut used = BTreeSet::new();
    let mut revoked_count = 0;
    let authorizations = scan::<8, _>(
        sql,
        "SELECT id, device_id, device_identity_public_key, pairing_authentication_public_key, authorization_version, state, revoked_revision FROM pairings WHERE id > ?1 ORDER BY id LIMIT 1",
        authorizations,
        |row| {
            let id = invalid(PairingId::new(server, counter(row, 0)?))?;
            let state = match invalid(row.get::<_, i64>(5))? {
                0 => PairingState::Authorized,
                1 => PairingState::Revoked {
                    revision: StoreRevision::new(counter(row, 6)?),
                },
                _ => return Err(StoreFailure::InvalidDurableState),
            };
            let proof = if let PairingState::Revoked { .. } = state {
                let (key, sequence, digest) = sql.query_row("SELECT operation_stream, operation_sequence, operation_digest FROM revocation_receipts WHERE pairing_id = ?1", [id.local().to_be_bytes().as_slice()], |row| Ok((counter(row, 0), counter(row, 1), fixed::<32>(row, 2)))).map_err(super::database_adapter::classify)?;
                let original = binding(sql, server, key?, sequence?, digest?)?;
                let proof = proofs
                    .iter()
                    .find(|v| {
                        receipt_binding(v).0 == original.id
                            && receipt_binding(v).1 == id
                            && receipt_binding(v).2 == 2
                    })
                    .ok_or(StoreFailure::InvalidDurableState)?;
                Some(proof.clone())
            } else {
                None
            };
            let clients = by_pairing.get(&id).cloned().unwrap_or_default();
            invalid(PairingAuthorization::new(PairingAuthorizationParts {
                record: PairingRecord {
                    id,
                    device: DeviceId::new(fixed(row, 1)?),
                    state,
                },
                authorization_version: FieldVersion::new(counter(row, 4)?),
                keys: invalid(PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: fixed(row, 2)?,
                    pairing_authentication_public_key: fixed(row, 3)?,
                }))?,
                clients,
                revocation_receipt: proof,
            }))
        },
    )?;
    for value in &authorizations {
        if let Some(proof) = &value.parts().revocation_receipt {
            used.insert(receipt_binding(proof).0);
            revoked_count += 1;
        }
    }
    let enrollments = scan::<16, _>(
        sql,
        "SELECT ticket_nonce, prospective_pairing_id, device_id, device_identity_public_key, pairing_authentication_public_key, initial_client_id, enrollment_stream, claim_digest, authorization_digest, enrollment_request_digest, issuer, issued_epoch, issued_revision, issued_at_millis, expires_at_millis, state, consumed_sequence, consumed_digest FROM enrollment_authorizations WHERE ticket_nonce > ?1 ORDER BY ticket_nonce LIMIT 1",
        enrollments,
        |row| {
            let pairing = invalid(PairingId::new(server, counter(row, 1)?))?;
            let client = invalid(ClientId::new(server, counter(row, 5)?))?;
            let stream = invalid(OperationStreamId::new(
                invalid(OperationStreamKey::new(server, counter(row, 6)?))?,
                pairing,
                client,
            ))?;
            let state = match invalid(row.get::<_, i64>(15))? {
                0 => EnrollmentReservationState::Reserved,
                1 => {
                    let original = binding(
                        sql,
                        server,
                        stream.key().local(),
                        counter(row, 16)?,
                        fixed(row, 17)?,
                    )?;
                    let proof = proofs
                        .iter()
                        .find(|v| {
                            receipt_binding(v).0 == original.id
                                && receipt_binding(v).1 == pairing
                                && receipt_binding(v).2 == 0
                        })
                        .ok_or(StoreFailure::InvalidDurableState)?;
                    EnrollmentReservationState::Consumed {
                        enrollment: original,
                        receipt: Box::new(proof.clone()),
                    }
                }
                2 => EnrollmentReservationState::Invalidated,
                _ => return Err(StoreFailure::InvalidDurableState),
            };
            invalid(EnrollmentReservation::new(EnrollmentReservationParts {
                ticket: invalid(EnrollmentTicketId::new(EnrollmentTicketParts {
                    server,
                    nonce: fixed(row, 0)?,
                }))?,
                target: invalid(PairingTarget::new(PairingTargetParts {
                    server,
                    pairing,
                    device: DeviceId::new(fixed(row, 2)?),
                }))?,
                keys: invalid(PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: fixed(row, 3)?,
                    pairing_authentication_public_key: fixed(row, 4)?,
                }))?,
                initial_client: client,
                enrollment_stream: stream,
                claim_digest: spaceterm_core::operations::RequestDigest::from_bytes(fixed(row, 7)?),
                authorization_digest: spaceterm_core::operations::RequestDigest::from_bytes(fixed(
                    row, 8,
                )?),
                enrollment_request_digest: spaceterm_core::operations::RequestDigest::from_bytes(
                    fixed(row, 9)?,
                ),
                issuer: codec::decode_enrollment_issuer(blob(row, 10)?, server)
                    .map_err(codec_error)?,
                issued_epoch: ServerEpoch::new(counter(row, 11)?),
                issued_revision: StoreRevision::new(counter(row, 12)?),
                issued_at_millis: counter(row, 13)?,
                expires_at_millis: counter(row, 14)?,
                state,
            }))
        },
    )?;
    for value in &enrollments {
        if let EnrollmentReservationState::Consumed { receipt, .. } = &value.parts().state {
            used.insert(receipt_binding(receipt).0);
        }
    }
    // Every row must be represented, including tombstones. Unknown ClientBinding
    // receipt rows are not silently dropped: their owner has no write slot here.
    if clients
        != authorizations
            .iter()
            .map(|a| a.parts().clients.len() as u64)
            .sum::<u64>()
        || used.len() != proofs.len()
        || revoked_count != revocations
    {
        return Err(StoreFailure::InvalidDurableState);
    }
    let value = invalid(PairingRecovery::new(PairingRecoveryParts {
        authorizations,
        enrollments,
    }))?;
    invalid(value.validate_for(server, recovered.store_revision, recovered.high_water))?;
    invalid(value.validate_receipts(reader))?;
    recovered.pairing = Some(value);
    Ok(())
}
