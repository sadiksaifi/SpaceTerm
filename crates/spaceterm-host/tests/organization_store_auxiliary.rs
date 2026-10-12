//! Auxiliary records share the production ledger and reducer validation boundary.
//! Only the unavailable pairing receipt provider is injected by test composition.

#[expect(
    dead_code,
    reason = "The shared fixture also serves authority fault and crash suites."
)]
mod organization_store_support;
use organization_store_support::*;
use spaceterm_core::operation_ledger::{
    canonical::{CanonicalCodec, canonical_request_digest},
    model::*,
};
use spaceterm_core::{
    authority::*,
    identity::*,
    operation_ledger_contracts::*,
    operations::*,
    organization::*,
    pairing::*,
    reducer::*,
    storage::*,
    worktrees::{intent::*, types::*},
};
use spaceterm_host::organization_store::*;
use spaceterm_host::storage::Database;

fn server() -> ServerId {
    identity().server
}
fn transaction(parts: TransactionParts) -> DurableTransaction {
    DurableTransaction::new_with_validation(parts, &services()).unwrap()
}
fn reject(store: &mut SqliteOrganizationStore, parts: TransactionParts) {
    let before = store.recover().unwrap();
    assert_eq!(
        store.commit(&transaction(parts)),
        CommitDisposition::NotApplied(StoreFailure::InvalidTransaction)
    );
    assert_eq!(store.recover().unwrap(), before);
}
fn only_writes(state: &RecoveredState, writes: Vec<OperationWrite>) -> TransactionParts {
    let mut parts = unchanged_parts(state);
    parts.operation_writes = writes;
    parts
}
fn target(pairing: u64, device: u8) -> PairingTarget {
    PairingTarget::new(PairingTargetParts {
        server: server(),
        pairing: PairingId::new(server(), pairing).unwrap(),
        device: DeviceId::new([device; 16]),
    })
    .unwrap()
}
fn reserve_enrollment(fixture: &mut Fixture) -> EnrollmentReservation {
    let before = fixture.store.recover().unwrap();
    let pairing = PairingId::new(server(), 2).unwrap();
    let client = ClientId::new(server(), 2).unwrap();
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server(), 3).unwrap(),
        pairing,
        client,
    )
    .unwrap();
    let ticket = EnrollmentTicketId::new(EnrollmentTicketParts {
        server: server(),
        nonce: [51; 16],
    })
    .unwrap();
    let claim_digest = RequestDigest::from_bytes([61; 32]);
    let command = Command::Pairing(PairingCommand::Enroll {
        ticket: ticket.clone(),
        claim_digest,
    });
    let enrollment_request_digest = canonical_request_digest(
        &StreamBinding::from_stream(stream),
        OperationId::new(stream, OperationSequence::new(1).unwrap()),
        &command,
    )
    .unwrap();
    let value = EnrollmentReservation::new(EnrollmentReservationParts {
        ticket,
        target: target(2, 33),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [44; 32],
            pairing_authentication_public_key: [55; 32],
        })
        .unwrap(),
        initial_client: client,
        enrollment_stream: stream,
        claim_digest,
        authorization_digest: RequestDigest::from_bytes([62; 32]),
        enrollment_request_digest,
        issuer: EnrollmentIssuer::AccountOwner {
            native_request: [64; 16],
            native_request_digest: RequestDigest::from_bytes([65; 32]),
        },
        issued_epoch: ServerEpoch::new(1),
        issued_revision: before.store_revision.checked_successor().unwrap(),
        issued_at_millis: 100,
        expires_at_millis: 200,
        state: EnrollmentReservationState::Reserved,
    })
    .unwrap();
    let context = fixture.context();
    let prepared = fixture
        .ledger
        .prepare_register_stream_with_purpose(
            StreamBinding::from_stream(stream),
            StreamPurpose::Enrollment { claim_digest },
            context,
        )
        .unwrap();
    let mut marks = before.high_water;
    marks.pairing = 2;
    marks.client = 2;
    marks.operation_stream = 3;
    let proposal = fixture.proposal(
        &prepared,
        vec![OperationWrite::Pairing(Box::new(
            PairingWrite::PutEnrollmentReservation {
                expected_kind: None,
                value: value.clone(),
            },
        ))],
        vec![],
        Some(marks),
    );
    let mut parts = proposal.parts().clone();
    parts.resulting_epoch = ServerEpoch::new(1);
    assert!(matches!(
        fixture.commit_proposal(prepared, transaction(parts)),
        TransitionResolution::StreamRegistered
    ));
    value
}
fn enrollment_completion(
    fixture: &mut Fixture,
    reservation: &EnrollmentReservation,
) -> (
    PreparedLedgerTransition,
    DurableTransaction,
    SignedPairingReceipt,
) {
    let p = reservation.parts();
    let binding = StreamBinding::from_stream(p.enrollment_stream);
    let command = Command::Pairing(PairingCommand::Enroll {
        ticket: p.ticket.clone(),
        claim_digest: p.claim_digest,
    });
    let operation = fixture.admit_command(binding, &command, PendingKind::AtomicMutation);
    let before = fixture.store.recover().unwrap();
    let pairing = p.target.parts().pairing;
    let client = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: p.initial_client,
            pairing,
        },
        ordinary: OperationStreamId::new(
            OperationStreamKey::new(server(), 4).unwrap(),
            pairing,
            p.initial_client,
        )
        .unwrap(),
        self_revocation: OperationStreamId::new(
            OperationStreamKey::new(server(), 5).unwrap(),
            pairing,
            p.initial_client,
        )
        .unwrap(),
    })
    .unwrap();
    let proof = signed_enrollment(
        reservation,
        &client,
        operation,
        before.store_revision.checked_successor().unwrap(),
    );
    let basis = OperationReductionBasis::new(OperationReductionBasisParts {
        binding: operation,
        base: before.organization.revision,
        resulting: before.organization.revision,
    })
    .unwrap();
    let context = TransactionContext::new(
        before.store_revision,
        before.organization.revision,
        before.organization.revision,
        Some(basis),
    )
    .unwrap();
    let mut prepared = fixture
        .ledger
        .prepare_completion(
            &binding,
            operation.id,
            operation.digest,
            CompactOutcome::new(
                &Outcome::Pairing(Box::new(PairingOutcome::Enrolled(proof.clone()))),
                &CanonicalCodec,
            )
            .unwrap(),
            before.organization.revision,
            context,
        )
        .unwrap();
    fixture
        .ledger
        .extend_prepared(
            &mut prepared,
            vec![
                LedgerWrite::RegisterStream(
                    StreamRecord::new(
                        StreamBinding::from_stream(client.parts().ordinary),
                        StreamPurpose::Ordinary,
                        StreamStatus::Active,
                        0,
                        0,
                        None,
                    )
                    .unwrap(),
                ),
                LedgerWrite::RegisterStream(
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
                    .unwrap(),
                ),
            ],
        )
        .unwrap();
    let authorization = PairingAuthorization::new(PairingAuthorizationParts {
        record: PairingRecord {
            id: pairing,
            device: p.target.parts().device,
            state: PairingState::Authorized,
        },
        authorization_version: FieldVersion::new(1),
        keys: p.keys.clone(),
        clients: vec![client],
        revocation_receipt: None,
    })
    .unwrap();
    let mut consumed = p.clone();
    consumed.state = EnrollmentReservationState::Consumed {
        enrollment: operation,
        receipt: Box::new(proof.clone()),
    };
    let mut marks = before.high_water;
    marks.operation_stream = 5;
    let transaction = fixture.proposal(
        &prepared,
        vec![
            OperationWrite::Pairing(Box::new(PairingWrite::PutAuthorization {
                expected_authorization_version: None,
                value: authorization,
            })),
            OperationWrite::Pairing(Box::new(PairingWrite::PutEnrollmentReservation {
                expected_kind: Some(EnrollmentReservationKind::Reserved),
                value: EnrollmentReservation::new(consumed).unwrap(),
            })),
        ],
        vec![],
        Some(marks),
    );
    (prepared, transaction, proof)
}

#[test]
fn pairing_and_operation_outcome_commit_atomically() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let reservation = reserve_enrollment(&mut fixture);
    let (prepared, proposal, proof) = enrollment_completion(&mut fixture, &reservation);
    let before = fixture.store.recover().unwrap();
    for omit_ledger in [true, false] {
        let mut parts = proposal.parts().clone();
        parts.operation_writes.retain(|w| if omit_ledger { !matches!(w, OperationWrite::Ledger(v) if matches!(v.as_ref(), LedgerWrite::CompleteOperation { .. })) } else { !matches!(w, OperationWrite::Pairing(v) if matches!(v.as_ref(), PairingWrite::PutAuthorization { .. })) });
        reject(&mut fixture.store, parts);
    }
    assert!(matches!(
        fixture.commit_proposal(prepared, proposal),
        TransitionResolution::Completed
    ));
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
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
    let enrollment = &recovered.pairing.as_ref().unwrap().parts().enrollments[0];
    assert!(
        matches!(&enrollment.parts().state, EnrollmentReservationState::Consumed { receipt, .. } if receipt.as_ref() == &proof)
    );
    assert_eq!(
        recovered.store_revision,
        before.store_revision.checked_successor().unwrap()
    );
}

#[test]
fn enrollment_authorization_is_consumed_once() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let reservation = reserve_enrollment(&mut fixture);
    let (prepared, proposal, proof) = enrollment_completion(&mut fixture, &reservation);
    assert!(matches!(
        fixture.commit_proposal(prepared, proposal),
        TransitionResolution::Completed
    ));
    drop(fixture);
    let mut store = reopen(directory.path());
    let before = store.recover().unwrap();
    let consumed = before.pairing.as_ref().unwrap().parts().enrollments[0].clone();
    reject(
        &mut store,
        only_writes(
            &before,
            vec![OperationWrite::Pairing(Box::new(
                PairingWrite::PutEnrollmentReservation {
                    expected_kind: Some(EnrollmentReservationKind::Consumed),
                    value: consumed.clone(),
                },
            ))],
        ),
    );
    let ledger = OperationLedger::recover(server(), before.ledger.unwrap()).unwrap();
    let EnrollmentReservationState::Consumed { enrollment, .. } = consumed.parts().state else {
        panic!("consumption required");
    };
    assert!(
        matches!(ledger.lookup(&StreamBinding::from_stream(enrollment.id.stream()), enrollment.id, Some(enrollment.digest)).unwrap(), OperationLookup::Completed { result: Outcome::Pairing(v), .. } if *v == PairingOutcome::Enrolled(proof))
    );
}

fn revoke(fixture: &mut Fixture) -> (OperationBinding, SignedPairingReceipt) {
    let binding = fixture.restricted();
    let operation = fixture.admit_command(
        binding,
        &Command::RevokePairing {
            pairing: binding.pairing(),
        },
        PendingKind::AtomicMutation,
    );
    let (prepared, proposal, proof) = fixture.revocation(operation);
    assert!(matches!(
        fixture.commit_proposal(prepared, proposal),
        TransitionResolution::Completed
    ));
    (operation, proof)
}
#[test]
fn revocation_and_signed_receipt_survive_lost_reply_and_reopen() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let (operation, proof) = revoke(&mut fixture);
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    let authorization = &recovered.pairing.as_ref().unwrap().parts().authorizations[0];
    assert_eq!(
        authorization.parts().revocation_receipt.as_ref(),
        Some(&proof)
    );
    let ledger = OperationLedger::recover(server(), recovered.ledger.unwrap()).unwrap();
    assert!(
        matches!(ledger.lookup(&StreamBinding::from_stream(operation.id.stream()), operation.id, Some(operation.digest)).unwrap(), OperationLookup::Completed { result: Outcome::Pairing(v), .. } if *v == PairingOutcome::Revoked(proof))
    );
}
fn register_additional_ordinary_stream(fixture: &mut Fixture) -> StreamBinding {
    let before = fixture.store.recover().unwrap();
    let mut marks = before.high_water;
    marks.operation_stream = marks.operation_stream.checked_add(1).unwrap();
    let binding = StreamBinding::from_stream(
        OperationStreamId::new(
            OperationStreamKey::new(server(), marks.operation_stream).unwrap(),
            fixture.binding.pairing(),
            fixture.binding.client(),
        )
        .unwrap(),
    );
    let context = fixture.context();
    let prepared = fixture
        .ledger
        .prepare_register_stream(binding, context)
        .unwrap();
    let proposal = fixture.proposal(&prepared, vec![], vec![], Some(marks));
    assert!(matches!(
        fixture.commit_proposal(prepared, proposal),
        TransitionResolution::StreamRegistered
    ));
    binding
}

fn retire_additional_stream(
    fixture: &mut Fixture,
    prepared: &mut PreparedLedgerTransition,
    proposal: DurableTransaction,
    binding: StreamBinding,
) -> DurableTransaction {
    let write = LedgerWrite::RetireStream {
        binding,
        reason: StreamRetirementReason::PairingRevoked,
    };
    fixture
        .ledger
        .extend_prepared(prepared, vec![write.clone()])
        .unwrap();
    let mut parts = proposal.parts().clone();
    parts
        .operation_writes
        .push(OperationWrite::Ledger(Box::new(write)));
    transaction(parts)
}

#[test]
fn revocation_fences_every_additional_ordinary_stream_atomically() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let additional = register_additional_ordinary_stream(&mut fixture);
    let restricted = fixture.restricted();
    let operation = fixture.admit_command(
        restricted,
        &Command::RevokePairing {
            pairing: restricted.pairing(),
        },
        PendingKind::AtomicMutation,
    );
    let (mut prepared, proposal, proof) = fixture.revocation(operation);
    // The recorded Client's two streams are retired, but its third ordinary
    // stream is still active. The complete candidate must reject this commit.
    reject(&mut fixture.store, proposal.parts().clone());
    let proposal = retire_additional_stream(&mut fixture, &mut prepared, proposal, additional);
    assert!(matches!(
        fixture.commit_proposal(prepared, proposal),
        TransitionResolution::Completed
    ));
    drop(fixture);

    let mut store = reopen(directory.path());
    let recovered = store.recover().unwrap();
    let streams = &recovered.ledger.as_ref().unwrap().streams;
    assert_eq!(streams.len(), 3);
    assert!(
        streams
            .iter()
            .all(|s| s.status == StreamStatus::Retired(StreamRetirementReason::PairingRevoked))
    );
    let context = TransactionContext::new(
        recovered.store_revision,
        recovered.organization.revision,
        recovered.organization.revision,
        None,
    )
    .unwrap();
    let mut ledger = OperationLedger::recover(server(), recovered.ledger.unwrap()).unwrap();
    assert!(matches!(
        ledger.lookup(&restricted, operation.id, Some(operation.digest)).unwrap(),
        OperationLookup::Completed { result: Outcome::Pairing(v), .. } if *v == PairingOutcome::Revoked(proof)
    ));
    assert!(matches!(
        ledger
            .prepare_admission(
                &additional,
                OperationId::new(additional.stream(), OperationSequence::new(1).unwrap()),
                &Command::AssessEndAllSessions { server: server() },
                PendingKind::AtomicMutation,
                context,
            )
            .unwrap(),
        AdmissionPreparation::Existing(OperationLookup::AcknowledgedRetired)
    ));
}

#[test]
fn recovery_rejects_active_additional_ordinary_stream_for_revoked_pairing() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let additional = register_additional_ordinary_stream(&mut fixture);
    let restricted = fixture.restricted();
    let operation = fixture.admit_command(
        restricted,
        &Command::RevokePairing {
            pairing: restricted.pairing(),
        },
        PendingKind::AtomicMutation,
    );
    let (mut prepared, proposal, _) = fixture.revocation(operation);
    let proposal = retire_additional_stream(&mut fixture, &mut prepared, proposal, additional);
    assert!(matches!(
        fixture.commit_proposal(prepared, proposal),
        TransitionResolution::Completed
    ));
    drop(fixture);
    // Only the closed, owned fixture is modified. This models retained state
    // from a writer that fenced only the two streams in PairingClientStreams.
    let mut database = Database::open_existing_for_application(
        directory.path(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    assert!(matches!(
        database.transaction(|sql| sql.execute(
            "UPDATE operation_streams SET status = 0, retirement_reason = NULL WHERE stream_key = ?1",
            [additional.stream().key().local().to_be_bytes().as_slice()],
        )),
        spaceterm_host::storage::CommitDisposition::Durable(1)
    ));
    assert!(matches!(
        SqliteOrganizationStore::open_existing_with_services(
            database,
            identity(),
            StoreLimits::production(),
            services(),
        ),
        Err(StoreFailure::InvalidDurableState)
    ));
}

#[test]
fn revoked_pairing_cannot_be_reactivated() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    revoke(&mut fixture);
    let before = fixture.store.recover().unwrap();
    let old = before.pairing.as_ref().unwrap().parts().authorizations[0].parts();
    let mut reactivated = old.clone();
    reactivated.record.state = PairingState::Authorized;
    reactivated.revocation_receipt = None;
    reactivated.authorization_version = FieldVersion::new(3);
    reject(
        &mut fixture.store,
        only_writes(
            &before,
            vec![OperationWrite::Pairing(Box::new(
                PairingWrite::PutAuthorization {
                    expected_authorization_version: Some(old.authorization_version),
                    value: PairingAuthorization::new(reactivated).unwrap(),
                },
            ))],
        ),
    );
}
#[test]
fn pairing_projection_preserves_both_keys_and_exact_client_streams_on_reopen() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let before = fixture.store.recover().unwrap();
    drop(fixture);
    assert_eq!(
        reopen(directory.path()).recover().unwrap().pairing,
        before.pairing
    );
}
#[test]
fn pairing_keys_and_existing_client_bindings_are_immutable() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let before = fixture.store.recover().unwrap();
    let old = before.pairing.as_ref().unwrap().parts().authorizations[0].parts();
    let mut changed = old.clone();
    changed.authorization_version = FieldVersion::new(2);
    changed.keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: [44; 32],
        pairing_authentication_public_key: [55; 32],
    })
    .unwrap();
    reject(
        &mut fixture.store,
        only_writes(
            &before,
            vec![OperationWrite::Pairing(Box::new(
                PairingWrite::PutAuthorization {
                    expected_authorization_version: Some(old.authorization_version),
                    value: PairingAuthorization::new(changed).unwrap(),
                },
            ))],
        ),
    );
}
#[test]
fn client_binding_cannot_move_to_a_later_pairing() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let before = fixture.store.recover().unwrap();
    let old = before.pairing.as_ref().unwrap().parts().authorizations[0].parts();
    let pairing = PairingId::new(server(), 2).unwrap();
    let client = old.clients[0].parts().client.id;
    let streams = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: client,
            pairing,
        },
        ordinary: OperationStreamId::new(
            OperationStreamKey::new(server(), 4).unwrap(),
            pairing,
            client,
        )
        .unwrap(),
        self_revocation: OperationStreamId::new(
            OperationStreamKey::new(server(), 5).unwrap(),
            pairing,
            client,
        )
        .unwrap(),
    })
    .unwrap();
    let value = PairingAuthorization::new(PairingAuthorizationParts {
        record: PairingRecord {
            id: pairing,
            device: DeviceId::new([77; 16]),
            state: PairingState::Authorized,
        },
        authorization_version: FieldVersion::new(1),
        keys: old.keys.clone(),
        clients: vec![streams],
        revocation_receipt: None,
    })
    .unwrap();
    let mut parts = only_writes(
        &before,
        vec![OperationWrite::Pairing(Box::new(
            PairingWrite::PutAuthorization {
                expected_authorization_version: None,
                value,
            },
        ))],
    );
    parts.resulting_high_water.pairing = 2;
    parts.resulting_high_water.operation_stream = 5;
    reject(&mut fixture.store, parts);
}
#[test]
fn duplicate_auxiliary_cas_targets_roll_back_without_partial_rows() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let before = fixture.store.recover().unwrap();
    let value = before.pairing.as_ref().unwrap().parts().authorizations[0].clone();
    let write = OperationWrite::Pairing(Box::new(PairingWrite::PutAuthorization {
        expected_authorization_version: Some(FieldVersion::new(1)),
        value,
    }));
    reject(
        &mut fixture.store,
        only_writes(&before, vec![write.clone(), write]),
    );
}
#[test]
fn enrollment_reservation_keeps_immutable_claim_and_never_returns_from_invalidated() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let reservation = reserve_enrollment(&mut fixture);
    let before = fixture.store.recover().unwrap();
    let mut changed = reservation.parts().clone();
    changed.claim_digest = RequestDigest::from_bytes([99; 32]);
    changed.state = EnrollmentReservationState::Invalidated;
    reject(
        &mut fixture.store,
        only_writes(
            &before,
            vec![OperationWrite::Pairing(Box::new(
                PairingWrite::PutEnrollmentReservation {
                    expected_kind: Some(EnrollmentReservationKind::Reserved),
                    value: EnrollmentReservation::new(changed).unwrap(),
                },
            ))],
        ),
    );
    let mut invalidated = reservation.parts().clone();
    invalidated.state = EnrollmentReservationState::Invalidated;
    let context = fixture.context();
    let prepared = fixture
        .ledger
        .prepare_retirement(
            &[StreamBinding::from_stream(
                reservation.parts().enrollment_stream,
            )],
            StreamRetirementReason::Expired,
            context,
        )
        .unwrap();
    let proposal = fixture.proposal(
        &prepared,
        vec![OperationWrite::Pairing(Box::new(
            PairingWrite::PutEnrollmentReservation {
                expected_kind: Some(EnrollmentReservationKind::Reserved),
                value: EnrollmentReservation::new(invalidated).unwrap(),
            },
        ))],
        vec![],
        None,
    );
    assert!(matches!(
        fixture.commit_proposal(prepared, proposal),
        TransitionResolution::StreamRetired
    ));
    let before = fixture.store.recover().unwrap();
    reject(
        &mut fixture.store,
        only_writes(
            &before,
            vec![OperationWrite::Pairing(Box::new(
                PairingWrite::PutEnrollmentReservation {
                    expected_kind: Some(EnrollmentReservationKind::Invalidated),
                    value: reservation,
                },
            ))],
        ),
    );
}

#[test]
fn git_intent_and_pending_reservation_commit_before_external_effect() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let binding = fixture.git_admit();
    let before = fixture.store.recover().unwrap();
    assert!(before.git_intents.is_empty());
    fixture.persist_git_progress(binding, GitIntentPhase::Prepared);
    let recovered = fixture.store.recover().unwrap();
    assert_eq!(recovered.git_intents[0].binding(), binding);
    assert_eq!(recovered.git_allocations[0].binding(), binding);
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert_eq!(recovered.git_intents[0].phase(), GitIntentPhase::Prepared);
}
#[test]
fn unfinished_git_intent_is_returned_without_reexecution() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let binding = fixture.git_admit();
    fixture.persist_git_progress(binding, GitIntentPhase::Prepared);
    fixture.persist_git_progress(binding, GitIntentPhase::Started);
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert_eq!(recovered.git_intents[0].phase(), GitIntentPhase::Started);
    assert_eq!(recovered.git_intents[0].binding(), binding);
    assert!(!directory.path().join("worktree").exists());
}
#[test]
fn unfinished_git_projection_and_allocation_survive_reopen_without_filesystem_queries() {
    unfinished_git_intent_is_returned_without_reexecution();
}
#[test]
fn git_completion_and_final_operation_outcome_are_atomic() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let binding = fixture.git_admit();
    for phase in [
        GitIntentPhase::Prepared,
        GitIntentPhase::Started,
        GitIntentPhase::ExternalFinished,
    ] {
        fixture.persist_git_progress(binding, phase);
    }
    let (prepared, proposal) = fixture.git_completion(binding);
    for omit_ledger in [true, false] {
        let mut parts = proposal.parts().clone();
        parts.operation_writes.retain(|w| {
            if omit_ledger {
                !matches!(w, OperationWrite::Ledger(_))
            } else {
                !matches!(w, OperationWrite::GitIntent(_))
            }
        });
        reject(&mut fixture.store, parts);
    }
    assert!(matches!(
        fixture.commit_proposal(prepared, proposal),
        TransitionResolution::Completed
    ));
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
    assert_eq!(recovered.organization.worktrees.len(), 1);
}
#[test]
fn git_completion_without_same_transaction_outcome_is_rejected() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let binding = fixture.git_admit();
    fixture.persist_git_progress(binding, GitIntentPhase::Prepared);
    let before = fixture.store.recover().unwrap();
    reject(
        &mut fixture.store,
        only_writes(
            &before,
            vec![
                OperationWrite::GitIntent(GitIntentMutation::Complete { binding }),
                OperationWrite::GitAllocation(GitAllocationMutation::Abandon { binding }),
            ],
        ),
    );
}
#[test]
fn repository_locator_cannot_be_deleted_while_reconciliation_intent_exists() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let binding = fixture.git_admit();
    fixture.persist_git_progress(binding, GitIntentPhase::Prepared);
    let before = fixture.store.recover().unwrap();
    let repository = before.git_repositories[0].scope().repository;
    reject(
        &mut fixture.store,
        only_writes(
            &before,
            vec![OperationWrite::GitRepository(
                GitRepositoryMutation::Delete(repository),
            )],
        ),
    );
}
#[test]
fn git_phase_and_digest_are_compared_inside_the_physical_transaction() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let binding = fixture.git_admit();
    fixture.persist_git_progress(binding, GitIntentPhase::Prepared);
    let (prepared, proposal) = fixture.git_progress(binding, GitIntentPhase::Started);
    assert!(matches!(
        fixture.commit_proposal(prepared, proposal.clone()),
        TransitionResolution::ExternalProgressRecorded
    ));
    let before = fixture.store.recover().unwrap();
    let mut stale = proposal.parts().clone();
    stale.expected_store_revision = before.store_revision;
    stale.resulting_store_revision = before.store_revision.checked_successor().unwrap();
    reject(&mut fixture.store, stale);
    let mut wrong = binding;
    wrong.digest = RequestDigest::from_bytes([99; 32]);
    reject(
        &mut fixture.store,
        only_writes(
            &before,
            vec![OperationWrite::GitIntent(GitIntentMutation::Advance {
                binding: wrong,
                expected_phase: GitIntentPhase::Started,
                next: GitIntentPhase::ExternalFinished,
                result: Some(RetainedExternalResult {
                    disposition: ExternalDisposition::DesiredStateObserved,
                    failure: None,
                    ended_sessions: 0,
                }),
            })],
        ),
    );
}
#[test]
fn git_intent_cannot_substitute_a_different_locator_for_the_same_repository() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let binding = fixture.git_admit();
    fixture.persist_git_progress(binding, GitIntentPhase::Prepared);
    let before = fixture.store.recover().unwrap();
    let mut changed = before.git_repositories[0].parts().clone();
    changed.main_root = ServerDirectory::new(server(), b"/different".to_vec()).unwrap();
    reject(
        &mut fixture.store,
        only_writes(
            &before,
            vec![OperationWrite::GitRepository(GitRepositoryMutation::Put(
                RepositoryLocator::new(changed).unwrap(),
            ))],
        ),
    );
}
#[test]
fn auxiliary_recovery_checks_reduced_count_limit_before_decoding() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let binding = fixture.git_admit();
    fixture.persist_git_progress(binding, GitIntentPhase::Prepared);
    drop(fixture);
    let limits = StoreLimits::production()
        .with_test_ceiling(limits::Collection::GitIntents, 0)
        .unwrap();
    let database = Database::open_existing_for_application(
        directory.path(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    assert!(matches!(
        SqliteOrganizationStore::open_existing(database, identity(), limits, services()),
        Err(StoreFailure::InvalidDurableState)
    ));
}
#[test]
fn remote_access_defaults_disabled_and_survives_reopen_only_after_commit() {
    let directory = private_directory();
    let mut fixture = ReducedFixture::new(directory.path());
    assert!(
        !fixture
            .reducer
            .snapshot()
            .state()
            .retained
            .as_ref()
            .unwrap()
            .remote_access()
            .value
            .is_enabled()
    );
    let policy = spaceterm_core::remote_access::RemoteAccessPolicy::enabled(
        server(),
        vec!["127.0.0.1:5200".parse().unwrap()],
    )
    .unwrap();
    fixture.execute(
        Command::SetRemoteAccess {
            server: server(),
            expected_version: FieldVersion::new(1),
            value: policy.clone(),
        },
        PendingKind::AtomicMutation,
    );
    drop(fixture);
    assert_eq!(
        reopen(directory.path())
            .recover()
            .unwrap()
            .organization
            .retained
            .unwrap()
            .remote_access()
            .value,
        policy
    );
}
#[test]
fn numeric_runtime_epoch_advance_is_checked_and_durable() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let before = fixture.store.recover().unwrap();
    let mut parts = unchanged_parts(&before);
    parts.resulting_epoch = ServerEpoch::new(before.last_epoch.get() + 2);
    assert!(DurableTransaction::new_with_validation(parts, &services()).is_err());
    let mut parts = unchanged_parts(&before);
    parts.resulting_epoch = before.last_epoch.checked_successor().unwrap();
    assert!(matches!(
        fixture.store.commit(&transaction(parts)),
        CommitDisposition::Durable(_)
    ));
    drop(fixture);
    assert_eq!(
        reopen(directory.path()).recover().unwrap().last_epoch,
        before.last_epoch.checked_successor().unwrap()
    );
}
#[test]
fn native_retirement_projection_preserves_original_request_without_pairing_identity() {
    use spaceterm_core::native_management::*;
    let directory = private_directory();
    let mut fixture = ReducedFixture::new(directory.path());
    let (_, _, pane) = fixture.create();
    fixture.start(pane);
    let request =
        NativeManagementRequestRef::new(server(), [44; 16], RequestDigest::from_bytes([45; 32]))
            .unwrap();
    let prepared = fixture
        .reducer
        .prepare_native_retirement(request, NativeRetirementAction::Stop, ReducerTick::new(11))
        .unwrap();
    fixture.commit_reduction(prepared, None);
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert_eq!(recovered.native_retirements[0].request(), request);
    assert_eq!(recovered.native_retirements[0].sessions().len(), 1);
    assert_eq!(recovered.organization.panes.len(), 1);
}

fn admit_reduction(
    fixture: &mut ReducedFixture,
    command: &Command,
    kind: PendingKind,
) -> ReductionContext {
    let binding = fixture
        .fixture
        .admit_command(fixture.fixture.binding, command, kind);
    let state = fixture.fixture.store.recover().unwrap();
    fixture
        .reducer
        .observe_ledger_commit(&CommitReceipt {
            store_revision: state.store_revision,
            organization_revision: state.organization.revision,
        })
        .unwrap();
    ReductionContext {
        principal: MutationPrincipal {
            pairing: binding.id.stream().pairing(),
            client: binding.id.stream().client(),
        },
        operation: binding.id,
        request_digest: binding.digest,
        now: ReducerTick::new(11),
        facts: ReductionFacts::default(),
    }
}
fn accept_close(
    fixture: &mut ReducedFixture,
    pane: PaneId,
) -> spaceterm_core::retirement::RetirementDescriptor {
    let assessed = fixture.execute(
        Command::AssessClose {
            target: CloseTarget::Pane(pane),
        },
        PendingKind::AtomicMutation,
    );
    let Resolution::Completed(Outcome::Assessment(assessment)) = assessed.resolution else {
        panic!("assessment required");
    };
    let accepted = fixture.execute(
        Command::CloseConfirmed {
            target: CloseTarget::Pane(pane),
            assessment: assessment.reference(),
        },
        PendingKind::External {
            effect: ExternalEffectKind::SessionLifecycle,
        },
    );
    let Resolution::AwaitingRetirement(plan) = accepted.resolution else {
        panic!("retirement required");
    };
    plan.descriptor
}
#[test]
fn pending_retirement_reopens_with_original_acceptance_and_no_cleanup_evidence() {
    use spaceterm_core::retirement::*;
    let directory = private_directory();
    let mut fixture = ReducedFixture::new(directory.path());
    let (_, _, pane) = fixture.create();
    let session = fixture.start(pane);
    let descriptor = accept_close(&mut fixture, pane);
    drop(fixture);
    let mut store = reopen(directory.path());
    let state = store.recover().unwrap();
    assert!(state.organization.workspaces.is_empty());
    let ledger = OperationLedger::recover(server(), state.ledger.unwrap()).unwrap();
    let mut fixture = ReducedFixture::from_recovered(store, ledger, directory.path());
    assert_eq!(
        fixture.reducer.retained().pending_retirements(),
        std::slice::from_ref(&descriptor)
    );
    assert!(
        fixture
            .reducer
            .prepare_retirement_completion(descriptor.batch())
            .is_err()
    );
    fixture.event(OrganizationEvent::RetirementCleanup {
        batch: descriptor.batch(),
        session,
        result: CleanupResult::Confirmed(
            ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
        ),
    });
    let reduction = fixture
        .reducer
        .prepare_retirement_completion(descriptor.batch())
        .unwrap();
    fixture.commit_reduction(reduction, None);
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert!(recovered.organization.workspaces.is_empty());
    let ledger = OperationLedger::recover(server(), recovered.ledger.unwrap()).unwrap();
    let binding = descriptor.binding();
    assert!(
        matches!(ledger.lookup(&StreamBinding::from_stream(binding.id.stream()), binding.id, Some(binding.digest)).unwrap(), OperationLookup::Completed { result: Outcome::Applied, publication_revision } if publication_revision == descriptor.acceptance_revision())
    );
}
#[test]
fn failed_retirement_cleanup_never_recreates_deleted_hierarchy() {
    use spaceterm_core::retirement::*;
    let directory = private_directory();
    let mut fixture = ReducedFixture::new(directory.path());
    let (_, _, pane) = fixture.create();
    let session = fixture.start(pane);
    let descriptor = accept_close(&mut fixture, pane);
    fixture.event(OrganizationEvent::RetirementCleanup {
        batch: descriptor.batch(),
        session,
        result: CleanupResult::Failed(FailureClass::RuntimeUnavailable),
    });
    assert!(
        fixture
            .reducer
            .prepare_retirement_completion(descriptor.batch())
            .is_err()
    );
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert!(recovered.organization.workspaces.is_empty());
    let pending = recovered
        .ledger
        .unwrap()
        .operations
        .into_iter()
        .find_map(|v| match v {
            RecoveredOperationRecord::Pending(v) if v.binding == descriptor.binding() => {
                v.external_progress
            }
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(pending.state(), ExternalProgressState::AcceptedRetirement(v) if matches!(v.parts().sessions[0].cleanup(), RetirementCleanupState::Failed(_)))
    );
}
#[test]
fn repair_pane_directory_commits_and_recovers_with_real_reducer() {
    let directory = private_directory();
    let mut fixture = ReducedFixture::new(directory.path());
    let (_, _, pane) = fixture.create();
    let repaired = ServerDirectory::new(server(), b"/repaired".to_vec()).unwrap();
    let result = fixture.execute(
        Command::RepairPaneDirectory {
            pane,
            expected_version: FieldVersion::new(1),
            value: repaired.clone(),
        },
        PendingKind::AtomicMutation,
    );
    assert!(matches!(
        result.resolution,
        Resolution::Completed(Outcome::Applied)
    ));
    drop(fixture);
    let pane = &reopen(directory.path())
        .recover()
        .unwrap()
        .organization
        .panes[0];
    assert_eq!(pane.starting_directory, repaired);
    assert_eq!(
        pane.confirmed_restoration_directory,
        Versioned {
            value: None,
            version: FieldVersion::new(2)
        }
    );
}
#[test]
fn acknowledgement_only_cannot_fabricate_organization_publication_revision() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    fixture.admit(1);
    fixture.complete(1);
    let (_, proposal) = fixture.acknowledgement();
    let mut parts = proposal.parts().clone();
    parts.resulting_organization_revision = parts
        .expected_organization_revision
        .checked_successor()
        .unwrap();
    reject(&mut fixture.store, parts);
}
#[test]
fn ledger_gap_prefix_binding_and_reservation_corruption_are_rejected() {
    for (index, statement) in [
        "UPDATE operation_streams SET admitted_through = x'0000000000000002' WHERE stream_key = x'0000000000000001'",
        "UPDATE operation_streams SET acknowledged_through = x'0000000000000001' WHERE stream_key = x'0000000000000001'",
        "UPDATE operation_streams SET client_id = x'0000000000000002' WHERE stream_key = x'0000000000000001'",
        "UPDATE operation_entries SET reservation_class = 2",
    ].into_iter().enumerate() {
        let directory = private_directory(); let mut fixture = Fixture::new(directory.path()); fixture.admit(1); drop(fixture);
        // Corrupt only the closed, owned fixture through the real authority.
        let mut database = Database::open_existing_for_application(directory.path(), migrations(), schema::APPLICATION_ID).unwrap();
        let result = database.transaction(|sql| sql.execute(statement, []).map(|_| ()));
        if index == 2 {
            // Composite foreign keys reject a Client substitution before recovery.
            assert!(matches!(result, spaceterm_host::storage::CommitDisposition::NotApplied(spaceterm_host::storage::StorageFailure::InvalidState)));
            assert!(SqliteOrganizationStore::open_existing_with_services(database, identity(), StoreLimits::production(), services()).is_ok());
            continue;
        }
        assert!(matches!(result, spaceterm_host::storage::CommitDisposition::Durable(())), "fixture corruption {index} failed: {result:?}");
        assert!(matches!(SqliteOrganizationStore::open_existing_with_services(database, identity(), StoreLimits::production(), services()), Err(StoreFailure::InvalidDurableState)));
    }
}
#[test]
fn exact_repeat_stream_registration_is_idempotent() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let before = fixture.store.recover().unwrap();
    let context = fixture.context();
    let prepared = fixture
        .ledger
        .prepare_register_stream(fixture.binding, context)
        .unwrap();
    assert!(matches!(
        fixture.commit(prepared, false),
        TransitionResolution::StreamRegistered
    ));
    let after = fixture.store.recover().unwrap();
    assert_eq!(after.ledger, before.ledger);
    assert_eq!(
        after.store_revision,
        before.store_revision.checked_successor().unwrap()
    );
}
#[test]
fn retirement_progress_codec_round_trips_and_rejects_corrupt_envelopes() {
    let directory = private_directory();
    let mut fixture = ReducedFixture::new(directory.path());
    let (_, _, pane) = fixture.create();
    fixture.start(pane);
    let descriptor = accept_close(&mut fixture, pane);
    let bytes = codec::encode_retirement(&descriptor).unwrap();
    assert_eq!(
        codec::decode_retirement(&bytes, server()).unwrap(),
        descriptor
    );
    // Fixed-width fields precede Close(Pane)'s two tags and eight-byte ID.
    let pane_count = 12 + 16 + 8 * 2 + 80 + 80 + 8 + 2 + 8;
    let mut oversized = bytes.clone();
    oversized[pane_count..pane_count + 4].copy_from_slice(
        &(spaceterm_core::retirement::MAX_RETIREMENT_PANES as u32 + 1).to_be_bytes(),
    );
    assert_eq!(
        codec::decode_retirement(&oversized, server()),
        Err(codec::CodecFailure::ResourceLimit)
    );
    let session_count = pane_count + 4 + 33;
    let mut oversized = bytes;
    oversized[session_count..session_count + 4].copy_from_slice(
        &(spaceterm_core::retirement::MAX_RETIREMENT_SESSIONS as u32 + 1).to_be_bytes(),
    );
    assert_eq!(
        codec::decode_retirement(&oversized, server()),
        Err(codec::CodecFailure::ResourceLimit)
    );
    let value = PendingExternalProgress::new(PendingExternalProgressParts {
        binding: descriptor.binding(),
        effect: ExternalEffectKind::SessionLifecycle,
        state: ExternalProgressState::AcceptedRetirement(Box::new(descriptor)),
    })
    .unwrap();
    let bytes = codec::encode_external_progress(&value).unwrap();
    assert_eq!(
        codec::decode_external_progress(&bytes, server()).unwrap(),
        value
    );
    for length in 0..bytes.len() {
        assert!(codec::decode_external_progress(&bytes[..length], server()).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(codec::decode_external_progress(&trailing, server()).is_err());
    let mut version = bytes.clone();
    version[5] = 2;
    assert!(codec::decode_external_progress(&version, server()).is_err());
    let mut kind = bytes.clone();
    kind[7] = 255;
    assert!(codec::decode_external_progress(&kind, server()).is_err());
    assert!(codec::decode_external_progress(&bytes, ServerId::new([99; 16])).is_err());
    let mut tag = bytes;
    tag[12 + 16 + 8 * 4 + 32] = 255;
    assert!(codec::decode_external_progress(&tag, server()).is_err());
}

// The two real owners disagree for an effect-free confirmed close. The store
// cannot invent an accepted retirement to make Applied admissible. Assert the
// fail-closed boundary until issue #520's owning producer supplies that progress.
#[test]
fn effect_free_confirmed_close_is_rejected_before_metadata_deletion() {
    let directory = private_directory();
    let mut fixture = ReducedFixture::new(directory.path());
    let (_, _, pane) = fixture.create();
    let assessed = fixture.execute(
        Command::AssessClose {
            target: CloseTarget::Pane(pane),
        },
        PendingKind::AtomicMutation,
    );
    let Resolution::Completed(Outcome::Assessment(assessment)) = assessed.resolution else {
        panic!("assessment required");
    };
    let command = Command::CloseConfirmed {
        target: CloseTarget::Pane(pane),
        assessment: assessment.reference(),
    };
    let mut context = admit_reduction(
        &mut fixture,
        &command,
        PendingKind::External {
            effect: ExternalEffectKind::SessionLifecycle,
        },
    );
    context.facts.retirement_outcome =
        Some(AppliedRetirementOutcome::new(&CanonicalCodec).unwrap());
    let reduction = fixture.reducer.prepare_command(&command, &context).unwrap();
    assert!(matches!(
        reduction.resolution(),
        Resolution::Completed(Outcome::Applied)
    ));
    let before = fixture.fixture.store.recover().unwrap();
    let basis = reduction
        .writes()
        .operation_writes()
        .iter()
        .find_map(|v| match v {
            OperationWrite::ReductionBasis(v) => Some(*v),
            _ => None,
        })
        .unwrap();
    let transaction_context = TransactionContext::new(
        before.store_revision,
        before.organization.revision,
        reduction.resulting_organization_revision(),
        Some(basis),
    )
    .unwrap();
    let failure = fixture
        .fixture
        .ledger
        .prepare_completion(
            &fixture.fixture.binding,
            context.operation,
            context.request_digest,
            CompactOutcome::new(&Outcome::Applied, &CanonicalCodec).unwrap(),
            reduction.resulting_organization_revision(),
            transaction_context,
        )
        .unwrap_err();
    assert_eq!(failure.class, LedgerFailureClass::InvalidExternalProgress);
    assert_eq!(fixture.fixture.store.recover().unwrap(), before);
    assert_eq!(before.organization.panes[0].id, pane);
    let retry_context = fixture.fixture.context();
    assert!(matches!(
        fixture
            .fixture
            .ledger
            .prepare_admission(
                &fixture.fixture.binding,
                context.operation,
                &command,
                PendingKind::External {
                    effect: ExternalEffectKind::SessionLifecycle
                },
                retry_context,
            )
            .unwrap(),
        AdmissionPreparation::Existing(OperationLookup::Pending)
    ));
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert_eq!(recovered, before);
    let ledger = OperationLedger::recover(server(), recovered.ledger.unwrap()).unwrap();
    assert_eq!(
        ledger
            .lookup(
                &StreamBinding::from_stream(context.operation.stream()),
                context.operation,
                Some(context.request_digest)
            )
            .unwrap(),
        OperationLookup::Pending
    );
}

struct FallbackFixture {
    reduced: ReducedFixture,
    binding: OperationBinding,
    locator: RepositoryLocator,
    reservation: GitAllocationReservation,
    removed_tab: TabRecord,
}
fn fallback_fixture(directory: &std::path::Path) -> FallbackFixture {
    let mut reduced = ReducedFixture::new(directory);
    let (workspace, tab, _) = reduced.create();
    let scope = RepositoryScope {
        server: server(),
        repository: RepositoryId::new(server(), 1).unwrap(),
    };
    let inventory_context = admit_reduction(
        &mut reduced,
        &Command::RefreshWorktrees { scope },
        PendingKind::AtomicMutation,
    );
    let inventory = GitInventoryPatch {
        repository: None,
        directory: ServerDirectory::new(server(), b"/repo".to_vec()).unwrap(),
        expected_inventory_version: None,
        worktrees: vec![
            GitInventoryWorktree {
                id: None,
                directory: ServerDirectory::new(server(), b"/repo".to_vec()).unwrap(),
                main: true,
                missing: false,
                listed: true,
            },
            GitInventoryWorktree {
                id: None,
                directory: ServerDirectory::new(server(), b"/removed".to_vec()).unwrap(),
                main: false,
                missing: false,
                listed: true,
            },
        ],
    };
    let prepared = reduced
        .reducer
        .prepare_git_inventory(&inventory, &inventory_context)
        .unwrap();
    reduced.commit_reduction(prepared, Some(inventory_context.binding()));
    let removed = WorktreeId::new(server(), 2).unwrap();
    reduced.event(OrganizationEvent::WorktreeOrganizationChanged(Box::new(
        WorktreeOrganizationPatch {
            binding: inventory_context.binding(),
            scope,
            expected_inventory_version: Some(FieldVersion::new(1)),
            repositories: vec![],
            worktrees: vec![],
            memberships: vec![WorktreeMembershipEdit {
                tab,
                expected_version: FieldVersion::new(1),
                worktree: Some(removed),
            }],
            settlement: None,
        },
    )));
    let context = admit_reduction(
        &mut reduced,
        &Command::RemoveWorktreeConfirmed {
            scope,
            worktree: removed,
            assessment: AssessmentRef {
                operation: inventory_context.operation,
                digest: inventory_context.request_digest,
            },
            force: false,
        },
        PendingKind::External {
            effect: ExternalEffectKind::GitWrite,
        },
    );
    let plan = WorkspaceFallbackPlan {
        workspace,
        worktree: WorktreeId::new(server(), 1).unwrap(),
        directory: ServerDirectory::new(server(), b"/repo".to_vec()).unwrap(),
    };
    let prepared = reduced
        .reducer
        .prepare_git_allocation(
            &GitAllocationRequest {
                repository: false,
                worktree: false,
                fallbacks: vec![plan.clone()],
            },
            &context,
        )
        .unwrap();
    let Resolution::AllocationReserved(reservation) = prepared.resolution() else {
        panic!("reservation required");
    };
    let reservation = reservation.as_ref().clone();
    reduced.commit_reduction(prepared, None);
    let state = reduced.fixture.store.recover().unwrap();
    let removed_tab = state.organization.tabs[0].clone();
    let pane = state.organization.panes[0].id;
    let locator = RepositoryLocator::new(RepositoryLocatorParts {
        scope,
        main_root: inventory.directory.clone(),
        common_directory: inventory.directory,
    })
    .unwrap();
    let affected = RetainedRemovalScope::new(RetainedRemovalScopeParts {
        server: server(),
        tabs: vec![removed_tab.clone()],
        panes: vec![RetainedAffectedPane {
            hierarchy: AssessedPane {
                workspace,
                tab,
                pane,
                session_generation: SessionGeneration::new(0),
                control_generation: ControlGeneration::new(0),
                context_generation: ContextGeneration::new(0),
            },
            session: None,
            startup_reserved: false,
            work_generation: CloseWorkGeneration::new(0),
        }],
        fallbacks: vec![plan],
    })
    .unwrap();
    let intent = GitWriteIntent::new(GitWriteIntentParts {
        binding: context.binding(),
        repository: locator.clone(),
        action: RetainedGitAction::Remove {
            worktree: removed,
            directory: ServerDirectory::new(server(), b"/removed".to_vec()).unwrap(),
            head: None,
            approval: ApprovedRemoval::Clean,
        },
        phase: GitIntentPhase::Prepared,
        assessment_digest: GitAssessmentDigest::from_bytes([88; 32]),
        affected,
        allocation: context.binding(),
        external_result: None,
    })
    .unwrap();
    let progress = PendingExternalProgress::new(PendingExternalProgressParts {
        binding: context.binding(),
        effect: ExternalEffectKind::GitWrite,
        state: ExternalProgressState::RecordedIntent {
            reference: ExternalIntentReference::new(ExternalIntentReferenceParts {
                binding: context.binding(),
                scope: ExternalIntentScope::Git(scope),
                phase: ExternalIntentPhase::Prepared,
                recorded_at: state.store_revision.checked_successor().unwrap(),
            })
            .unwrap(),
            retirement: None,
        },
    })
    .unwrap();
    let transaction_context = reduced.fixture.context();
    let prepared = reduced
        .fixture
        .ledger
        .prepare_external_progress(
            &reduced.fixture.binding,
            context.operation,
            context.request_digest,
            progress,
            transaction_context,
        )
        .unwrap();
    let proposal = reduced.fixture.proposal(
        &prepared,
        vec![
            OperationWrite::GitRepository(GitRepositoryMutation::Put(locator.clone())),
            OperationWrite::GitIntent(GitIntentMutation::Insert(Box::new(intent))),
        ],
        vec![],
        None,
    );
    assert!(matches!(
        reduced.fixture.commit_proposal(prepared, proposal),
        TransitionResolution::ExternalProgressRecorded
    ));
    // Admission/progress receipts carry no organization authority.
    let state = reduced.fixture.store.recover().unwrap();
    reduced
        .reducer
        .observe_ledger_commit(&CommitReceipt {
            store_revision: state.store_revision,
            organization_revision: state.organization.revision,
        })
        .unwrap();
    for phase in [GitIntentPhase::Started, GitIntentPhase::ExternalFinished] {
        reduced
            .fixture
            .persist_git_progress(context.binding(), phase);
        let state = reduced.fixture.store.recover().unwrap();
        reduced
            .reducer
            .observe_ledger_commit(&CommitReceipt {
                store_revision: state.store_revision,
                organization_revision: state.organization.revision,
            })
            .unwrap();
    }
    // Exercise settlement after a cold restart, not an in-memory reservation.
    drop(reduced);
    let mut store = reopen(directory);
    let ledger =
        OperationLedger::recover(server(), store.recover().unwrap().ledger.unwrap()).unwrap();
    let reduced = ReducedFixture::from_recovered(store, ledger, directory);
    FallbackFixture {
        reduced,
        binding: context.binding(),
        locator,
        reservation,
        removed_tab,
    }
}
fn fallback_completion(
    fixture: &mut FallbackFixture,
) -> (
    PreparedReduction,
    PreparedLedgerTransition,
    DurableTransaction,
) {
    let before = fixture.reduced.fixture.store.recover().unwrap();
    let creator_still_authorized = before
        .pairing
        .as_ref()
        .unwrap()
        .parts()
        .authorizations
        .iter()
        .any(|v| {
            v.parts().record.id == fixture.binding.id.stream().pairing()
                && v.parts().record.state == PairingState::Authorized
        });
    let patch = WorktreeOrganizationPatch {
        binding: fixture.binding,
        scope: fixture.locator.scope(),
        expected_inventory_version: Some(FieldVersion::new(1)),
        repositories: vec![],
        worktrees: vec![],
        memberships: vec![],
        settlement: Some(GitOrganizationSettlement {
            reservation: fixture.reservation.clone(),
            phase: GitIntentPhase::ExternalFinished,
            remove_tabs: vec![fixture.removed_tab.clone()],
            creator_still_authorized,
        }),
    };
    let reduction = fixture
        .reduced
        .reducer
        .prepare_event(&OrganizationEvent::WorktreeOrganizationChanged(Box::new(
            patch,
        )))
        .unwrap();
    let before = fixture.reduced.fixture.store.recover().unwrap();
    let mut parts = unchanged_parts(&before);
    parts.resulting_organization_revision = reduction.resulting_organization_revision();
    parts.resulting_high_water = reduction.writes().resulting_high_water();
    reduction.writes().append_to(&mut parts).unwrap();
    let basis = parts
        .operation_writes
        .iter()
        .find_map(|w| match w {
            OperationWrite::ReductionBasis(v) => Some(*v),
            _ => None,
        })
        .unwrap();
    let context = TransactionContext::new(
        before.store_revision,
        before.organization.revision,
        parts.resulting_organization_revision,
        Some(basis),
    )
    .unwrap();
    let outcome = Outcome::Git(
        GitMutationOutcome::new(GitMutationOutcomeParts::Removed {
            worktree: WorktreeId::new(server(), 2).unwrap(),
            closed_tabs: 1,
            ended_sessions: 0,
            revision: parts.resulting_organization_revision,
        })
        .unwrap(),
    );
    let prepared = fixture
        .reduced
        .fixture
        .ledger
        .prepare_completion(
            &fixture.reduced.fixture.binding,
            fixture.binding.id,
            fixture.binding.digest,
            CompactOutcome::new(&outcome, &CanonicalCodec).unwrap(),
            parts.resulting_organization_revision,
            context,
        )
        .unwrap();
    prepared.projection().append_to(&mut parts).unwrap();
    parts
        .operation_writes
        .push(OperationWrite::GitIntent(GitIntentMutation::Complete {
            binding: fixture.binding,
        }));
    (reduction, prepared, transaction(parts))
}
#[test]
fn reserved_fallback_completion_with_real_producers_preserves_worktree_on_reopen() {
    let directory = private_directory();
    let mut fixture = fallback_fixture(directory.path());
    let fallback = fixture.reservation.parts().fallbacks[0].clone();
    let (reduction, prepared, proposal) = fallback_completion(&mut fixture);
    let CommitDisposition::Durable(receipt) = fixture.reduced.fixture.store.commit(&proposal)
    else {
        panic!("fallback completion must be durable");
    };
    fixture
        .reduced
        .fixture
        .ledger
        .resolve_commit(prepared, CommitDisposition::Durable(receipt))
        .unwrap();
    fixture
        .reduced
        .reducer
        .apply_committed(reduction, &receipt)
        .unwrap();
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert!(recovered.git_allocations.is_empty() && recovered.git_intents.is_empty());
    assert_eq!(recovered.organization.tabs[0].id, fallback.tab);
    assert_eq!(
        recovered.organization.tabs[0].worktree,
        Some(fallback.plan.worktree)
    );
    assert_eq!(recovered.organization.panes[0].id, fallback.pane);
}
#[test]
fn reserved_fallback_tab_cannot_omit_or_substitute_worktree_membership() {
    let directory = private_directory();
    let mut fixture = fallback_fixture(directory.path());
    let (_, _, proposal) = fallback_completion(&mut fixture);
    for membership in [None, Some(WorktreeId::new(server(), 2).unwrap())] {
        let mut parts = proposal.parts().clone();
        for write in &mut parts.organization_writes {
            if let OrganizationWrite::PutTab(value) = write {
                value.worktree = membership;
            }
        }
        reject(&mut fixture.reduced.fixture.store, parts);
    }
}
#[test]
fn reserved_fallback_requires_original_pending_binding_and_atomic_consumption() {
    let directory = private_directory();
    let mut fixture = fallback_fixture(directory.path());
    let (_, _, proposal) = fallback_completion(&mut fixture);
    for omit in [0, 1, 2] {
        let mut parts = proposal.parts().clone();
        parts.operation_writes.retain(|w| match omit {
            0 => !matches!(w, OperationWrite::GitAllocation(_)),
            1 => !matches!(w, OperationWrite::Ledger(_)),
            _ => !matches!(w, OperationWrite::GitIntent(_)),
        });
        reject(&mut fixture.reduced.fixture.store, parts);
    }
    let mut parts = proposal.parts().clone();
    for write in &mut parts.operation_writes {
        if let OperationWrite::GitAllocation(GitAllocationMutation::Consume { binding }) = write {
            binding.digest = RequestDigest::from_bytes([99; 32]);
        }
    }
    reject(&mut fixture.reduced.fixture.store, parts);
}
#[test]
fn kill_workspace_does_not_remove_unfinished_git_reconciliation_data() {
    let directory = private_directory();
    let mut reduced = ReducedFixture::new(directory.path());
    let (workspace, _, pane) = reduced.create();
    let binding = reduced.fixture.git_admit();
    reduced
        .fixture
        .persist_git_progress(binding, GitIntentPhase::Prepared);
    reduced
        .fixture
        .persist_git_progress(binding, GitIntentPhase::Started);
    // The Git fixture reserves organization IDs. Reconstruct the reducer from
    // their committed marks rather than treating a ledger receipt as allocation.
    drop(reduced);
    let mut store = reopen(directory.path());
    let ledger =
        OperationLedger::recover(server(), store.recover().unwrap().ledger.unwrap()).unwrap();
    let mut reduced = ReducedFixture::from_recovered(store, ledger, directory.path());
    let runtime = reduced.reducer.snapshot().runtime()[0];
    reduced.event(OrganizationEvent::ControlChanged {
        pane,
        expected: runtime.control,
        replacement: ControlRecord::Held {
            client: reduced.fixture.binding.client(),
            generation: runtime.control_generation.checked_successor().unwrap(),
        },
    });
    reduced.start(pane);
    let assessed = reduced.execute(
        Command::AssessKillWorkspace { workspace },
        PendingKind::AtomicMutation,
    );
    let Resolution::Completed(Outcome::Assessment(assessment)) = assessed.resolution else {
        panic!("assessment required");
    };
    let result = reduced.execute(
        Command::KillWorkspaceConfirmed {
            workspace,
            assessment: assessment.reference(),
        },
        PendingKind::External {
            effect: ExternalEffectKind::SessionLifecycle,
        },
    );
    assert!(matches!(
        result.resolution,
        Resolution::AwaitingRetirement(_)
    ));
    drop(reduced);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert!(recovered.organization.workspaces.is_empty());
    assert_eq!(recovered.git_intents[0].binding(), binding);
    assert_eq!(recovered.git_intents[0].phase(), GitIntentPhase::Started);
}

#[test]
fn mixed_database_recovers_admitted_completed_pending_retirement_and_started_git() {
    let directory = private_directory();
    let mut reduced = ReducedFixture::new(directory.path());
    let (_, _, pane) = reduced.create();
    reduced.start(pane);
    let retirement = accept_close(&mut reduced, pane);
    let git = reduced.fixture.git_admit();
    reduced
        .fixture
        .persist_git_progress(git, GitIntentPhase::Prepared);
    reduced
        .fixture
        .persist_git_progress(git, GitIntentPhase::Started);
    let command = reduced.fixture.command();
    let admission = reduced.fixture.admit_command(
        reduced.fixture.binding,
        &command,
        PendingKind::AtomicMutation,
    );
    let before = reduced.fixture.store.recover().unwrap();
    drop(reduced);
    let mut store = reopen(directory.path());
    let recovered = store.recover().unwrap();
    assert_eq!(recovered, before);
    assert!(recovered.organization.workspaces.is_empty());
    assert_eq!(recovered.git_intents[0].binding(), git);
    let ledger = OperationLedger::recover(server(), recovered.ledger.unwrap()).unwrap();
    assert!(matches!(
        ledger
            .lookup(
                &StreamBinding::from_stream(admission.id.stream()),
                admission.id,
                Some(admission.digest)
            )
            .unwrap(),
        OperationLookup::Pending
    ));
    assert!(matches!(
        ledger
            .lookup(
                &StreamBinding::from_stream(retirement.binding().id.stream()),
                retirement.binding().id,
                Some(retirement.binding().digest)
            )
            .unwrap(),
        OperationLookup::Pending
    ));
    let reduced = ReducedFixture::from_recovered(store, ledger, directory.path());
    assert_eq!(
        reduced.reducer.retained().pending_retirements(),
        std::slice::from_ref(&retirement)
    );
}
#[test]
fn acknowledgement_changes_store_revision_without_changing_organization_revision() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    fixture.admit(1);
    fixture.complete(1);
    let before = fixture.store.recover().unwrap();
    let (prepared, proposal) = fixture.acknowledgement();
    assert!(matches!(
        fixture.commit_proposal(prepared, proposal),
        TransitionResolution::Acknowledged { through: 1 }
    ));
    let after = fixture.store.recover().unwrap();
    assert_eq!(after.organization, before.organization);
    assert_eq!(
        after.store_revision,
        before.store_revision.checked_successor().unwrap()
    );
}
#[test]
fn enrollment_acknowledgement_compacts_only_original_registered_stream() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let reservation = reserve_enrollment(&mut fixture);
    let (prepared, proposal, proof) = enrollment_completion(&mut fixture, &reservation);
    fixture.commit_proposal(prepared, proposal);
    let binding = StreamBinding::from_stream(reservation.parts().enrollment_stream);
    let context = fixture.context();
    let prepared = fixture
        .ledger
        .prepare_acknowledgement(&binding, 1, context)
        .unwrap()
        .unwrap();
    let proposal = fixture.proposal(&prepared, vec![], vec![], None);
    fixture.commit_proposal(prepared, proposal);
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    let rows = recovered.ledger.as_ref().unwrap();
    assert_eq!(
        rows.streams
            .iter()
            .find(|v| v.binding == binding)
            .unwrap()
            .acknowledged_through,
        1
    );
    assert_eq!(
        rows.streams
            .iter()
            .filter(|v| v.acknowledged_through == 0)
            .count(),
        4
    );
    assert!(
        matches!(&recovered.pairing.unwrap().parts().enrollments[0].parts().state, EnrollmentReservationState::Consumed { receipt, .. } if receipt.as_ref() == &proof)
    );
}
#[test]
fn restricted_acknowledgement_preserves_original_unacknowledged_receipt() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    fixture.admit(1);
    fixture.complete(1);
    let (operation, proof) = revoke(&mut fixture);
    let (prepared, proposal) = fixture.acknowledgement();
    fixture.commit_proposal(prepared, proposal);
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert_eq!(
        recovered.pairing.unwrap().parts().authorizations[0]
            .parts()
            .revocation_receipt
            .as_ref(),
        Some(&proof)
    );
    let ledger = OperationLedger::recover(server(), recovered.ledger.unwrap()).unwrap();
    assert!(
        matches!(ledger.lookup(&StreamBinding::from_stream(operation.id.stream()), operation.id, Some(operation.digest)).unwrap(), OperationLookup::Completed { result: Outcome::Pairing(v), .. } if *v == PairingOutcome::Revoked(proof))
    );
}
#[test]
fn external_progress_codec_preserves_every_awaiting_effect_and_recorded_phase() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let binding = fixture.git_admit();
    for effect in [
        ExternalEffectKind::GitWrite,
        ExternalEffectKind::SessionLifecycle,
        ExternalEffectKind::ServerManagement,
    ] {
        let value = PendingExternalProgress::new(PendingExternalProgressParts {
            binding,
            effect,
            state: ExternalProgressState::AwaitingIntent,
        })
        .unwrap();
        assert_eq!(
            codec::decode_external_progress(
                &codec::encode_external_progress(&value).unwrap(),
                server()
            )
            .unwrap(),
            value
        );
    }
    for (phase, scope, effect) in [
        (
            ExternalIntentPhase::Prepared,
            ExternalIntentScope::Git(RepositoryScope {
                server: server(),
                repository: RepositoryId::new(server(), 1).unwrap(),
            }),
            ExternalEffectKind::GitWrite,
        ),
        (
            ExternalIntentPhase::Started,
            ExternalIntentScope::Git(RepositoryScope {
                server: server(),
                repository: RepositoryId::new(server(), 1).unwrap(),
            }),
            ExternalEffectKind::GitWrite,
        ),
        (
            ExternalIntentPhase::ExternalFinished,
            ExternalIntentScope::Git(RepositoryScope {
                server: server(),
                repository: RepositoryId::new(server(), 1).unwrap(),
            }),
            ExternalEffectKind::GitWrite,
        ),
        (
            ExternalIntentPhase::Started,
            ExternalIntentScope::Management,
            ExternalEffectKind::ServerManagement,
        ),
    ] {
        let value = PendingExternalProgress::new(PendingExternalProgressParts {
            binding,
            effect,
            state: ExternalProgressState::RecordedIntent {
                reference: ExternalIntentReference::new(ExternalIntentReferenceParts {
                    binding,
                    scope,
                    phase,
                    recorded_at: StoreRevision::new(20),
                })
                .unwrap(),
                retirement: None,
            },
        })
        .unwrap();
        assert_eq!(
            codec::decode_external_progress(
                &codec::encode_external_progress(&value).unwrap(),
                server()
            )
            .unwrap(),
            value
        );
    }
}

#[test]
fn started_git_settlement_after_end_all_with_revoked_creator_real_composition() {
    use spaceterm_core::retirement::*;
    let directory = private_directory();
    let mut fixture = fallback_fixture(directory.path());
    let additional = register_additional_ordinary_stream(&mut fixture.reduced.fixture);
    let state = fixture.reduced.fixture.store.recover().unwrap();
    // No runtime exists yet. Reconstruct with the stream allocation high-water
    // before creating any live state; an observation receipt has no allocator.
    fixture.reduced.reducer = OrganizationReducer::from_recovered(
        RetainedOrganization::new(state.organization, state.high_water, home())
            .with_git_allocations(state.git_allocations),
        state.store_revision,
        state.last_epoch,
        OrganizationLimits::default(),
    )
    .unwrap();
    let pane = fixture.reduced.reducer.snapshot().state().panes[0].id;
    let runtime = fixture.reduced.reducer.snapshot().runtime()[0];
    fixture.reduced.event(OrganizationEvent::ControlChanged {
        pane,
        expected: runtime.control,
        replacement: ControlRecord::Held {
            client: fixture.binding.id.stream().client(),
            generation: runtime.control_generation.checked_successor().unwrap(),
        },
    });
    let session = fixture.reduced.start(pane);
    let assessed = fixture.reduced.execute(
        Command::AssessEndAllSessions { server: server() },
        PendingKind::AtomicMutation,
    );
    let Resolution::Completed(Outcome::Assessment(assessment)) = assessed.resolution else {
        panic!("assessment required");
    };
    let accepted = fixture.reduced.execute(
        Command::EndAllSessions {
            server: server(),
            assessment: assessment.reference(),
        },
        PendingKind::External {
            effect: ExternalEffectKind::SessionLifecycle,
        },
    );
    let Resolution::AwaitingRetirement(plan) = accepted.resolution else {
        panic!("retirement required");
    };
    fixture.reduced.event(OrganizationEvent::RetirementCleanup {
        batch: plan.descriptor.batch(),
        session,
        result: CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
    });
    let prepared = fixture
        .reduced
        .reducer
        .prepare_retirement_completion(plan.descriptor.batch())
        .unwrap();
    fixture.reduced.commit_reduction(prepared, None);
    let stream = fixture.reduced.fixture.restricted();
    let operation = fixture.reduced.fixture.admit_command(
        stream,
        &Command::RevokePairing {
            pairing: stream.pairing(),
        },
        PendingKind::AtomicMutation,
    );
    let state = fixture.reduced.fixture.store.recover().unwrap();
    fixture
        .reduced
        .reducer
        .observe_ledger_commit(&CommitReceipt {
            store_revision: state.store_revision,
            organization_revision: state.organization.revision,
        })
        .unwrap();
    let (mut prepared, proposal, _) = fixture.reduced.fixture.revocation(operation);
    reject(&mut fixture.reduced.fixture.store, proposal.parts().clone());
    let proposal = retire_additional_stream(
        &mut fixture.reduced.fixture,
        &mut prepared,
        proposal,
        additional,
    );
    assert!(matches!(
        fixture.reduced.fixture.commit_proposal(prepared, proposal),
        TransitionResolution::Completed
    ));
    let context = fixture.reduced.fixture.context();
    assert!(matches!(
        fixture
            .reduced
            .fixture
            .ledger
            .prepare_admission(
                &additional,
                OperationId::new(additional.stream(), OperationSequence::new(1).unwrap()),
                &Command::AssessEndAllSessions { server: server() },
                PendingKind::AtomicMutation,
                context,
            )
            .unwrap(),
        AdmissionPreparation::Existing(OperationLookup::AcknowledgedRetired)
    ));
    let state = fixture.reduced.fixture.store.recover().unwrap();
    fixture
        .reduced
        .reducer
        .observe_ledger_commit(&CommitReceipt {
            store_revision: state.store_revision,
            organization_revision: state.organization.revision,
        })
        .unwrap();
    let (reduction, prepared, proposal) = fallback_completion(&mut fixture);
    let CommitDisposition::Durable(receipt) = fixture.reduced.fixture.store.commit(&proposal)
    else {
        panic!("Started settlement must complete");
    };
    fixture
        .reduced
        .fixture
        .ledger
        .resolve_commit(prepared, CommitDisposition::Durable(receipt))
        .unwrap();
    fixture
        .reduced
        .reducer
        .apply_committed(reduction, &receipt)
        .unwrap();
    assert_eq!(
        fixture.reduced.reducer.snapshot().runtime()[0].control,
        ControlRecord::Unclaimed
    );
    assert_eq!(
        fixture.reduced.reducer.snapshot().runtime()[0].phase,
        SessionPhase::NotStarted
    );
    drop(fixture);
    let recovered = reopen(directory.path()).recover().unwrap();
    assert!(recovered.git_intents.is_empty() && recovered.git_allocations.is_empty());
    assert_eq!(
        recovered.organization.tabs[0].worktree,
        Some(WorktreeId::new(server(), 1).unwrap())
    );
}
#[test]
fn repair_directory_unavailable_pane_restores_startup_eligibility_without_launching() {
    let directory = private_directory();
    let mut reduced = ReducedFixture::new(directory.path());
    let (_, _, pane) = reduced.create();
    reduced.event(OrganizationEvent::DirectoryUnavailable {
        pane,
        expected_generation: SessionGeneration::new(0),
    });
    assert_eq!(
        reduced.reducer.snapshot().runtime()[0].phase,
        SessionPhase::DirectoryUnavailable
    );
    reduced.execute(
        Command::RepairPaneDirectory {
            pane,
            expected_version: FieldVersion::new(1),
            value: ServerDirectory::new(server(), b"/available".to_vec()).unwrap(),
        },
        PendingKind::AtomicMutation,
    );
    assert_eq!(
        reduced.reducer.snapshot().runtime()[0].phase,
        SessionPhase::NotStarted
    );
    assert_eq!(
        reduced.reducer.snapshot().runtime()[0].session_generation,
        SessionGeneration::new(0)
    );
    drop(reduced);
    assert_eq!(
        reopen(directory.path())
            .recover()
            .unwrap()
            .organization
            .panes[0]
            .starting_directory
            .bytes(),
        b"/available"
    );
}

#[test]
fn revocation_requires_both_authorization_and_exact_signed_ledger_outcome() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let stream = fixture.restricted();
    let operation = fixture.admit_command(
        stream,
        &Command::RevokePairing {
            pairing: stream.pairing(),
        },
        PendingKind::AtomicMutation,
    );
    let (_, proposal, _) = fixture.revocation(operation);
    let mut omitted = proposal.parts().clone();
    omitted
        .operation_writes
        .retain(|v| !matches!(v, OperationWrite::Pairing(_)));
    reject(&mut fixture.store, omitted);
    let mut mismatched = proposal.parts().clone();
    for write in &mut mismatched.operation_writes {
        if let OperationWrite::Ledger(v) = write
            && let LedgerWrite::CompleteOperation { record, .. } = v.as_mut()
        {
            record.outcome = CompactOutcome::new(&Outcome::Applied, &CanonicalCodec).unwrap();
        }
    }
    reject(&mut fixture.store, mismatched);
}
#[test]
fn enrollment_consumption_requires_the_exact_signed_ledger_outcome() {
    let directory = private_directory();
    let mut fixture = Fixture::new(directory.path());
    let reservation = reserve_enrollment(&mut fixture);
    let (_, proposal, _) = enrollment_completion(&mut fixture, &reservation);
    let mut mismatched = proposal.parts().clone();
    for write in &mut mismatched.operation_writes {
        if let OperationWrite::Ledger(v) = write
            && let LedgerWrite::CompleteOperation { record, .. } = v.as_mut()
        {
            record.outcome = CompactOutcome::new(&Outcome::Applied, &CanonicalCodec).unwrap();
        }
    }
    reject(&mut fixture.store, mismatched);
}
