use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    authority::StoreRevision,
    identity::{ClientId, DeviceId, OperationStreamId, OperationStreamKey, PairingId, ServerId},
    operations::{OperationId, OperationSequence, RequestDigest},
    pairing::*,
    storage::StoreFailure,
};
use spaceterm_host::organization_store::pairing::PairingReceiptReader;
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ServerPin, SigningKey},
    pairing_adapter::ReceiptCodec,
};
use spaceterm_server::pairing::store_projection::SignedReceiptReader;

struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
    fn pin(&self) -> ServerPin {
        ServerPin {
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

fn revocation(key: &Key) -> SignedPairingReceipt {
    let server = ServerId::new(key.pin().identity);
    let pairing = PairingId::new(server, 1).unwrap();
    let client = ClientId::new(server, 1).unwrap();
    let operation = OperationId::new(
        OperationStreamId::new(OperationStreamKey::new(server, 2).unwrap(), pairing, client)
            .unwrap(),
        OperationSequence::new(1).unwrap(),
    );
    ReceiptCodec::new(key.pin())
        .sign(
            PairingReceiptBody::Revocation {
                receipt: RevocationReceipt {
                    server,
                    pairing,
                    device: DeviceId::new([19; 16]),
                    operation,
                    revision: StoreRevision::new(4),
                    receipt_digest: RequestDigest::from_bytes([0; 32]),
                },
                authentication_key_digest: RequestDigest::from_bytes([23; 32]),
                revocation_event: [29; 16],
            },
            key,
        )
        .unwrap()
}

#[test]
fn production_receipt_reader_verifies_exact_bytes_schema_pin_and_claims() {
    let key = Key::new(71);
    let receipt = revocation(&key);
    let reader = SignedReceiptReader::new(key.pin()).unwrap();
    let p = receipt.parts();
    assert_eq!(
        reader
            .read_receipt(p.schema, &p.canonical_body, &p.signature)
            .unwrap(),
        receipt
    );
    assert_eq!(
        reader.read_receipt(2, &p.canonical_body, &p.signature),
        Err(StoreFailure::UnknownSchema)
    );
    let mut changed = p.canonical_body.clone();
    changed[0] ^= 1;
    assert_eq!(
        reader.read_receipt(1, &changed, &p.signature),
        Err(StoreFailure::InvalidDurableState)
    );
    assert_eq!(
        SignedReceiptReader::new(Key::new(72).pin())
            .unwrap()
            .read_receipt(1, &p.canonical_body, &p.signature),
        Err(StoreFailure::InvalidDurableState)
    );
    assert_eq!(
        reader.read_receipt(1, &p.canonical_body, &[0; 63]),
        Err(StoreFailure::InvalidDurableState)
    );
    assert_eq!(
        reader.read_receipt(1, &vec![0; MAX_SIGNED_PAIRING_RECEIPT_BYTES], &p.signature),
        Err(StoreFailure::InvalidDurableState)
    );
    let mut forged = p.clone();
    if let PairingReceiptBody::Revocation { receipt, .. } = &mut forged.claims {
        receipt.receipt_digest = RequestDigest::from_bytes([1; 32]);
    }
    assert!(
        reader
            .validate_receipt(&SignedPairingReceipt::new(forged).unwrap())
            .is_err()
    );
}

#[path = "store_pairing_composition/support.rs"]
mod support;
use spaceterm_core::{
    operation_ledger::{canonical::CanonicalCodec, model::*},
    operation_ledger_contracts::*,
    operations::*,
    pairing::policy::PairingPolicy,
    storage::{CommitDisposition, DurableTransaction, OperationWrite, OrganizationStore},
};
use support::*;

#[test]
fn enrollment_consumption_and_signed_outcome_use_production_store_services() {
    let directory = Directory::new();
    let mut fixture = Fixture::new(&directory.0);
    let (reservation, proof) = fixture.enroll(19);
    let binding = Fixture::enroll_request(&reservation).binding();
    let recovered = fixture.recovered();
    assert_eq!(
        recovered
            .pairing
            .as_ref()
            .unwrap()
            .parts()
            .authorizations
            .len(),
        1
    );
    assert!(
        matches!(&recovered.pairing.as_ref().unwrap().parts().enrollments[0].parts().state, EnrollmentReservationState::Consumed { receipt, enrollment } if receipt.as_ref() == &proof && *enrollment == binding)
    );
    assert_eq!(
        fixture
            .ledger
            .lookup(
                &StreamBinding::from_stream(binding.id.stream()),
                binding.id,
                Some(binding.digest)
            )
            .unwrap(),
        OperationLookup::Completed {
            result: Outcome::Pairing(Box::new(PairingOutcome::Enrolled(proof.clone()))),
            publication_revision: recovered.organization.revision
        }
    );
    let state = fixture.snapshot();
    assert_eq!(
        PairingPolicy::default()
            .check_enrollment_authorization(
                &state,
                &reservation.parts().ticket,
                binding,
                reservation.parts().claim_digest,
                RequestDigest::from_bytes([0; 32]),
                u64::MAX
            )
            .unwrap(),
        Some(proof)
    );
    drop(fixture);
    assert_eq!(reopen(&directory.0).unwrap().recover().unwrap(), recovered);
}

#[test]
fn signed_revocation_faults_keep_authorization_receipt_and_outcome_atomic_with_production_receipt_reader()
 {
    use spaceterm_host::storage::testing::FaultPoint;
    let points = [
        FaultPoint::BeforeCommit,
        FaultPoint::Write,
        FaultPoint::DatabaseWrite,
        FaultPoint::Sync,
        FaultPoint::DatabaseSync,
        FaultPoint::DirectorySync,
        #[cfg(target_os = "macos")]
        FaultPoint::DeleteFullSync,
        FaultPoint::AfterCommit,
    ];
    for point in points {
        let directory = Directory::new();
        let mut fixture = Fixture::new(&directory.0);
        let (reservation, enrollment) = fixture.enroll(19);
        let request = Fixture::revoke_request(&enrollment);
        fixture.admit(&request);
        let before = fixture.recovered();
        let (prepared, transition, proof) =
            fixture.prepare_revocation(&request, &reservation.parts().target);
        let transaction = fixture.transaction(&prepared, Some(&transition));
        let control = fixture.store.authority_test_control();
        control.fail_once(point, 0);
        let disposition = fixture.store.commit(&transaction);
        assert!(
            !control.fault_pending(),
            "fault did not fire: {point:?}, {disposition:?}"
        );
        if point == FaultPoint::BeforeCommit {
            assert!(matches!(disposition, CommitDisposition::NotApplied(_)));
        } else {
            assert_eq!(disposition, CommitDisposition::Indeterminate);
        }
        fixture
            .ledger
            .resolve_commit(prepared, disposition)
            .unwrap();
        if disposition == CommitDisposition::Indeterminate {
            control.repeat_sync().unwrap();
            assert_eq!(fixture.store.recover(), Err(StoreFailure::RecoveryRequired));
            let calls = control.sql_calls();
            assert_eq!(
                fixture.store.commit(&transaction),
                CommitDisposition::NotApplied(StoreFailure::RecoveryRequired)
            );
            assert_eq!(control.sql_calls(), calls);
        }
        drop(fixture);
        let recovered = reopen(&directory.0).unwrap().recover().unwrap();
        if point == FaultPoint::BeforeCommit {
            assert_eq!(recovered, before);
        }
        let ledger =
            OperationLedger::recover(identity().server, recovered.ledger.clone().unwrap()).unwrap();
        let authorization = &recovered.pairing.as_ref().unwrap().parts().authorizations[0];
        match ledger
            .lookup(
                &StreamBinding::from_stream(request.binding().id.stream()),
                request.binding().id,
                Some(request.binding().digest),
            )
            .unwrap()
        {
            OperationLookup::Completed { result, .. } => {
                assert!(matches!(
                    authorization.parts().record.state,
                    PairingState::Revoked { .. }
                ));
                assert_eq!(
                    authorization.parts().revocation_receipt.as_ref(),
                    Some(&proof)
                );
                assert_eq!(
                    result,
                    Outcome::Pairing(Box::new(PairingOutcome::Revoked(proof)))
                );
                assert!(
                    recovered
                        .ledger
                        .as_ref()
                        .unwrap()
                        .streams
                        .iter()
                        .filter(|h| h.purpose == StreamPurpose::Ordinary)
                        .all(|h| h.status
                            == StreamStatus::Retired(StreamRetirementReason::PairingRevoked))
                );
            }
            OperationLookup::Pending => {
                assert_eq!(authorization.parts().record.state, PairingState::Authorized);
                assert!(authorization.parts().revocation_receipt.is_none());
                assert_ne!(point, FaultPoint::AfterCommit);
            }
            other => panic!("original revocation identity lost: {other:?}"),
        }
    }
}

#[test]
fn enrollment_reserves_revocation_capacity_before_commit() {
    let directory = Directory::new();
    let mut fixture = Fixture::new(&directory.0);
    let (reservation, secret) = fixture.reserve(19);
    fixture.admit(&Fixture::enroll_request(&reservation));
    let before = fixture.recovered();
    let (prepared, transition, _) = fixture.prepare_enrollment(&reservation, &secret);
    let tx = fixture.transaction(&prepared, Some(&transition));
    let registered = tx
        .parts()
        .operation_writes
        .iter()
        .find_map(|write| match write {
            OperationWrite::Ledger(write) => match write.as_ref() {
                LedgerWrite::RegisterStream(header)
                    if header.purpose == StreamPurpose::SelfRevocation =>
                {
                    Some(header)
                }
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    assert_eq!(
        registered.dormant_reservation,
        Some(OutcomeReservation::full(
            ReservationClass::DedicatedSelfRevocation
        ))
    );
    assert!(
        before
            .pairing
            .as_ref()
            .unwrap()
            .parts()
            .authorizations
            .is_empty()
    );
    assert_eq!(fixture.recovered(), before);
    fixture.commit(prepared, Some(&transition));
    let after = fixture.recovered();
    assert!(after.ledger.unwrap().streams.contains(registered));
    assert_eq!(after.pairing.unwrap().parts().authorizations.len(), 1);
}

#[test]
fn additional_client_binding_reserves_distinct_identity_and_streams() {
    use spaceterm_core::authority::FieldVersion;
    use spaceterm_core::identity::IdentityHighWater;
    let directory = Directory::new();
    let mut fixture = Fixture::new(&directory.0);
    let (reservation, enrollment) = fixture.enroll(19);
    let PairingReceiptBody::Enrollment {
        client: initial, ..
    } = &enrollment.parts().claims
    else {
        unreachable!()
    };
    let target = reservation.parts().target.clone();
    let request = CanonicalOperationRequest::new(
        OperationId::new(initial.parts().ordinary, OperationSequence::new(1).unwrap()),
        Command::Pairing(PairingCommand::BindAdditionalClient {
            target: target.clone(),
            expected_authorization_version: FieldVersion::new(1),
            claim_digest: RequestDigest::from_bytes([41; 32]),
        }),
        &CanonicalCodec,
    )
    .unwrap();
    fixture.admit(&request);
    let state = fixture.snapshot();
    let policy = PairingPolicy::default();
    let actor = policy
        .authorize_client(
            &state,
            initial.parts().client.pairing,
            initial.parts().client.id,
        )
        .unwrap();
    let client_id = ClientId::new(state.server(), state.high_water().client + 1).unwrap();
    let streams = fixture.streams(actor.pairing(), client_id);
    let key = Key::new(71);
    let proof = ReceiptCodec::new(key.pin())
        .sign(
            PairingReceiptBody::ClientBinding {
                target,
                keys: reservation.parts().keys.clone(),
                client: streams.clone(),
                operation: request.binding(),
                revision: state.revision().checked_successor().unwrap(),
            },
            &key,
        )
        .unwrap();
    let transition = policy
        .prepare_client_binding(
            &state,
            &actor,
            request.binding(),
            FieldVersion::new(1),
            streams.clone(),
            proof.clone(),
        )
        .unwrap();
    assert!(transition.high_water.dominates(IdentityHighWater {
        client: client_id.local(),
        ..state.high_water()
    }));
    let prepared = fixture.completion(request.binding(), &transition);
    fixture.commit(prepared, Some(&transition));
    let recovered = fixture.recovered();
    let authorization = &recovered.pairing.as_ref().unwrap().parts().authorizations[0];
    assert_eq!(
        authorization.parts().clients,
        vec![initial.clone(), streams.clone()]
    );
    assert_ne!(streams.parts().ordinary, initial.parts().ordinary);
    assert_ne!(
        streams.parts().self_revocation,
        initial.parts().self_revocation
    );
    assert_eq!(
        fixture
            .ledger
            .stream(&StreamBinding::from_stream(streams.parts().self_revocation))
            .unwrap()
            .dormant_reservation,
        Some(OutcomeReservation::full(
            ReservationClass::DedicatedSelfRevocation
        ))
    );
    drop(fixture);
    assert_eq!(reopen(&directory.0).unwrap().recover().unwrap(), recovered);
}

#[test]
fn expired_ticket_invalidation_releases_outstanding_ticket_quota() {
    let directory = Directory::new();
    let mut fixture = Fixture::new(&directory.0);
    for device in 100..100 + MAX_OUTSTANDING_ENROLLMENTS as u8 {
        fixture.reserve(device);
    }
    let policy = PairingPolicy::default();
    assert_eq!(
        policy.check_reservation_capacity(&fixture.snapshot(), DeviceId::new([199; 16])),
        Err(PairingFailure::LimitExceeded)
    );
    let before = fixture.recovered();
    let invalidation = policy
        .prepare_ticket_invalidations(&fixture.snapshot(), 301000)
        .unwrap();
    let context = fixture.context();
    let bindings: Vec<_> = invalidation
        .retire_streams
        .iter()
        .map(|stream| StreamBinding::from_stream(*stream))
        .collect();
    let prepared = fixture
        .ledger
        .prepare_retirement(&bindings, StreamRetirementReason::Expired, context)
        .unwrap();
    let mut parts = fixture.parts();
    prepared.projection().append_to(&mut parts).unwrap();
    parts.operation_writes.extend(
        invalidation
            .writes
            .parts()
            .writes
            .iter()
            .cloned()
            .map(|write| OperationWrite::Pairing(Box::new(write))),
    );
    let tx = DurableTransaction::new_with_validation(parts, &services()).unwrap();
    let disposition = fixture.store.commit(&tx);
    assert!(matches!(disposition, CommitDisposition::Durable(_)));
    fixture
        .ledger
        .resolve_commit(prepared, disposition)
        .unwrap();
    let (new, _) = fixture.reserve(199);
    assert!(new.parts().target.parts().pairing.local() > before.high_water.pairing);
    let recovered = fixture.recovered();
    assert_eq!(
        recovered
            .pairing
            .unwrap()
            .parts()
            .enrollments
            .iter()
            .filter(|r| r.parts().state.kind() == EnrollmentReservationKind::Reserved)
            .count(),
        1
    );
    assert_eq!(
        recovered
            .ledger
            .unwrap()
            .streams
            .iter()
            .filter(|s| s.status == StreamStatus::Retired(StreamRetirementReason::Expired))
            .count(),
        MAX_OUTSTANDING_ENROLLMENTS
    );
}

#[test]
fn enrollment_acknowledgement_compacts_only_original_registered_stream() {
    let directory = Directory::new();
    let mut fixture = Fixture::new(&directory.0);
    let (first, first_proof) = fixture.enroll(19);
    let (second, second_proof) = fixture.enroll(31);
    let first_binding = Fixture::enroll_request(&first).binding();
    let second_binding = Fixture::enroll_request(&second).binding();
    fixture.acknowledge(first_binding);
    assert_eq!(
        fixture
            .ledger
            .lookup(
                &StreamBinding::from_stream(first_binding.id.stream()),
                first_binding.id,
                Some(first_binding.digest)
            )
            .unwrap(),
        OperationLookup::AcknowledgedRetired
    );
    assert!(
        matches!(fixture.ledger.lookup(&StreamBinding::from_stream(second_binding.id.stream()), second_binding.id, Some(second_binding.digest)).unwrap(), OperationLookup::Completed { result: Outcome::Pairing(outcome), .. } if *outcome == PairingOutcome::Enrolled(second_proof))
    );
    let state = fixture.snapshot();
    assert_eq!(
        PairingPolicy::default()
            .completed_enrollment(
                &state,
                &first.parts().ticket,
                first_binding,
                first.parts().claim_digest
            )
            .unwrap(),
        Some(first_proof)
    );
    let recovered = fixture.recovered();
    drop(fixture);
    assert_eq!(reopen(&directory.0).unwrap().recover().unwrap(), recovered);
}

#[test]
fn self_revocation_acknowledgement_preserves_enrollment_outcome_and_revocation_receipt() {
    let directory = Directory::new();
    let mut fixture = Fixture::new(&directory.0);
    let (reservation, proof) = fixture.enroll(19);
    let enrollment_binding = Fixture::enroll_request(&reservation).binding();
    let request = Fixture::revoke_request(&proof);
    fixture.admit(&request);
    let (prepared, transition, revocation) =
        fixture.prepare_revocation(&request, &reservation.parts().target);
    fixture.commit(prepared, Some(&transition));
    fixture.acknowledge(request.binding());
    assert_eq!(
        fixture
            .ledger
            .lookup(
                &StreamBinding::from_stream(request.binding().id.stream()),
                request.binding().id,
                Some(request.binding().digest)
            )
            .unwrap(),
        OperationLookup::AcknowledgedRetired
    );
    assert!(
        matches!(fixture.ledger.lookup(&StreamBinding::from_stream(enrollment_binding.id.stream()), enrollment_binding.id, Some(enrollment_binding.digest)).unwrap(), OperationLookup::Completed { result: Outcome::Pairing(outcome), .. } if *outcome == PairingOutcome::Enrolled(proof))
    );
    let recovered = fixture.recovered();
    assert_eq!(
        recovered.pairing.as_ref().unwrap().parts().authorizations[0]
            .parts()
            .revocation_receipt,
        Some(revocation)
    );
    drop(fixture);
    assert_eq!(reopen(&directory.0).unwrap().recover().unwrap(), recovered);
}

#[test]
fn production_services_reject_mismatched_home_invalid_account_and_missing_pin() {
    use spaceterm_core::organization::ServerDirectory;
    use spaceterm_server::adapters::store::production_services;
    let home = ServerDirectory::new(identity().server, b"/fixture-home".to_vec()).unwrap();
    assert!(production_services(identity(), home.clone()).is_ok());
    let other = ServerDirectory::new(ServerId::new([12; 16]), b"/fixture-home".to_vec()).unwrap();
    assert!(matches!(
        production_services(identity(), other),
        Err(StoreFailure::IdentityMismatch)
    ));
    for uid in [0, u32::MAX] {
        let mut value = identity();
        value.account.account_uid = uid;
        assert!(matches!(
            production_services(value, home.clone()),
            Err(StoreFailure::IdentityMismatch)
        ));
    }
    let mut value = identity();
    value.server_public_key = [0; 32];
    assert!(matches!(
        production_services(value, home),
        Err(StoreFailure::IdentityMismatch)
    ));
}

#[test]
fn enrollment_lost_commit_receipt_reopens_without_duplicate_pairing() {
    use spaceterm_host::storage::testing::FaultPoint;
    let directory = Directory::new();
    let mut fixture = Fixture::new(&directory.0);
    let (reservation, authorization) = fixture.reserve(19);
    let request = Fixture::enroll_request(&reservation);
    fixture.admit(&request);
    let (prepared, transition, receipt) = fixture.prepare_enrollment(&reservation, &authorization);
    let tx = fixture.transaction(&prepared, Some(&transition));
    let control = fixture.store.authority_test_control();
    control.fail_once(FaultPoint::AfterCommit, 0);
    assert_eq!(fixture.store.commit(&tx), CommitDisposition::Indeterminate);
    assert!(!control.fault_pending());
    fixture
        .ledger
        .resolve_commit(prepared, CommitDisposition::Indeterminate)
        .unwrap();
    drop(fixture);
    let mut store = reopen(&directory.0).unwrap();
    let recovered = store.recover().unwrap();
    let mut ledger =
        OperationLedger::recover(identity().server, recovered.ledger.clone().unwrap()).unwrap();
    let context = spaceterm_server::adapters::core::CommitCursor::from_recovered(&recovered)
        .context()
        .unwrap();
    let admission = ledger
        .prepare_admission(
            &StreamBinding::from_stream(request.binding().id.stream()),
            request.binding().id,
            request.command(),
            PendingKind::AtomicMutation,
            context,
        )
        .unwrap();
    assert!(
        matches!(admission, AdmissionPreparation::Existing(OperationLookup::Completed { result: Outcome::Pairing(outcome), .. }) if *outcome == PairingOutcome::Enrolled(receipt))
    );
    assert_eq!(
        recovered
            .pairing
            .as_ref()
            .unwrap()
            .parts()
            .authorizations
            .len(),
        1
    );
    assert_eq!(store.recover().unwrap(), recovered);
}

#[test]
fn unknown_schema_never_reinitializes_pairing_state() {
    use spaceterm_host::{
        organization_store::{migrations, schema},
        storage::{CommitDisposition as NativeCommit, Database},
    };
    let directory = Directory::new();
    let mut fixture = Fixture::new(&directory.0);
    fixture.enroll(19);
    drop(fixture);
    let mut database =
        Database::open_existing_for_application(&directory.0, migrations(), schema::APPLICATION_ID)
            .unwrap();
    let original: (Vec<u8>, Vec<u8>, Vec<u8>) = database
        .query_row(
            "SELECT server_id, server_public_key, store_revision FROM store_metadata",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(matches!(
        database
            .transaction(|sql| sql.execute("UPDATE pairing_receipts SET format_version = 2", [])),
        NativeCommit::Durable(1)
    ));
    drop(database);
    assert!(matches!(
        reopen(&directory.0),
        Err(StoreFailure::UnknownSchema)
    ));
    let database =
        Database::open_existing_for_application(&directory.0, migrations(), schema::APPLICATION_ID)
            .unwrap();
    let retained: (Vec<u8>, Vec<u8>, Vec<u8>) = database
        .query_row(
            "SELECT server_id, server_public_key, store_revision FROM store_metadata",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(retained, original);
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM pairings", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT format_version FROM pairing_receipts", [], |row| row
                .get::<_, u16>(0))
            .unwrap(),
        2
    );
}

#[test]
fn locked_pairing_storage_never_replaces_identity() {
    use spaceterm_host::{
        organization_store::{migrations, schema},
        storage::{CommitDisposition as NativeCommit, Database},
    };
    let directory = Directory::new();
    let mut fixture = Fixture::new(&directory.0);
    let (reservation, _) = fixture.enroll(19);
    let before = fixture.recovered();
    let request = Fixture::enroll_request(&reservation);
    let context = fixture.context();
    let prepared = fixture
        .ledger
        .prepare_acknowledgement(
            &StreamBinding::from_stream(request.binding().id.stream()),
            1,
            context,
        )
        .unwrap()
        .unwrap();
    let tx = fixture.transaction(&prepared, None);
    let mut database =
        Database::open_existing_for_application(&directory.0, migrations(), schema::APPLICATION_ID)
            .unwrap();
    let (locked, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        database.transaction(|sql| {
            sql.execute(
                "UPDATE store_metadata SET encoding_version = encoding_version",
                [],
            )?;
            locked.send(()).unwrap();
            wait.recv().unwrap();
            Ok(())
        })
    });
    ready.recv().unwrap();
    let disposition = fixture.store.commit(&tx);
    release.send(()).unwrap();
    assert!(matches!(writer.join().unwrap(), NativeCommit::Durable(())));
    assert_eq!(
        disposition,
        CommitDisposition::NotApplied(StoreFailure::Unavailable)
    );
    fixture
        .ledger
        .resolve_commit(prepared, disposition)
        .unwrap();
    assert_eq!(fixture.recovered(), before);
    drop(fixture);
    assert_eq!(reopen(&directory.0).unwrap().recover().unwrap(), before);
}

#[test]
fn restricted_acknowledgement_preserves_original_unacknowledged_receipt() {
    use spaceterm_core::pairing::transition::PairingTransition;
    let directory = Directory::new();
    let mut fixture = Fixture::new(&directory.0);
    let (target, target_proof) = fixture.enroll(19);
    let (_, issuer_proof) = fixture.enroll(31);
    let PairingReceiptBody::Enrollment { client: issuer, .. } = &issuer_proof.parts().claims else {
        panic!("enrollment")
    };
    let state = fixture.snapshot();
    let policy = PairingPolicy::default();
    let actor = policy
        .authorize_client(
            &state,
            issuer.parts().client.pairing,
            issuer.parts().client.id,
        )
        .unwrap();
    let assess = CanonicalOperationRequest::new(
        OperationId::new(issuer.parts().ordinary, OperationSequence::new(1).unwrap()),
        Command::Pairing(PairingCommand::AssessUnpair {
            target: target.parts().target.clone(),
        }),
        &CanonicalCodec,
    )
    .unwrap();
    fixture.admit(&assess);
    let state = fixture.snapshot();
    let assessment = policy
        .assess_unpair(
            &state,
            &actor,
            AssessmentRef {
                operation: assess.binding().id,
                digest: assess.binding().digest,
            },
            target.parts().target.clone(),
        )
        .unwrap();
    let transition = PairingTransition {
        expected_store: state.revision(),
        high_water: state.high_water(),
        writes: PairingWrites::default(),
        register_streams: vec![],
        retire_ordinary_streams: vec![],
        release_clients: vec![],
        outcome: PairingOutcome::UnpairAssessed(assessment.clone()),
    };
    let prepared = fixture.completion(assess.binding(), &transition);
    fixture.commit(prepared, Some(&transition));
    let request = CanonicalOperationRequest::new(
        OperationId::new(issuer.parts().ordinary, OperationSequence::new(2).unwrap()),
        Command::Pairing(PairingCommand::UnpairConfirmed {
            assessment: assessment.clone(),
        }),
        &CanonicalCodec,
    )
    .unwrap();
    fixture.admit(&request);
    let state = fixture.snapshot();
    let key = Key::new(71);
    let receipt = ReceiptCodec::new(key.pin())
        .sign(
            PairingReceiptBody::Revocation {
                receipt: RevocationReceipt {
                    server: state.server(),
                    pairing: target.parts().target.parts().pairing,
                    device: target.parts().target.parts().device,
                    operation: request.binding().id,
                    revision: state.revision().checked_successor().unwrap(),
                    receipt_digest: RequestDigest::from_bytes([0; 32]),
                },
                authentication_key_digest:
                    spaceterm_core::pairing::policy::authentication_key_digest_for(
                        &target.parts().keys,
                    ),
                revocation_event: [29; 16],
            },
            &key,
        )
        .unwrap();
    let transition = policy
        .prepare_unpair(
            &state,
            &actor,
            request.binding(),
            &assessment,
            receipt.clone(),
        )
        .unwrap();
    let prepared = fixture.completion(request.binding(), &transition);
    fixture.commit(prepared, Some(&transition));
    let before = fixture.recovered();
    let restricted = Fixture::revoke_request(&target_proof);
    let context = fixture.context();
    assert_eq!(
        fixture
            .ledger
            .prepare_acknowledgement(
                &StreamBinding::from_stream(restricted.binding().id.stream()),
                1,
                context
            )
            .unwrap_err()
            .class,
        LedgerFailureClass::AcknowledgementAhead
    );
    assert_eq!(fixture.recovered(), before);
    assert!(
        matches!(fixture.ledger.lookup(&StreamBinding::from_stream(request.binding().id.stream()), request.binding().id, Some(request.binding().digest)).unwrap(), OperationLookup::Completed { result: Outcome::Pairing(outcome), .. } if *outcome == PairingOutcome::Revoked(receipt.clone()))
    );
    assert_eq!(
        before.pairing.as_ref().unwrap().parts().authorizations[0]
            .parts()
            .revocation_receipt
            .as_ref(),
        Some(&receipt)
    );
    assert_eq!(
        before.pairing.as_ref().unwrap().parts().authorizations[1]
            .parts()
            .record
            .state,
        PairingState::Authorized
    );
    drop(fixture);
    assert_eq!(reopen(&directory.0).unwrap().recover().unwrap(), before);
}
