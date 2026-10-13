//! Pairing extension for the one serialized server actor. It owns policy and
//! signatures, not a listener, database, ledger, retry loop or runtime registry.

use super::management::{AccountOwnerEnrollmentPort, EnrollmentAuthority};
use crate::contracts::ServerFuture;
use spaceterm_core::pairing::{
    policy::{EnrollmentMutation, PairingPolicy, PairingSnapshot},
    transition::PairingTransition,
};
use spaceterm_core::{
    authority::{FieldVersion, StoreRevision},
    identity::*,
    operation_ledger_contracts::{CanonicalOperationRequest, LedgerCodec, StreamRecord},
    operations::{Command, OperationBinding, OperationId, OperationSequence, RequestDigest},
    pairing::*,
};
use spaceterm_host::native_deployment::contracts::NativeRequestId;
use spaceterm_protocol::{
    authentication::{ServerPin, SigningKey},
    pairing_adapter::{
        EnrollmentAuthorization, ReceiptCodec, VerifiedDeviceClaim, VerifiedEnrollmentSubmission,
        enrollment_proposal_digest, verify_client_binding_claim,
    },
};
use std::sync::Arc;

/// The native journal identity is not convertible to an ordinary OperationId.
pub enum PairingActorRequest {
    Operation(CanonicalOperationRequest),
    AccountOwner {
        request: NativeRequestId,
        digest: RequestDigest,
        proposal: EnrollmentProposal,
    },
}
/// Available only while the actor serializes this admitted mutation. Ledger
/// capacity checks include dormant reservations and permanent retired headers.
pub trait PairingMutationContext {
    fn snapshot(&self) -> &PairingSnapshot;
    fn now_millis(&self) -> u64;
    fn reserve_stream_capacity(&mut self, streams: &[StreamRecord]) -> Result<(), PairingFailure>;
}
pub struct PreparedPairingMutation {
    pub transition: PairingTransition,
    pub authorization: Option<EnrollmentAuthorization>,
}
pub trait PairingMutation: Send {
    fn prepare(
        self: Box<Self>,
        context: &mut dyn PairingMutationContext,
    ) -> Result<PreparedPairingMutation, PairingFailure>;
}
impl<F> PairingMutation for F
where
    F: FnOnce(&mut dyn PairingMutationContext) -> Result<PreparedPairingMutation, PairingFailure>
        + Send,
{
    fn prepare(
        self: Box<Self>,
        context: &mut dyn PairingMutationContext,
    ) -> Result<PreparedPairingMutation, PairingFailure> {
        self(context)
    }
}
/// The actor resolves an original result before invoking the planner. For a new
/// identified operation it commits store-only ledger admission before allocation.
/// Enrollment streams must already be registered with Enrollment purpose; unknown
/// streams are rejected. Owner requests use native-journal admission, never an
/// ordinary identity. The actor checks assessments against their retained original
/// operation/digest/result, revalidates authority, enters revocation barriers,
/// merges exact reducer releases and stream retirement with pairing/outcome writes,
/// and commits once. Durable success waits for connection/worker invalidation.
/// NotApplied yields no secret/receipt. Indeterminate seals admissions until recovery.
/// Before issuance and after epoch advancement, commit prepare_ticket_invalidations
/// and Enrollment-purpose stream retirement without fabricating an operation.
/// Already-Started external work remains service-owned and is not cancelled.
/// Mutation closures own their dependencies and can be transferred to the actor
/// task. Dropping an observer must not abandon an admitted mutation or its barrier.
pub trait PairingActorPort: Send + Sync {
    fn execute<'a>(
        &'a self,
        request: PairingActorRequest,
        mutation: Box<dyn PairingMutation>,
    ) -> ServerFuture<'a, Result<PairingExecution, PairingFailure>>;
    /// Checks exact retained Client/self-revocation stream and canonical digest.
    /// A receipt caused by another device is lookup data, not a fabricated result.
    fn restricted_status<'a>(
        &'a self,
        binding: OperationBinding,
        target: PairingTarget,
    ) -> ServerFuture<'a, Result<Option<SignedPairingReceipt>, PairingFailure>>;
    /// Historical Enrollment-purpose lookup checks the original claim and both
    /// verified keys without consuming a ticket or granting ordinary access.
    fn enrollment_status<'a>(
        &'a self,
        binding: OperationBinding,
        ticket: EnrollmentTicketId,
        claim_digest: RequestDigest,
    ) -> ServerFuture<'a, Result<Option<SignedPairingReceipt>, PairingFailure>>;
    /// The same original Enrollment-purpose binding supplies cumulative ack 1.
    fn enrollment_acknowledge<'a>(
        &'a self,
        binding: OperationBinding,
        ticket: EnrollmentTicketId,
        claim_digest: RequestDigest,
    ) -> ServerFuture<'a, Result<(), PairingFailure>>;
    /// Uses only cumulative acknowledgement. It cannot register a stream,
    /// admit another request, or reactivate ordinary authorization.
    fn restricted_acknowledge<'a>(
        &'a self,
        binding: OperationBinding,
        target: PairingTarget,
    ) -> ServerFuture<'a, Result<(), PairingFailure>>;
}
pub struct PairingExecution {
    pub outcome: PairingOutcome,
    pub authorization: Option<EnrollmentAuthorization>,
}

#[derive(Clone)]
pub struct PairingService {
    actor: Arc<dyn PairingActorPort>,
    signer: Arc<dyn SigningKey>,
    codec: Arc<dyn LedgerCodec>,
    owner: Arc<dyn AccountOwnerEnrollmentPort>,
    pin: ServerPin,
}
impl PairingService {
    pub fn new(
        actor: Arc<dyn PairingActorPort>,
        signer: Arc<dyn SigningKey>,
        codec: Arc<dyn LedgerCodec>,
        owner: Arc<dyn AccountOwnerEnrollmentPort>,
        pin: ServerPin,
    ) -> Result<Self, PairingFailure> {
        if pin.identity == [0; 16] || signer.public_key() != pin.public_key {
            return Err(PairingFailure::IdentityMismatch);
        }
        Ok(Self {
            actor,
            signer,
            codec,
            owner,
            pin,
        })
    }
    fn operation(
        &self,
        id: OperationId,
        command: PairingCommand,
    ) -> Result<CanonicalOperationRequest, PairingFailure> {
        if command.server().bytes() != self.pin.identity {
            return Err(PairingFailure::IdentityMismatch);
        }
        CanonicalOperationRequest::new(id, Command::Pairing(command), self.codec.as_ref())
            .map_err(|_| PairingFailure::InvalidRequest)
    }
    /// Takes the bounded proposal from the authenticated issuer's ordinary
    /// request. Candidate key possession is required only at restricted enrollment.
    pub async fn issue_direct_authorization(
        &self,
        operation: OperationId,
        proposal: EnrollmentProposal,
    ) -> Result<PairingExecution, PairingFailure> {
        let request = self.operation(
            operation,
            PairingCommand::IssueDirectAuthorization {
                proposal: proposal.clone(),
            },
        )?;
        let binding = request.binding();
        let service = self.clone();
        self.actor
            .execute(
                PairingActorRequest::Operation(request),
                Box::new(move |context: &mut dyn PairingMutationContext| {
                    let state = context.snapshot();
                    let policy = PairingPolicy::default();
                    policy.authorize_client(
                        state,
                        operation.stream().pairing(),
                        operation.stream().client(),
                    )?;
                    let version = state
                        .authorization(operation.stream().pairing())?
                        .parts()
                        .authorization_version;
                    service.reserve(
                        context,
                        proposal,
                        EnrollmentAuthority::paired(binding, version),
                    )
                }),
            )
            .await
    }
    /// First pairing and last-device recovery enter only here, through the native
    /// request provider. No paired identity or operation stream is synthesized.
    pub async fn issue_owner_authorization(
        &self,
        request: NativeRequestId,
        digest: RequestDigest,
        claim: VerifiedDeviceClaim,
    ) -> Result<PairingExecution, PairingFailure> {
        let proposal = claim.proposal().clone();
        if proposal.parts().server.bytes() != self.pin.identity {
            return Err(PairingFailure::IdentityMismatch);
        }
        let native_request = request;
        let service = self.clone();
        self.actor
            .execute(
                PairingActorRequest::AccountOwner {
                    request,
                    digest,
                    proposal,
                },
                Box::new(move |context: &mut dyn PairingMutationContext| {
                    let authority = EnrollmentAuthority::owner(
                        service.owner.as_ref(),
                        &native_request,
                        digest,
                        context.snapshot().server(),
                        claim.proposal(),
                    )?;
                    service.reserve(context, claim.proposal().clone(), authority)
                }),
            )
            .await
    }
    fn reserve(
        &self,
        context: &mut dyn PairingMutationContext,
        proposal: EnrollmentProposal,
        authority: EnrollmentAuthority,
    ) -> Result<PreparedPairingMutation, PairingFailure> {
        let state = context.snapshot();
        let claim_digest =
            enrollment_proposal_digest(&proposal).map_err(|_| PairingFailure::InvalidRequest)?;
        let p = proposal.parts();
        PairingPolicy::default().check_reservation_capacity(state, p.device)?;
        let mut pairings =
            IdAllocator::<PairingId>::from_high_water(state.server(), state.high_water().pairing);
        let pairing = pairings
            .allocate()
            .map_err(|_| PairingFailure::LimitExceeded)?;
        let mut clients =
            IdAllocator::<ClientId>::from_high_water(state.server(), state.high_water().client);
        let client = clients
            .allocate()
            .map_err(|_| PairingFailure::LimitExceeded)?;
        let mut streams = IdAllocator::<OperationStreamKey>::from_high_water(
            state.server(),
            state.high_water().operation_stream,
        );
        let stream = OperationStreamId::new(
            streams
                .allocate()
                .map_err(|_| PairingFailure::LimitExceeded)?,
            pairing,
            client,
        )
        .map_err(|_| PairingFailure::InvalidRequest)?;
        let enrollment = OperationId::new(
            stream,
            OperationSequence::new(1).map_err(|_| PairingFailure::InvalidRequest)?,
        );
        let authorization = EnrollmentAuthorization::generate()
            .map_err(|_| PairingFailure::AuthorizationUnavailable)?;
        // A separate random ticket nonce is public comparison data, not a bearer.
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| PairingFailure::AuthorizationUnavailable)?;
        let ticket = EnrollmentTicketId::new(EnrollmentTicketParts {
            server: state.server(),
            nonce,
        })
        .map_err(|_| PairingFailure::InvalidRequest)?;
        let enrollment_request = self.operation(
            enrollment,
            PairingCommand::Enroll {
                ticket: ticket.clone(),
                claim_digest,
            },
        )?;
        let now = context.now_millis();
        let reservation = EnrollmentReservation::new(EnrollmentReservationParts {
            ticket,
            target: PairingTarget::new(PairingTargetParts {
                server: state.server(),
                device: p.device,
                pairing,
            })
            .map_err(|_| PairingFailure::InvalidRequest)?,
            keys: p.keys.clone(),
            initial_client: client,
            enrollment_stream: stream,
            claim_digest,
            authorization_digest: authorization.digest(),
            enrollment_request_digest: enrollment_request.binding().digest,
            issuer: authority.into_issuer(),
            issued_epoch: state.epoch(),
            issued_revision: next_revision(state)?,
            issued_at_millis: now,
            expires_at_millis: now
                .checked_add(MAX_ENROLLMENT_LIFETIME_MILLIS)
                .ok_or(PairingFailure::LimitExceeded)?,
            state: EnrollmentReservationState::Reserved,
        })
        .map_err(|_| PairingFailure::InvalidRequest)?;
        let transition = PairingPolicy::default().prepare_reservation(state, reservation)?;
        context.reserve_stream_capacity(&transition.register_streams)?;
        Ok(PreparedPairingMutation {
            transition,
            authorization: Some(authorization),
        })
    }
    pub async fn lookup_enrollment(
        &self,
        connection: &spaceterm_protocol::pairing_adapter::EnrollmentConnection,
    ) -> Result<Option<SignedPairingReceipt>, PairingFailure> {
        let e = connection.reservation().parts();
        if e.target.parts().server.bytes() != self.pin.identity {
            return Err(PairingFailure::IdentityMismatch);
        }
        let id = OperationId::new(
            e.enrollment_stream,
            OperationSequence::new(1).map_err(|_| PairingFailure::InvalidRequest)?,
        );
        let request = self.operation(
            id,
            PairingCommand::Enroll {
                ticket: e.ticket.clone(),
                claim_digest: e.claim_digest,
            },
        )?;
        if request.binding().digest != e.enrollment_request_digest {
            return Err(PairingFailure::RequestIdentityReused);
        }
        self.actor
            .enrollment_status(request.binding(), e.ticket.clone(), e.claim_digest)
            .await
    }
    pub async fn acknowledge_enrollment(
        &self,
        connection: &spaceterm_protocol::pairing_adapter::EnrollmentConnection,
    ) -> Result<(), PairingFailure> {
        let e = connection.reservation().parts();
        let id = OperationId::new(
            e.enrollment_stream,
            OperationSequence::new(1).map_err(|_| PairingFailure::InvalidRequest)?,
        );
        let request = self.operation(
            id,
            PairingCommand::Enroll {
                ticket: e.ticket.clone(),
                claim_digest: e.claim_digest,
            },
        )?;
        if request.binding().digest != e.enrollment_request_digest {
            return Err(PairingFailure::RequestIdentityReused);
        }
        self.actor
            .enrollment_acknowledge(request.binding(), e.ticket.clone(), e.claim_digest)
            .await
    }
    pub async fn enroll(
        &self,
        submission: VerifiedEnrollmentSubmission,
    ) -> Result<PairingExecution, PairingFailure> {
        let e = submission.reservation().parts();
        let id = OperationId::new(
            e.enrollment_stream,
            OperationSequence::new(1).map_err(|_| PairingFailure::InvalidRequest)?,
        );
        let request = self.operation(
            id,
            PairingCommand::Enroll {
                ticket: e.ticket.clone(),
                claim_digest: e.claim_digest,
            },
        )?;
        let binding = request.binding();
        if binding.digest != e.enrollment_request_digest {
            return Err(PairingFailure::RequestIdentityReused);
        }
        let service = self.clone();
        self.actor
            .execute(
                PairingActorRequest::Operation(request),
                Box::new(move |context: &mut dyn PairingMutationContext| {
                    let e = submission.reservation().parts();
                    let policy = PairingPolicy::default();
                    let state = context.snapshot();
                    if let Some(receipt) = policy.check_enrollment_authorization(
                        state,
                        &e.ticket,
                        binding,
                        e.claim_digest,
                        submission.authorization_digest(),
                        context.now_millis(),
                    )? {
                        return Ok(PreparedPairingMutation {
                            transition: no_change(state, PairingOutcome::Enrolled(receipt)),
                            authorization: None,
                        });
                    }
                    if let EnrollmentIssuer::AccountOwner {
                        native_request,
                        native_request_digest,
                    } = &state.reservation(&e.ticket)?.parts().issuer
                    {
                        let request = NativeRequestId::new(*native_request)
                            .map_err(|_| PairingFailure::InvalidDurableState)?;
                        service.owner.revalidate_owner_issuance(
                            &request,
                            *native_request_digest,
                            state.server(),
                            e.claim_digest,
                        )?;
                    }
                    let client =
                        allocate_client_streams(state, e.target.parts().pairing, e.initial_client)?;
                    let streams = registration(&client)?;
                    context.reserve_stream_capacity(&streams)?;
                    let state = context.snapshot();
                    let claims = PairingReceiptBody::Enrollment {
                        target: e.target.clone(),
                        keys: e.keys.clone(),
                        client: client.clone(),
                        enrollment: binding,
                        revision: next_revision(state)?,
                    };
                    let receipt = ReceiptCodec::new(service.pin)
                        .sign(claims, service.signer.as_ref())
                        .map_err(|_| PairingFailure::ReceiptInvalid)?;
                    let transition = policy.prepare_enrollment(
                        state,
                        EnrollmentMutation {
                            ticket: e.ticket.clone(),
                            enrollment: binding,
                            claim_digest: e.claim_digest,
                            authorization_digest: submission.authorization_digest(),
                            now_millis: context.now_millis(),
                            client,
                            receipt,
                        },
                    )?;
                    Ok(PreparedPairingMutation {
                        transition,
                        authorization: None,
                    })
                }),
            )
            .await
    }
    pub async fn bind_additional_client(
        &self,
        operation: OperationId,
        target: PairingTarget,
        expected: FieldVersion,
        claim_digest: RequestDigest,
        device_proof: [u8; 64],
        auth_proof: [u8; 64],
    ) -> Result<PairingExecution, PairingFailure> {
        let request = self.operation(
            operation,
            PairingCommand::BindAdditionalClient {
                target: target.clone(),
                expected_authorization_version: expected,
                claim_digest,
            },
        )?;
        let binding = request.binding();
        let service = self.clone();
        self.actor
            .execute(
                PairingActorRequest::Operation(request),
                Box::new(move |context: &mut dyn PairingMutationContext| {
                    let state = context.snapshot();
                    let policy = PairingPolicy::default();
                    let actor = policy.authorize_client(
                        state,
                        operation.stream().pairing(),
                        operation.stream().client(),
                    )?;
                    let a = state.authorization(actor.pairing())?.parts();
                    if target.parts().device != actor.device()
                        || target.parts().pairing != actor.pairing()
                    {
                        return Err(PairingFailure::IdentityMismatch);
                    }
                    verify_client_binding_claim(
                        &target,
                        expected,
                        operation,
                        &a.keys,
                        claim_digest,
                        &device_proof,
                        &auth_proof,
                    )
                    .map_err(|_| PairingFailure::AuthorizationDenied)?;
                    if a.authorization_version != expected {
                        return Err(PairingFailure::StaleAssessment);
                    }
                    if a.clients.len() >= MAX_CLIENTS_PER_PAIRING {
                        return Err(PairingFailure::LimitExceeded);
                    }
                    let mut ids = IdAllocator::<ClientId>::from_high_water(
                        state.server(),
                        state.high_water().client,
                    );
                    let client = ids.allocate().map_err(|_| PairingFailure::LimitExceeded)?;
                    let client = allocate_client_streams(state, actor.pairing(), client)?;
                    let streams = registration(&client)?;
                    let keys = a.keys.clone();
                    context.reserve_stream_capacity(&streams)?;
                    let state = context.snapshot();
                    let receipt = ReceiptCodec::new(service.pin)
                        .sign(
                            PairingReceiptBody::ClientBinding {
                                target,
                                keys,
                                client: client.clone(),
                                operation: binding,
                                revision: next_revision(state)?,
                            },
                            service.signer.as_ref(),
                        )
                        .map_err(|_| PairingFailure::ReceiptInvalid)?;
                    Ok(PreparedPairingMutation {
                        transition: policy.prepare_client_binding(
                            state, &actor, binding, expected, client, receipt,
                        )?,
                        authorization: None,
                    })
                }),
            )
            .await
    }
    pub async fn assess_unpair(
        &self,
        operation: OperationId,
        target: PairingTarget,
    ) -> Result<PairingExecution, PairingFailure> {
        let request = self.operation(
            operation,
            PairingCommand::AssessUnpair {
                target: target.clone(),
            },
        )?;
        let binding = request.binding();
        self.actor
            .execute(
                PairingActorRequest::Operation(request),
                Box::new(move |context: &mut dyn PairingMutationContext| {
                    let state = context.snapshot();
                    let policy = PairingPolicy::default();
                    let actor = policy.authorize_client(
                        state,
                        operation.stream().pairing(),
                        operation.stream().client(),
                    )?;
                    let assessment = policy.assess_unpair(
                        state,
                        &actor,
                        spaceterm_core::operations::AssessmentRef {
                            operation,
                            digest: binding.digest,
                        },
                        target,
                    )?;
                    Ok(PreparedPairingMutation {
                        transition: no_change(state, PairingOutcome::UnpairAssessed(assessment)),
                        authorization: None,
                    })
                }),
            )
            .await
    }
    pub async fn unpair(
        &self,
        operation: OperationId,
        assessment: UnpairAssessment,
    ) -> Result<PairingExecution, PairingFailure> {
        let request = self.operation(
            operation,
            PairingCommand::UnpairConfirmed {
                assessment: assessment.clone(),
            },
        )?;
        let binding = request.binding();
        let service = self.clone();
        self.actor
            .execute(
                PairingActorRequest::Operation(request),
                Box::new(move |context: &mut dyn PairingMutationContext| {
                    let state = context.snapshot();
                    let policy = PairingPolicy::default();
                    let actor = policy.authorize_client(
                        state,
                        operation.stream().pairing(),
                        operation.stream().client(),
                    )?;
                    let receipt =
                        service.revocation_receipt(state, &assessment.parts().target, binding)?;
                    Ok(PreparedPairingMutation {
                        transition: policy.prepare_unpair(
                            state,
                            &actor,
                            binding,
                            &assessment,
                            receipt,
                        )?,
                        authorization: None,
                    })
                }),
            )
            .await
    }
    pub(crate) fn revocation_receipt(
        &self,
        state: &PairingSnapshot,
        target: &PairingTarget,
        binding: OperationBinding,
    ) -> Result<SignedPairingReceipt, PairingFailure> {
        let a = state.authorization(target.parts().pairing)?.parts();
        if let Some(receipt) = &a.revocation_receipt {
            return Ok(receipt.clone());
        }
        let mut event = [0; 16];
        getrandom::fill(&mut event).map_err(|_| PairingFailure::AuthorizationUnavailable)?;
        ReceiptCodec::new(self.pin)
            .sign(
                PairingReceiptBody::Revocation {
                    receipt: RevocationReceipt {
                        server: state.server(),
                        pairing: target.parts().pairing,
                        device: target.parts().device,
                        operation: binding.id,
                        revision: next_revision(state)?,
                        receipt_digest: RequestDigest::from_bytes([0; 32]),
                    },
                    authentication_key_digest:
                        spaceterm_core::pairing::policy::authentication_key_digest_for(&a.keys),
                    revocation_event: event,
                },
                self.signer.as_ref(),
            )
            .map_err(|_| PairingFailure::ReceiptInvalid)
    }
    pub(crate) fn pin(&self) -> ServerPin {
        self.pin
    }
    pub(crate) fn actor(&self) -> &dyn PairingActorPort {
        self.actor.as_ref()
    }
    pub(crate) fn self_revoke_request(
        &self,
        operation: OperationId,
        target: PairingTarget,
    ) -> Result<CanonicalOperationRequest, PairingFailure> {
        self.operation(operation, PairingCommand::SelfRevoke { target })
    }
}
pub(crate) fn no_change(state: &PairingSnapshot, outcome: PairingOutcome) -> PairingTransition {
    PairingTransition {
        expected_store: state.revision(),
        high_water: state.high_water(),
        writes: PairingWrites::default(),
        register_streams: vec![],
        retire_ordinary_streams: vec![],
        release_clients: vec![],
        outcome,
    }
}
fn next_revision(state: &PairingSnapshot) -> Result<StoreRevision, PairingFailure> {
    state
        .revision()
        .checked_successor()
        .ok_or(PairingFailure::LimitExceeded)
}
fn allocate_client_streams(
    state: &PairingSnapshot,
    pairing: PairingId,
    client: ClientId,
) -> Result<PairingClientStreams, PairingFailure> {
    let mut allocator = IdAllocator::<OperationStreamKey>::from_high_water(
        state.server(),
        state.high_water().operation_stream,
    );
    let mut allocate = || {
        OperationStreamId::new(
            allocator
                .allocate()
                .map_err(|_| PairingFailure::LimitExceeded)?,
            pairing,
            client,
        )
        .map_err(|_| PairingFailure::IdentityMismatch)
    };
    PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: client,
            pairing,
        },
        ordinary: allocate()?,
        self_revocation: allocate()?,
    })
    .map_err(|_| PairingFailure::InvalidRequest)
}
fn registration(client: &PairingClientStreams) -> Result<Vec<StreamRecord>, PairingFailure> {
    use spaceterm_core::operation_ledger_contracts::*;
    Ok(vec![
        StreamRecord::new(
            StreamBinding::from_stream(client.parts().ordinary),
            StreamPurpose::Ordinary,
            StreamStatus::Active,
            0,
            0,
            None,
        )
        .map_err(|_| PairingFailure::InvalidDurableState)?,
        StreamRecord::new(
            StreamBinding::from_stream(client.parts().self_revocation),
            StreamPurpose::SelfRevocation,
            StreamStatus::Active,
            0,
            0,
            Some(OutcomeReservation::full(
                ReservationClass::DedicatedSelfRevocation,
            )),
        )
        .map_err(|_| PairingFailure::InvalidDurableState)?,
    ])
}

pub(crate) fn server_failure(value: PairingFailure) -> crate::contracts::ServerFailure {
    use crate::contracts::ServerFailureClass as Class;
    crate::failure::failure(match value {
        PairingFailure::LimitExceeded => Class::Capacity,
        PairingFailure::StorageUnavailable => Class::StorageUnavailable,
        PairingFailure::InvalidDurableState
        | PairingFailure::RecoveryRequired
        | PairingFailure::UnsafeStorage
        | PairingFailure::UnknownSchema => Class::RecoveryRequired,
        PairingFailure::InvalidRequest | PairingFailure::IdentityMismatch => Class::ScopeMismatch,
        PairingFailure::StaleAssessment | PairingFailure::RequestIdentityReused => {
            Class::StaleAuthority
        }
        PairingFailure::AuthorizationUnavailable
        | PairingFailure::CredentialUnavailable
        | PairingFailure::ServiceManagerUnavailable
        | PairingFailure::UnsupportedTarget => Class::NativeUnavailable,
        PairingFailure::Offline
        | PairingFailure::RouteChanged
        | PairingFailure::TimedOut
        | PairingFailure::Cancelled => Class::RuntimeUnavailable,
        PairingFailure::FingerprintMismatch
        | PairingFailure::AuthorizationRequired
        | PairingFailure::AuthorizationDenied
        | PairingFailure::PairingRevoked
        | PairingFailure::ClientNotBound
        | PairingFailure::AuthorizationExpired
        | PairingFailure::AuthorizationConsumed
        | PairingFailure::Incompatible
        | PairingFailure::ExplicitStartRequired
        | PairingFailure::CredentialIntegrity
        | PairingFailure::ReceiptInvalid
        | PairingFailure::RevocationUnconfirmed => Class::Authentication,
    })
}
