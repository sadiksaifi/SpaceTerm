use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    authority::*, identity::*, operation_ledger::canonical::CanonicalCodec,
    operation_ledger_contracts::*, operations::*, organization::ServerDirectory, pairing::*,
    storage::*,
};
use spaceterm_host::native_deployment::contracts::NativeRequestId;
use spaceterm_protocol::{
    ProtocolError,
    authentication::{self, SigningKey},
    pairing_adapter::*,
};
use spaceterm_server::{
    adapters::core::CorePorts,
    api::*,
    contracts::*,
    pairing::{
        management::AccountOwnerEnrollmentPort, protocol_lookup::PairingProtocolLookup, service::*,
    },
    service::RecoveredServer,
};
use std::sync::Arc;

#[path = "support/job_a.rs"]
mod native;
#[allow(dead_code)]
#[path = "store_pairing_composition/support.rs"]
mod physical;

struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
    fn pin(&self) -> authentication::ServerPin {
        authentication::ServerPin {
            identity: [11; 16],
            public_key: self.public_key(),
        }
    }
}
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
impl ServerCredentialPort for Key {}
struct Authority;
impl StartupAuthorityPort for Authority {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure> {
        Ok(physical::identity().server)
    }
    fn validate_credentials(
        &self,
        credentials: &dyn ServerCredentialPort,
        _: ServerId,
    ) -> Result<(), ServerFailure> {
        assert_eq!(
            credentials.public_key(),
            physical::identity().server_public_key
        );
        Ok(())
    }
}
struct Clock(std::sync::atomic::AtomicU64);
impl ServerClock for Clock {
    fn now(&self) -> Result<spaceterm_core::observations::MonotonicTick, ServerFailure> {
        Ok(spaceterm_core::observations::MonotonicTick::new(
            self.0.load(std::sync::atomic::Ordering::SeqCst),
        ))
    }
}
struct Owner;
impl AccountOwnerEnrollmentPort for Owner {
    fn validate_owner_request(
        &self,
        _: &NativeRequestId,
        _: RequestDigest,
        server: ServerId,
        proposal: &EnrollmentProposal,
    ) -> Result<(), PairingFailure> {
        if proposal.parts().server != server {
            return Err(PairingFailure::IdentityMismatch);
        }
        Ok(())
    }
    fn revalidate_owner_issuance(
        &self,
        _: &NativeRequestId,
        _: RequestDigest,
        _: ServerId,
        _: RequestDigest,
    ) -> Result<(), PairingFailure> {
        Ok(())
    }
}
struct DenyLookup;
impl authentication::PairingLookup for DenyLookup {
    fn lookup(
        &self,
        _: [u8; 16],
        _: [u8; 16],
        _: [u8; 16],
    ) -> Result<authentication::PairingRecord, ProtocolError> {
        Err(ProtocolError::PairingDenied)
    }
}
impl PairingAuthorizationPort for DenyLookup {}

struct Fixture {
    directory: physical::Directory,
    server: RecoveredServer,
    service: PairingService,
    signer: Arc<Key>,
    stop: Arc<native::Host>,
    runtime: Arc<native::Factory>,
    external: Arc<native::External>,
    faults: spaceterm_host::storage::testing::TestControl,
    clock: Arc<Clock>,
    outcomes: Arc<native::Outcomes>,
}
impl Fixture {
    async fn new() -> Self {
        let directory = physical::Directory::new();
        let storage = physical::Fixture::new(&directory.0);
        Self::open(directory, storage.store).await
    }
    async fn open(
        directory: physical::Directory,
        store: spaceterm_host::organization_store::SqliteOrganizationStore,
    ) -> Self {
        let faults = store.authority_test_control();
        let mut boundary = native::Fixture::new();
        let stop = boundary.host.clone();
        let runtime = boundary.factory.clone();
        let external = boundary.external.clone();
        let outcomes = boundary.outcomes.clone();
        let clock = Arc::new(Clock(std::sync::atomic::AtomicU64::new(1000)));
        boundary.dependencies.clock = clock.clone();
        let signer = Arc::new(Key::new(71));
        boundary.config.host.authority = Arc::new(Authority);
        boundary.config.core = CorePorts::production(
            physical::identity(),
            ServerDirectory::new(
                physical::identity().server,
                directory.0.as_os_str().as_encoded_bytes().to_vec(),
            )
            .unwrap(),
            clock.clone(),
        )
        .unwrap();
        boundary.dependencies.store = Box::new(store);
        boundary.dependencies.credentials = signer.clone();
        boundary.dependencies.pairing = Arc::new(DenyLookup);
        let server = RecoveredServer::recover(boundary.config, boundary.dependencies)
            .await
            .unwrap();
        let service = PairingService::new(
            Arc::new(server.handle()),
            signer.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Owner),
            signer.pin(),
        )
        .unwrap();
        Self {
            directory,
            server,
            service,
            signer,
            stop,
            runtime,
            external,
            faults,
            clock,
            outcomes,
        }
    }
    async fn close(self) -> physical::Directory {
        self.server
            .handle()
            .shutdown(spaceterm_server::shutdown::TrustedShutdownRequest {
                authority: self.stop,
            })
            .await
            .unwrap();
        self.server.wait().await.unwrap();
        self.directory
    }
    async fn enroll(&self, seed: u8, major: u32) -> (EnrollmentReservation, SignedPairingReceipt) {
        let issued = self.issue(seed).await;
        let PairingOutcome::AuthorizationReserved { ticket, .. } = issued.outcome else {
            panic!("authorization")
        };
        let reservation = self
            .server
            .handle()
            .pairing_snapshot()
            .unwrap()
            .reservation(&ticket)
            .unwrap()
            .clone();
        let (mut client, mut server) = self.channel(&reservation, seed, major);
        let submit = client
            .submission_message(issued.authorization.as_ref().unwrap(), &Key::new(seed))
            .unwrap();
        let reply = self
            .service
            .enrollment_frame(&mut server, &submit)
            .await
            .unwrap();
        let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
            client.decode(&reply).unwrap().kind.unwrap()
        else {
            panic!("result")
        };
        (
            reservation,
            ReceiptCodec::new(self.signer.pin())
                .from_wire(&result.receipt.unwrap())
                .unwrap(),
        )
    }
    fn handshake(
        &self,
        seed: u8,
        streams: &PairingClientStreams,
        purpose: authentication::Purpose,
        major: u32,
    ) -> (authentication::Established, authentication::Established) {
        let lookup = PairingProtocolLookup::new(self.server.handle());
        let p = authentication::Principal {
            server: self.signer.pin().identity,
            device: [seed; 16],
            pairing: native::local(streams.parts().client.pairing.local()),
            client: native::local(streams.parts().client.id.local()),
        };
        let config = authentication::HandshakeConfig {
            majors: vec![major],
            capabilities: vec![spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN],
            ..Default::default()
        };
        let (client, hello) = authentication::ClientHandshake::begin(
            self.signer.pin(),
            p,
            purpose,
            config.clone(),
            Arc::new(Key::new(seed + 1)),
        )
        .unwrap();
        let (server, hello) = authentication::ServerHandshake::accept(
            p.server,
            self.server.handle().snapshot().epoch().get(),
            config,
            self.signer.clone(),
            &lookup,
            &hello,
        )
        .unwrap();
        let (client, proof) = client.receive_server(&hello).unwrap();
        let (server, ready) = server.receive_device(&lookup, &proof).unwrap();
        (client.receive_ready(&ready).unwrap(), server)
    }
    fn principal(
        &self,
        seed: u8,
        streams: &PairingClientStreams,
        generation: u64,
        major: u32,
    ) -> VerifiedPrincipal {
        let (_, authentication::Established::Ordinary(codec)) =
            self.handshake(seed, streams, authentication::Purpose::Ordinary, major)
        else {
            panic!("ordinary")
        };
        VerifiedPrincipal::from_authenticated(&codec, ConnectionGeneration::new(generation))
            .unwrap()
    }
    fn claim(&self, seed: u8) -> VerifiedDeviceClaim {
        let device = Key::new(seed);
        let auth = Key::new(seed + 1);
        let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
            server: physical::identity().server,
            device: DeviceId::new([seed; 16]),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: device.public_key(),
                pairing_authentication_public_key: auth.public_key(),
            })
            .unwrap(),
            claim_nonce: [seed; 32],
        })
        .unwrap();
        let (device_proof, auth_proof) =
            VerifiedDeviceClaim::proofs(&proposal, &device, &auth).unwrap();
        VerifiedDeviceClaim::verify(proposal, &device_proof, &auth_proof).unwrap()
    }
    async fn issue(&self, seed: u8) -> PairingExecution {
        self.service
            .issue_owner_authorization(
                NativeRequestId::new([seed; 16]).unwrap(),
                RequestDigest::from_bytes([seed; 32]),
                self.claim(seed),
            )
            .await
            .unwrap()
    }
    fn channel(
        &self,
        reservation: &EnrollmentReservation,
        seed: u8,
        major: u32,
    ) -> (EnrollmentConnection, EnrollmentConnection) {
        let lookup = PairingProtocolLookup::new(self.server.handle());
        let config = vec![major];
        let public =
            PublicEnrollmentReservation::from_wire(&reservation_to_wire(reservation)).unwrap();
        let (client, hello) = EnrollmentClient::begin(
            self.signer.pin(),
            public,
            config.clone(),
            Arc::new(Key::new(seed)),
            Arc::new(Key::new(seed + 1)),
        )
        .unwrap();
        let (server, hello) = EnrollmentServer::accept(
            self.signer.pin().identity,
            config,
            self.signer.clone(),
            &lookup,
            &hello,
        )
        .unwrap();
        let (client, proof) = client.receive_server(&hello).unwrap();
        let (server, ready) = server.receive_proof(&lookup, &proof).unwrap();
        (client.receive_ready(&ready).unwrap(), server)
    }
}

#[tokio::test]
async fn public_enrollment_delivery_and_historical_lookup_use_real_store_in_both_majors() {
    for major in [1, 2] {
        let f = Fixture::new().await;
        let issued = f.issue(42).await;
        let PairingOutcome::AuthorizationReserved { ticket, .. } = issued.outcome else {
            panic!("authorization")
        };
        let reservation = f
            .server
            .handle()
            .pairing_snapshot()
            .unwrap()
            .reservation(&ticket)
            .unwrap()
            .clone();
        let (mut client, mut server) = f.channel(&reservation, 42, major);
        let submit = client
            .submission_message(issued.authorization.as_ref().unwrap(), &Key::new(42))
            .unwrap();
        let reply = f
            .service
            .enrollment_frame(&mut server, &submit)
            .await
            .unwrap();
        let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
            client.decode(&reply).unwrap().kind.unwrap()
        else {
            panic!("result")
        };
        let receipt = ReceiptCodec::new(f.signer.pin())
            .from_wire(&result.receipt.unwrap())
            .unwrap();
        ReceiptCodec::new(f.signer.pin())
            .validate_receipt(&receipt)
            .unwrap();
        assert_eq!(
            f.service.lookup_enrollment(&server).await.unwrap(),
            Some(receipt.clone())
        );
        let state = f.server.handle().pairing_snapshot().unwrap();
        assert_eq!(state.recovery().parts().authorizations.len(), 1);
        assert!(server.receive_submission(&submit).is_err());
        // Physical recovery checks use the production receipt reader, not a decoded fixture claim.
        let directory = f.close().await;
        let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
        assert_eq!(recovered.pairing.unwrap().parts().authorizations.len(), 1);
    }
}

fn enrolled_client(receipt: &SignedPairingReceipt) -> (PairingTarget, PairingClientStreams) {
    let PairingReceiptBody::Enrollment { target, client, .. } = &receipt.parts().claims else {
        panic!("enrollment")
    };
    (target.clone(), client.clone())
}
fn self_request(target: PairingTarget, client: &PairingClientStreams) -> CanonicalOperationRequest {
    CanonicalOperationRequest::new(
        OperationId::new(
            client.parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        ),
        Command::Pairing(PairingCommand::SelfRevoke { target }),
        &CanonicalCodec,
    )
    .unwrap()
}

#[tokio::test]
async fn self_unpair_pending_revocation_returns_exact_signed_receipt_after_ordinary_revocation() {
    use spaceterm_protocol::wire::pairing as wire;
    for major in [1, 2] {
        let f = Fixture::new().await;
        let (reservation, enrollment) = f.enroll(42, major).await;
        let (target, streams) = enrolled_client(&enrollment);
        let principal = f.principal(42, &streams, 1, major);
        native::connect(&f.server.handle(), principal).await;
        let (
            authentication::Established::Restricted(mut client),
            authentication::Established::Restricted(mut server),
        ) = f.handshake(
            42,
            &streams,
            authentication::Purpose::RevocationStatus,
            major,
        )
        else {
            panic!("restricted")
        };
        let request = self_request(target.clone(), &streams);
        let codec = PairingRestrictedCodec::new(f.signer.pin());
        let message = wire::RestrictedExtension {
            kind: Some(wire::restricted_extension::Kind::Revoke(
                wire::SelfRevocation {
                    operation: Some(operation_to_wire(request.binding().id)),
                    request_digest: request.binding().digest.bytes().to_vec(),
                    target: Some(target_to_wire(&target)),
                },
            )),
        };
        let frame = client.encode_pairing_extension(&message, &codec).unwrap();
        let frame =
            spaceterm_server::pairing::restricted::dispatch_frame(&f.service, &mut server, &frame)
                .await
                .unwrap();
        let wire::restricted_extension::Kind::Result(result) = client
            .decode_pairing_extension(&frame, &codec)
            .unwrap()
            .kind
            .unwrap()
        else {
            panic!("result")
        };
        assert_eq!(result.disposition, 1);
        let receipt = ReceiptCodec::new(f.signer.pin())
            .from_wire(&result.receipt.unwrap())
            .unwrap();
        assert!(
            f.server
                .handle()
                .lookup_state(principal, request.binding())
                .await
                .is_err()
        );
        assert_eq!(
            f.server
                .handle()
                .self_revocation_status(request.binding(), target.clone())
                .await
                .unwrap(),
            Some(receipt.clone())
        );
        f.server
            .handle()
            .self_revocation_acknowledge(request.binding(), target.clone())
            .await
            .unwrap();
        assert_eq!(
            f.server
                .handle()
                .enrollment_status(
                    Fixture::enrollment_binding(&reservation),
                    reservation.parts().ticket.clone(),
                    reservation.parts().claim_digest
                )
                .await
                .unwrap(),
            Some(enrollment.clone())
        );
        let directory = f.close().await;
        let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
        assert_eq!(
            recovered.pairing.as_ref().unwrap().parts().authorizations[0]
                .parts()
                .revocation_receipt
                .as_ref(),
            Some(&receipt)
        );
        let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
            physical::identity().server,
            recovered.ledger.unwrap(),
        )
        .unwrap();
        assert_eq!(
            ledger
                .lookup(
                    &StreamBinding::from_stream(request.binding().id.stream()),
                    request.binding().id,
                    Some(request.binding().digest)
                )
                .unwrap(),
            OperationLookup::AcknowledgedRetired
        );
        assert!(
            matches!(ledger.lookup(&StreamBinding::from_stream(reservation.parts().enrollment_stream), Fixture::enrollment_binding(&reservation).id, Some(reservation.parts().enrollment_request_digest)).unwrap(), OperationLookup::Completed { result: Outcome::Pairing(outcome), .. } if *outcome == PairingOutcome::Enrolled(enrollment))
        );
    }
}
impl Fixture {
    fn enrollment_binding(reservation: &EnrollmentReservation) -> OperationBinding {
        OperationBinding {
            id: OperationId::new(
                reservation.parts().enrollment_stream,
                OperationSequence::new(1).unwrap(),
            ),
            digest: reservation.parts().enrollment_request_digest,
        }
    }
}

#[tokio::test]
async fn enrollment_streams_cannot_enter_ordinary_submit_lookup_or_acknowledgement() {
    let f = Fixture::new().await;
    let (reservation, receipt) = f.enroll(42, 2).await;
    let (_, streams) = enrolled_client(&receipt);
    let principal = f.principal(42, &streams, 1, 2);
    native::connect(&f.server.handle(), principal).await;
    let request = CanonicalOperationRequest::new(
        Fixture::enrollment_binding(&reservation).id,
        Command::Pairing(PairingCommand::Enroll {
            ticket: reservation.parts().ticket.clone(),
            claim_digest: reservation.parts().claim_digest,
        }),
        &CanonicalCodec,
    )
    .unwrap();
    assert!(
        f.server
            .handle()
            .submit(principal, request.clone())
            .await
            .is_err()
    );
    assert!(
        f.server
            .handle()
            .lookup_state(principal, request.binding())
            .await
            .is_err()
    );
    assert!(
        f.server
            .handle()
            .acknowledge(
                principal,
                reservation.parts().enrollment_stream,
                OperationSequence::new(1).unwrap()
            )
            .await
            .is_err()
    );
    f.close().await;
}

#[tokio::test]
async fn paired_issuance_to_enrollment_uses_real_dispatch_and_ledger_in_both_majors() {
    use prost::Message;
    use spaceterm_protocol::{
        codec::Payload,
        convert::identity,
        wire::{pairing as wire, v1, v2},
    };
    use spaceterm_server::{
        pairing::dispatch::PairingOrdinaryDispatch,
        protocol_dispatch::{ActorOrdinaryDispatch, OrdinaryDispatchPort},
    };
    for major in [1, 2] {
        let f = Fixture::new().await;
        let (_, receipt) = f.enroll(42, major).await;
        let (_, streams) = enrolled_client(&receipt);
        let (
            authentication::Established::Ordinary(mut client),
            authentication::Established::Ordinary(mut server),
        ) = f.handshake(42, &streams, authentication::Purpose::Ordinary, major)
        else {
            panic!("ordinary")
        };
        let principal =
            VerifiedPrincipal::from_authenticated(&server, ConnectionGeneration::new(1)).unwrap();
        native::connect(&f.server.handle(), principal).await;
        let id = OperationId::new(streams.parts().ordinary, OperationSequence::new(1).unwrap());
        let claim = f.claim(52);
        macro_rules! request {
            ($p:ident, $major:ident) => {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::Operation($p::Operation {
                        id: Some(identity::encode_operation(id)),
                        action: Some($p::operation::Action::IdentifiedPairing(
                            $p::PairingRequest {
                                request: 0,
                                operation: Some(operation_to_wire(id)),
                                action: Some($p::pairing_request::Action::IssueAuthorization(
                                    wire::IssueAuthorization {
                                        proposal: Some(proposal_to_wire(claim.proposal())),
                                    },
                                )),
                                ..Default::default()
                            },
                        )),
                    })),
                })
            };
        }
        let request = if major == 1 {
            request!(v1, Major1)
        } else {
            request!(v2, Major2)
        };
        let bytes = client.encode(&request).unwrap();
        let request = server.decode(&bytes).unwrap();
        let dispatch = PairingOrdinaryDispatch::new(
            f.service.clone(),
            Arc::new(ActorOrdinaryDispatch::new(Arc::new(CanonicalCodec))),
        );
        let response = dispatch
            .dispatch(principal, request, f.server.handle())
            .await
            .unwrap()
            .unwrap();
        let response = client.decode(&server.encode(&response).unwrap()).unwrap();
        let delivery = match response {
            Payload::Major1(message) => {
                let v1::message::Kind::Outcome(outcome) = message.kind.unwrap() else {
                    panic!("outcome")
                };
                let v1::outcome::Result::PairingResult(result) = outcome.result.unwrap() else {
                    panic!("pairing")
                };
                assert_eq!(outcome.operation.unwrap(), operation_to_wire(id));
                result.authorization_delivery.unwrap()
            }
            Payload::Major2(message) => {
                let v2::message::Kind::Outcome(outcome) = message.kind.unwrap() else {
                    panic!("outcome")
                };
                let v2::outcome::Result::PairingResult(result) = outcome.result.unwrap() else {
                    panic!("pairing")
                };
                assert_eq!(outcome.operation.unwrap(), operation_to_wire(id));
                result.authorization_delivery.unwrap()
            }
        };
        let lookup_binding = CanonicalOperationRequest::new(
            id,
            Command::Pairing(PairingCommand::IssueDirectAuthorization {
                proposal: claim.proposal().clone(),
            }),
            &CanonicalCodec,
        )
        .unwrap()
        .binding();
        macro_rules! lookup {
            ($p:ident, $major:ident) => {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::OperationLookupRequest(
                        $p::OperationLookupRequest {
                            operation: Some(operation_to_wire(id)),
                            request_digest: lookup_binding.digest.bytes().to_vec(),
                        },
                    )),
                })
            };
        }
        let lookup = if major == 1 {
            lookup!(v1, Major1)
        } else {
            lookup!(v2, Major2)
        };
        let lookup = server.decode(&client.encode(&lookup).unwrap()).unwrap();
        let resolved = dispatch
            .dispatch(principal, lookup, f.server.handle())
            .await
            .unwrap()
            .unwrap();
        match client.decode(&server.encode(&resolved).unwrap()).unwrap() {
            Payload::Major1(message) => {
                let v1::message::Kind::OperationLookupResult(result) = message.kind.unwrap() else {
                    panic!("lookup")
                };
                let v1::operation_lookup_result::Disposition::Completed(outcome) =
                    result.disposition.unwrap()
                else {
                    panic!("completed")
                };
                let v1::outcome::Result::PairingResult(result) = outcome.result.unwrap() else {
                    panic!("pairing")
                };
                assert!(result.reservation.is_some());
                assert!(result.authorization_delivery.is_none());
            }
            Payload::Major2(message) => {
                let v2::message::Kind::OperationLookupResult(result) = message.kind.unwrap() else {
                    panic!("lookup")
                };
                let v2::operation_lookup_result::Disposition::Completed(outcome) =
                    result.disposition.unwrap()
                else {
                    panic!("completed")
                };
                let v2::outcome::Result::PairingResult(result) = outcome.result.unwrap() else {
                    panic!("pairing")
                };
                assert!(result.reservation.is_some());
                assert!(result.authorization_delivery.is_none());
            }
        }
        let (public, authorization) =
            EnrollmentAuthorization::from_delivery(delivery.encode_to_vec().into()).unwrap();
        let reservation = f
            .server
            .handle()
            .pairing_snapshot()
            .unwrap()
            .reservation(&public.parts().ticket)
            .unwrap()
            .clone();
        let (mut candidate, mut server) = f.channel(&reservation, 52, major);
        let submit = candidate
            .submission_message(&authorization, &Key::new(52))
            .unwrap();
        let outcome = f
            .service
            .enroll(server.receive_submission(&submit).unwrap())
            .await
            .unwrap();
        assert!(matches!(outcome.outcome, PairingOutcome::Enrolled(_)));
        let command = PairingCommand::IssueDirectAuthorization {
            proposal: claim.proposal().clone(),
        };
        let binding =
            CanonicalOperationRequest::new(id, Command::Pairing(command), &CanonicalCodec)
                .unwrap()
                .binding();
        assert!(
            matches!(f.server.handle().lookup_state(principal, binding).await.unwrap(), OperationLookup::Completed { result: Outcome::Pairing(outcome), .. } if matches!(*outcome, PairingOutcome::AuthorizationReserved { .. }))
        );
        let directory = f.close().await;
        let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
        assert_eq!(
            recovered
                .pairing
                .as_ref()
                .unwrap()
                .parts()
                .authorizations
                .len(),
            2
        );
        let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
            physical::identity().server,
            recovered.ledger.unwrap(),
        )
        .unwrap();
        let enrollment = Fixture::enrollment_binding(&reservation);
        assert!(
            matches!(ledger.lookup(&StreamBinding::from_stream(enrollment.id.stream()), enrollment.id, Some(enrollment.digest)).unwrap(), OperationLookup::Completed { result: Outcome::Pairing(outcome), .. } if matches!(*outcome, PairingOutcome::Enrolled(_)))
        );
    }
}

#[tokio::test]
async fn direct_enrollment_requires_exact_fingerprint_verification_and_single_use_authorization() {
    use spaceterm_client::pairing::enrollment::DirectEnrollmentAttempt;
    let f = Fixture::new().await;
    let route = RevocationRouteReference::new([5; 16]);
    let attempt = DirectEnrollmentAttempt::new(f.signer.pin(), route, 1).unwrap();
    assert!(
        attempt
            .confirm(f.signer.pin(), route, 1, "SHA256:wrong")
            .is_err()
    );
    assert!(
        f.server
            .handle()
            .pairing_snapshot()
            .unwrap()
            .recovery()
            .parts()
            .enrollments
            .is_empty()
    );
    let verified = attempt
        .confirm(f.signer.pin(), route, 1, &attempt.fingerprint())
        .unwrap();
    assert_eq!(verified.pin(), f.signer.pin());
    let issued = f.issue(42).await;
    let PairingOutcome::AuthorizationReserved { ticket, .. } = issued.outcome else {
        panic!("authorization")
    };
    let reservation = f
        .server
        .handle()
        .pairing_snapshot()
        .unwrap()
        .reservation(&ticket)
        .unwrap()
        .clone();
    let (mut first, mut first_server) = f.channel(&reservation, 42, 2);
    let (mut second, mut second_server) = f.channel(&reservation, 42, 2);
    let a = first
        .submission_message(issued.authorization.as_ref().unwrap(), &Key::new(42))
        .unwrap();
    let b = second
        .submission_message(issued.authorization.as_ref().unwrap(), &Key::new(42))
        .unwrap();
    let first = f
        .service
        .enroll(first_server.receive_submission(&a).unwrap())
        .await
        .unwrap();
    let second = f
        .service
        .enroll(second_server.receive_submission(&b).unwrap())
        .await
        .unwrap();
    assert_eq!(first.outcome, second.outcome);
    assert_eq!(
        f.server
            .handle()
            .pairing_snapshot()
            .unwrap()
            .recovery()
            .parts()
            .authorizations
            .len(),
        1
    );
    assert!(
        f.service
            .issue_owner_authorization(
                NativeRequestId::new([43; 16]).unwrap(),
                RequestDigest::from_bytes([43; 32]),
                f.claim(42)
            )
            .await
            .is_err()
    );
    f.close().await;
}

#[tokio::test]
async fn additional_client_binding_reserves_distinct_identity_and_streams_in_real_actor() {
    let f = Fixture::new().await;
    let (_, receipt) = f.enroll(42, 2).await;
    let (target, initial) = enrolled_client(&receipt);
    let issuer = f.principal(42, &initial, 1, 2);
    native::connect(&f.server.handle(), issuer).await;
    let id = OperationId::new(initial.parts().ordinary, OperationSequence::new(1).unwrap());
    let (device, auth) = sign_client_binding_claim(
        &target,
        FieldVersion::new(1),
        id,
        &Key::new(42),
        &Key::new(43),
    )
    .unwrap();
    let (digest, _) = client_binding_claim(&target, FieldVersion::new(1), id).unwrap();
    let execution = f
        .service
        .bind_additional_client(
            issuer,
            id,
            target.clone(),
            FieldVersion::new(1),
            digest,
            (device, auth),
        )
        .await
        .unwrap();
    let PairingOutcome::ClientBound(proof) = execution.outcome else {
        panic!("binding")
    };
    let PairingReceiptBody::ClientBinding { client, .. } = &proof.parts().claims else {
        panic!("binding")
    };
    assert_ne!(client.parts().client.id, initial.parts().client.id);
    assert_ne!(client.parts().ordinary, initial.parts().ordinary);
    let principal = f.principal(42, client, 1, 2);
    native::connect(&f.server.handle(), principal).await;
    let directory = f.close().await;
    let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
    let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
        physical::identity().server,
        recovered.ledger.unwrap(),
    )
    .unwrap();
    let stream = ledger
        .stream(&StreamBinding::from_stream(client.parts().self_revocation))
        .unwrap();
    assert_eq!(stream.purpose, StreamPurpose::SelfRevocation);
    assert_eq!(
        stream.dormant_reservation,
        Some(OutcomeReservation::full(
            ReservationClass::DedicatedSelfRevocation
        ))
    );
    assert_eq!(
        recovered.pairing.unwrap().parts().authorizations[0]
            .parts()
            .clients
            .len(),
        2
    );
}

#[tokio::test]
async fn expired_ticket_invalidation_releases_outstanding_quota_without_reusing_identities() {
    let f = Fixture::new().await;
    for seed in 1..=32 {
        f.issue(seed).await;
    }
    assert!(
        f.service
            .issue_owner_authorization(
                NativeRequestId::new([33; 16]).unwrap(),
                RequestDigest::from_bytes([33; 32]),
                f.claim(33)
            )
            .await
            .is_err()
    );
    let old = f.server.handle().pairing_snapshot().unwrap();
    f.clock.0.store(301001, std::sync::atomic::Ordering::SeqCst);
    f.server
        .handle()
        .invalidate_pairing_tickets()
        .await
        .unwrap();
    f.issue(34).await;
    let state = f.server.handle().pairing_snapshot().unwrap();
    assert_eq!(state.high_water().pairing, old.high_water().pairing + 1);
    assert_eq!(
        state
            .recovery()
            .parts()
            .enrollments
            .iter()
            .filter(|e| e.parts().state.kind() == EnrollmentReservationKind::Reserved)
            .count(),
        1
    );
    let original = &old.recovery().parts().enrollments[0];
    let (mut client, mut server) = f.channel(original, 1, 2);
    let lookup = client.lookup_message().unwrap();
    let response = f
        .service
        .enrollment_frame(&mut server, &lookup)
        .await
        .unwrap();
    let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
        client.decode(&response).unwrap().kind.unwrap()
    else {
        panic!("result")
    };
    assert!(result.receipt.is_none());
    let directory = f.close().await;
    let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
    for stream in recovered.ledger.unwrap().streams().iter().take(32) {
        assert!(matches!(stream.status, StreamStatus::Retired(_)));
    }
}

async fn execute_self_revocation(
    f: &Fixture,
    target: PairingTarget,
    streams: &PairingClientStreams,
) -> Result<SignedPairingReceipt, PairingFailure> {
    let request = self_request(target.clone(), streams);
    let (_, authentication::Established::Restricted(connection)) =
        f.handshake(42, streams, authentication::Purpose::RevocationStatus, 2)
    else {
        panic!("restricted")
    };
    let result = f
        .service
        .restricted_exchange(
            &connection,
            spaceterm_protocol::wire::pairing::RestrictedExtension {
                kind: Some(
                    spaceterm_protocol::wire::pairing::restricted_extension::Kind::Revoke(
                        spaceterm_protocol::wire::pairing::SelfRevocation {
                            operation: Some(operation_to_wire(request.binding().id)),
                            request_digest: request.binding().digest.bytes().to_vec(),
                            target: Some(target_to_wire(&target)),
                        },
                    ),
                ),
            },
        )
        .await?;
    match result {
        spaceterm_server::pairing::restricted::RestrictedRevocationResult::Executed(receipt)
        | spaceterm_server::pairing::restricted::RestrictedRevocationResult::AlreadyRevoked(
            receipt,
        ) => Ok(receipt),
        _ => Err(PairingFailure::RevocationUnconfirmed),
    }
}

#[tokio::test]
async fn revocation_sync_failure_closes_admission_until_recovery() {
    use spaceterm_host::storage::testing::FaultPoint;
    let f = Fixture::new().await;
    let (_, enrollment) = f.enroll(42, 2).await;
    let (target, streams) = enrolled_client(&enrollment);
    let request = self_request(target.clone(), &streams);
    let drain = native::Gate::new();
    *f.external.gate.lock().unwrap() = Some(drain.clone());
    let next = f.faults.clone();
    f.faults.barrier(FaultPoint::AfterCommit, move || {
        next.fail_once(FaultPoint::DatabaseSync, 0)
    });
    assert!(matches!(
        execute_self_revocation(&f, target.clone(), &streams).await,
        Err(PairingFailure::RecoveryRequired)
    ));
    assert!(!f.faults.fault_pending());
    assert_eq!(f.server.handle().phase(), ServicePhase::RecoveryRequired);
    f.faults.repeat_sync().unwrap();
    let calls = f.faults.sql_calls();
    assert!(
        f.server
            .handle()
            .self_revocation_status(request.binding(), target)
            .await
            .is_err()
    );
    assert!(
        f.service
            .issue_owner_authorization(
                NativeRequestId::new([52; 16]).unwrap(),
                RequestDigest::from_bytes([52; 32]),
                f.claim(52)
            )
            .await
            .is_err()
    );
    assert_eq!(f.faults.sql_calls(), calls);
    let directory = f.directory;
    drain.release();
    let _ = f.server.wait().await;
    let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
    let state = recovered.pairing.as_ref().unwrap().parts().authorizations[0].parts();
    let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
        physical::identity().server,
        recovered.ledger.unwrap(),
    )
    .unwrap();
    match ledger
        .lookup(
            &StreamBinding::from_stream(request.binding().id.stream()),
            request.binding().id,
            Some(request.binding().digest),
        )
        .unwrap()
    {
        OperationLookup::Pending => assert_eq!(state.record.state, PairingState::Authorized),
        OperationLookup::Completed {
            result: Outcome::Pairing(outcome),
            ..
        } => assert_eq!(
            *outcome,
            PairingOutcome::Revoked(state.revocation_receipt.clone().unwrap())
        ),
        value => panic!("unexpected recovery: {value:?}"),
    }
}

fn kill_on_commit(
    control: spaceterm_host::storage::testing::TestControl,
    point: spaceterm_host::storage::testing::FaultPoint,
    commits: usize,
) {
    let next = control.clone();
    control.barrier(point, move || {
        if commits > 1 {
            kill_on_commit(next, point, commits - 1);
        } else {
            // Kill this owned child, without unwinding actor/store destructors.
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", &std::process::id().to_string()])
                .status();
            std::process::exit(74);
        }
    });
}
async fn revocation_crash(test: &str, after: bool) {
    use spaceterm_host::storage::testing::FaultPoint;
    use std::os::unix::process::ExitStatusExt;
    const DIRECTORY: &str = "SPACETERM_TEST_PAIRING_CRASH_DIRECTORY";
    if let Some(path) = std::env::var_os(DIRECTORY) {
        let directory = physical::Directory(path.into());
        let store = physical::reopen(&directory.0).unwrap();
        let f = Fixture::open(directory, store).await;
        let state = f.server.handle().pairing_snapshot().unwrap();
        let e = state.recovery().parts().enrollments[0].parts();
        let EnrollmentReservationState::Consumed { receipt, .. } = &e.state else {
            panic!("consumed")
        };
        let (target, streams) = enrolled_client(receipt);
        kill_on_commit(
            f.faults.clone(),
            if after {
                FaultPoint::AfterCommit
            } else {
                FaultPoint::BeforeCommit
            },
            2,
        );
        execute_self_revocation(&f, target, &streams).await.unwrap();
        panic!("crash barrier did not fire");
    }
    let f = Fixture::new().await;
    let (_, enrollment) = f.enroll(42, 2).await;
    let (target, streams) = enrolled_client(&enrollment);
    let request = self_request(target.clone(), &streams);
    let directory = f.close().await;
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(DIRECTORY, &directory.0)
        .output()
        .unwrap();
    assert_eq!(
        child.status.signal(),
        Some(9),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let store = physical::reopen(&directory.0).unwrap();
    let f = Fixture::open(directory, store).await;
    let state = f.server.handle().pairing_snapshot().unwrap();
    let a = state.authorization(target.parts().pairing).unwrap().parts();
    if after {
        let proof = f
            .server
            .handle()
            .self_revocation_status(request.binding(), target)
            .await
            .unwrap()
            .unwrap();
        ReceiptCodec::new(f.signer.pin())
            .validate_receipt(&proof)
            .unwrap();
        assert_eq!(a.revocation_receipt.as_ref(), Some(&proof));
        assert!(matches!(a.record.state, PairingState::Revoked { .. }));
    } else {
        assert_eq!(a.record.state, PairingState::Authorized);
        assert!(a.revocation_receipt.is_none());
        execute_self_revocation(&f, target, &streams).await.unwrap();
    }
    f.close().await;
}
#[tokio::test]
async fn revocation_crash_before_commit_preserves_active_pairing() {
    revocation_crash(
        "revocation_crash_before_commit_preserves_active_pairing",
        false,
    )
    .await;
}
#[tokio::test]
async fn revocation_crash_after_commit_before_reply_recovers_receipt() {
    revocation_crash(
        "revocation_crash_after_commit_before_reply_recovers_receipt",
        true,
    )
    .await;
}

#[tokio::test]
async fn enrollment_crash_after_commit_does_not_duplicate_pairing() {
    use spaceterm_host::storage::testing::FaultPoint;
    use std::os::unix::process::ExitStatusExt;
    const DIRECTORY: &str = "SPACETERM_TEST_PAIRING_ENROLL_CRASH_DIRECTORY";
    const STEP: &str = "SPACETERM_TEST_PAIRING_ENROLL_CRASH_STEP";
    if let Some(path) = std::env::var_os(DIRECTORY) {
        let directory = physical::Directory(path.into());
        let store = physical::reopen(&directory.0).unwrap();
        let f = Fixture::open(directory, store).await;
        let step = std::env::var(STEP).unwrap().parse().unwrap();
        kill_on_commit(f.faults.clone(), FaultPoint::AfterCommit, step);
        f.enroll(42, 2).await;
        panic!("crash barrier did not fire");
    }
    for step in 1..=3 {
        let f = Fixture::new().await;
        let directory = f.close().await;
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "enrollment_crash_after_commit_does_not_duplicate_pairing",
                "--nocapture",
            ])
            .env(DIRECTORY, &directory.0)
            .env(STEP, step.to_string())
            .output()
            .unwrap();
        assert_eq!(
            child.status.signal(),
            Some(9),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        let store = physical::reopen(&directory.0).unwrap();
        let f = Fixture::open(directory, store).await;
        let state = f.server.handle().pairing_snapshot().unwrap();
        let reservation = &state.recovery().parts().enrollments[0];
        assert_eq!(state.high_water().pairing, 1);
        assert_eq!(
            state.recovery().parts().authorizations.len(),
            usize::from(step == 3)
        );
        if step == 3 {
            let expected = f
                .server
                .handle()
                .enrollment_status(
                    Fixture::enrollment_binding(reservation),
                    reservation.parts().ticket.clone(),
                    reservation.parts().claim_digest,
                )
                .await
                .unwrap()
                .unwrap();
            let (mut client, mut server) = f.channel(reservation, 42, 2);
            let lookup = client.lookup_message().unwrap();
            let reply = f
                .service
                .enrollment_frame(&mut server, &lookup)
                .await
                .unwrap();
            let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
                client.decode(&reply).unwrap().kind.unwrap()
            else {
                panic!("result")
            };
            assert_eq!(
                ReceiptCodec::new(f.signer.pin())
                    .from_wire(&result.receipt.unwrap())
                    .unwrap(),
                expected
            );
            let unavailable_secret = EnrollmentAuthorization::generate().unwrap();
            let submit = client
                .submission_message(&unavailable_secret, &Key::new(42))
                .unwrap();
            assert_eq!(
                f.service
                    .enroll(server.receive_submission(&submit).unwrap())
                    .await
                    .unwrap()
                    .outcome,
                PairingOutcome::Enrolled(expected)
            );
            assert_eq!(
                f.server
                    .handle()
                    .pairing_snapshot()
                    .unwrap()
                    .high_water()
                    .pairing,
                1
            );
        } else {
            assert_eq!(
                reservation.parts().state.kind(),
                EnrollmentReservationKind::Invalidated
            );
            assert!(
                f.server
                    .handle()
                    .enrollment_status(
                        Fixture::enrollment_binding(reservation),
                        reservation.parts().ticket.clone(),
                        reservation.parts().claim_digest
                    )
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(f.issue(52).await.authorization.is_some());
            assert_eq!(
                f.server
                    .handle()
                    .pairing_snapshot()
                    .unwrap()
                    .high_water()
                    .pairing,
                2
            );
        }
        f.close().await;
    }
}

async fn start_session(
    f: &Fixture,
    principal: VerifiedPrincipal,
    stream: OperationStreamId,
    first_sequence: u64,
) -> (SessionKey, ControlGrant) {
    use spaceterm_core::{control::*, geometry::*, organization::*};
    let handle = f.server.handle();
    native::connect(&handle, principal).await;
    let request = |sequence, command| {
        CanonicalOperationRequest::new(
            OperationId::new(stream, OperationSequence::new(sequence).unwrap()),
            command,
            &CanonicalCodec,
        )
        .unwrap()
    };
    let OperationResponse::Completed {
        outcome: Outcome::CreatedWithControl(created),
        ..
    } = handle
        .submit(
            principal,
            request(
                first_sequence,
                Command::CreateWorkspace {
                    server: stream.server(),
                    source: WorkspaceCreationSource::ServerHome {
                        server: stream.server(),
                    },
                    name: NameDraft::Custom(NameText::new("Pairing".into()).unwrap()),
                },
            ),
        )
        .await
        .unwrap()
    else {
        panic!("created")
    };
    let grant = created.control().grant();
    let generation = handle
        .snapshot()
        .runtime()
        .iter()
        .find(|pane| pane.pane == grant.pane())
        .unwrap()
        .session_generation;
    let fence = EffectFence::new(
        SessionKey {
            epoch: principal.epoch(),
            pane: grant.pane(),
            generation,
        },
        grant,
        principal.binding(),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    let intent = StartupIntent::new(
        OperationId::new(stream, OperationSequence::new(first_sequence + 1).unwrap()),
        fence,
        StartupSource::StartSession,
    )
    .unwrap();
    handle
        .submit(
            principal,
            request(first_sequence + 1, Command::BeginStartup(intent)),
        )
        .await
        .unwrap();
    let OperationResponse::Completed {
        outcome: Outcome::SessionStarted(session),
        ..
    } = handle
        .submit(
            principal,
            request(
                first_sequence + 2,
                Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
                    intent: intent.id(),
                    fence,
                    grid: MeasuredGrid::new(
                        37,
                        113,
                        CellPixels::new(9, 18).unwrap(),
                        SUPPORTED_GRID_LIMITS,
                    )
                    .unwrap(),
                }),
            ),
        )
        .await
        .unwrap()
    else {
        panic!("session")
    };
    let mut snapshot = handle.subscribe_snapshot();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !snapshot.borrow().runtime().iter().any(|pane| pane.pane == session.pane && matches!(pane.phase, SessionPhase::Running { generation } if generation == session.generation)) {
            snapshot.changed().await.unwrap();
        }
    }).await.unwrap();
    (session, grant)
}

#[tokio::test]
async fn last_device_unpair_keeps_server_sessions_and_organization_and_refuses_ordinary_recovery() {
    use spaceterm_core::organization::{ControlRecord, SessionPhase};
    let f = Fixture::new().await;
    let (_, enrollment) = f.enroll(42, 2).await;
    let (target, streams) = enrolled_client(&enrollment);
    let principal = f.principal(42, &streams, 1, 2);
    let (session, _) = start_session(&f, principal, streams.parts().ordinary, 1).await;
    let before = f.server.handle().snapshot();
    execute_self_revocation(&f, target.clone(), &streams)
        .await
        .unwrap();
    let after = f.server.handle().snapshot();
    assert_eq!(after.state().workspaces, before.state().workspaces);
    assert_eq!(after.state().panes, before.state().panes);
    assert_eq!(after.state().settings, before.state().settings);
    assert!(
        matches!(after.runtime()[0].phase, SessionPhase::Running { generation } if generation == session.generation)
    );
    assert_eq!(after.runtime()[0].control, ControlRecord::Unclaimed);
    assert!(f.server.handle().register(principal).await.is_err());
    let lookup = PairingProtocolLookup::new(f.server.handle());
    let p = authentication::Principal {
        server: target.parts().server.bytes(),
        device: target.parts().device.bytes(),
        pairing: native::local(target.parts().pairing.local()),
        client: native::local(streams.parts().client.id.local()),
    };
    let (_, hello) = authentication::ClientHandshake::begin(
        f.signer.pin(),
        p,
        authentication::Purpose::Ordinary,
        Default::default(),
        Arc::new(Key::new(43)),
    )
    .unwrap();
    assert!(
        authentication::ServerHandshake::accept(
            p.server,
            after.epoch().get(),
            Default::default(),
            f.signer.clone(),
            &lookup,
            &hello
        )
        .is_err()
    );
    let runtime = f.runtime.states.lock().unwrap()[0].clone();
    assert_eq!(runtime.lock().unwrap().retire_calls, 0);
    assert_eq!(f.runtime.starts.lock().unwrap().len(), 1);
    f.close().await;
}

#[tokio::test]
async fn unpair_releases_all_target_clients_exact_grants_without_ending_sessions() {
    use spaceterm_core::organization::{ControlRecord, SessionPhase};
    let f = Fixture::new().await;
    let (_, enrollment) = f.enroll(42, 2).await;
    let (target, initial) = enrolled_client(&enrollment);
    let issuer = f.principal(42, &initial, 1, 2);
    native::connect(&f.server.handle(), issuer).await;
    let bind = OperationId::new(initial.parts().ordinary, OperationSequence::new(1).unwrap());
    let (digest, _) = client_binding_claim(&target, FieldVersion::new(1), bind).unwrap();
    let (device, auth) = sign_client_binding_claim(
        &target,
        FieldVersion::new(1),
        bind,
        &Key::new(42),
        &Key::new(43),
    )
    .unwrap();
    let PairingOutcome::ClientBound(bound) = f
        .service
        .bind_additional_client(
            issuer,
            bind,
            target.clone(),
            FieldVersion::new(1),
            digest,
            (device, auth),
        )
        .await
        .unwrap()
        .outcome
    else {
        panic!("bound")
    };
    let PairingReceiptBody::ClientBinding { client: second, .. } = &bound.parts().claims else {
        panic!("client")
    };
    let first_principal = f.principal(42, &initial, 2, 2);
    let (first_session, _) = start_session(&f, first_principal, initial.parts().ordinary, 2).await;
    // The second Client has its own ordinary sequence domain.
    let second_principal = f.principal(42, second, 1, 2);
    let (second_session, _) = start_session(&f, second_principal, second.parts().ordinary, 1).await;
    let (_, other) = f.enroll(52, 2).await;
    let (_, other_streams) = enrolled_client(&other);
    let other_principal = f.principal(52, &other_streams, 1, 2);
    let (other_session, _) =
        start_session(&f, other_principal, other_streams.parts().ordinary, 1).await;
    let before = f.server.handle().snapshot();
    execute_self_revocation(&f, target, &initial).await.unwrap();
    let after = f.server.handle().snapshot();
    assert_eq!(after.state().workspaces, before.state().workspaces);
    assert_eq!(after.state().panes, before.state().panes);
    for pane in after.runtime() {
        assert!(matches!(pane.phase, SessionPhase::Running { .. }));
        if [first_session.pane, second_session.pane].contains(&pane.pane) {
            let old = before
                .runtime()
                .iter()
                .find(|old| old.pane == pane.pane)
                .unwrap();
            assert_eq!(pane.control, ControlRecord::Unclaimed);
            assert_eq!(
                pane.control_generation,
                old.control_generation.checked_successor().unwrap()
            );
        } else if pane.pane == other_session.pane {
            assert_eq!(
                pane,
                before
                    .runtime()
                    .iter()
                    .find(|old| old.pane == pane.pane)
                    .unwrap()
            );
        }
    }
    assert!(
        f.server
            .handle()
            .runtime_access(second_principal, second_session)
            .await
            .is_err()
    );
    assert!(
        f.server
            .handle()
            .runtime_access(other_principal, other_session)
            .await
            .is_ok()
    );
    assert_eq!(f.runtime.starts.lock().unwrap().len(), 3);
    for runtime in f.runtime.states.lock().unwrap().iter() {
        assert_eq!(runtime.lock().unwrap().retire_calls, 0);
    }
    f.close().await;
}

#[tokio::test]
async fn revocation_fences_queued_input_resize_and_native_completions_before_success() {
    use spaceterm_core::{
        geometry::*, native_requests::*, observations::MonotonicTick,
        runtime_authority::EffectDisposition,
    };
    use spaceterm_terminal::runtime_contracts::{FencedRuntimeCommand, RuntimeCommand};
    let f = Fixture::new().await;
    let (_, enrollment) = f.enroll(42, 2).await;
    let (target, streams) = enrolled_client(&enrollment);
    let principal = f.principal(42, &streams, 1, 2);
    let (session, grant) = start_session(&f, principal, streams.parts().ordinary, 1).await;
    let access = f
        .server
        .handle()
        .runtime_access(principal, session)
        .await
        .unwrap();
    let fence = EffectFence::new(
        session,
        grant,
        principal.binding(),
        access.authority.parts().context,
        None,
    )
    .unwrap();
    let input = access
        .handle
        .try_submit(FencedRuntimeCommand {
            fence,
            command: RuntimeCommand::FocusReport(true),
        })
        .unwrap();
    let resize = access
        .handle
        .try_submit(FencedRuntimeCommand {
            fence,
            command: RuntimeCommand::Resize(
                MeasuredGrid::new(
                    38,
                    114,
                    CellPixels::new(9, 18).unwrap(),
                    SUPPORTED_GRID_LIMITS,
                )
                .unwrap(),
            ),
        })
        .unwrap();
    let request = NativeRequest::new(
        fence,
        spaceterm_core::native_requests::NativeRequestId::new(1).unwrap(),
        MonotonicTick::new(1),
        MonotonicTick::new(1),
        Some(MonotonicTick::new(100)),
        NativeRequestKind::ClipboardWrite(NativeText::new(String::new()).unwrap()),
    )
    .unwrap();
    let native = access
        .handle
        .complete_native(
            NativeCompletion::new(&request, NativeCompletionKind::ClipboardWritten).unwrap(),
        )
        .unwrap();
    execute_self_revocation(&f, target, &streams).await.unwrap();
    assert_eq!(input.await.unwrap(), EffectDisposition::StaleAuthority);
    assert_eq!(resize.await.unwrap(), EffectDisposition::StaleAuthority);
    assert_eq!(native.await.unwrap(), EffectDisposition::StaleAuthority);
    assert_eq!(
        f.runtime.states.lock().unwrap()[0]
            .lock()
            .unwrap()
            .retire_calls,
        0
    );
    f.close().await;
}

#[tokio::test]
async fn mutation_admitted_before_revocation_revalidates_before_commit() {
    let f = Fixture::new().await;
    let (_, enrollment) = f.enroll(42, 2).await;
    let (target, streams) = enrolled_client(&enrollment);
    let principal = f.principal(42, &streams, 1, 2);
    native::connect(&f.server.handle(), principal).await;
    let id = OperationId::new(streams.parts().ordinary, OperationSequence::new(1).unwrap());
    let proposal = f.claim(52).proposal().clone();
    let request = CanonicalOperationRequest::new(
        id,
        Command::Pairing(PairingCommand::IssueDirectAuthorization {
            proposal: proposal.clone(),
        }),
        &CanonicalCodec,
    )
    .unwrap();
    assert!(matches!(
        f.server.handle().submit(principal, request).await.unwrap(),
        OperationResponse::Pending(_)
    ));
    execute_self_revocation(&f, target, &streams).await.unwrap();
    let before = f.server.handle().pairing_snapshot().unwrap();
    assert!(
        f.service
            .issue_direct_authorization(principal, id, proposal)
            .await
            .is_err()
    );
    let after = f.server.handle().pairing_snapshot().unwrap();
    assert_eq!(after.high_water(), before.high_water());
    assert_eq!(after.recovery(), before.recovery());
    f.close().await;
}

#[path = "support/job_b.rs"]
mod ipc;
async fn ipc_ordinary(
    f: &Fixture,
    listener: &spaceterm_host::ipc::IpcListener,
    directory: &ipc::Directory,
    authenticator: &spaceterm_server::connection::Authenticator,
    seed: u8,
    streams: &PairingClientStreams,
    major: u32,
) -> (
    spaceterm_server::connection::AuthenticatedPeer,
    spaceterm_host::ipc::IpcTransport,
) {
    use spaceterm_host::transport::MessageTransport;
    let transport = directory.connect().await;
    let pending = listener.accept().await.unwrap();
    let client = async {
        let p = authentication::Principal {
            server: f.signer.pin().identity,
            device: [seed; 16],
            pairing: native::local(streams.parts().client.pairing.local()),
            client: native::local(streams.parts().client.id.local()),
        };
        let config = authentication::HandshakeConfig {
            majors: vec![major],
            ..Default::default()
        };
        let (handshake, hello) = authentication::ClientHandshake::begin(
            f.signer.pin(),
            p,
            authentication::Purpose::Ordinary,
            config,
            Arc::new(Key::new(seed + 1)),
        )
        .unwrap();
        transport.send(&hello).await.unwrap();
        let (handshake, proof) = handshake
            .receive_server(&transport.recv().await.unwrap().unwrap())
            .unwrap();
        transport.send(&proof).await.unwrap();
        handshake
            .receive_ready(&transport.recv().await.unwrap().unwrap())
            .unwrap();
    };
    let (peer, _) = tokio::join!(
        authenticator.authenticate(pending, authenticator.try_admit().unwrap()),
        client
    );
    (peer.unwrap(), transport)
}

#[tokio::test]
async fn unpair_closes_every_target_connection_and_only_target_connections() {
    use spaceterm_host::transport::MessageTransport;
    use spaceterm_server::connection::*;
    for major in [1, 2] {
        let f = Fixture::new().await;
        let (_, enrollment) = f.enroll(42, major).await;
        let (target, first) = enrolled_client(&enrollment);
        let directory = ipc::Directory::new();
        let listener = directory.listener();
        let probe = directory.connect().await;
        let pending = listener.accept().await.unwrap();
        let uid = pending.peer_facts().local_uid.unwrap();
        probe.close().await.unwrap();
        pending.close().await.unwrap();
        let authenticator = Authenticator::new_with_enrollment(
            AuthenticationConfig {
                server: physical::identity().server,
                epoch: f.server.handle().snapshot().epoch(),
                owning_uid: uid,
                handshake: Default::default(),
                credentials: f.signer.clone(),
                pairing: Arc::new(PairingProtocolLookup::new(f.server.handle())),
                authority: Arc::new(f.server.handle()),
                unauthenticated_limit: 16,
                authenticated_limit: 32,
                deadline: std::time::Duration::from_secs(5),
            },
            Arc::new(PairingProtocolLookup::new(f.server.handle())),
        )
        .unwrap();
        let (first_peer, first_transport) =
            ipc_ordinary(&f, &listener, &directory, &authenticator, 42, &first, major).await;
        let PeerConnection::Ordinary(session) = &first_peer.connection else {
            panic!("ordinary")
        };
        let issuer = session.principal;
        f.server
            .handle()
            .reconcile(issuer, f.server.handle().snapshot().state().revision)
            .await
            .unwrap();
        let id = OperationId::new(first.parts().ordinary, OperationSequence::new(1).unwrap());
        let (digest, _) = client_binding_claim(&target, FieldVersion::new(1), id).unwrap();
        let (device, auth) = sign_client_binding_claim(
            &target,
            FieldVersion::new(1),
            id,
            &Key::new(42),
            &Key::new(43),
        )
        .unwrap();
        let PairingOutcome::ClientBound(proof) = f
            .service
            .bind_additional_client(
                issuer,
                id,
                target.clone(),
                FieldVersion::new(1),
                digest,
                (device, auth),
            )
            .await
            .unwrap()
            .outcome
        else {
            panic!("binding")
        };
        let PairingReceiptBody::ClientBinding { client: second, .. } = &proof.parts().claims else {
            panic!("client")
        };
        let (_, other_enrollment) = f.enroll(52, major).await;
        let (_, other) = enrolled_client(&other_enrollment);
        let (second_peer, second_transport) =
            ipc_ordinary(&f, &listener, &directory, &authenticator, 42, second, major).await;
        let (other_peer, other_transport) =
            ipc_ordinary(&f, &listener, &directory, &authenticator, 52, &other, major).await;
        execute_self_revocation(&f, target, &first).await.unwrap();
        assert!(*first_peer.closing.borrow());
        assert!(*second_peer.closing.borrow());
        assert!(!*other_peer.closing.borrow());
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), first_transport.recv())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), second_transport.recv())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        let PeerConnection::Ordinary(other_session) = &other_peer.connection else {
            panic!("ordinary")
        };
        authenticator
            .check_current(other_session.principal)
            .unwrap();
        first_peer.close().await.unwrap();
        second_peer.close().await.unwrap();
        other_peer.close().await.unwrap();
        other_transport.close().await.unwrap();
        authenticator.close().await.unwrap();
        f.close().await;
    }
}

#[tokio::test]
async fn enrollment_reserves_revocation_capacity_before_commit() {
    use spaceterm_host::storage::testing::FaultPoint;
    let f = Fixture::new().await;
    let execution = f.issue(42).await;
    let PairingOutcome::AuthorizationReserved { ticket, .. } = execution.outcome else {
        panic!("authorization")
    };
    let state = f.server.handle().pairing_snapshot().unwrap();
    let reservation = state.reservation(&ticket).unwrap().clone();
    let (mut client, mut server) = f.channel(&reservation, 42, 2);
    let submit = client
        .submission_message(execution.authorization.as_ref().unwrap(), &Key::new(42))
        .unwrap();
    let submission = server.receive_submission(&submit).unwrap();
    let gate = native::StoreGate::new();
    let held = gate.clone();
    let final_commit = f.faults.clone();
    f.faults.barrier(FaultPoint::AfterCommit, move || {
        final_commit.barrier(FaultPoint::BeforeCommit, move || held.block());
    });
    let service = f.service.clone();
    let task = tokio::spawn(async move { service.enroll(submission).await });
    gate.entered().await;
    assert!(!task.is_finished());
    let before = f.server.handle().pairing_snapshot().unwrap();
    assert!(before.recovery().parts().authorizations.is_empty());
    assert_eq!(
        before.reservation(&ticket).unwrap().parts().state.kind(),
        EnrollmentReservationKind::Reserved
    );
    gate.release();
    let PairingOutcome::Enrolled(receipt) = task.await.unwrap().unwrap().outcome else {
        panic!("enrolled")
    };
    let (_, streams) = enrolled_client(&receipt);
    let directory = f.close().await;
    let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
    let header = recovered
        .ledger
        .unwrap()
        .streams()
        .iter()
        .find(|stream| stream.binding.stream() == streams.parts().self_revocation)
        .unwrap()
        .clone();
    assert_eq!(header.purpose, StreamPurpose::SelfRevocation);
    assert_eq!(
        header.dormant_reservation,
        Some(OutcomeReservation::full(
            ReservationClass::DedicatedSelfRevocation
        ))
    );
}

#[tokio::test]
async fn enrollment_acknowledgement_compacts_only_original_registered_stream_after_expiry() {
    let f = Fixture::new().await;
    let (reservation, enrollment) = f.enroll(42, 2).await;
    let (mut client, mut server) = f.channel(&reservation, 42, 2);
    f.clock.0.store(301001, std::sync::atomic::Ordering::SeqCst);
    let binding = Fixture::enrollment_binding(&reservation);
    f.server
        .handle()
        .enrollment_acknowledge(
            binding,
            reservation.parts().ticket.clone(),
            reservation.parts().claim_digest,
        )
        .await
        .unwrap();
    let frame = client.lookup_message().unwrap();
    let response = f
        .service
        .enrollment_frame(&mut server, &frame)
        .await
        .unwrap();
    let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
        client.decode(&response).unwrap().kind.unwrap()
    else {
        panic!("result")
    };
    assert_eq!(
        ReceiptCodec::new(f.signer.pin())
            .from_wire(&result.receipt.unwrap())
            .unwrap(),
        enrollment
    );
    let directory = f.close().await;
    let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
    let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
        physical::identity().server,
        recovered.ledger.unwrap(),
    )
    .unwrap();
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest)
            )
            .unwrap(),
        OperationLookup::AcknowledgedRetired
    );
    let (_, streams) = enrolled_client(&enrollment);
    assert_eq!(
        ledger
            .stream(&StreamBinding::from_stream(streams.parts().ordinary))
            .unwrap()
            .admitted_through,
        0
    );
    assert_eq!(
        ledger
            .stream(&StreamBinding::from_stream(streams.parts().self_revocation))
            .unwrap()
            .dormant_reservation,
        Some(OutcomeReservation::full(
            ReservationClass::DedicatedSelfRevocation
        ))
    );
}

#[tokio::test]
async fn restricted_acknowledgement_crash_preserves_receipt_and_enrollment_result() {
    use spaceterm_host::storage::testing::FaultPoint;
    use std::os::unix::process::ExitStatusExt;
    const DIRECTORY: &str = "SPACETERM_TEST_PAIRING_ACK_CRASH_DIRECTORY";
    if let Some(path) = std::env::var_os(DIRECTORY) {
        let directory = physical::Directory(path.into());
        let store = physical::reopen(&directory.0).unwrap();
        let f = Fixture::open(directory, store).await;
        let state = f.server.handle().pairing_snapshot().unwrap();
        let EnrollmentReservationState::Consumed { receipt, .. } =
            &state.recovery().parts().enrollments[0].parts().state
        else {
            panic!("consumed")
        };
        let (target, streams) = enrolled_client(receipt);
        let request = self_request(target.clone(), &streams);
        kill_on_commit(f.faults.clone(), FaultPoint::AfterCommit, 1);
        f.server
            .handle()
            .self_revocation_acknowledge(request.binding(), target)
            .await
            .unwrap();
        panic!("crash barrier did not fire");
    }
    let f = Fixture::new().await;
    let (reservation, enrollment) = f.enroll(42, 2).await;
    let (target, streams) = enrolled_client(&enrollment);
    let receipt = execute_self_revocation(&f, target.clone(), &streams)
        .await
        .unwrap();
    let directory = f.close().await;
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "restricted_acknowledgement_crash_preserves_receipt_and_enrollment_result",
            "--nocapture",
        ])
        .env(DIRECTORY, &directory.0)
        .output()
        .unwrap();
    assert_eq!(
        child.status.signal(),
        Some(9),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
    assert_eq!(
        recovered.pairing.as_ref().unwrap().parts().authorizations[0]
            .parts()
            .revocation_receipt
            .as_ref(),
        Some(&receipt)
    );
    let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
        physical::identity().server,
        recovered.ledger.unwrap(),
    )
    .unwrap();
    let binding = self_request(target, &streams).binding();
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest)
            )
            .unwrap(),
        OperationLookup::AcknowledgedRetired
    );
    let original = Fixture::enrollment_binding(&reservation);
    assert!(
        matches!(ledger.lookup(&StreamBinding::from_stream(original.id.stream()), original.id, Some(original.digest)).unwrap(), OperationLookup::Completed { result: Outcome::Pairing(outcome), .. } if *outcome == PairingOutcome::Enrolled(enrollment))
    );
}

#[tokio::test]
async fn already_revoked_by_another_device_returns_original_target_receipt_without_admitting_restricted_operation()
 {
    let f = Fixture::new().await;
    let (_, enrollment) = f.enroll(42, 2).await;
    let (target, target_streams) = enrolled_client(&enrollment);
    let (_, initiating) = f.enroll(52, 2).await;
    let (_, streams) = enrolled_client(&initiating);
    let principal = f.principal(52, &streams, 1, 2);
    native::connect(&f.server.handle(), principal).await;
    let assessment_id =
        OperationId::new(streams.parts().ordinary, OperationSequence::new(1).unwrap());
    let PairingOutcome::UnpairAssessed(assessment) = f
        .service
        .assess_unpair(principal, assessment_id, target.clone())
        .await
        .unwrap()
        .outcome
    else {
        panic!("assessment")
    };
    let confirm = OperationId::new(streams.parts().ordinary, OperationSequence::new(2).unwrap());
    let binding = CanonicalOperationRequest::new(
        confirm,
        Command::Pairing(PairingCommand::UnpairConfirmed {
            assessment: assessment.clone(),
        }),
        &CanonicalCodec,
    )
    .unwrap()
    .binding();
    let PairingOutcome::Revoked(receipt) = f
        .service
        .unpair(principal, confirm, assessment.clone())
        .await
        .unwrap()
        .outcome
    else {
        panic!("revoked")
    };
    let self_binding = self_request(target.clone(), &target_streams).binding();
    assert_eq!(
        f.server
            .handle()
            .self_revocation_status(self_binding, target.clone())
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    f.server
        .handle()
        .self_revocation_acknowledge(self_binding, target)
        .await
        .unwrap();
    assert!(
        matches!(f.server.handle().lookup_state(principal, binding).await.unwrap(), OperationLookup::Completed { result: Outcome::Pairing(outcome), .. } if *outcome == PairingOutcome::Revoked(receipt.clone()))
    );
    assert_eq!(
        f.service
            .unpair(principal, confirm, assessment)
            .await
            .unwrap()
            .outcome,
        PairingOutcome::Revoked(receipt.clone())
    );
    let directory = f.close().await;
    let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
    let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
        physical::identity().server,
        recovered.ledger.unwrap(),
    )
    .unwrap();
    assert_eq!(
        ledger
            .stream(&StreamBinding::from_stream(self_binding.id.stream()))
            .unwrap()
            .admitted_through,
        0
    );
    assert!(
        matches!(ledger.lookup(&StreamBinding::from_stream(binding.id.stream()), binding.id, Some(binding.digest)).unwrap(), OperationLookup::Completed { result: Outcome::Pairing(outcome), .. } if *outcome == PairingOutcome::Revoked(receipt))
    );
}

#[tokio::test]
async fn queued_ordinary_pairing_operations_cannot_outlive_their_connection() {
    use spaceterm_server::{
        pairing::dispatch::PairingOrdinaryDispatch,
        protocol_dispatch::{ActorOrdinaryDispatch, OrdinaryDispatchPort},
    };
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    for major in [1, 2] {
        for superseded in [false, true] {
            for action in ["issue", "bind", "confirm"] {
                let f = Fixture::new().await;
                let (_, enrollment) = f.enroll(42, major).await;
                let (target, streams) = enrolled_client(&enrollment);
                let (_, enrollment) = f.enroll(52, major).await;
                let (other, _) = enrolled_client(&enrollment);
                let principal = f.principal(42, &streams, 1, major);
                native::connect(&f.server.handle(), principal).await;
                let assess =
                    OperationId::new(streams.parts().ordinary, OperationSequence::new(1).unwrap());
                let PairingOutcome::UnpairAssessed(assessment) = f
                    .service
                    .assess_unpair(principal, assess, other)
                    .await
                    .unwrap()
                    .outcome
                else {
                    panic!("assessed")
                };
                let id =
                    OperationId::new(streams.parts().ordinary, OperationSequence::new(2).unwrap());
                let (claim_digest, _) =
                    client_binding_claim(&target, FieldVersion::new(1), id).unwrap();
                let proofs = sign_client_binding_claim(
                    &target,
                    FieldVersion::new(1),
                    id,
                    &Key::new(42),
                    &Key::new(43),
                )
                .unwrap();
                let command = match action {
                    "issue" => PairingCommand::IssueDirectAuthorization {
                        proposal: f.claim(62).proposal().clone(),
                    },
                    "bind" => PairingCommand::BindAdditionalClient {
                        target,
                        expected_authorization_version: FieldVersion::new(1),
                        claim_digest,
                    },
                    "confirm" => PairingCommand::UnpairConfirmed { assessment },
                    _ => unreachable!(),
                };
                let request =
                    CanonicalOperationRequest::new(id, Command::Pairing(command), &CanonicalCodec)
                        .unwrap();
                let before = f.server.handle().pairing_snapshot().unwrap();
                let handle = f.server.handle();
                let dispatcher = PairingOrdinaryDispatch::new(
                    f.service.clone(),
                    Arc::new(ActorOrdinaryDispatch::new(Arc::new(CanonicalCodec))),
                );
                let (
                    authentication::Established::Ordinary(mut client),
                    authentication::Established::Ordinary(mut server),
                ) = f.handshake(42, &streams, authentication::Purpose::Ordinary, major)
                else {
                    panic!("ordinary")
                };
                let payload = server
                    .decode(
                        &client
                            .encode(&pairing_wire_request(major, &request, Some(proofs)))
                            .unwrap(),
                    )
                    .unwrap();
                let mut execute = dispatcher.dispatch(principal, payload, handle.clone());
                let mut disconnect = Box::pin(handle.disconnect(principal));
                let replacement = f.principal(42, &streams, 2, major);
                let mut register = Box::pin(handle.register(replacement));
                let mut context = Context::from_waker(Waker::noop());
                // Queue without yielding to the actor. Disconnect has reserved,
                // prioritized ingress; a replacement is ordered ahead of Execute.
                if superseded {
                    assert!(matches!(
                        register.as_mut().poll(&mut context),
                        Poll::Pending
                    ));
                }
                assert!(matches!(execute.as_mut().poll(&mut context), Poll::Pending));
                if superseded {
                    register.await.unwrap();
                } else {
                    assert!(matches!(
                        disconnect.as_mut().poll(&mut context),
                        Poll::Pending
                    ));
                    disconnect.await.unwrap();
                }
                assert!(
                    execute.await.is_err(),
                    "inert connection must not admit {action}"
                );
                let after = handle.pairing_snapshot().unwrap();
                assert_eq!(after.high_water(), before.high_water());
                assert_eq!(after.recovery(), before.recovery());
                assert_eq!(after.revision(), before.revision());
                if superseded {
                    handle
                        .reconcile(replacement, handle.snapshot().state().revision)
                        .await
                        .unwrap();
                } else {
                    native::connect(&handle, replacement).await;
                }
                assert_eq!(
                    handle
                        .lookup_state(replacement, request.binding())
                        .await
                        .unwrap(),
                    OperationLookup::NeverAdmitted
                );
                f.close().await;
            }
        }
    }
}

fn pairing_wire_request(
    major: u32,
    request: &CanonicalOperationRequest,
    proofs: Option<([u8; 64], [u8; 64])>,
) -> spaceterm_protocol::codec::Payload {
    use spaceterm_protocol::{
        codec::Payload,
        convert::identity,
        wire::{pairing as wire, v1, v2},
    };
    let binding = request.binding();
    macro_rules! profile {
        ($p:ident, $major:ident) => {{
            let action = match request.command() {
                Command::Pairing(PairingCommand::IssueDirectAuthorization { proposal }) => {
                    $p::pairing_request::Action::IssueAuthorization(wire::IssueAuthorization {
                        proposal: Some(proposal_to_wire(proposal)),
                    })
                }
                Command::Pairing(PairingCommand::AssessUnpair { target }) => {
                    $p::pairing_request::Action::AssessUnpair(wire::AssessUnpair {
                        target: Some(target_to_wire(target)),
                    })
                }
                Command::Pairing(PairingCommand::UnpairConfirmed { assessment }) => {
                    $p::pairing_request::Action::UnpairConfirmed(wire::UnpairConfirmed {
                        assessment: Some(wire::UnpairAssessment {
                            assessment_operation: Some(operation_to_wire(
                                assessment.parts().reference.operation,
                            )),
                            assessment_digest: assessment.parts().reference.digest.bytes().to_vec(),
                            target: Some(target_to_wire(&assessment.parts().target)),
                            authorization_version: assessment.parts().authorization_version.get(),
                            clients: assessment
                                .parts()
                                .clients
                                .iter()
                                .map(|client| identity::encode_client(client.id))
                                .collect(),
                        }),
                    })
                }
                Command::Pairing(PairingCommand::BindAdditionalClient {
                    target,
                    expected_authorization_version,
                    claim_digest,
                }) => {
                    let (device, auth) = proofs.unwrap();
                    $p::pairing_request::Action::BindAdditionalClient(wire::BindAdditionalClient {
                        target: Some(target_to_wire(target)),
                        expected_authorization_version: expected_authorization_version.get(),
                        claim_digest: claim_digest.bytes().to_vec(),
                        device_identity_proof: device.to_vec(),
                        pairing_authentication_proof: auth.to_vec(),
                    })
                }
                _ => panic!("ordinary pairing action"),
            };
            Payload::$major($p::Message {
                kind: Some($p::message::Kind::Operation($p::Operation {
                    id: Some(identity::encode_operation(binding.id)),
                    action: Some($p::operation::Action::IdentifiedPairing(
                        $p::PairingRequest {
                            request: 0,
                            operation: Some(operation_to_wire(binding.id)),
                            action: Some(action),
                            ..Default::default()
                        },
                    )),
                })),
            })
        }};
    }
    match major {
        1 => profile!(v1, Major1),
        2 => profile!(v2, Major2),
        _ => panic!("major"),
    }
}
fn assessment_from_wire(
    value: &spaceterm_protocol::wire::pairing::UnpairAssessment,
) -> UnpairAssessment {
    use spaceterm_protocol::convert::identity;
    let target = target_from_wire(value.target.as_ref().unwrap()).unwrap();
    UnpairAssessment::new(UnpairAssessmentParts {
        reference: AssessmentRef {
            operation: operation_from_wire(value.assessment_operation.as_ref().unwrap()).unwrap(),
            digest: RequestDigest::from_bytes(
                value.assessment_digest.as_slice().try_into().unwrap(),
            ),
        },
        target: target.clone(),
        authorization_version: FieldVersion::new(value.authorization_version),
        clients: value
            .clients
            .iter()
            .map(|client| ClientRecord {
                id: identity::decode_client(client).unwrap(),
                pairing: target.parts().pairing,
            })
            .collect(),
    })
    .unwrap()
}
struct PairingWireOutcome {
    operation: OperationId,
    revision: u64,
    disposition: u32,
    failure: i32,
    assessment: Option<UnpairAssessment>,
}
fn pairing_wire_outcome(payload: spaceterm_protocol::codec::Payload) -> PairingWireOutcome {
    use spaceterm_protocol::{
        codec::Payload,
        wire::{v1, v2},
    };
    macro_rules! profile {
        ($message:expr, $p:ident) => {{
            let outcome = match $message.kind.unwrap() {
                $p::message::Kind::Outcome(outcome) => outcome,
                $p::message::Kind::OperationLookupResult(result) => {
                    match result.disposition.unwrap() {
                        $p::operation_lookup_result::Disposition::Completed(outcome) => *outcome,
                        _ => panic!("completed lookup"),
                    }
                }
                _ => panic!("outcome"),
            };
            let $p::outcome::Result::PairingResult(result) = outcome.result.unwrap() else {
                panic!("pairing result")
            };
            assert!(result.authorization_delivery.is_none());
            PairingWireOutcome {
                operation: operation_from_wire(outcome.operation.as_ref().unwrap()).unwrap(),
                revision: outcome.publication_revision,
                disposition: result.disposition,
                failure: result.classified_failure,
                assessment: result
                    .unpair_assessment
                    .as_deref()
                    .map(assessment_from_wire),
            }
        }};
    }
    match payload {
        Payload::Major1(message) => profile!(message, v1),
        Payload::Major2(message) => profile!(message, v2),
    }
}
async fn dispatch_pairing_payload(
    f: &Fixture,
    seed: u8,
    streams: &PairingClientStreams,
    principal: VerifiedPrincipal,
    major: u32,
    payload: spaceterm_protocol::codec::Payload,
) -> Result<spaceterm_protocol::codec::Payload, ServerFailure> {
    use spaceterm_server::{
        pairing::dispatch::PairingOrdinaryDispatch,
        protocol_dispatch::{ActorOrdinaryDispatch, OrdinaryDispatchPort},
    };
    let (
        authentication::Established::Ordinary(mut client),
        authentication::Established::Ordinary(mut server),
    ) = f.handshake(seed, streams, authentication::Purpose::Ordinary, major)
    else {
        panic!("ordinary")
    };
    let request = server.decode(&client.encode(&payload).unwrap()).unwrap();
    let dispatcher = PairingOrdinaryDispatch::new(
        f.service.clone(),
        Arc::new(ActorOrdinaryDispatch::new(Arc::new(CanonicalCodec))),
    );
    let response = dispatcher
        .dispatch(principal, request, f.server.handle())
        .await?
        .unwrap();
    Ok(client.decode(&server.encode(&response).unwrap()).unwrap())
}
fn pairing_lookup_payload(
    major: u32,
    binding: OperationBinding,
) -> spaceterm_protocol::codec::Payload {
    use spaceterm_protocol::{
        codec::Payload,
        wire::{v1, v2},
    };
    macro_rules! profile {
        ($p:ident, $major:ident) => {
            Payload::$major($p::Message {
                kind: Some($p::message::Kind::OperationLookupRequest(
                    $p::OperationLookupRequest {
                        operation: Some(operation_to_wire(binding.id)),
                        request_digest: binding.digest.bytes().to_vec(),
                    },
                )),
            })
        };
    }
    match major {
        1 => profile!(v1, Major1),
        2 => profile!(v2, Major2),
        _ => panic!("major"),
    }
}
#[tokio::test]
async fn acknowledged_unpair_assessment_remains_valid_in_both_majors() {
    for major in [1, 2] {
        let f = Fixture::new().await;
        let (_, target_receipt) = f.enroll(42, major).await;
        let (target, _) = enrolled_client(&target_receipt);
        let (_, enrollment) = f.enroll(52, major).await;
        let (_, streams) = enrolled_client(&enrollment);
        let principal = f.principal(52, &streams, 1, major);
        native::connect(&f.server.handle(), principal).await;
        let request = |sequence, command| {
            CanonicalOperationRequest::new(
                OperationId::new(
                    streams.parts().ordinary,
                    OperationSequence::new(sequence).unwrap(),
                ),
                Command::Pairing(command),
                &CanonicalCodec,
            )
            .unwrap()
        };
        let assess = request(
            1,
            PairingCommand::AssessUnpair {
                target: target.clone(),
            },
        );
        let outcome = pairing_wire_outcome(
            dispatch_pairing_payload(
                &f,
                52,
                &streams,
                principal,
                major,
                pairing_wire_request(major, &assess, None),
            )
            .await
            .unwrap(),
        );
        assert_eq!(outcome.operation, assess.binding().id);
        assert_eq!(outcome.disposition, 4);
        let assessment = outcome.assessment.unwrap();
        f.server
            .handle()
            .acknowledge(
                principal,
                streams.parts().ordinary,
                OperationSequence::new(1).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            f.server
                .handle()
                .lookup_state(principal, assess.binding())
                .await
                .unwrap(),
            OperationLookup::AcknowledgedRetired
        );
        let confirm = request(2, PairingCommand::UnpairConfirmed { assessment });
        let outcome = pairing_wire_outcome(
            dispatch_pairing_payload(
                &f,
                52,
                &streams,
                principal,
                major,
                pairing_wire_request(major, &confirm, None),
            )
            .await
            .unwrap(),
        );
        assert_eq!(outcome.operation, confirm.binding().id);
        assert_eq!(outcome.disposition, 5);
        assert_eq!(outcome.failure, 0);
        assert!(matches!(
            f.server
                .handle()
                .pairing_snapshot()
                .unwrap()
                .authorization(target.parts().pairing)
                .unwrap()
                .parts()
                .record
                .state,
            PairingState::Revoked { .. }
        ));
        let original_revision = outcome.revision;
        let directory = f.close().await;
        let store = physical::reopen(&directory.0).unwrap();
        let f = Fixture::open(directory, store).await;
        let principal = f.principal(52, &streams, 2, major);
        native::connect(&f.server.handle(), principal).await;
        let replay = pairing_wire_outcome(
            dispatch_pairing_payload(
                &f,
                52,
                &streams,
                principal,
                major,
                pairing_lookup_payload(major, confirm.binding()),
            )
            .await
            .unwrap(),
        );
        assert_eq!(replay.disposition, 5);
        assert_eq!(replay.revision, original_revision);
        f.close().await;
    }
}

#[tokio::test]
async fn deterministic_pairing_rejections_survive_lookup_reopen_and_cumulative_ack_in_both_majors()
{
    for major in [1, 2] {
        let f = Fixture::new().await;
        let (_, enrollment) = f.enroll(42, major).await;
        let (target, target_streams) = enrolled_client(&enrollment);
        let target_principal = f.principal(42, &target_streams, 1, major);
        native::connect(&f.server.handle(), target_principal).await;
        let (_, enrollment) = f.enroll(52, major).await;
        let (issuer_target, streams) = enrolled_client(&enrollment);
        let principal = f.principal(52, &streams, 1, major);
        native::connect(&f.server.handle(), principal).await;
        let request = |sequence, command| {
            CanonicalOperationRequest::new(
                OperationId::new(
                    streams.parts().ordinary,
                    OperationSequence::new(sequence).unwrap(),
                ),
                Command::Pairing(command),
                &CanonicalCodec,
            )
            .unwrap()
        };
        let assess = request(
            1,
            PairingCommand::AssessUnpair {
                target: target.clone(),
            },
        );
        let assessed = pairing_wire_outcome(
            dispatch_pairing_payload(
                &f,
                52,
                &streams,
                principal,
                major,
                pairing_wire_request(major, &assess, None),
            )
            .await
            .unwrap(),
        );
        f.server
            .handle()
            .acknowledge(
                principal,
                streams.parts().ordinary,
                OperationSequence::new(1).unwrap(),
            )
            .await
            .unwrap();
        let bind = OperationId::new(
            target_streams.parts().ordinary,
            OperationSequence::new(1).unwrap(),
        );
        let (digest, _) = client_binding_claim(&target, FieldVersion::new(1), bind).unwrap();
        let (device, auth) = sign_client_binding_claim(
            &target,
            FieldVersion::new(1),
            bind,
            &Key::new(42),
            &Key::new(43),
        )
        .unwrap();
        f.service
            .bind_additional_client(
                target_principal,
                bind,
                target.clone(),
                FieldVersion::new(1),
                digest,
                (device, auth),
            )
            .await
            .unwrap();
        let stale = request(
            2,
            PairingCommand::UnpairConfirmed {
                assessment: assessed.assessment.unwrap(),
            },
        );
        let binding_id =
            OperationId::new(streams.parts().ordinary, OperationSequence::new(3).unwrap());
        let (claim_digest, _) =
            client_binding_claim(&issuer_target, FieldVersion::new(1), binding_id).unwrap();
        let denied = request(
            3,
            PairingCommand::BindAdditionalClient {
                target: issuer_target,
                expected_authorization_version: FieldVersion::new(1),
                claim_digest,
            },
        );
        let before = f.server.handle().pairing_snapshot().unwrap();
        let mut revisions = vec![];
        for (request, expected, proofs) in
            [(&stale, 10, None), (&denied, 6, Some(([0; 64], [0; 64])))]
        {
            let result = pairing_wire_outcome(
                dispatch_pairing_payload(
                    &f,
                    52,
                    &streams,
                    principal,
                    major,
                    pairing_wire_request(major, request, proofs),
                )
                .await
                .unwrap(),
            );
            assert_eq!(result.operation, request.binding().id);
            assert_eq!(result.disposition, 7);
            assert_eq!(result.failure, expected);
            revisions.push(result.revision);
            let replay = pairing_wire_outcome(
                dispatch_pairing_payload(
                    &f,
                    52,
                    &streams,
                    principal,
                    major,
                    pairing_lookup_payload(major, request.binding()),
                )
                .await
                .unwrap(),
            );
            assert_eq!(replay.failure, expected);
            assert_eq!(replay.revision, result.revision);
        }
        let after = f.server.handle().pairing_snapshot().unwrap();
        assert_eq!(after.high_water(), before.high_water());
        assert_eq!(after.recovery(), before.recovery());
        let directory = f.close().await;
        let store = physical::reopen(&directory.0).unwrap();
        let f = Fixture::open(directory, store).await;
        let principal = f.principal(52, &streams, 2, major);
        native::connect(&f.server.handle(), principal).await;
        for ((request, expected), revision) in
            [(&stale, 10), (&denied, 6)].into_iter().zip(revisions)
        {
            let replay = pairing_wire_outcome(
                dispatch_pairing_payload(
                    &f,
                    52,
                    &streams,
                    principal,
                    major,
                    pairing_lookup_payload(major, request.binding()),
                )
                .await
                .unwrap(),
            );
            assert_eq!(replay.failure, expected);
            assert_eq!(replay.revision, revision);
        }
        let last = request(4, PairingCommand::AssessUnpair { target });
        let result = pairing_wire_outcome(
            dispatch_pairing_payload(
                &f,
                52,
                &streams,
                principal,
                major,
                pairing_wire_request(major, &last, None),
            )
            .await
            .unwrap(),
        );
        assert_eq!(result.disposition, 4);
        f.server
            .handle()
            .acknowledge(
                principal,
                streams.parts().ordinary,
                OperationSequence::new(4).unwrap(),
            )
            .await
            .unwrap();
        for request in [&stale, &denied, &last] {
            assert_eq!(
                f.server
                    .handle()
                    .lookup_state(principal, request.binding())
                    .await
                    .unwrap(),
                OperationLookup::AcknowledgedRetired
            );
        }
        f.close().await;
    }
}

#[tokio::test]
async fn ordinary_pairing_reserves_reliable_capacity_before_any_admission_in_both_majors() {
    use std::sync::atomic::Ordering;
    for major in [1, 2] {
        let f = Fixture::new().await;
        let (_, enrollment) = f.enroll(42, major).await;
        let (target, _) = enrolled_client(&enrollment);
        let (_, enrollment) = f.enroll(52, major).await;
        let (issuer_target, streams) = enrolled_client(&enrollment);
        let principal = f.principal(52, &streams, 1, major);
        native::connect(&f.server.handle(), principal).await;
        let request = |sequence, command| {
            CanonicalOperationRequest::new(
                OperationId::new(
                    streams.parts().ordinary,
                    OperationSequence::new(sequence).unwrap(),
                ),
                Command::Pairing(command),
                &CanonicalCodec,
            )
            .unwrap()
        };
        let assess = request(
            1,
            PairingCommand::AssessUnpair {
                target: target.clone(),
            },
        );
        let assessed = pairing_wire_outcome(
            dispatch_pairing_payload(
                &f,
                52,
                &streams,
                principal,
                major,
                pairing_wire_request(major, &assess, None),
            )
            .await
            .unwrap(),
        );
        let confirm = request(
            2,
            PairingCommand::UnpairConfirmed {
                assessment: assessed.assessment.unwrap(),
            },
        );
        let issue = request(
            2,
            PairingCommand::IssueDirectAuthorization {
                proposal: f.claim(62).proposal().clone(),
            },
        );
        let (claim_digest, _) =
            client_binding_claim(&issuer_target, FieldVersion::new(1), confirm.binding().id)
                .unwrap();
        let proofs = sign_client_binding_claim(
            &issuer_target,
            FieldVersion::new(1),
            confirm.binding().id,
            &Key::new(52),
            &Key::new(53),
        )
        .unwrap();
        let bind = request(
            2,
            PairingCommand::BindAdditionalClient {
                target: issuer_target,
                expected_authorization_version: FieldVersion::new(1),
                claim_digest,
            },
        );
        let before = f.server.handle().pairing_snapshot().unwrap();
        let publications = f.outcomes.publications.load(Ordering::SeqCst);
        f.outcomes.rejected.store(true, Ordering::SeqCst);
        for (request, proofs) in [(&issue, None), (&bind, Some(proofs)), (&confirm, None)] {
            let Err(error) = dispatch_pairing_payload(
                &f,
                52,
                &streams,
                principal,
                major,
                pairing_wire_request(major, request, proofs),
            )
            .await
            else {
                panic!("no outcome capacity means no admission");
            };
            assert_eq!(error.class, ServerFailureClass::Capacity);
            assert_eq!(
                f.server
                    .handle()
                    .lookup_state(principal, request.binding())
                    .await
                    .unwrap(),
                OperationLookup::NeverAdmitted
            );
            let after = f.server.handle().pairing_snapshot().unwrap();
            assert_eq!(after.high_water(), before.high_water());
            assert_eq!(after.recovery(), before.recovery());
            assert_eq!(after.revision(), before.revision());
        }
        assert_eq!(f.outcomes.publications.load(Ordering::SeqCst), publications);
        f.outcomes.rejected.store(false, Ordering::SeqCst);
        let execution = f
            .service
            .issue_direct_authorization(
                principal,
                issue.binding().id,
                f.claim(62).proposal().clone(),
            )
            .await
            .unwrap();
        assert!(matches!(
            execution.outcome,
            PairingOutcome::AuthorizationReserved { .. }
        ));
        assert_eq!(
            f.outcomes.publications.load(Ordering::SeqCst),
            publications + 1
        );
        f.close().await;
    }
}

#[tokio::test]
async fn ordinary_pairing_revalidates_connection_after_durable_admission_before_planning() {
    use spaceterm_host::storage::testing::FaultPoint;
    for major in [1, 2] {
        let f = Fixture::new().await;
        let (_, enrollment) = f.enroll(42, major).await;
        let (_, streams) = enrolled_client(&enrollment);
        let principal = f.principal(42, &streams, 1, major);
        native::connect(&f.server.handle(), principal).await;
        let proposal = f.claim(52).proposal().clone();
        let request = CanonicalOperationRequest::new(
            OperationId::new(streams.parts().ordinary, OperationSequence::new(1).unwrap()),
            Command::Pairing(PairingCommand::IssueDirectAuthorization {
                proposal: proposal.clone(),
            }),
            &CanonicalCodec,
        )
        .unwrap();
        let before = f.server.handle().pairing_snapshot().unwrap();
        let gate = native::StoreGate::new();
        let held = gate.clone();
        f.faults
            .barrier(FaultPoint::AfterCommit, move || held.block());
        let service = f.service.clone();
        let operation = request.binding().id;
        let execute = tokio::spawn(async move {
            service
                .issue_direct_authorization(principal, operation, proposal)
                .await
        });
        gate.entered().await;
        f.server.handle().disconnect(principal).await.unwrap();
        gate.release();
        assert!(matches!(
            execute.await.unwrap().unwrap().outcome,
            PairingOutcome::Failed(PairingFailure::StaleAssessment)
        ));
        let after = f.server.handle().pairing_snapshot().unwrap();
        assert_eq!(after.high_water(), before.high_water());
        assert_eq!(after.recovery(), before.recovery());
        let replacement = f.principal(42, &streams, 2, major);
        native::connect(&f.server.handle(), replacement).await;
        assert!(
            matches!(f.server.handle().lookup_state(replacement,request.binding()).await.unwrap(),OperationLookup::Completed{result:Outcome::Pairing(outcome),..} if *outcome==PairingOutcome::Failed(PairingFailure::StaleAssessment))
        );
        f.close().await;
    }
}

#[tokio::test]
async fn acknowledged_unpair_assessments_reject_forged_expired_and_restarted_comparisons() {
    for major in [1, 2] {
        for invalidation in ["forged", "expired", "restart"] {
            let f = Fixture::new().await;
            let (_, enrollment) = f.enroll(42, major).await;
            let (target, _) = enrolled_client(&enrollment);
            let (_, enrollment) = f.enroll(52, major).await;
            let (_, streams) = enrolled_client(&enrollment);
            let principal = f.principal(52, &streams, 1, major);
            native::connect(&f.server.handle(), principal).await;
            let operation =
                OperationId::new(streams.parts().ordinary, OperationSequence::new(1).unwrap());
            let PairingOutcome::UnpairAssessed(mut assessment) = f
                .service
                .assess_unpair(principal, operation, target.clone())
                .await
                .unwrap()
                .outcome
            else {
                panic!("assessment")
            };
            f.server
                .handle()
                .acknowledge(
                    principal,
                    streams.parts().ordinary,
                    OperationSequence::new(1).unwrap(),
                )
                .await
                .unwrap();
            let f = match invalidation {
                "forged" => {
                    let mut parts = assessment.parts().clone();
                    parts.reference.digest = RequestDigest::from_bytes([0; 32]);
                    assessment = UnpairAssessment::new(parts).unwrap();
                    f
                }
                "expired" => {
                    f.clock.0.store(61_000, std::sync::atomic::Ordering::SeqCst);
                    f
                }
                "restart" => {
                    let directory = f.close().await;
                    let store = physical::reopen(&directory.0).unwrap();
                    Fixture::open(directory, store).await
                }
                _ => unreachable!(),
            };
            let principal = if invalidation == "restart" {
                let principal = f.principal(52, &streams, 2, major);
                native::connect(&f.server.handle(), principal).await;
                principal
            } else {
                principal
            };
            let before = f.server.handle().pairing_snapshot().unwrap();
            let request = CanonicalOperationRequest::new(
                OperationId::new(streams.parts().ordinary, OperationSequence::new(2).unwrap()),
                Command::Pairing(PairingCommand::UnpairConfirmed { assessment }),
                &CanonicalCodec,
            )
            .unwrap();
            let result = pairing_wire_outcome(
                dispatch_pairing_payload(
                    &f,
                    52,
                    &streams,
                    principal,
                    major,
                    pairing_wire_request(major, &request, None),
                )
                .await
                .unwrap(),
            );
            assert_eq!(result.disposition, 7);
            assert_eq!(result.failure, 10);
            let after = f.server.handle().pairing_snapshot().unwrap();
            assert_eq!(after.high_water(), before.high_water());
            assert_eq!(after.recovery(), before.recovery());
            f.close().await;
        }
    }
}

#[tokio::test]
async fn acknowledged_unpair_comparison_capacity_is_bounded_and_expires() {
    let f = Fixture::new().await;
    let (_, enrollment) = f.enroll(42, 2).await;
    let (target, streams) = enrolled_client(&enrollment);
    let principal = f.principal(42, &streams, 1, 2);
    native::connect(&f.server.handle(), principal).await;
    for sequence in 1..=17 {
        let operation = OperationId::new(
            streams.parts().ordinary,
            OperationSequence::new(sequence).unwrap(),
        );
        let result = f
            .service
            .assess_unpair(principal, operation, target.clone())
            .await
            .unwrap();
        if sequence == 17 {
            assert_eq!(
                result.outcome,
                PairingOutcome::Failed(PairingFailure::LimitExceeded)
            );
        } else {
            assert!(matches!(result.outcome, PairingOutcome::UnpairAssessed(_)));
        }
        f.server
            .handle()
            .acknowledge(
                principal,
                streams.parts().ordinary,
                OperationSequence::new(sequence).unwrap(),
            )
            .await
            .unwrap();
    }
    f.clock.0.store(61_000, std::sync::atomic::Ordering::SeqCst);
    let operation = OperationId::new(
        streams.parts().ordinary,
        OperationSequence::new(18).unwrap(),
    );
    assert!(matches!(
        f.service
            .assess_unpair(principal, operation, target)
            .await
            .unwrap()
            .outcome,
        PairingOutcome::UnpairAssessed(_)
    ));
    f.close().await;
}

#[tokio::test]
async fn pairing_rejection_commit_faults_preserve_pending_or_seal_admission() {
    use spaceterm_host::storage::testing::FaultPoint;
    for indeterminate in [false, true] {
        let f = Fixture::new().await;
        let (_, enrollment) = f.enroll(42, 2).await;
        let (target, streams) = enrolled_client(&enrollment);
        let principal = f.principal(42, &streams, 1, 2);
        native::connect(&f.server.handle(), principal).await;
        let operation =
            OperationId::new(streams.parts().ordinary, OperationSequence::new(1).unwrap());
        let (claim_digest, _) =
            client_binding_claim(&target, FieldVersion::new(1), operation).unwrap();
        let request = CanonicalOperationRequest::new(
            operation,
            Command::Pairing(PairingCommand::BindAdditionalClient {
                target: target.clone(),
                expected_authorization_version: FieldVersion::new(1),
                claim_digest,
            }),
            &CanonicalCodec,
        )
        .unwrap();
        let next = f.faults.clone();
        f.faults.barrier(FaultPoint::AfterCommit, move || {
            next.fail_once(
                if indeterminate {
                    FaultPoint::DatabaseSync
                } else {
                    FaultPoint::BeforeCommit
                },
                0,
            )
        });
        let drain = native::Gate::new();
        if indeterminate {
            *f.external.gate.lock().unwrap() = Some(drain.clone());
        }
        let result = f
            .service
            .bind_additional_client(
                principal,
                operation,
                target.clone(),
                FieldVersion::new(1),
                claim_digest,
                ([0; 64], [0; 64]),
            )
            .await;
        assert!(!f.faults.fault_pending());
        if indeterminate {
            assert!(matches!(result, Err(PairingFailure::RecoveryRequired)));
            assert_eq!(f.server.handle().phase(), ServicePhase::RecoveryRequired);
            let calls = f.faults.sql_calls();
            assert!(
                f.service
                    .bind_additional_client(
                        principal,
                        operation,
                        target,
                        FieldVersion::new(1),
                        claim_digest,
                        ([0; 64], [0; 64])
                    )
                    .await
                    .is_err()
            );
            assert_eq!(f.faults.sql_calls(), calls);
            drain.release();
            let directory = f.directory;
            let _ = f.server.wait().await;
            let recovered = physical::reopen(&directory.0).unwrap().recover().unwrap();
            assert_eq!(
                recovered.pairing.unwrap().parts().authorizations[0]
                    .parts()
                    .clients
                    .len(),
                1
            );
            let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
                physical::identity().server,
                recovered.ledger.unwrap(),
            )
            .unwrap();
            assert!(matches!(
                ledger
                    .lookup(
                        &StreamBinding::from_stream(operation.stream()),
                        operation,
                        Some(request.binding().digest)
                    )
                    .unwrap(),
                OperationLookup::Pending
                    | OperationLookup::Completed {
                        result: Outcome::Pairing(_),
                        ..
                    }
            ));
        } else {
            assert!(matches!(result, Err(PairingFailure::StorageUnavailable)));
            assert_eq!(
                f.server
                    .handle()
                    .lookup_state(principal, request.binding())
                    .await
                    .unwrap(),
                OperationLookup::Pending
            );
            assert_eq!(
                f.service
                    .bind_additional_client(
                        principal,
                        operation,
                        target,
                        FieldVersion::new(1),
                        claim_digest,
                        ([0; 64], [0; 64])
                    )
                    .await
                    .unwrap()
                    .outcome,
                PairingOutcome::Failed(PairingFailure::AuthorizationDenied)
            );
            f.server
                .handle()
                .acknowledge(
                    principal,
                    streams.parts().ordinary,
                    OperationSequence::new(1).unwrap(),
                )
                .await
                .unwrap();
            f.close().await;
        }
    }
}
