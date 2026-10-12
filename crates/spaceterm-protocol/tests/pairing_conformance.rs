use prost::Message;
use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::authority::{ServerEpoch, StoreRevision};
use spaceterm_core::identity::*;
use spaceterm_core::operations::*;
use spaceterm_core::pairing::*;
use spaceterm_protocol::ProtocolError;
use spaceterm_protocol::authentication::{ServerPin, SigningKey};
use spaceterm_protocol::pairing_adapter::{
    EnrollmentClient, EnrollmentConnection, EnrollmentLookup, EnrollmentServer,
    PublicEnrollmentReservation, ReceiptCodec, client_binding_claim, reservation_to_wire,
    sign_client_binding_claim, verify_client_binding_claim,
};
use spaceterm_protocol::pairing_adapter::{operation_to_wire, receipt_to_wire, target_to_wire};
use std::sync::Arc;

struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Arc<Self> {
        Arc::new(Self(
            Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap(),
        ))
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
fn stream(server: ServerId, local: u64) -> OperationStreamId {
    OperationStreamId::new(
        OperationStreamKey::new(server, local).unwrap(),
        PairingId::new(server, 1).unwrap(),
        ClientId::new(server, 1).unwrap(),
    )
    .unwrap()
}
fn reservation(device: &dyn SigningKey, auth: &dyn SigningKey) -> EnrollmentReservation {
    let server = ServerId::new([8; 16]);
    EnrollmentReservation::new(EnrollmentReservationParts {
        ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
            server,
            nonce: [4; 16],
        })
        .unwrap(),
        target: PairingTarget::new(PairingTargetParts {
            server,
            device: DeviceId::new([3; 16]),
            pairing: PairingId::new(server, 1).unwrap(),
        })
        .unwrap(),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key(),
            pairing_authentication_public_key: auth.public_key(),
        })
        .unwrap(),
        initial_client: ClientId::new(server, 1).unwrap(),
        enrollment_stream: stream(server, 1),
        claim_digest: RequestDigest::from_bytes([5; 32]),
        authorization_digest: RequestDigest::from_bytes([6; 32]),
        enrollment_request_digest: RequestDigest::from_bytes([7; 32]),
        issuer: EnrollmentIssuer::AccountOwner {
            native_request: [9; 16],
            native_request_digest: RequestDigest::from_bytes([10; 32]),
        },
        issued_epoch: ServerEpoch::new(1),
        issued_revision: StoreRevision::new(1),
        issued_at_millis: 0,
        expires_at_millis: 100,
        state: EnrollmentReservationState::Reserved,
    })
    .unwrap()
}
fn claims(r: &EnrollmentReservation) -> PairingReceiptBody {
    let p = r.parts();
    PairingReceiptBody::Enrollment {
        target: p.target.clone(),
        keys: p.keys.clone(),
        client: PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: p.initial_client,
                pairing: p.target.parts().pairing,
            },
            ordinary: stream(p.target.parts().server, 2),
            self_revocation: stream(p.target.parts().server, 3),
        })
        .unwrap(),
        enrollment: OperationBinding {
            id: OperationId::new(p.enrollment_stream, OperationSequence::new(1).unwrap()),
            digest: p.enrollment_request_digest,
        },
        revision: StoreRevision::new(2),
    }
}
struct Lookup(EnrollmentReservation);
impl EnrollmentLookup for Lookup {
    fn lookup(&self, ticket: &EnrollmentTicketId) -> Result<EnrollmentReservation, ProtocolError> {
        if ticket == &self.0.parts().ticket {
            Ok(self.0.clone())
        } else {
            Err(ProtocolError::PairingDenied)
        }
    }
}
#[test]
fn receipt_rejects_wrong_server_pairing_key_and_signature() {
    let server = Key::new(1);
    let device = Key::new(2);
    let auth = Key::new(3);
    let r = reservation(device.as_ref(), auth.as_ref());
    let codec = ReceiptCodec::new(ServerPin {
        identity: r.parts().target.parts().server.bytes(),
        public_key: server.public_key(),
    });
    let receipt = codec.sign(claims(&r), server.as_ref()).unwrap();
    assert_eq!(codec.verify(&receipt).unwrap(), receipt.parts().claims);
    let wrong = ReceiptCodec::new(ServerPin {
        identity: [1; 16],
        public_key: server.public_key(),
    });
    assert!(wrong.verify(&receipt).is_err());
    let wrong_key = ReceiptCodec::new(ServerPin {
        identity: codec.pin().identity,
        public_key: device.public_key(),
    });
    assert!(wrong_key.verify(&receipt).is_err());
    let mut tampered = receipt.parts().clone();
    tampered.signature[0] ^= 1;
    assert!(
        codec
            .verify(&SignedPairingReceipt::new(tampered).unwrap())
            .is_err()
    );
}
#[test]
fn enrollment_and_receipts_round_trip_in_both_majors() {
    for majors in [vec![2, 1], vec![1]] {
        let server = Key::new(1);
        let device = Key::new(2);
        let auth = Key::new(3);
        let r = reservation(device.as_ref(), auth.as_ref());
        let lookup = Lookup(r.clone());
        let pin = ServerPin {
            identity: r.parts().target.parts().server.bytes(),
            public_key: server.public_key(),
        };
        let (client, hello) = EnrollmentClient::begin(
            pin,
            PublicEnrollmentReservation::decode(&reservation_to_wire(&r).encode_to_vec()).unwrap(),
            majors.clone(),
            device,
            auth,
        )
        .unwrap();
        let (server, reply) =
            EnrollmentServer::accept(pin.identity, vec![2, 1], server, &lookup, &hello).unwrap();
        let (client, proof) = client.receive_server(&reply).unwrap();
        let (mut server, ready) = server.receive_proof(&lookup, &proof).unwrap();
        let mut client = client.receive_ready(&ready).unwrap();
        assert_eq!(client.major(), majors[0]);
        let request = client.lookup_message().unwrap();
        let received = server.decode(&request).unwrap();
        assert!(matches!(
            received.kind,
            Some(spaceterm_protocol::wire::pairing::enrollment_message::Kind::Lookup(_))
        ));
        assert_eq!(server.decode(&request), Err(ProtocolError::Sequence));
    }
}

fn channel() -> (EnrollmentConnection, EnrollmentConnection, Arc<Key>) {
    let signer = Key::new(1);
    let device = Key::new(2);
    let auth = Key::new(3);
    let r = reservation(device.as_ref(), auth.as_ref());
    let lookup = Lookup(r.clone());
    let pin = ServerPin {
        identity: r.parts().target.parts().server.bytes(),
        public_key: signer.public_key(),
    };
    let (client, hello) = EnrollmentClient::begin(
        pin,
        PublicEnrollmentReservation::decode(&reservation_to_wire(&r).encode_to_vec()).unwrap(),
        vec![2, 1],
        device,
        auth.clone(),
    )
    .unwrap();
    let (server, reply) =
        EnrollmentServer::accept(pin.identity, vec![2, 1], signer, &lookup, &hello).unwrap();
    let (client, proof) = client.receive_server(&reply).unwrap();
    let (server, ready) = server.receive_proof(&lookup, &proof).unwrap();
    (client.receive_ready(&ready).unwrap(), server, auth)
}
#[test]
fn enrollment_channel_rejects_cross_connection_replay_and_purpose_escalation() {
    let (mut client, mut server, _) = channel();
    let request = client.lookup_message().unwrap();
    let (_, mut other_server, _) = channel();
    assert_eq!(
        other_server.decode(&request),
        Err(ProtocolError::Authentication)
    );
    assert!(server.decode(&request).is_ok());
    let wrong_direction = spaceterm_protocol::wire::pairing::EnrollmentMessage {
        kind: Some(
            spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(
                spaceterm_protocol::wire::pairing::EnrollmentResult {
                    operation: Some(operation_to_wire(OperationId::new(
                        client.reservation().parts().enrollment_stream,
                        OperationSequence::new(1).unwrap(),
                    ))),
                    request_digest: client
                        .reservation()
                        .parts()
                        .enrollment_request_digest
                        .bytes()
                        .to_vec(),
                    receipt: None,
                    failure: Some(spaceterm_protocol::wire::common::Failure {
                        classification: 1,
                        ..Default::default()
                    }),
                },
            ),
        ),
    };
    assert_eq!(
        client.encode(&wrong_direction),
        Err(ProtocolError::UnexpectedMessage)
    );
}
#[test]
fn receipt_verifies_exact_received_bytes_and_compares_decoded_claims() {
    let signer = Key::new(1);
    let device = Key::new(2);
    let auth = Key::new(3);
    let r = reservation(device.as_ref(), auth.as_ref());
    let codec = ReceiptCodec::new(ServerPin {
        identity: r.parts().target.parts().server.bytes(),
        public_key: signer.public_key(),
    });
    let proof = codec.sign(claims(&r), signer.as_ref()).unwrap();
    let mut tampered = proof.parts().clone();
    tampered.canonical_body.push(0);
    assert_eq!(
        codec.verify(&SignedPairingReceipt::new(tampered).unwrap()),
        Err(ProtocolError::Authentication)
    );
    let mut forged = proof.parts().clone();
    if let PairingReceiptBody::Enrollment { revision, .. } = &mut forged.claims {
        *revision = StoreRevision::new(3);
    }
    assert_eq!(
        codec.verify(&SignedPairingReceipt::new(forged).unwrap()),
        Err(ProtocolError::Authentication)
    );
}
#[test]
fn pairing_payload_preflight_rejects_bounds_plus_one() {
    let signer = Key::new(1);
    let device = Key::new(2);
    let auth = Key::new(3);
    let r = reservation(device.as_ref(), auth.as_ref());
    let lookup = Lookup(r);
    let result = EnrollmentServer::accept(
        [8; 16],
        vec![2, 1],
        signer,
        &lookup,
        &vec![0; MAX_PAIRING_MANAGEMENT_BYTES + 1],
    );
    assert!(matches!(result, Err(ProtocolError::LimitExceeded)));
}
#[test]
fn additional_client_proof_is_bound_to_original_operation_and_both_key_roles() {
    let device = Key::new(2);
    let auth = Key::new(3);
    let r = reservation(device.as_ref(), auth.as_ref());
    let target = &r.parts().target;
    let version = spaceterm_core::authority::FieldVersion::new(1);
    let op = OperationId::new(
        stream(target.parts().server, 2),
        OperationSequence::new(1).unwrap(),
    );
    let (claim, _) = client_binding_claim(target, version, op).unwrap();
    let (d, a) =
        sign_client_binding_claim(target, version, op, device.as_ref(), auth.as_ref()).unwrap();
    assert!(
        verify_client_binding_claim(target, version, op, &r.parts().keys, claim, &d, &a).is_ok()
    );
    let next = OperationId::new(op.stream(), OperationSequence::new(2).unwrap());
    assert!(
        verify_client_binding_claim(target, version, next, &r.parts().keys, claim, &d, &a).is_err()
    );
    assert!(
        verify_client_binding_claim(target, version, op, &r.parts().keys, claim, &a, &d).is_err()
    );
}

#[test]
fn enrollment_client_receives_only_public_authorization_delivery_in_both_majors() {
    use spaceterm_protocol::pairing_adapter::EnrollmentAuthorization;
    for versions in [vec![2, 1], vec![1]] {
        let signer = Key::new(1);
        let device = Key::new(2);
        let auth = Key::new(3);
        let r = reservation(device.as_ref(), auth.as_ref());
        let pin = ServerPin {
            identity: r.parts().target.parts().server.bytes(),
            public_key: signer.public_key(),
        };
        let issued = EnrollmentAuthorization::generate().unwrap();
        let bytes = issued.delivery_message(&r).unwrap();
        let (public, received) = EnrollmentAuthorization::from_delivery(bytes).unwrap();
        assert_eq!(received.digest(), issued.digest());
        let lookup = Lookup(r);
        let (client, hello) =
            EnrollmentClient::begin(pin, public, versions.clone(), device.clone(), auth).unwrap();
        let (server, reply) =
            EnrollmentServer::accept(pin.identity, vec![2, 1], signer, &lookup, &hello).unwrap();
        let (client, proof) = client.receive_server(&reply).unwrap();
        let (mut server, ready) = server.receive_proof(&lookup, &proof).unwrap();
        let mut client = client.receive_ready(&ready).unwrap();
        assert_eq!(client.major(), versions[0]);
        let bytes = client
            .submission_message(&received, device.as_ref())
            .unwrap();
        let submission = server.receive_submission(&bytes).unwrap();
        assert_eq!(submission.authorization_digest(), issued.digest());
    }
}

#[test]
fn public_enrollment_delivery_rejects_inconsistent_identities_and_bearer_bounds() {
    use prost::Message;
    use spaceterm_protocol::{pairing_adapter::EnrollmentAuthorization, wire::pairing};
    use zeroize::Zeroizing;
    let device = Key::new(2);
    let auth = Key::new(3);
    let r = reservation(device.as_ref(), auth.as_ref());
    let wire = spaceterm_protocol::pairing_adapter::reservation_to_wire(&r);
    for malformed in 0..6 {
        let mut public = wire.clone();
        let mut bearer = vec![0xa5; 32];
        match malformed {
            0 => public.initial_client.as_mut().unwrap().local_id += 1,
            1 => public.enrollment_operation.as_mut().unwrap().sequence = 2,
            2 => {
                public
                    .ticket
                    .as_mut()
                    .unwrap()
                    .server
                    .as_mut()
                    .unwrap()
                    .value = vec![1; 16]
            }
            3 => public.claim_digest.pop().map(|_| ()).unwrap(),
            4 => {
                bearer.pop();
            }
            5 => bearer.push(1),
            _ => unreachable!(),
        }
        let mut delivery = pairing::AuthorizationDelivery {
            reservation: Some(public),
            single_use_authorization: bearer,
        };
        let bytes = Zeroizing::new(delivery.encode_to_vec());
        use zeroize::Zeroize;
        delivery.single_use_authorization.zeroize();
        assert!(EnrollmentAuthorization::from_delivery(bytes).is_err());
    }
    assert!(matches!(
        EnrollmentAuthorization::from_delivery(Zeroizing::new(vec![
            0;
            MAX_PAIRING_MANAGEMENT_BYTES
                + 1
        ])),
        Err(ProtocolError::LimitExceeded)
    ));
}

struct PairedLookup(spaceterm_protocol::authentication::PairingRecord);
impl spaceterm_protocol::authentication::PairingLookup for PairedLookup {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<spaceterm_protocol::authentication::PairingRecord, ProtocolError> {
        if (server, pairing, client)
            != (
                self.0.principal.server,
                self.0.principal.pairing,
                self.0.principal.client,
            )
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(self.0)
    }
}
#[test]
fn restricted_exchange_authenticates_original_binding_in_both_majors() {
    use spaceterm_protocol::{
        authentication::*,
        pairing_adapter::{PairingRestrictedCodec, RestrictedExtensionCodec},
        wire::pairing as wire,
    };
    for versions in [vec![2, 1], vec![1]] {
        let signer = Key::new(1);
        let device = Key::new(2);
        let auth = Key::new(3);
        let r = reservation(device.as_ref(), auth.as_ref());
        let pin = ServerPin {
            identity: r.parts().target.parts().server.bytes(),
            public_key: signer.public_key(),
        };
        let mut local = [0; 16];
        local[15] = 1;
        let principal = Principal {
            server: pin.identity,
            device: r.parts().target.parts().device.bytes(),
            pairing: local,
            client: local,
        };
        let lookup = PairedLookup(spaceterm_protocol::authentication::PairingRecord {
            principal,
            device_public_key: auth.public_key(),
            active: false,
        });
        let (client, hello) = ClientHandshake::begin(
            pin,
            principal,
            Purpose::RevocationStatus,
            HandshakeConfig {
                majors: versions.clone(),
                ..Default::default()
            },
            auth,
        )
        .unwrap();
        let (server, reply) = ServerHandshake::accept(
            pin.identity,
            1,
            HandshakeConfig::default(),
            signer.clone(),
            &lookup,
            &hello,
        )
        .unwrap();
        let (client, proof) = client.receive_server(&reply).unwrap();
        let (server, ready) = server.receive_device(&lookup, &proof).unwrap();
        let mut client = match client.receive_ready(&ready).unwrap() {
            Established::Restricted(client) => client,
            _ => panic!("ordinary escalation"),
        };
        let mut server = match server {
            Established::Restricted(server) => server,
            _ => panic!("ordinary escalation"),
        };
        let codec = PairingRestrictedCodec::new(pin);
        let operation = OperationId::new(
            stream(r.parts().target.parts().server, 3),
            OperationSequence::new(1).unwrap(),
        );
        let request = wire::RestrictedExtension {
            kind: Some(wire::restricted_extension::Kind::Revoke(
                wire::SelfRevocation {
                    operation: Some(operation_to_wire(operation)),
                    request_digest: [7; 32].to_vec(),
                    target: Some(target_to_wire(&r.parts().target)),
                },
            )),
        };
        let frame = client.encode_pairing_extension(&request, &codec).unwrap();
        assert_eq!(
            server.decode_pairing_extension(&frame, &codec).unwrap(),
            request
        );
        assert_eq!(
            server.decode_pairing_extension(&frame, &codec),
            Err(ProtocolError::Sequence)
        );
        let bad = wire::RestrictedExtension {
            kind: Some(wire::restricted_extension::Kind::Acknowledgement(
                wire::RestrictedAcknowledgement {
                    operation: Some(operation_to_wire(operation)),
                    request_digest: [7; 32].to_vec(),
                    target: Some(target_to_wire(&r.parts().target)),
                    acknowledged_through: 2,
                },
            )),
        };
        assert_eq!(
            codec.encode_restricted_extension(
                &bad,
                principal,
                spaceterm_protocol::wire::common::Direction::ClientToServer,
                Default::default()
            ),
            Err(ProtocolError::Malformed)
        );
    }
}
#[test]
fn restricted_response_distinguishes_another_operations_receipt_from_execution() {
    use spaceterm_protocol::{
        authentication::Principal,
        pairing_adapter::{PairingRestrictedCodec, RestrictedExtensionCodec},
        wire::{common, pairing as wire},
    };
    let signer = Key::new(1);
    let device = Key::new(2);
    let auth = Key::new(3);
    let r = reservation(device.as_ref(), auth.as_ref());
    let target = &r.parts().target;
    let pin = ServerPin {
        identity: target.parts().server.bytes(),
        public_key: signer.public_key(),
    };
    let mut local = [0; 16];
    local[15] = 1;
    let principal = Principal {
        server: pin.identity,
        device: target.parts().device.bytes(),
        pairing: local,
        client: local,
    };
    let requested = OperationId::new(
        stream(target.parts().server, 3),
        OperationSequence::new(1).unwrap(),
    );
    let caused = OperationId::new(
        stream(target.parts().server, 2),
        OperationSequence::new(2).unwrap(),
    );
    let receipt = ReceiptCodec::new(pin)
        .sign(
            PairingReceiptBody::Revocation {
                receipt: RevocationReceipt {
                    server: target.parts().server,
                    pairing: target.parts().pairing,
                    device: target.parts().device,
                    operation: caused,
                    revision: StoreRevision::new(9),
                    receipt_digest: RequestDigest::from_bytes([0; 32]),
                },
                authentication_key_digest:
                    spaceterm_core::pairing::policy::authentication_key_digest_for(&r.parts().keys),
                revocation_event: [3; 16],
            },
            signer.as_ref(),
        )
        .unwrap();
    let codec = PairingRestrictedCodec::new(pin);
    let make = |disposition| wire::RestrictedExtension {
        kind: Some(wire::restricted_extension::Kind::Result(
            wire::RestrictedResult {
                request_operation: Some(operation_to_wire(requested)),
                request_digest: [7; 32].to_vec(),
                target: Some(target_to_wire(target)),
                disposition,
                receipt: Some(receipt_to_wire(&receipt)),
            },
        )),
    };
    assert!(
        codec
            .encode_restricted_extension(
                &make(2),
                principal,
                common::Direction::ServerToClient,
                Default::default()
            )
            .is_ok()
    );
    assert_eq!(
        codec.encode_restricted_extension(
            &make(1),
            principal,
            common::Direction::ServerToClient,
            Default::default()
        ),
        Err(ProtocolError::PairingDenied)
    );
}

#[test]
fn public_enrollment_requires_both_candidate_key_proofs_before_readiness() {
    use spaceterm_protocol::{pairing_adapter::EnrollmentAuthorization, wire::pairing};
    for versions in [vec![2, 1], vec![1]] {
        for device_role in [true, false] {
            let signer = Key::new(1);
            let device = Key::new(2);
            let auth = Key::new(3);
            let r = reservation(device.as_ref(), auth.as_ref());
            let pin = ServerPin {
                identity: r.parts().target.parts().server.bytes(),
                public_key: signer.public_key(),
            };
            let issued = EnrollmentAuthorization::generate().unwrap();
            let (public, _) =
                EnrollmentAuthorization::from_delivery(issued.delivery_message(&r).unwrap())
                    .unwrap();
            let lookup = Lookup(r);
            let (client, hello) =
                EnrollmentClient::begin(pin, public, versions.clone(), device, auth).unwrap();
            let (server, reply) =
                EnrollmentServer::accept(pin.identity, vec![2, 1], signer, &lookup, &hello)
                    .unwrap();
            let (_, proof) = client.receive_server(&reply).unwrap();
            let mut message = pairing::EnrollmentExchange::decode(proof.as_slice()).unwrap();
            match &mut message.kind {
                Some(pairing::enrollment_exchange::Kind::Proof(p)) => {
                    if device_role {
                        p.device_identity_proof[0] ^= 1;
                    } else {
                        p.pairing_authentication_proof[0] ^= 1;
                    }
                }
                _ => panic!("wrong proof"),
            }
            assert!(matches!(
                server.receive_proof(&lookup, &message.encode_to_vec()),
                Err(ProtocolError::Authentication)
            ));
        }
    }
}
