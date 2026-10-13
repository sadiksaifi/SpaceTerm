//! Pairing's projection into the single core-owned durability boundary.

use spaceterm_core::{
    pairing::policy::PairingSnapshot,
    pairing::transition::PairingTransition,
    pairing::*,
    storage::{OperationWrite, TransactionParts},
};

/// Reads the pairing protocol's retained receipt format against the original
/// native Server pin. It owns no signer or credential and cannot issue receipts.
pub struct SignedReceiptReader(spaceterm_protocol::pairing_adapter::ReceiptCodec);
impl SignedReceiptReader {
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
impl spaceterm_host::organization_store::pairing::PairingReceiptReader for SignedReceiptReader {
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

/// Ledger registration/retirement/completion and reducer control release remain
/// actor-owned. This function adds only pairing's writes and allocation marks.
pub fn append_pairing_writes(
    parts: &mut TransactionParts,
    transition: &PairingTransition,
) -> Result<(), PairingFailure> {
    if parts.expected_store_revision != transition.expected_store
        || !transition.high_water.dominates(parts.expected_high_water)
        || !parts.resulting_high_water.dominates(transition.high_water)
    {
        return Err(PairingFailure::InvalidDurableState);
    }
    transition
        .writes
        .validate_for(
            parts.server,
            parts.resulting_store_revision,
            parts.resulting_high_water,
        )
        .map_err(|_| PairingFailure::InvalidDurableState)?;
    for write in &transition.writes.parts().writes {
        parts
            .operation_writes
            .push(OperationWrite::Pairing(Box::new(write.clone())));
    }
    Ok(())
}

/// Pure post-transaction comparison interpreter. Org-store still owns its SQL,
/// complete bundle validation and the physical expected-version comparison.
pub fn project_pairing(
    state: &PairingSnapshot,
    transition: &PairingTransition,
    validator: &dyn PairingReceiptValidator,
) -> Result<PairingSnapshot, PairingFailure> {
    if transition.expected_store != state.revision()
        || !transition.high_water.dominates(state.high_water())
    {
        return Err(PairingFailure::StaleAssessment);
    }
    let revision = state
        .revision()
        .checked_successor()
        .ok_or(PairingFailure::LimitExceeded)?;
    transition
        .writes
        .validate_for(state.server(), revision, transition.high_water)
        .map_err(|_| PairingFailure::InvalidDurableState)?;
    transition
        .writes
        .validate_receipts(validator)
        .map_err(|_| PairingFailure::ReceiptInvalid)?;
    let mut candidate = state.recovery().parts().clone();
    for write in &transition.writes.parts().writes {
        match write {
            PairingWrite::PutAuthorization {
                expected_authorization_version,
                value,
            } => {
                let index = candidate
                    .authorizations
                    .iter()
                    .position(|a| a.parts().record.id == value.parts().record.id);
                match (index, expected_authorization_version) {
                    (None, None) => {
                        let new = value.parts();
                        let original = state
                            .recovery()
                            .parts()
                            .enrollments
                            .iter()
                            .find(|e| e.parts().target.parts().pairing == new.record.id)
                            .ok_or(PairingFailure::InvalidDurableState)?
                            .parts();
                        let consumed = transition.writes.parts().writes.iter().any(|write| matches!(write,
                            PairingWrite::PutEnrollmentReservation { expected_kind: Some(EnrollmentReservationKind::Reserved), value }
                            if value.parts().ticket == original.ticket && value.parts().state.kind() == EnrollmentReservationKind::Consumed));
                        if !consumed
                            || original.state.kind() != EnrollmentReservationKind::Reserved
                            || new.record.device != original.target.parts().device
                            || new.keys != original.keys
                            || new.clients.len() != 1
                            || new.clients[0].parts().client.id != original.initial_client
                        {
                            return Err(PairingFailure::InvalidDurableState);
                        }
                        candidate.authorizations.push(value.clone());
                    }
                    (Some(index), Some(expected)) => {
                        let old = candidate.authorizations[index].parts();
                        let new = value.parts();
                        if old.authorization_version != *expected
                            || old.record.state != PairingState::Authorized
                            || old.record.device != new.record.device
                            || old.keys != new.keys
                            || !old.clients.iter().all(|c| new.clients.contains(c))
                            || (new.record.state != PairingState::Authorized
                                && old.clients != new.clients)
                        {
                            return Err(PairingFailure::InvalidDurableState);
                        }
                        match new.record.state {
                            PairingState::Authorized => {
                                let added: Vec<_> = new
                                    .clients
                                    .iter()
                                    .filter(|c| !old.clients.contains(c))
                                    .collect();
                                if added.len() != 1
                                    || added[0].parts().client.id.local()
                                        <= state.high_water().client
                                    || added[0].parts().ordinary.key().local()
                                        <= state.high_water().operation_stream
                                    || added[0].parts().self_revocation.key().local()
                                        <= state.high_water().operation_stream
                                {
                                    return Err(PairingFailure::InvalidDurableState);
                                }
                            }
                            PairingState::Revoked { revision: recorded }
                                if recorded == revision => {}
                            _ => return Err(PairingFailure::InvalidDurableState),
                        }
                        candidate.authorizations[index] = value.clone();
                    }
                    _ => return Err(PairingFailure::InvalidDurableState),
                }
            }
            PairingWrite::PutEnrollmentReservation {
                expected_kind,
                value,
            } => {
                let index = candidate
                    .enrollments
                    .iter()
                    .position(|e| e.parts().ticket == value.parts().ticket);
                match (index, expected_kind) {
                    (None, None) => {
                        let e = value.parts();
                        if e.target.parts().pairing.local() <= state.high_water().pairing
                            || e.initial_client.local() <= state.high_water().client
                            || e.enrollment_stream.key().local()
                                <= state.high_water().operation_stream
                            || e.issued_epoch != state.epoch()
                            || e.issued_revision != revision
                        {
                            return Err(PairingFailure::InvalidDurableState);
                        }
                        candidate.enrollments.push(value.clone());
                    }
                    (Some(index), Some(EnrollmentReservationKind::Reserved)) => {
                        let old = candidate.enrollments[index].parts();
                        let new = value.parts();
                        let mut compare = new.clone();
                        compare.state = old.state.clone();
                        if old.state.kind() != EnrollmentReservationKind::Reserved
                            || compare != *old
                            || new.state.kind() == EnrollmentReservationKind::Reserved
                        {
                            return Err(PairingFailure::InvalidDurableState);
                        }
                        if let EnrollmentReservationState::Consumed { receipt, .. } = &new.state
                            && receipt.parts().claims.revision() != revision
                        {
                            return Err(PairingFailure::InvalidDurableState);
                        }
                        candidate.enrollments[index] = value.clone();
                    }
                    _ => return Err(PairingFailure::InvalidDurableState),
                }
            }
        }
    }
    let recovery =
        PairingRecovery::new(candidate).map_err(|_| PairingFailure::InvalidDurableState)?;
    recovery
        .validate_receipts(validator)
        .map_err(|_| PairingFailure::ReceiptInvalid)?;
    PairingSnapshot::new(
        state.server(),
        state.epoch(),
        revision,
        transition.high_water,
        recovery,
    )
}
