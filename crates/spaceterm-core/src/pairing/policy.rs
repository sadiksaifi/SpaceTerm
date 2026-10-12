//! Deterministic pairing policy. Inputs contain verified comparison facts, not
//! clocks, native authority, bearer secrets, or executable effects.

use super::transition::PairingTransition;
use super::*;
use crate::authority::{FieldVersion, ServerEpoch, StoreRevision};
use crate::identity::{
    ClientId, DeviceId, IdentityHighWater, OperationStreamId, PairingId, ServerId,
};
use crate::operation_ledger_contracts::{
    OutcomeReservation, ReservationClass, StreamBinding, StreamPurpose, StreamRecord, StreamStatus,
};
use crate::operations::{AssessmentRef, OperationBinding};
use std::collections::BTreeSet;

/// Store-validated comparison snapshot. No principal is inferred from a receipt.
#[derive(Clone)]
pub struct PairingSnapshot {
    server: ServerId,
    epoch: ServerEpoch,
    revision: StoreRevision,
    marks: IdentityHighWater,
    recovery: PairingRecovery,
}
impl PairingSnapshot {
    pub fn new(
        server: ServerId,
        epoch: ServerEpoch,
        revision: StoreRevision,
        marks: IdentityHighWater,
        recovery: PairingRecovery,
    ) -> Result<Self, PairingFailure> {
        if server.bytes() == [0; 16] || epoch.get() == 0 {
            return Err(PairingFailure::InvalidDurableState);
        }
        recovery
            .validate_for(server, revision, marks)
            .map_err(|_| PairingFailure::InvalidDurableState)?;
        Ok(Self {
            server,
            epoch,
            revision,
            marks,
            recovery,
        })
    }
    pub const fn server(&self) -> ServerId {
        self.server
    }
    pub const fn epoch(&self) -> ServerEpoch {
        self.epoch
    }
    pub const fn revision(&self) -> StoreRevision {
        self.revision
    }
    pub const fn high_water(&self) -> IdentityHighWater {
        self.marks
    }
    pub fn recovery(&self) -> &PairingRecovery {
        &self.recovery
    }
    pub fn authorization(
        &self,
        pairing: PairingId,
    ) -> Result<&PairingAuthorization, PairingFailure> {
        if pairing.server() != self.server {
            return Err(PairingFailure::IdentityMismatch);
        }
        self.recovery
            .parts()
            .authorizations
            .iter()
            .find(|a| a.parts().record.id == pairing)
            .ok_or(PairingFailure::AuthorizationDenied)
    }
    pub fn reservation(
        &self,
        ticket: &EnrollmentTicketId,
    ) -> Result<&EnrollmentReservation, PairingFailure> {
        if ticket.parts().server != self.server {
            return Err(PairingFailure::IdentityMismatch);
        }
        self.recovery
            .parts()
            .enrollments
            .iter()
            .find(|e| &e.parts().ticket == ticket)
            .ok_or(PairingFailure::AuthorizationDenied)
    }
}

/// Comparison principal produced after authentication. Every mutation rechecks it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingPrincipal {
    server: ServerId,
    pairing: PairingId,
    device: DeviceId,
    client: ClientId,
}
impl PairingPrincipal {
    pub const fn server(self) -> ServerId {
        self.server
    }
    pub const fn pairing(self) -> PairingId {
        self.pairing
    }
    pub const fn device(self) -> DeviceId {
        self.device
    }
    pub const fn client(self) -> ClientId {
        self.client
    }
}

/// Smaller limits support admission tests. They cannot enlarge production bounds.
#[derive(Clone, Copy)]
pub struct PairingLimits {
    pub active: usize,
    pub clients: usize,
    pub lifetimes: usize,
    pub outstanding: usize,
}
impl Default for PairingLimits {
    fn default() -> Self {
        Self {
            active: MAX_ACTIVE_PAIRINGS,
            clients: MAX_CLIENTS_PER_PAIRING,
            lifetimes: MAX_PAIRING_LIFETIMES,
            outstanding: MAX_OUTSTANDING_ENROLLMENTS,
        }
    }
}
impl PairingLimits {
    pub fn validate(self) -> Result<Self, PairingFailure> {
        let max = Self::default();
        if self.active == 0
            || self.active > max.active
            || self.clients == 0
            || self.clients > max.clients
            || self.lifetimes == 0
            || self.lifetimes > max.lifetimes
            || self.outstanding == 0
            || self.outstanding > max.outstanding
        {
            return Err(PairingFailure::InvalidRequest);
        }
        Ok(self)
    }
}
#[derive(Default)]
pub struct PairingPolicy {
    limits: PairingLimits,
}

/// The adapter has verified claim signatures and the transient authorization.
/// Policy compares their digests with the durable reservation independently.
pub struct EnrollmentMutation {
    pub ticket: EnrollmentTicketId,
    pub enrollment: OperationBinding,
    pub claim_digest: crate::operations::RequestDigest,
    pub authorization_digest: crate::operations::RequestDigest,
    pub now_millis: u64,
    pub client: PairingClientStreams,
    pub receipt: SignedPairingReceipt,
}
impl PairingPolicy {
    pub fn new(limits: PairingLimits) -> Result<Self, PairingFailure> {
        Ok(Self {
            limits: limits.validate()?,
        })
    }
    pub fn authorize_client(
        &self,
        state: &PairingSnapshot,
        pairing: PairingId,
        client: ClientId,
    ) -> Result<PairingPrincipal, PairingFailure> {
        let a = state.authorization(pairing)?.parts();
        if a.record.state != PairingState::Authorized {
            return Err(PairingFailure::PairingRevoked);
        }
        if !a.clients.iter().any(|c| c.parts().client.id == client) {
            return Err(PairingFailure::ClientNotBound);
        }
        Ok(PairingPrincipal {
            server: state.server,
            pairing,
            device: a.record.device,
            client,
        })
    }
    fn actor(
        &self,
        state: &PairingSnapshot,
        actor: &PairingPrincipal,
        operation: OperationBinding,
    ) -> Result<(), PairingFailure> {
        if self.authorize_client(state, actor.pairing, actor.client)? != *actor {
            return Err(PairingFailure::IdentityMismatch);
        }
        let a = state.authorization(actor.pairing)?.parts();
        if !a.clients.iter().any(|c| {
            c.parts().client.id == actor.client && c.parts().ordinary == operation.id.stream()
        }) {
            return Err(PairingFailure::ClientNotBound);
        }
        Ok(())
    }
    fn issuer(
        &self,
        state: &PairingSnapshot,
        issuer: &EnrollmentIssuer,
    ) -> Result<(), PairingFailure> {
        if let EnrollmentIssuer::Paired {
            operation,
            authorization_version,
        } = issuer
        {
            let actor = self.authorize_client(
                state,
                operation.id.stream().pairing(),
                operation.id.stream().client(),
            )?;
            self.actor(state, &actor, *operation)?;
            if state
                .authorization(actor.pairing)?
                .parts()
                .authorization_version
                != *authorization_version
            {
                return Err(PairingFailure::StaleAssessment);
            }
        }
        // Account-owner capability validation belongs to the native actor boundary.
        Ok(())
    }
    /// Expiry and epoch changes never erase prospective identities. The actor
    /// commits this proposal before issuing replacement authorizations or
    /// publishing startup readiness. Consumed receipts are not invalidated.
    pub fn prepare_ticket_invalidations(
        &self,
        state: &PairingSnapshot,
        now_millis: u64,
    ) -> Result<super::transition::PairingTicketInvalidations, PairingFailure> {
        let mut writes = Vec::new();
        let mut retire_streams = Vec::new();
        for reservation in &state.recovery.parts().enrollments {
            let e = reservation.parts();
            if e.state.kind() == EnrollmentReservationKind::Reserved
                && (e.issued_epoch != state.epoch || now_millis >= e.expires_at_millis)
            {
                let mut invalidated = e.clone();
                invalidated.state = EnrollmentReservationState::Invalidated;
                writes.push(PairingWrite::PutEnrollmentReservation {
                    expected_kind: Some(EnrollmentReservationKind::Reserved),
                    value: EnrollmentReservation::new(invalidated).map_err(contract_failure)?,
                });
                retire_streams.push(e.enrollment_stream);
            }
        }
        Ok(super::transition::PairingTicketInvalidations {
            expected_store: state.revision,
            writes: PairingWrites::new(PairingWritesParts { writes }).map_err(contract_failure)?,
            retire_streams,
        })
    }
    pub fn check_reservation_capacity(
        &self,
        state: &PairingSnapshot,
        device: DeviceId,
    ) -> Result<(), PairingFailure> {
        self.enrollment_capacity(state, true)?;
        if state.recovery.parts().authorizations.iter().any(|a| {
            a.parts().record.device == device && a.parts().record.state == PairingState::Authorized
        }) {
            return Err(PairingFailure::AuthorizationConsumed);
        }
        Ok(())
    }
    pub fn prepare_reservation(
        &self,
        state: &PairingSnapshot,
        reservation: EnrollmentReservation,
    ) -> Result<PairingTransition, PairingFailure> {
        let e = reservation.parts();
        if e.state.kind() != EnrollmentReservationKind::Reserved
            || e.target.parts().server != state.server
            || e.issued_epoch != state.epoch
            || state.revision.checked_successor() != Some(e.issued_revision)
        {
            return Err(PairingFailure::IdentityMismatch);
        }
        self.issuer(state, &e.issuer)?;
        if state
            .recovery
            .parts()
            .enrollments
            .iter()
            .any(|old| old.parts().ticket == e.ticket)
        {
            return Err(PairingFailure::RequestIdentityReused);
        }
        if e.target.parts().pairing.local() <= state.marks.pairing
            || e.initial_client.local() <= state.marks.client
            || e.enrollment_stream.key().local() <= state.marks.operation_stream
        {
            return Err(PairingFailure::RequestIdentityReused);
        }
        self.check_reservation_capacity(state, e.target.parts().device)?;
        let mut marks = state.marks;
        marks.pairing = e.target.parts().pairing.local();
        marks.client = e.initial_client.local();
        marks.operation_stream = e.enrollment_stream.key().local();
        let stream = stream_record(
            e.enrollment_stream,
            StreamPurpose::Enrollment {
                claim_digest: e.claim_digest,
            },
        )?;
        let outcome = PairingOutcome::AuthorizationReserved {
            ticket: e.ticket.clone(),
            enrollment: crate::operations::OperationId::new(
                e.enrollment_stream,
                crate::operations::OperationSequence::new(1)
                    .map_err(|_| PairingFailure::InvalidRequest)?,
            ),
            claim_digest: e.claim_digest,
        };
        self.transition(
            state,
            marks,
            vec![PairingWrite::PutEnrollmentReservation {
                expected_kind: None,
                value: reservation,
            }],
            vec![stream],
            vec![],
            vec![],
            outcome,
        )
    }
    fn enrollment_capacity(
        &self,
        state: &PairingSnapshot,
        reserving: bool,
    ) -> Result<(), PairingFailure> {
        let p = state.recovery.parts();
        let active = p
            .authorizations
            .iter()
            .filter(|a| a.parts().record.state == PairingState::Authorized)
            .count();
        let outstanding = p
            .enrollments
            .iter()
            .filter(|e| e.parts().state.kind() == EnrollmentReservationKind::Reserved)
            .count();
        let lifetimes: BTreeSet<_> = p
            .authorizations
            .iter()
            .map(|a| a.parts().record.id)
            .chain(
                p.enrollments
                    .iter()
                    .map(|e| e.parts().target.parts().pairing),
            )
            .collect();
        if active >= self.limits.active
            || (reserving
                && (outstanding >= self.limits.outstanding
                    || lifetimes.len() >= self.limits.lifetimes))
        {
            return Err(PairingFailure::LimitExceeded);
        }
        Ok(())
    }
    /// Resolve committed enrollment before expiry, epoch, issuer or secret checks.
    /// Historical lookup still requires proof of the originally bound two keys.
    pub fn completed_enrollment(
        &self,
        state: &PairingSnapshot,
        ticket: &EnrollmentTicketId,
        binding: OperationBinding,
        claim_digest: crate::operations::RequestDigest,
    ) -> Result<Option<SignedPairingReceipt>, PairingFailure> {
        let e = state.reservation(ticket)?.parts();
        if binding.id.stream() != e.enrollment_stream
            || binding.id.sequence().get() != 1
            || binding.digest != e.enrollment_request_digest
            || claim_digest != e.claim_digest
        {
            return Err(PairingFailure::RequestIdentityReused);
        }
        Ok(match &e.state {
            EnrollmentReservationState::Consumed {
                enrollment,
                receipt,
            } if *enrollment == binding => Some((**receipt).clone()),
            EnrollmentReservationState::Consumed { .. } => {
                return Err(PairingFailure::RequestIdentityReused);
            }
            _ => None,
        })
    }
    pub fn check_enrollment_authorization(
        &self,
        state: &PairingSnapshot,
        ticket: &EnrollmentTicketId,
        binding: OperationBinding,
        claim_digest: crate::operations::RequestDigest,
        authorization_digest: crate::operations::RequestDigest,
        now_millis: u64,
    ) -> Result<Option<SignedPairingReceipt>, PairingFailure> {
        if let Some(receipt) = self.completed_enrollment(state, ticket, binding, claim_digest)? {
            return Ok(Some(receipt));
        }
        let e = state.reservation(ticket)?.parts();
        if e.state.kind() != EnrollmentReservationKind::Reserved {
            return Err(PairingFailure::AuthorizationConsumed);
        }
        if e.issued_epoch != state.epoch
            || now_millis < e.issued_at_millis
            || now_millis >= e.expires_at_millis
        {
            return Err(PairingFailure::AuthorizationExpired);
        }
        if e.authorization_digest != authorization_digest {
            return Err(PairingFailure::AuthorizationDenied);
        }
        self.issuer(state, &e.issuer)?;
        self.enrollment_capacity(state, false)?;
        if state.recovery.parts().authorizations.iter().any(|a| {
            a.parts().record.device == e.target.parts().device
                && a.parts().record.state == PairingState::Authorized
        }) {
            return Err(PairingFailure::AuthorizationConsumed);
        }
        Ok(None)
    }
    pub fn prepare_enrollment(
        &self,
        state: &PairingSnapshot,
        request: EnrollmentMutation,
    ) -> Result<PairingTransition, PairingFailure> {
        if let Some(receipt) = self.check_enrollment_authorization(
            state,
            &request.ticket,
            request.enrollment,
            request.claim_digest,
            request.authorization_digest,
            request.now_millis,
        )? {
            return self.transition(
                state,
                state.marks,
                vec![],
                vec![],
                vec![],
                vec![],
                PairingOutcome::Enrolled(receipt),
            );
        }
        let e = state.reservation(&request.ticket)?.parts();
        let c = request.client.parts();
        if c.client.id != e.initial_client || c.client.pairing != e.target.parts().pairing {
            return Err(PairingFailure::IdentityMismatch);
        }
        let marks = self.fresh_streams(state, &request.client)?;
        let claims = PairingReceiptBody::Enrollment {
            target: e.target.clone(),
            keys: e.keys.clone(),
            client: request.client.clone(),
            enrollment: request.enrollment,
            revision: next_revision(state)?,
        };
        if request.receipt.parts().claims != claims {
            return Err(PairingFailure::ReceiptInvalid);
        }
        let authorization = PairingAuthorization::new(PairingAuthorizationParts {
            record: PairingRecord {
                id: e.target.parts().pairing,
                device: e.target.parts().device,
                state: PairingState::Authorized,
            },
            authorization_version: FieldVersion::new(1),
            keys: e.keys.clone(),
            clients: vec![request.client.clone()],
            revocation_receipt: None,
        })
        .map_err(contract_failure)?;
        let mut consumed = e.clone();
        consumed.state = EnrollmentReservationState::Consumed {
            enrollment: request.enrollment,
            receipt: Box::new(request.receipt.clone()),
        };
        let registration = client_stream_records(&request.client)?;
        self.transition(
            state,
            marks,
            vec![
                PairingWrite::PutAuthorization {
                    expected_authorization_version: None,
                    value: authorization,
                },
                PairingWrite::PutEnrollmentReservation {
                    expected_kind: Some(EnrollmentReservationKind::Reserved),
                    value: EnrollmentReservation::new(consumed).map_err(contract_failure)?,
                },
            ],
            registration,
            vec![],
            vec![],
            PairingOutcome::Enrolled(request.receipt),
        )
    }
    fn fresh_streams(
        &self,
        state: &PairingSnapshot,
        client: &PairingClientStreams,
    ) -> Result<IdentityHighWater, PairingFailure> {
        let c = client.parts();
        if c.ordinary.server() != state.server
            || c.ordinary.key().local() <= state.marks.operation_stream
            || c.self_revocation.key().local() <= state.marks.operation_stream
        {
            return Err(PairingFailure::RequestIdentityReused);
        }
        let mut marks = state.marks;
        marks.operation_stream = c
            .ordinary
            .key()
            .local()
            .max(c.self_revocation.key().local());
        Ok(marks)
    }
    /// New Client identity and both streams are actor-issued after durable admission.
    pub fn prepare_client_binding(
        &self,
        state: &PairingSnapshot,
        actor: &PairingPrincipal,
        operation: OperationBinding,
        expected: FieldVersion,
        client: PairingClientStreams,
        receipt: SignedPairingReceipt,
    ) -> Result<PairingTransition, PairingFailure> {
        self.actor(state, actor, operation)?;
        let a = state.authorization(actor.pairing)?.parts();
        if a.authorization_version != expected {
            return Err(PairingFailure::StaleAssessment);
        }
        if a.clients.len() >= self.limits.clients {
            return Err(PairingFailure::LimitExceeded);
        }
        if client.parts().client.pairing != actor.pairing
            || client.parts().client.id.local() <= state.marks.client
        {
            return Err(PairingFailure::RequestIdentityReused);
        }
        let mut marks = self.fresh_streams(state, &client)?;
        marks.client = client.parts().client.id.local();
        let target = PairingTarget::new(PairingTargetParts {
            server: actor.server,
            pairing: actor.pairing,
            device: actor.device,
        })
        .map_err(contract_failure)?;
        if receipt.parts().claims
            != (PairingReceiptBody::ClientBinding {
                target,
                keys: a.keys.clone(),
                client: client.clone(),
                operation,
                revision: next_revision(state)?,
            })
        {
            return Err(PairingFailure::ReceiptInvalid);
        }
        let registration = client_stream_records(&client)?;
        let mut updated = a.clone();
        updated.authorization_version = expected
            .checked_successor()
            .ok_or(PairingFailure::LimitExceeded)?;
        updated.clients.push(client);
        updated.clients.sort_by_key(|c| c.parts().client.id);
        self.transition(
            state,
            marks,
            vec![PairingWrite::PutAuthorization {
                expected_authorization_version: Some(expected),
                value: PairingAuthorization::new(updated).map_err(contract_failure)?,
            }],
            registration,
            vec![],
            vec![],
            PairingOutcome::ClientBound(receipt),
        )
    }
    pub fn assess_unpair(
        &self,
        state: &PairingSnapshot,
        actor: &PairingPrincipal,
        reference: AssessmentRef,
        target: PairingTarget,
    ) -> Result<UnpairAssessment, PairingFailure> {
        self.actor(
            state,
            actor,
            OperationBinding {
                id: reference.operation,
                digest: reference.digest,
            },
        )?;
        let a = state.authorization(target.parts().pairing)?.parts();
        if a.record.device != target.parts().device {
            return Err(PairingFailure::IdentityMismatch);
        }
        if a.record.state != PairingState::Authorized {
            return Err(PairingFailure::PairingRevoked);
        }
        UnpairAssessment::new(UnpairAssessmentParts {
            reference,
            target,
            authorization_version: a.authorization_version,
            clients: a.clients.iter().map(|c| c.parts().client).collect(),
        })
        .map_err(contract_failure)
    }
    pub fn prepare_unpair(
        &self,
        state: &PairingSnapshot,
        actor: &PairingPrincipal,
        operation: OperationBinding,
        assessment: &UnpairAssessment,
        receipt: SignedPairingReceipt,
    ) -> Result<PairingTransition, PairingFailure> {
        self.actor(state, actor, operation)?;
        if assessment.parts().reference.operation.stream() != operation.id.stream() {
            return Err(PairingFailure::StaleAssessment);
        }
        let a = state
            .authorization(assessment.parts().target.parts().pairing)?
            .parts();
        if a.authorization_version != assessment.parts().authorization_version
            || a.clients
                .iter()
                .map(|c| c.parts().client)
                .collect::<Vec<_>>()
                != assessment.parts().clients
        {
            return Err(PairingFailure::StaleAssessment);
        }
        self.revoke(state, &assessment.parts().target, operation, receipt)
    }
    pub fn prepare_self_revocation(
        &self,
        state: &PairingSnapshot,
        target: &PairingTarget,
        operation: OperationBinding,
        receipt: SignedPairingReceipt,
    ) -> Result<PairingTransition, PairingFailure> {
        let a = state.authorization(target.parts().pairing)?.parts();
        if operation.id.sequence().get() != 1
            || !a
                .clients
                .iter()
                .any(|c| c.parts().self_revocation == operation.id.stream())
        {
            return Err(PairingFailure::ClientNotBound);
        }
        self.revoke(state, target, operation, receipt)
    }
    fn revoke(
        &self,
        state: &PairingSnapshot,
        target: &PairingTarget,
        operation: OperationBinding,
        receipt: SignedPairingReceipt,
    ) -> Result<PairingTransition, PairingFailure> {
        let a = state.authorization(target.parts().pairing)?.parts();
        if a.record.device != target.parts().device {
            return Err(PairingFailure::IdentityMismatch);
        }
        if let Some(original) = &a.revocation_receipt {
            return self.transition(
                state,
                state.marks,
                vec![],
                vec![],
                vec![],
                vec![],
                PairingOutcome::AlreadyRevoked(original.clone()),
            );
        }
        match &receipt.parts().claims {
            PairingReceiptBody::Revocation {
                receipt: body,
                authentication_key_digest,
                ..
            } if body.server == state.server
                && body.pairing == a.record.id
                && body.device == a.record.device
                && body.operation == operation.id
                && body.revision == next_revision(state)?
                && *authentication_key_digest == authentication_key_digest_for(&a.keys) => {}
            _ => return Err(PairingFailure::ReceiptInvalid),
        }
        let mut updated = a.clone();
        updated.authorization_version = a
            .authorization_version
            .checked_successor()
            .ok_or(PairingFailure::LimitExceeded)?;
        updated.record.state = PairingState::Revoked {
            revision: next_revision(state)?,
        };
        updated.revocation_receipt = Some(receipt.clone());
        self.transition(
            state,
            state.marks,
            vec![PairingWrite::PutAuthorization {
                expected_authorization_version: Some(a.authorization_version),
                value: PairingAuthorization::new(updated).map_err(contract_failure)?,
            }],
            vec![],
            a.clients.iter().map(|c| c.parts().ordinary).collect(),
            a.clients.iter().map(|c| c.parts().client.id).collect(),
            PairingOutcome::Revoked(receipt),
        )
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "one complete transaction proposal"
    )]
    fn transition(
        &self,
        state: &PairingSnapshot,
        high_water: IdentityHighWater,
        writes: Vec<PairingWrite>,
        register_streams: Vec<StreamRecord>,
        retire_ordinary_streams: Vec<OperationStreamId>,
        release_clients: Vec<ClientId>,
        outcome: PairingOutcome,
    ) -> Result<PairingTransition, PairingFailure> {
        Ok(PairingTransition {
            expected_store: state.revision,
            high_water,
            writes: PairingWrites::new(PairingWritesParts { writes }).map_err(contract_failure)?,
            register_streams,
            retire_ordinary_streams,
            release_clients,
            outcome,
        })
    }
}
pub fn authentication_key_digest_for(keys: &PairingKeys) -> crate::operations::RequestDigest {
    use sha2::{Digest, Sha256};
    crate::operations::RequestDigest::from_bytes(
        Sha256::digest(keys.parts().pairing_authentication_public_key).into(),
    )
}
fn next_revision(state: &PairingSnapshot) -> Result<StoreRevision, PairingFailure> {
    state
        .revision
        .checked_successor()
        .ok_or(PairingFailure::LimitExceeded)
}
fn contract_failure(error: PairingContractError) -> PairingFailure {
    match error {
        PairingContractError::LimitExceeded => PairingFailure::LimitExceeded,
        _ => PairingFailure::InvalidRequest,
    }
}
fn stream_record(
    stream: OperationStreamId,
    purpose: StreamPurpose,
) -> Result<StreamRecord, PairingFailure> {
    let dormant = match purpose {
        StreamPurpose::Ordinary => None,
        StreamPurpose::SelfRevocation => Some(OutcomeReservation::full(
            ReservationClass::DedicatedSelfRevocation,
        )),
        StreamPurpose::Enrollment { .. } => Some(OutcomeReservation::full(
            ReservationClass::DedicatedEnrollment,
        )),
    };
    StreamRecord::new(
        StreamBinding::from_stream(stream),
        purpose,
        StreamStatus::Active,
        0,
        0,
        dormant,
    )
    .map_err(|_| PairingFailure::InvalidDurableState)
}
fn client_stream_records(
    client: &PairingClientStreams,
) -> Result<Vec<StreamRecord>, PairingFailure> {
    Ok(vec![
        stream_record(client.parts().ordinary, StreamPurpose::Ordinary)?,
        stream_record(
            client.parts().self_revocation,
            StreamPurpose::SelfRevocation,
        )?,
    ])
}
