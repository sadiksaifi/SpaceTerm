use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    authority::{ServerEpoch, StoreRevision},
    identity::*,
    operation_ledger_contracts::*,
    operations::*,
    pairing::policy::PairingSnapshot,
    pairing::*,
};
use spaceterm_host::native_deployment::contracts::NativeRequestId;
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ServerPin, SigningKey},
    pairing_adapter::*,
};
use spaceterm_server::{
    contracts::ServerFuture,
    pairing::{
        management::AccountOwnerEnrollmentPort, service::*, store_projection::project_pairing,
    },
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

struct Key(Ed25519KeyPair, std::sync::atomic::AtomicUsize);
impl Key {
    fn new(seed: u8) -> Arc<Self> {
        Arc::new(Self(
            Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap(),
            std::sync::atomic::AtomicUsize::new(0),
        ))
    }
}
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
// Only the frozen codec port is faked. Ledger encoding belongs to op-ledger,
// which is absent from this base. Unsupported codec operations fail explicitly.
struct Codec;
impl LedgerCodec for Codec {
    fn canonical_request_digest(
        &self,
        binding: &StreamBinding,
        operation: OperationId,
        command: &Command,
    ) -> Result<RequestDigest, LedgerFailure> {
        binding.check_operation(operation)?;
        RequestDigest::for_command(command)
            .map_err(|_| LedgerFailure::new(LedgerFailureClass::InvalidRequest))
    }
    fn encode_canonical_request(
        &self,
        _: &StreamBinding,
        _: OperationId,
        _: &Command,
    ) -> Result<Vec<u8>, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::UnsupportedFormat))
    }
    fn decode_canonical_request(
        &self,
        _: &[u8],
    ) -> Result<CanonicalOperationRequest, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::UnsupportedFormat))
    }
    fn encode_compact_outcome(&self, _: &Outcome) -> Result<Vec<u8>, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::UnsupportedFormat))
    }
    fn decode_compact_outcome(&self, _: &[u8]) -> Result<Outcome, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::UnsupportedFormat))
    }
}
struct Owner;
impl AccountOwnerEnrollmentPort for Owner {
    fn revalidate_owner_issuance(
        &self,
        request: &NativeRequestId,
        digest: RequestDigest,
        _: ServerId,
        _: RequestDigest,
    ) -> Result<(), PairingFailure> {
        if ![[7; 16], [8; 16]].contains(request.as_bytes())
            || digest != RequestDigest::from_bytes([9; 32])
        {
            return Err(PairingFailure::AuthorizationDenied);
        }
        Ok(())
    }

    fn validate_owner_request(
        &self,
        request: &NativeRequestId,
        digest: RequestDigest,
        server: ServerId,
        proposal: &EnrollmentProposal,
    ) -> Result<(), PairingFailure> {
        if ![[7; 16], [8; 16]].contains(request.as_bytes())
            || digest != RequestDigest::from_bytes([9; 32])
            || proposal.parts().server != server
        {
            return Err(PairingFailure::AuthorizationDenied);
        }
        Ok(())
    }
}
struct State {
    snapshot: PairingSnapshot,
    now: u64,
    capacity: usize,
    streams: Vec<StreamRecord>,
    outcomes: BTreeMap<OperationId, (RequestDigest, PairingOutcome)>,
    owner_outcomes: BTreeMap<[u8; 16], (RequestDigest, EnrollmentProposal, PairingOutcome)>,
    events: Vec<&'static str>,
    failure: Option<PairingFailure>,
}
struct Actor {
    state: Mutex<State>,
    receipts: ReceiptCodec,
}
struct Context<'a>(&'a mut State);
impl PairingMutationContext for Context<'_> {
    fn snapshot(&self) -> &PairingSnapshot {
        &self.0.snapshot
    }
    fn now_millis(&self) -> u64 {
        self.0.now
    }
    fn reserve_stream_capacity(&mut self, streams: &[StreamRecord]) -> Result<(), PairingFailure> {
        self.0.events.push("capacity");
        if self.0.streams.len() + streams.len() > self.0.capacity {
            return Err(PairingFailure::LimitExceeded);
        }
        for stream in streams {
            stream.validate_shape().unwrap();
        }
        Ok(())
    }
}
impl PairingActorPort for Actor {
    fn execute<'a>(
        &'a self,
        request: PairingActorRequest,
        mutation: Box<dyn PairingMutation>,
    ) -> ServerFuture<'a, Result<PairingExecution, PairingFailure>> {
        Box::pin(async move {
            let mut s = self.state.lock().unwrap();
            let mut native = None;
            let binding = match request {
                PairingActorRequest::Operation(request) => {
                    let binding = request.binding();
                    if let Some((digest, outcome)) = s.outcomes.get(&binding.id) {
                        if *digest != binding.digest {
                            return Err(PairingFailure::RequestIdentityReused);
                        }
                        return Ok(PairingExecution {
                            outcome: outcome.clone(),
                            authorization: None,
                        });
                    }
                    let stream = s
                        .streams
                        .iter()
                        .find(|s| s.binding.stream() == binding.id.stream())
                        .ok_or(PairingFailure::ClientNotBound)?;
                    if stream.status != StreamStatus::Active {
                        return Err(PairingFailure::PairingRevoked);
                    }
                    match (&stream.purpose, request.command()) {
                        (
                            StreamPurpose::Enrollment { .. },
                            Command::Pairing(PairingCommand::Enroll { .. }),
                        )
                        | (StreamPurpose::Ordinary, _)
                        | (
                            StreamPurpose::SelfRevocation,
                            Command::Pairing(PairingCommand::SelfRevoke { .. }),
                        ) => (),
                        _ => return Err(PairingFailure::AuthorizationDenied),
                    }
                    Some(binding)
                }
                PairingActorRequest::AccountOwner {
                    request,
                    digest,
                    proposal,
                } => {
                    let id = *request.as_bytes();
                    if let Some((old_digest, old_proposal, outcome)) = s.owner_outcomes.get(&id) {
                        if old_digest != &digest || old_proposal != &proposal {
                            return Err(PairingFailure::RequestIdentityReused);
                        }
                        return Ok(PairingExecution {
                            outcome: outcome.clone(),
                            authorization: None,
                        });
                    }
                    native = Some((id, digest, proposal));
                    None
                }
            };
            s.events.push("admission");
            if let Some(failure) = s.failure {
                return Err(failure);
            }
            let prepared = mutation.prepare(&mut Context(&mut s))?;
            let snapshot = project_pairing(&s.snapshot, &prepared.transition, &self.receipts)?;
            s.events.push("commit");
            for stream in &prepared.transition.retire_ordinary_streams {
                if let Some(record) = s.streams.iter_mut().find(|s| s.binding.stream() == *stream) {
                    record.status = StreamStatus::Retired(StreamRetirementReason::PairingRevoked);
                }
            }
            s.streams
                .extend(prepared.transition.register_streams.clone());
            s.snapshot = snapshot;
            if let Some((id, digest, proposal)) = native {
                s.owner_outcomes
                    .insert(id, (digest, proposal, prepared.transition.outcome.clone()));
            }
            if let Some(binding) = binding {
                s.outcomes.insert(
                    binding.id,
                    (binding.digest, prepared.transition.outcome.clone()),
                );
            }
            if !prepared.transition.release_clients.is_empty() {
                s.events.push("invalidation");
            }
            Ok(PairingExecution {
                outcome: prepared.transition.outcome,
                authorization: prepared.authorization,
            })
        })
    }
    fn restricted_status<'a>(
        &'a self,
        binding: OperationBinding,
        target: PairingTarget,
    ) -> ServerFuture<'a, Result<Option<SignedPairingReceipt>, PairingFailure>> {
        Box::pin(async move {
            let s = self.state.lock().unwrap();
            let a = s.snapshot.authorization(target.parts().pairing)?.parts();
            if a.record.device != target.parts().device
                || !a
                    .clients
                    .iter()
                    .any(|c| c.parts().self_revocation == binding.id.stream())
            {
                return Err(PairingFailure::ClientNotBound);
            }
            Ok(a.revocation_receipt.clone())
        })
    }
    fn enrollment_status<'a>(
        &'a self,
        binding: OperationBinding,
        ticket: EnrollmentTicketId,
        claim_digest: RequestDigest,
    ) -> ServerFuture<'a, Result<Option<SignedPairingReceipt>, PairingFailure>> {
        Box::pin(async move {
            spaceterm_core::pairing::policy::PairingPolicy::default().completed_enrollment(
                &self.state.lock().unwrap().snapshot,
                &ticket,
                binding,
                claim_digest,
            )
        })
    }
    fn enrollment_acknowledge<'a>(
        &'a self,
        _: OperationBinding,
        _: EnrollmentTicketId,
        _: RequestDigest,
    ) -> ServerFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async { Err(PairingFailure::InvalidRequest) })
    }
    fn restricted_acknowledge<'a>(
        &'a self,
        _: OperationBinding,
        _: PairingTarget,
    ) -> ServerFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async { Err(PairingFailure::InvalidRequest) })
    }
}
impl EnrollmentLookup for Actor {
    fn lookup(&self, ticket: &EnrollmentTicketId) -> Result<EnrollmentReservation, ProtocolError> {
        self.state
            .lock()
            .unwrap()
            .snapshot
            .reservation(ticket)
            .cloned()
            .map_err(|_| ProtocolError::PairingDenied)
    }
}
struct Fixture {
    actor: Arc<Actor>,
    service: PairingService,
    server: Arc<Key>,
    device: Arc<Key>,
    auth: Arc<Key>,
    pin: ServerPin,
}
impl Fixture {
    fn new() -> Self {
        let server = Key::new(1);
        let device = Key::new(2);
        let auth = Key::new(3);
        let pin = ServerPin {
            identity: [8; 16],
            public_key: server.public_key(),
        };
        let actor = Arc::new(Actor {
            state: Mutex::new(State {
                snapshot: PairingSnapshot::new(
                    ServerId::new(pin.identity),
                    ServerEpoch::new(1),
                    StoreRevision::new(0),
                    IdentityHighWater::default(),
                    PairingRecovery::default(),
                )
                .unwrap(),
                now: 10,
                capacity: 10,
                streams: vec![],
                outcomes: BTreeMap::new(),
                owner_outcomes: BTreeMap::new(),
                events: vec![],
                failure: None,
            }),
            receipts: ReceiptCodec::new(pin),
        });
        let service = PairingService::new(
            actor.clone(),
            server.clone(),
            Arc::new(Codec),
            Arc::new(Owner),
            pin,
        )
        .unwrap();
        Self {
            actor,
            service,
            server,
            device,
            auth,
            pin,
        }
    }
    fn claim(&self) -> VerifiedDeviceClaim {
        let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
            server: ServerId::new(self.pin.identity),
            device: DeviceId::new([4; 16]),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: self.device.public_key(),
                pairing_authentication_public_key: self.auth.public_key(),
            })
            .unwrap(),
            claim_nonce: [1; 32],
        })
        .unwrap();
        let (device, auth) =
            VerifiedDeviceClaim::proofs(&proposal, self.device.as_ref(), self.auth.as_ref())
                .unwrap();
        VerifiedDeviceClaim::verify(proposal, &device, &auth).unwrap()
    }
    async fn issue(&self) -> PairingExecution {
        let id = 7 + self
            .actor
            .state
            .lock()
            .unwrap()
            .snapshot
            .high_water()
            .pairing as u8;
        self.service
            .issue_owner_authorization(
                NativeRequestId::new([id; 16]).unwrap(),
                RequestDigest::from_bytes([9; 32]),
                self.claim(),
            )
            .await
            .unwrap()
    }
    fn channel(&self) -> (EnrollmentConnection, EnrollmentConnection) {
        // Only serialize the server's public delivery facts across this boundary.
        let wire = reservation_to_wire(
            self.actor
                .state
                .lock()
                .unwrap()
                .snapshot
                .recovery()
                .parts()
                .enrollments
                .last()
                .unwrap(),
        );
        let r = PublicEnrollmentReservation::from_wire(&wire).unwrap();
        let (client, hello) = EnrollmentClient::begin(
            self.pin,
            r,
            vec![2, 1],
            self.device.clone(),
            self.auth.clone(),
        )
        .unwrap();
        let (server, reply) = EnrollmentServer::accept(
            self.pin.identity,
            vec![2, 1],
            self.server.clone(),
            self.actor.as_ref(),
            &hello,
        )
        .unwrap();
        let (client, proof) = client.receive_server(&reply).unwrap();
        let (server, ready) = server.receive_proof(self.actor.as_ref(), &proof).unwrap();
        (client.receive_ready(&ready).unwrap(), server)
    }
    async fn enroll(&self) -> SignedPairingReceipt {
        let issued = self.issue().await;
        let (mut client, mut server) = self.channel();
        let bytes = client
            .submission_message(issued.authorization.as_ref().unwrap(), self.device.as_ref())
            .unwrap();
        let submission = server.receive_submission(&bytes).unwrap();
        match self.service.enroll(submission).await.unwrap().outcome {
            PairingOutcome::Enrolled(receipt) => receipt,
            _ => panic!("wrong outcome"),
        }
    }
}
#[tokio::test]
async fn owner_first_enrollment_uses_native_identity_and_reserved_stream() {
    let f = Fixture::new();
    let receipt = f.enroll().await;
    let state = f.actor.state.lock().unwrap();
    assert_eq!(state.snapshot.server().bytes(), f.pin.identity);
    assert_eq!(state.snapshot.recovery().parts().authorizations.len(), 1);
    assert_eq!(state.streams.len(), 3);
    assert!(matches!(
        state.streams[0].purpose,
        StreamPurpose::Enrollment { .. }
    ));
    assert_eq!(
        state.streams[2].dormant_reservation,
        Some(OutcomeReservation::full(
            ReservationClass::DedicatedSelfRevocation
        ))
    );
    assert!(ReceiptCodec::new(f.pin).verify(&receipt).is_ok());
    assert_eq!(
        state.events,
        vec![
            "admission",
            "capacity",
            "commit",
            "admission",
            "capacity",
            "commit"
        ]
    );
}
#[tokio::test]
async fn enrollment_reserves_revocation_capacity_before_commit() {
    let f = Fixture::new();
    let issued = f.issue().await;
    f.actor.state.lock().unwrap().capacity = 2;
    let (mut client, mut server) = f.channel();
    let bytes = client
        .submission_message(issued.authorization.as_ref().unwrap(), f.device.as_ref())
        .unwrap();
    let submission = server.receive_submission(&bytes).unwrap();
    assert!(matches!(
        f.service.enroll(submission).await,
        Err(PairingFailure::LimitExceeded)
    ));
    assert!(
        f.actor
            .state
            .lock()
            .unwrap()
            .snapshot
            .recovery()
            .parts()
            .authorizations
            .is_empty()
    );
}
#[tokio::test]
async fn completed_enrollment_retry_survives_ticket_expiry() {
    let f = Fixture::new();
    let receipt = f.enroll().await;
    f.actor.state.lock().unwrap().now = 400000;
    let (mut client, mut server) = f.channel();
    let original = f.service.lookup_enrollment(&server).await.unwrap().unwrap();
    assert_eq!(original, receipt);
    let bytes = client.lookup_message().unwrap();
    assert!(server.decode(&bytes).is_ok());
}
#[tokio::test]
async fn additional_client_binding_reserves_distinct_identity_and_streams() {
    let f = Fixture::new();
    let receipt = f.enroll().await;
    let (target, client) = match &receipt.parts().claims {
        PairingReceiptBody::Enrollment { target, client, .. } => (target.clone(), client.clone()),
        _ => panic!("wrong receipt"),
    };
    let operation = OperationId::new(client.parts().ordinary, OperationSequence::new(1).unwrap());
    let version = spaceterm_core::authority::FieldVersion::new(1);
    let (claim, _) = client_binding_claim(&target, version, operation).unwrap();
    let (device, auth) = sign_client_binding_claim(
        &target,
        version,
        operation,
        f.device.as_ref(),
        f.auth.as_ref(),
    )
    .unwrap();
    let result = f
        .service
        .bind_additional_client(operation, target, version, claim, device, auth)
        .await
        .unwrap();
    let proof = match result.outcome {
        PairingOutcome::ClientBound(proof) => proof,
        _ => panic!("wrong outcome"),
    };
    let bound = match &proof.parts().claims {
        PairingReceiptBody::ClientBinding { client, .. } => client,
        _ => panic!("wrong receipt"),
    };
    assert_ne!(bound.parts().client.id, client.parts().client.id);
    let state = f.actor.state.lock().unwrap();
    assert_eq!(state.streams.len(), 5);
    assert_eq!(
        state.snapshot.recovery().parts().authorizations[0]
            .parts()
            .clients
            .len(),
        2
    );
}
#[tokio::test]
async fn owner_issuance_rejects_unverified_native_request_without_pairing_identity() {
    let f = Fixture::new();
    let result = f
        .service
        .issue_owner_authorization(
            NativeRequestId::new([6; 16]).unwrap(),
            RequestDigest::from_bytes([9; 32]),
            f.claim(),
        )
        .await;
    assert!(matches!(result, Err(PairingFailure::AuthorizationDenied)));
    let state = f.actor.state.lock().unwrap();
    assert_eq!(state.snapshot.high_water(), IdentityHighWater::default());
    assert!(state.streams.is_empty());
}
#[tokio::test]
async fn indeterminate_admission_never_returns_receipt_or_authorization() {
    let f = Fixture::new();
    f.actor.state.lock().unwrap().failure = Some(PairingFailure::RecoveryRequired);
    let result = f
        .service
        .issue_owner_authorization(
            NativeRequestId::new([7; 16]).unwrap(),
            RequestDigest::from_bytes([9; 32]),
            f.claim(),
        )
        .await;
    assert!(matches!(result, Err(PairingFailure::RecoveryRequired)));
    assert!(
        f.actor
            .state
            .lock()
            .unwrap()
            .snapshot
            .recovery()
            .parts()
            .enrollments
            .is_empty()
    );
}

#[tokio::test]
async fn invalid_authorization_is_rejected_before_receipt_signing() {
    let f = Fixture::new();
    let _issued = f.issue().await;
    let (mut client, mut server) = f.channel();
    let wrong = EnrollmentAuthorization::generate().unwrap();
    let bytes = client
        .submission_message(&wrong, f.device.as_ref())
        .unwrap();
    let submission = server.receive_submission(&bytes).unwrap();
    let before = f.server.1.load(std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        f.service.enroll(submission).await,
        Err(PairingFailure::AuthorizationDenied)
    ));
    assert_eq!(f.server.1.load(std::sync::atomic::Ordering::SeqCst), before);
}

fn receipt_target(receipt: &SignedPairingReceipt) -> (PairingTarget, PairingClientStreams) {
    match &receipt.parts().claims {
        PairingReceiptBody::Enrollment { target, client, .. } => (target.clone(), client.clone()),
        _ => panic!("wrong receipt"),
    }
}
async fn revoke(
    f: &Fixture,
    target: PairingTarget,
    client: &PairingClientStreams,
) -> SignedPairingReceipt {
    let assessment_id =
        OperationId::new(client.parts().ordinary, OperationSequence::new(1).unwrap());
    let assessed = f
        .service
        .assess_unpair(assessment_id, target)
        .await
        .unwrap();
    let assessment = match assessed.outcome {
        PairingOutcome::UnpairAssessed(assessment) => assessment,
        _ => panic!("wrong outcome"),
    };
    let operation = OperationId::new(client.parts().ordinary, OperationSequence::new(2).unwrap());
    match f
        .service
        .unpair(operation, assessment)
        .await
        .unwrap()
        .outcome
    {
        PairingOutcome::Revoked(receipt) => receipt,
        _ => panic!("wrong outcome"),
    }
}
#[tokio::test]
async fn revocation_retires_ordinary_streams_and_retains_restricted_proof() {
    let f = Fixture::new();
    let receipt = f.enroll().await;
    let (target, client) = receipt_target(&receipt);
    let proof = revoke(&f, target, &client).await;
    assert!(ReceiptCodec::new(f.pin).verify(&proof).is_ok());
    let state = f.actor.state.lock().unwrap();
    let a = state.snapshot.recovery().parts().authorizations[0].parts();
    assert!(matches!(a.record.state, PairingState::Revoked { .. }));
    assert_eq!(a.revocation_receipt.as_ref(), Some(&proof));
    assert_eq!(
        state
            .streams
            .iter()
            .find(|s| s.binding.stream() == client.parts().ordinary)
            .unwrap()
            .status,
        StreamStatus::Retired(StreamRetirementReason::PairingRevoked)
    );
    assert_eq!(
        state
            .streams
            .iter()
            .find(|s| s.binding.stream() == client.parts().self_revocation)
            .unwrap()
            .status,
        StreamStatus::Active
    );
    assert_eq!(state.events.last(), Some(&"invalidation"));
}
#[tokio::test]
async fn revoked_stream_cannot_execute_another_operation() {
    let f = Fixture::new();
    let receipt = f.enroll().await;
    let (target, client) = receipt_target(&receipt);
    let _proof = revoke(&f, target.clone(), &client).await;
    let operation = OperationId::new(client.parts().ordinary, OperationSequence::new(3).unwrap());
    assert!(matches!(
        f.service.assess_unpair(operation, target).await,
        Err(PairingFailure::PairingRevoked)
    ));
}
#[tokio::test]
async fn last_device_owner_recovery_creates_new_lifetime_without_reactivation() {
    let f = Fixture::new();
    let receipt = f.enroll().await;
    let (target, client) = receipt_target(&receipt);
    let proof = revoke(&f, target, &client).await;
    let second = f.enroll().await;
    let (new_target, new_client) = receipt_target(&second);
    assert_ne!(new_target.parts().pairing, client.parts().client.pairing);
    assert_ne!(new_client.parts().client.id, client.parts().client.id);
    let state = f.actor.state.lock().unwrap();
    let a = &state.snapshot.recovery().parts().authorizations;
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].parts().revocation_receipt.as_ref(), Some(&proof));
    assert_eq!(a[1].parts().record.state, PairingState::Authorized);
}
struct Source(Arc<Actor>);
impl spaceterm_server::pairing::protocol_lookup::PairingSnapshotPort for Source {
    fn snapshot(&self) -> Result<PairingSnapshot, ProtocolError> {
        Ok(self.0.state.lock().unwrap().snapshot.clone())
    }
}
fn restricted_channel(
    f: &Fixture,
    target: &PairingTarget,
    client: &PairingClientStreams,
    versions: Vec<u32>,
) -> spaceterm_protocol::connection::RestrictedConnection {
    use spaceterm_protocol::authentication::*;
    let local = |value: u64| {
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&value.to_be_bytes());
        bytes
    };
    let principal = Principal {
        server: f.pin.identity,
        device: target.parts().device.bytes(),
        pairing: local(target.parts().pairing.local()),
        client: local(client.parts().client.id.local()),
    };
    let config = HandshakeConfig {
        majors: versions,
        ..Default::default()
    };
    let lookup = spaceterm_server::pairing::protocol_lookup::PairingProtocolLookup::new(Source(
        f.actor.clone(),
    ));
    let (client, hello) = ClientHandshake::begin(
        f.pin,
        principal,
        Purpose::RevocationStatus,
        config.clone(),
        f.auth.clone(),
    )
    .unwrap();
    let (server, reply) =
        ServerHandshake::accept(f.pin.identity, 1, config, f.server.clone(), &lookup, &hello)
            .unwrap();
    let (client, proof) = client.receive_server(&reply).unwrap();
    let (server, ready) = server.receive_device(&lookup, &proof).unwrap();
    assert!(matches!(
        client.receive_ready(&ready).unwrap(),
        Established::Restricted(_)
    ));
    match server {
        Established::Restricted(server) => server,
        _ => panic!("ordinary escalation"),
    }
}
#[tokio::test]
async fn already_revoked_by_ordinary_operation_returns_original_target_receipt_in_both_majors() {
    let f = Fixture::new();
    let receipt = f.enroll().await;
    let (target, client) = receipt_target(&receipt);
    let proof = revoke(&f, target.clone(), &client).await;
    let operation = OperationId::new(
        client.parts().self_revocation,
        OperationSequence::new(1).unwrap(),
    );
    let command = Command::Pairing(PairingCommand::SelfRevoke {
        target: target.clone(),
    });
    let digest = Codec
        .canonical_request_digest(
            &StreamBinding::from_stream(operation.stream()),
            operation,
            &command,
        )
        .unwrap();
    for versions in [vec![2, 1], vec![1]] {
        let connection = restricted_channel(&f, &target, &client, versions);
        let request = spaceterm_protocol::wire::pairing::RestrictedExtension {
            kind: Some(
                spaceterm_protocol::wire::pairing::restricted_extension::Kind::Revoke(
                    spaceterm_protocol::wire::pairing::SelfRevocation {
                        operation: Some(operation_to_wire(operation)),
                        request_digest: digest.bytes().to_vec(),
                        target: Some(target_to_wire(&target)),
                    },
                ),
            ),
        };
        let result = f
            .service
            .restricted_exchange(&connection, request)
            .await
            .unwrap();
        assert_eq!(
            result,
            spaceterm_server::pairing::restricted::RestrictedRevocationResult::AlreadyRevoked(
                proof.clone()
            )
        );
    }
    assert!(
        !f.actor
            .state
            .lock()
            .unwrap()
            .outcomes
            .contains_key(&operation)
    );
}

#[tokio::test]
async fn lost_revocation_reply_is_resolved_after_authorization_is_revoked() {
    for versions in [vec![2, 1], vec![1]] {
        let f = Fixture::new();
        let enrollment = f.enroll().await;
        let (target, client) = receipt_target(&enrollment);
        let operation = OperationId::new(
            client.parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        );
        let command = Command::Pairing(PairingCommand::SelfRevoke {
            target: target.clone(),
        });
        let digest = Codec
            .canonical_request_digest(
                &StreamBinding::from_stream(operation.stream()),
                operation,
                &command,
            )
            .unwrap();
        let request = spaceterm_protocol::wire::pairing::RestrictedExtension {
            kind: Some(
                spaceterm_protocol::wire::pairing::restricted_extension::Kind::Revoke(
                    spaceterm_protocol::wire::pairing::SelfRevocation {
                        operation: Some(operation_to_wire(operation)),
                        request_digest: digest.bytes().to_vec(),
                        target: Some(target_to_wire(&target)),
                    },
                ),
            ),
        };
        let connection = restricted_channel(&f, &target, &client, versions.clone());
        let result = f
            .service
            .restricted_exchange(&connection, request.clone())
            .await
            .unwrap();
        assert!(matches!(
            result,
            spaceterm_server::pairing::restricted::RestrictedRevocationResult::Executed(_)
        ));
        let revision = f.actor.state.lock().unwrap().snapshot.revision();
        let new_connection = restricted_channel(&f, &target, &client, versions);
        assert_eq!(
            f.service
                .restricted_exchange(&new_connection, request)
                .await
                .unwrap(),
            result
        );
        assert_eq!(f.actor.state.lock().unwrap().snapshot.revision(), revision);
    }
}
#[tokio::test]
async fn revoked_key_is_rejected_for_ordinary_authentication() {
    use spaceterm_protocol::authentication::*;
    let f = Fixture::new();
    let receipt = f.enroll().await;
    let (target, client) = receipt_target(&receipt);
    let _proof = revoke(&f, target.clone(), &client).await;
    let local = |value: u64| {
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&value.to_be_bytes());
        bytes
    };
    let principal = Principal {
        server: f.pin.identity,
        device: target.parts().device.bytes(),
        pairing: local(target.parts().pairing.local()),
        client: local(client.parts().client.id.local()),
    };
    let (_, hello) = ClientHandshake::begin(
        f.pin,
        principal,
        Purpose::Ordinary,
        HandshakeConfig::default(),
        f.auth.clone(),
    )
    .unwrap();
    let lookup = spaceterm_server::pairing::protocol_lookup::PairingProtocolLookup::new(Source(
        f.actor.clone(),
    ));
    assert!(matches!(
        ServerHandshake::accept(
            f.pin.identity,
            1,
            HandshakeConfig::default(),
            f.server.clone(),
            &lookup,
            &hello
        ),
        Err(ProtocolError::PairingDenied)
    ));
}
#[tokio::test]
async fn projection_rejects_revoked_record_reactivation() {
    let f = Fixture::new();
    let receipt = f.enroll().await;
    let (target, client) = receipt_target(&receipt);
    let _proof = revoke(&f, target, &client).await;
    let state = f.actor.state.lock().unwrap();
    let a = state.snapshot.recovery().parts().authorizations[0].parts();
    let mut value = a.clone();
    value.record.state = PairingState::Authorized;
    value.revocation_receipt = None;
    value.authorization_version = a.authorization_version.checked_successor().unwrap();
    let transition = spaceterm_core::pairing::transition::PairingTransition {
        expected_store: state.snapshot.revision(),
        high_water: state.snapshot.high_water(),
        writes: PairingWrites::new(PairingWritesParts {
            writes: vec![PairingWrite::PutAuthorization {
                expected_authorization_version: Some(a.authorization_version),
                value: PairingAuthorization::new(value).unwrap(),
            }],
        })
        .unwrap(),
        register_streams: vec![],
        retire_ordinary_streams: vec![],
        release_clients: vec![],
        outcome: PairingOutcome::Failed(PairingFailure::InvalidRequest),
    };
    assert!(matches!(
        project_pairing(&state.snapshot, &transition, &f.actor.receipts),
        Err(PairingFailure::InvalidDurableState)
    ));
}

#[tokio::test]
async fn concurrent_ticket_consumption_creates_one_pairing() {
    let f = Fixture::new();
    let issued = f.issue().await;
    let (mut first, mut first_server) = f.channel();
    let (mut second, mut second_server) = f.channel();
    let authorization = issued.authorization.as_ref().unwrap();
    let a = first
        .submission_message(authorization, f.device.as_ref())
        .unwrap();
    let b = second
        .submission_message(authorization, f.device.as_ref())
        .unwrap();
    let first = first_server.receive_submission(&a).unwrap();
    let second = second_server.receive_submission(&b).unwrap();
    let (first, second) = tokio::join!(f.service.enroll(first), f.service.enroll(second));
    assert_eq!(first.unwrap().outcome, second.unwrap().outcome);
    assert_eq!(
        f.actor
            .state
            .lock()
            .unwrap()
            .snapshot
            .recovery()
            .parts()
            .authorizations
            .len(),
        1
    );
}

#[tokio::test]
async fn projection_rejects_activation_without_original_consumed_reservation() {
    let f = Fixture::new();
    let state = f.actor.state.lock().unwrap();
    let server = state.snapshot.server();
    let pairing = PairingId::new(server, 1).unwrap();
    let client = ClientId::new(server, 1).unwrap();
    let stream = |n| {
        OperationStreamId::new(OperationStreamKey::new(server, n).unwrap(), pairing, client)
            .unwrap()
    };
    let value = PairingAuthorization::new(PairingAuthorizationParts {
        record: PairingRecord {
            id: pairing,
            device: DeviceId::new([4; 16]),
            state: PairingState::Authorized,
        },
        authorization_version: spaceterm_core::authority::FieldVersion::new(1),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: f.device.public_key(),
            pairing_authentication_public_key: f.auth.public_key(),
        })
        .unwrap(),
        clients: vec![
            PairingClientStreams::new(PairingClientStreamsParts {
                client: ClientRecord {
                    id: client,
                    pairing,
                },
                ordinary: stream(2),
                self_revocation: stream(3),
            })
            .unwrap(),
        ],
        revocation_receipt: None,
    })
    .unwrap();
    let transition = spaceterm_core::pairing::transition::PairingTransition {
        expected_store: state.snapshot.revision(),
        high_water: IdentityHighWater {
            pairing: 1,
            client: 1,
            operation_stream: 3,
            ..Default::default()
        },
        writes: PairingWrites::new(PairingWritesParts {
            writes: vec![PairingWrite::PutAuthorization {
                expected_authorization_version: None,
                value,
            }],
        })
        .unwrap(),
        register_streams: vec![],
        retire_ordinary_streams: vec![],
        release_clients: vec![],
        outcome: PairingOutcome::Failed(PairingFailure::InvalidRequest),
    };
    assert!(matches!(
        project_pairing(&state.snapshot, &transition, &f.actor.receipts),
        Err(PairingFailure::InvalidDurableState)
    ));
}

fn candidate_proposal(f: &Fixture, device: &Key, auth: &Key) -> EnrollmentProposal {
    EnrollmentProposal::new(EnrollmentProposalParts {
        server: ServerId::new(f.pin.identity),
        device: DeviceId::new([6; 16]),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key(),
            pairing_authentication_public_key: auth.public_key(),
        })
        .unwrap(),
        claim_nonce: [2; 32],
    })
    .unwrap()
}

fn paired_issue_from_wire(
    f: &Fixture,
    initial: &SignedPairingReceipt,
    proposal: &EnrollmentProposal,
    versions: Vec<u32>,
) -> (OperationId, EnrollmentProposal) {
    use spaceterm_protocol::{
        authentication::*,
        codec::Payload,
        wire::{pairing, v1, v2},
    };
    let (target, binding) = receipt_target(initial);
    let operation = OperationId::new(binding.parts().ordinary, OperationSequence::new(1).unwrap());
    let local = |value: u64| {
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&value.to_be_bytes());
        bytes
    };
    let principal = Principal {
        server: f.pin.identity,
        device: target.parts().device.bytes(),
        pairing: local(target.parts().pairing.local()),
        client: local(binding.parts().client.id.local()),
    };
    let config = HandshakeConfig {
        majors: versions.clone(),
        ..Default::default()
    };
    let lookup = spaceterm_server::pairing::protocol_lookup::PairingProtocolLookup::new(Source(
        f.actor.clone(),
    ));
    let (client, hello) = ClientHandshake::begin(
        f.pin,
        principal,
        Purpose::Ordinary,
        config.clone(),
        f.auth.clone(),
    )
    .unwrap();
    let (server, reply) =
        ServerHandshake::accept(f.pin.identity, 1, config, f.server.clone(), &lookup, &hello)
            .unwrap();
    let (client, proof) = client.receive_server(&reply).unwrap();
    let (server, ready) = server.receive_device(&lookup, &proof).unwrap();
    let mut client = match client.receive_ready(&ready).unwrap() {
        Established::Ordinary(c) => c,
        _ => panic!("restricted channel"),
    };
    let mut server = match server {
        Established::Ordinary(c) => c,
        _ => panic!("restricted channel"),
    };
    assert_eq!(client.major(), versions[0]);
    let request = pairing::IssueAuthorization {
        proposal: Some(proposal_to_wire(proposal)),
    };
    macro_rules! send_issue {
        ($profile:ident, $variant:ident) => {{
            let payload = Payload::$variant($profile::Message {
                kind: Some($profile::message::Kind::Operation($profile::Operation {
                    id: Some(operation_to_wire(operation)),
                    action: Some($profile::operation::Action::IdentifiedPairing(
                        $profile::PairingRequest {
                            request: 1,
                            operation: Some(operation_to_wire(operation)),
                            action: Some($profile::pairing_request::Action::IssueAuthorization(
                                request,
                            )),
                        },
                    )),
                })),
            });
            let frame = client.encode(&payload).unwrap();
            let received = server.decode(&frame).unwrap();
            match received {
                Payload::$variant($profile::Message {
                    kind: Some($profile::message::Kind::Operation(op)),
                }) => {
                    let id = operation_from_wire(op.id.as_ref().unwrap()).unwrap();
                    match op.action.unwrap() {
                        $profile::operation::Action::IdentifiedPairing(p) => {
                            match p.action.unwrap() {
                                $profile::pairing_request::Action::IssueAuthorization(issue) => (
                                    id,
                                    proposal_from_wire(issue.proposal.as_ref().unwrap()).unwrap(),
                                ),
                                _ => panic!("wrong pairing action"),
                            }
                        }
                        _ => panic!("wrong operation"),
                    }
                }
                _ => panic!("wrong profile or payload"),
            }
        }};
    }
    match client.major() {
        1 => send_issue!(v1, Major1),
        2 => send_issue!(v2, Major2),
        _ => unreachable!(),
    }
}

fn candidate_channel(
    f: &Fixture,
    public: PublicEnrollmentReservation,
    device: Arc<Key>,
    auth: Arc<Key>,
    versions: Vec<u32>,
) -> (EnrollmentConnection, EnrollmentConnection) {
    let (client, hello) =
        EnrollmentClient::begin(f.pin, public, versions.clone(), device, auth).unwrap();
    let (server, reply) = EnrollmentServer::accept(
        f.pin.identity,
        vec![2, 1],
        f.server.clone(),
        f.actor.as_ref(),
        &hello,
    )
    .unwrap();
    let (client, proof) = client.receive_server(&reply).unwrap();
    let (server, ready) = server.receive_proof(f.actor.as_ref(), &proof).unwrap();
    let client = client.receive_ready(&ready).unwrap();
    assert_eq!(client.major(), versions[0]);
    (client, server)
}

fn delivered_candidate(
    f: &Fixture,
    issued: &PairingExecution,
) -> (PublicEnrollmentReservation, EnrollmentAuthorization) {
    let bytes = {
        let s = f.actor.state.lock().unwrap();
        issued
            .authorization
            .as_ref()
            .unwrap()
            .delivery_message(s.snapshot.recovery().parts().enrollments.last().unwrap())
            .unwrap()
    };
    EnrollmentAuthorization::from_delivery(bytes).unwrap()
}

fn enrollment_receipt_response(
    client: &mut EnrollmentConnection,
    server: &mut EnrollmentConnection,
    receipt: &SignedPairingReceipt,
    pin: ServerPin,
) -> SignedPairingReceipt {
    use spaceterm_protocol::wire::pairing;
    let p = server.reservation().parts();
    let operation = OperationId::new(p.enrollment_stream, OperationSequence::new(1).unwrap());
    let message = pairing::EnrollmentMessage {
        kind: Some(pairing::enrollment_message::Kind::Result(
            pairing::EnrollmentResult {
                operation: Some(operation_to_wire(operation)),
                request_digest: p.enrollment_request_digest.bytes().to_vec(),
                receipt: Some(receipt_to_wire(receipt)),
                failure: None,
            },
        )),
    };
    let bytes = server.encode(&message).unwrap();
    match client.decode(&bytes).unwrap().kind.unwrap() {
        pairing::enrollment_message::Kind::Result(r) => ReceiptCodec::new(pin)
            .from_wire(r.receipt.as_ref().unwrap())
            .unwrap(),
        _ => panic!("wrong response"),
    }
}

#[tokio::test]
async fn paired_issuance_to_enrollment_and_historical_lookup_from_serialized_messages_in_both_majors()
 {
    for versions in [vec![2, 1], vec![1]] {
        let f = Fixture::new();
        let initial = f.enroll().await;
        let device = Key::new(4);
        let auth = Key::new(5);
        let proposal = candidate_proposal(&f, &device, &auth);
        let (operation, received) =
            paired_issue_from_wire(&f, &initial, &proposal, versions.clone());
        let issued = f
            .service
            .issue_direct_authorization(operation, received)
            .await
            .unwrap();
        let (public, authorization) = delivered_candidate(&f, &issued);
        let (mut client, mut server) = candidate_channel(
            &f,
            public.clone(),
            device.clone(),
            auth.clone(),
            versions.clone(),
        );
        let bytes = client
            .submission_message(&authorization, device.as_ref())
            .unwrap();
        let submitted = server.receive_submission(&bytes).unwrap();
        let result = f.service.enroll(submitted).await.unwrap();
        let receipt = match result.outcome {
            PairingOutcome::Enrolled(r) => r,
            _ => panic!("wrong outcome"),
        };
        let received_receipt =
            enrollment_receipt_response(&mut client, &mut server, &receipt, f.pin);
        let claims = ReceiptCodec::new(f.pin).verify(&received_receipt).unwrap();
        assert!(
            matches!(claims, PairingReceiptBody::Enrollment { ref keys, .. } if keys == &proposal.parts().keys)
        );
        f.actor.state.lock().unwrap().now = MAX_ENROLLMENT_LIFETIME_MILLIS + 100;
        drop(authorization);
        let (mut client, mut server) = candidate_channel(&f, public, device, auth, versions);
        server.decode(&client.lookup_message().unwrap()).unwrap();
        let original = f.service.lookup_enrollment(&server).await.unwrap().unwrap();
        let recovered = enrollment_receipt_response(&mut client, &mut server, &original, f.pin);
        assert_eq!(recovered, received_receipt);
        assert_eq!(
            f.actor
                .state
                .lock()
                .unwrap()
                .snapshot
                .recovery()
                .parts()
                .authorizations
                .len(),
            2
        );
    }
}

#[tokio::test]
async fn revoked_paired_issuer_cannot_activate_wire_authorization() {
    for versions in [vec![2, 1], vec![1]] {
        let f = Fixture::new();
        let initial = f.enroll().await;
        let device = Key::new(4);
        let auth = Key::new(5);
        let proposal = candidate_proposal(&f, &device, &auth);
        let (operation, proposal) =
            paired_issue_from_wire(&f, &initial, &proposal, versions.clone());
        let issued = f
            .service
            .issue_direct_authorization(operation, proposal)
            .await
            .unwrap();
        let (public, authorization) = delivered_candidate(&f, &issued);
        let (mut client, mut server) =
            candidate_channel(&f, public, device.clone(), auth, versions);
        let bytes = client
            .submission_message(&authorization, device.as_ref())
            .unwrap();
        let submitted = server.receive_submission(&bytes).unwrap();
        let (target, client) = receipt_target(&initial);
        // Self-revocation avoids reusing ordinary issuance operation 1.
        let operation = OperationId::new(
            client.parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        );
        let connection = restricted_channel(&f, &target, &client, vec![2, 1]);
        let digest = RequestDigest::for_command(&Command::Pairing(PairingCommand::SelfRevoke {
            target: target.clone(),
        }))
        .unwrap();
        f.service
            .restricted_exchange(
                &connection,
                spaceterm_protocol::wire::pairing::RestrictedExtension {
                    kind: Some(
                        spaceterm_protocol::wire::pairing::restricted_extension::Kind::Revoke(
                            spaceterm_protocol::wire::pairing::SelfRevocation {
                                operation: Some(operation_to_wire(operation)),
                                request_digest: digest.bytes().to_vec(),
                                target: Some(target_to_wire(&target)),
                            },
                        ),
                    ),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            f.service.enroll(submitted).await,
            Err(PairingFailure::PairingRevoked)
        ));
        assert_eq!(
            f.actor
                .state
                .lock()
                .unwrap()
                .snapshot
                .recovery()
                .parts()
                .authorizations
                .len(),
            1
        );
    }
}
