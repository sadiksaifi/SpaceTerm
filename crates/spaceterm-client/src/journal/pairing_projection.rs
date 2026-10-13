//! Relational projection of pairing's public recovery facts, not credential authority.
//!
//! The pairing port maps CredentialScope to the namespace/server/reference columns below,
//! PreparedCredential to those columns plus public_key, and PreparedEnrollment to
//! JournalEnrollment. The pairing facade uses the aggregate journal revision for
//! every CAS. Saved bindings, suppression and revocation phases stay in their
//! existing canonical journal records; this projection does not duplicate them.
//! No enrollment bearer, private key, credential lease or native authority is stored.

use super::*;

// Includes JSON expansion of bounded route and receipt bytes in the native
// aggregate. Preparation reserves BOTH activation and restricted ack residency.
pub(super) const PAIRING_RECORD_RESERVATION: usize = 32 * 1024;
const ENROLLMENT_RESERVATION: usize = 2 * PAIRING_RECORD_RESERVATION;

use spaceterm_core::{
    operations::RequestDigest,
    pairing::{EnrollmentTicketId, PairingKeys, RevocationRouteReference},
};

/// Public OpenSSH effective configuration captured before enrollment. It is
/// comparison data, not local file or credential authority. Older slots have no
/// capture and native restricted SSH retries refuse them.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct JournalSshCapture(String);
impl JournalSshCapture {
    pub const MAX_BYTES: usize = 16 * 1024;
    pub fn new(configuration: String) -> Result<Self, ClientFailure> {
        if configuration.is_empty()
            || configuration.len() > Self::MAX_BYTES
            || configuration.bytes().any(|b| b == 0 || b == b'\r')
        {
            return Err(invalid());
        }
        Ok(Self(configuration))
    }
    pub fn configuration(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for JournalSshCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JournalSshCapture(..)")
    }
}
impl TryFrom<String> for JournalSshCapture {
    type Error = ClientFailure;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<JournalSshCapture> for String {
    fn from(value: JournalSshCapture) -> Self {
        value.0
    }
}

/// A never-reused native slot and its public comparison metadata. None in server
/// denotes the shared device-identity key; Some denotes pairing authentication.
/// Pairing's native provider alone turns these columns into CredentialScope.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JournalCredentialSlot {
    pub namespace: String,
    pub server: Option<ServerId>,
    pub reference: CredentialReference,
    pub public_key: Option<[u8; 32]>,
    #[serde(default)]
    pub route_reference: Option<RevocationRouteReference>,
    /// Present only before enrollment transfers this route into SavedTarget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteDescription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_capture: Option<JournalSshCapture>,
    pub generation: LocalPairingGeneration,
}
impl std::fmt::Debug for JournalCredentialSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JournalCredentialSlot(..)")
    }
}
impl JournalCredentialSlot {
    fn validate(&self) -> Result<(), ClientFailure> {
        if self.namespace.is_empty()
            || self.namespace.len() > 96
            || !self
                .namespace
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
            || self.reference.opaque() == [0; 16]
            || self.server.is_some_and(|server| server.bytes() == [0; 16])
            || self.public_key == Some([0; 32])
            || (self.ssh_capture.is_some()
                && (self.server.is_none() || self.route_reference.is_none()))
            || self
                .route_reference
                .is_some_and(|route| self.server.is_none() || route.opaque() == [0; 16])
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Fixed columns for pairing_adapter::PublicEnrollmentReservation. There is no
/// authorization digest, issuer, expiry or bearer in this public-only projection.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEnrollmentReservation {
    pub ticket: EnrollmentTicketId,
    pub claim_digest: RequestDigest,
}

/// PreparedEnrollment's other reservation fields are already present in target,
/// keys and enrollment. Keep them once and reconstruct the public wire record at
/// the pairing boundary. The immutable concrete route is held by SavedTarget;
/// route_reference is pairing's canonical comparison-only route identity.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEnrollment {
    pub target: SavedTarget,
    pub keys: PairingKeys,
    pub route_reference: RevocationRouteReference,
    pub enrollment: OperationBinding,
    pub reservation: Option<JournalEnrollmentReservation>,
}
impl std::fmt::Debug for JournalEnrollment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JournalEnrollment(..)")
    }
}

/// The verified binding and this acknowledgement work commit in the SAME local
/// transaction. The full prepared record survives until restricted ack confirms.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEnrollmentAcknowledgement {
    pub prepared: JournalEnrollment,
    pub device_credential: CredentialReference,
    pub receipt: SignedPairingReceipt,
}
impl std::fmt::Debug for JournalEnrollmentAcknowledgement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JournalEnrollmentAcknowledgement(..)")
    }
}

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingJournalProjection {
    #[serde(with = "fixed_u64")]
    generation_high_water: u64,
    #[serde(deserialize_with = "bounded::headers")]
    pub(super) slots: Vec<JournalCredentialSlot>,
    #[serde(deserialize_with = "bounded::revocations")]
    pub(super) credential_preparations: Vec<CredentialReference>,
    #[serde(deserialize_with = "bounded::revocations")]
    prepared: Vec<JournalEnrollment>,
    #[serde(deserialize_with = "bounded::revocations")]
    acknowledgements: Vec<JournalEnrollmentAcknowledgement>,
}
impl std::fmt::Debug for PairingJournalProjection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingJournalProjection(..)")
    }
}
impl PairingJournalProjection {
    pub fn generation_high_water(&self) -> u64 {
        self.generation_high_water
    }
    pub(super) fn retain_generation(&mut self, generation: LocalPairingGeneration) {
        self.generation_high_water = self.generation_high_water.max(generation.get());
    }
    pub fn slots(&self) -> &[JournalCredentialSlot] {
        &self.slots
    }
    pub fn credential_preparations(&self) -> &[CredentialReference] {
        &self.credential_preparations
    }
    pub fn prepared(&self) -> &[JournalEnrollment] {
        &self.prepared
    }
    pub fn acknowledgements(&self) -> &[JournalEnrollmentAcknowledgement] {
        &self.acknowledgements
    }
    pub fn slot(&self, reference: CredentialReference) -> Option<&JournalCredentialSlot> {
        self.slots.iter().find(|slot| slot.reference == reference)
    }
    pub(super) fn validate(
        &self,
        state: &RecoveredClientState,
        verifier: &dyn ReceiptVerifier,
        limits: JournalLimits,
    ) -> Result<usize, ClientFailure> {
        use std::collections::BTreeSet;
        let mut references = BTreeSet::new();
        let mut generations = BTreeSet::new();
        if self.slots.len() > 4096 {
            return Err(crate::failure::resource_limit());
        }
        let mut resident = 0usize;
        for slot in &self.slots {
            slot.validate()?;
            if !references.insert(slot.reference.opaque())
                || !generations.insert(slot.generation.get())
                || slot.generation.get() > self.generation_high_water
            {
                return Err(invalid());
            }
            resident = resident
                .checked_add(
                    256 + slot
                        .ssh_capture
                        .as_ref()
                        .map_or(0, |capture| capture.configuration().len() * 2),
                )
                .ok_or_else(crate::failure::resource_limit)?;
        }
        let reservations = state
            .bindings
            .len()
            .checked_add(state.revocations.len())
            .and_then(|n| n.checked_add(self.credential_preparations.len()))
            .and_then(|n| n.checked_add(self.prepared.len()))
            .ok_or_else(crate::failure::resource_limit)?;
        if reservations > limits.pending_revocations {
            return Err(crate::failure::resource_limit());
        }
        let mut owned = BTreeSet::new();
        for reference in &self.credential_preparations {
            let slot = self.slot(*reference).ok_or_else(invalid)?;
            if !owned.insert(reference.opaque()) {
                return Err(invalid());
            }
            let reserve = if slot.server.is_some() {
                ENROLLMENT_RESERVATION
            } else {
                PAIRING_RECORD_RESERVATION
            };
            resident = resident
                .checked_add(reserve)
                .ok_or_else(crate::failure::resource_limit)?;
        }
        let mut operations = BTreeSet::new();
        for prepared in &self.prepared {
            self.validate_enrollment(prepared)?;
            let reference = prepared.target.credential;
            if self
                .slot(reference)
                .is_none_or(|slot| slot.route_reference != Some(prepared.route_reference))
                || !owned.insert(reference.opaque())
                || !operations.insert(prepared.enrollment.id)
            {
                return Err(invalid());
            }
            resident = resident
                .checked_add(ENROLLMENT_RESERVATION)
                .ok_or_else(crate::failure::resource_limit)?;
        }
        for ack in &self.acknowledgements {
            self.validate_enrollment(&ack.prepared)?;
            if ack.prepared.reservation.is_none()
                || self
                    .slot(ack.prepared.target.credential)
                    .is_none_or(|slot| slot.route_reference != Some(ack.prepared.route_reference))
                || !operations.insert(ack.prepared.enrollment.id)
            {
                return Err(invalid());
            }
            let device = self.slot(ack.device_credential).ok_or_else(invalid)?;
            let pairing = self
                .slot(ack.prepared.target.credential)
                .ok_or_else(invalid)?;
            if device.server.is_some()
                || device.namespace != pairing.namespace
                || device.public_key != Some(ack.prepared.keys.parts().device_identity_public_key)
            {
                return Err(invalid());
            }
            validate_enrollment_receipt(&ack.prepared, &ack.receipt, verifier)?;
            resident = resident
                .checked_add(PAIRING_RECORD_RESERVATION)
                .ok_or_else(crate::failure::resource_limit)?;
        }
        // A pairing-specific slot can have one preparation or one original pairing
        // owner. A shared device slot is never a revocation cleanup target.
        for saved in &state.bindings {
            if let Some(slot) = self.slot(saved.target.credential) {
                let target = saved.target.target()?;
                let keys = match &saved.proof.parts().claims {
                    PairingReceiptBody::Enrollment { keys, .. }
                    | PairingReceiptBody::ClientBinding { keys, .. } => keys,
                    _ => return Err(invalid()),
                };
                validate_pairing_slot(slot, &target, keys)?;
                if slot.route_reference.is_none() || !owned.insert(slot.reference.opaque()) {
                    return Err(invalid());
                }
            }
        }
        for record in &state.revocations {
            if let Some(slot) = self.slot(record.target.credential)
                && (slot.server != Some(record.target.server)
                    || slot.generation != record.target.generation
                    || slot.route_reference != Some(record.task.parts().original.route())
                    || slot.public_key.is_none()
                    || !owned.insert(slot.reference.opaque()))
            {
                return Err(invalid());
            }
        }
        Ok(resident)
    }
    fn validate_enrollment(&self, prepared: &JournalEnrollment) -> Result<(), ClientFailure> {
        let target = prepared.target.target()?;
        prepared.keys.validate().map_err(|_| invalid())?;
        let slot = self.slot(target.credential()).ok_or_else(invalid)?;
        validate_pairing_slot(slot, &target, &prepared.keys)?;
        if prepared.route_reference.opaque() == [0; 16]
            || prepared.enrollment.id.stream().pairing() != target.pairing()
            || prepared.enrollment.id.stream().client() != target.client()
            || prepared.enrollment.id.sequence().get() != 1
            || prepared
                .reservation
                .as_ref()
                .is_some_and(|r| r.ticket.parts().server != target.server())
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn apply(
        state: &mut RecoveredClientState,
        change: &ClientJournalChange,
        verifier: &dyn ReceiptVerifier,
    ) -> Result<bool, ClientFailure> {
        match change {
            ClientJournalChange::ReserveCredential(slot) => {
                slot.validate()?;
                let projection = &mut state.pairing;
                if slot.public_key.is_some()
                    || slot.ssh_capture.is_some()
                    || slot.route_reference.is_some()
                    || slot.route.is_some()
                    || projection.slot(slot.reference).is_some()
                    || projection.generation_high_water.checked_add(1)
                        != Some(slot.generation.get())
                {
                    return Err(invalid());
                }
                projection.generation_high_water = slot.generation.get();
                projection.credential_preparations.push(slot.reference);
                projection.slots.push(slot.clone());
            }
            ClientJournalChange::BindCredentialRoute {
                reference,
                route_reference,
                route,
            } => {
                let projection = &mut state.pairing;
                if !projection.credential_preparations.contains(reference)
                    || route_reference.opaque() == [0; 16]
                {
                    return Err(invalid());
                }
                let slot = projection
                    .slots
                    .iter_mut()
                    .find(|slot| slot.reference == *reference && slot.server.is_some())
                    .ok_or_else(invalid)?;
                if slot
                    .route_reference
                    .is_some_and(|old| old != *route_reference)
                    || slot.route.as_ref().is_some_and(|old| old != route)
                {
                    return Err(invalid());
                }
                slot.route_reference = Some(*route_reference);
                slot.route = Some(route.clone());
            }
            ClientJournalChange::CaptureRestrictedSsh { reference, capture } => {
                let projection = &mut state.pairing;
                if !projection.credential_preparations.contains(reference) {
                    return Err(invalid());
                }
                let slot = projection
                    .slots
                    .iter_mut()
                    .find(|slot| slot.reference == *reference)
                    .ok_or_else(invalid)?;
                if slot.route_reference.is_none()
                    || !matches!(
                        slot.route.as_ref().map(RouteDescription::parts),
                        Some(crate::api::RouteParts::Ssh { .. })
                    )
                    || slot.ssh_capture.as_ref().is_some_and(|old| old != capture)
                {
                    return Err(invalid());
                }
                slot.ssh_capture = Some(capture.clone());
            }
            ClientJournalChange::RecordCredentialPublicKey {
                reference,
                public_key,
            } => {
                let projection = &mut state.pairing;
                if !projection.credential_preparations.contains(reference) || *public_key == [0; 32]
                {
                    return Err(invalid());
                }
                let slot = projection
                    .slots
                    .iter_mut()
                    .find(|slot| slot.reference == *reference)
                    .ok_or_else(invalid)?;
                if slot.public_key.is_some_and(|old| old != *public_key) {
                    return Err(invalid());
                }
                slot.public_key = Some(*public_key);
            }
            ClientJournalChange::PrepareEnrollment(prepared) => {
                state.pairing.validate_enrollment(prepared)?;
                let reference = prepared.target.credential;
                let projection = &mut state.pairing;
                let index = projection
                    .credential_preparations
                    .iter()
                    .position(|old| *old == reference)
                    .ok_or_else(invalid)?;
                let slot = projection
                    .slots
                    .iter_mut()
                    .find(|slot| slot.reference == reference)
                    .ok_or_else(invalid)?;
                if slot
                    .route_reference
                    .is_some_and(|old| old != prepared.route_reference)
                {
                    return Err(invalid());
                }
                slot.route_reference = Some(prepared.route_reference);
                slot.route = None;
                projection.credential_preparations.remove(index);
                projection.prepared.push(*prepared.clone());
            }
            ClientJournalChange::SaveEnrollment {
                binding,
                acknowledgement,
            } => {
                let prepared = &acknowledgement.prepared;
                state.pairing.validate_enrollment(prepared)?;
                validate_enrollment_receipt(prepared, &acknowledgement.receipt, verifier)?;
                if binding.target != prepared.target || binding.proof != acknowledgement.receipt {
                    return Err(invalid());
                }
                let index = state
                    .pairing
                    .prepared
                    .iter()
                    .position(|old| old == prepared)
                    .ok_or_else(invalid)?;
                state.pairing.prepared.remove(index);
                state
                    .pairing
                    .acknowledgements
                    .push(*acknowledgement.clone());
                // The ordinary SaveBinding transition is applied by the caller in
                // this SAME aggregate transaction, never by a second physical store.
            }
            ClientJournalChange::FinishEnrollmentAcknowledgement(id) => {
                let index = state
                    .pairing
                    .acknowledgements
                    .iter()
                    .position(|ack| ack.prepared.enrollment.id == *id)
                    .ok_or_else(invalid)?;
                state.pairing.acknowledgements.remove(index);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
fn validate_pairing_slot(
    slot: &JournalCredentialSlot,
    target: &ConnectionTarget,
    keys: &PairingKeys,
) -> Result<(), ClientFailure> {
    if slot.server != Some(target.server())
        || slot.generation != target.incarnation()
        || slot.public_key != Some(keys.parts().pairing_authentication_public_key)
    {
        return Err(invalid());
    }
    Ok(())
}
fn validate_enrollment_receipt(
    prepared: &JournalEnrollment,
    receipt: &SignedPairingReceipt,
    verifier: &dyn ReceiptVerifier,
) -> Result<(), ClientFailure> {
    let target = prepared.target.target()?;
    receipt.validate().map_err(|_| invalid())?;
    match &receipt.parts().claims {
        PairingReceiptBody::Enrollment {
            target: claim,
            keys,
            enrollment,
            client,
            ..
        } if claim.parts().server == target.server()
            && claim.parts().pairing == target.pairing()
            && claim.parts().device == target.device()
            && keys == &prepared.keys
            && *enrollment == prepared.enrollment
            && client.parts().client.id == target.client() => {}
        _ => return Err(invalid()),
    }
    verifier
        .verify(target.pin(), receipt)
        .map_err(|_| invalid())
}
