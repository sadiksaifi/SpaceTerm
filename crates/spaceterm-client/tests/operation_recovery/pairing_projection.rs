use super::*;
use spaceterm_client::journal::*;
use spaceterm_core::{identity::*, operations::*, pairing::*};
use std::sync::Arc;

async fn empty_journal() -> Arc<JournalController> {
    let codec = Arc::new(Codec::default());
    Arc::new(
        JournalController::recover(
            Arc::new(MemoryJournal::new(codec.clone())),
            codec,
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap(),
    )
}
fn slot(generation: u64, device: bool) -> JournalCredentialSlot {
    JournalCredentialSlot {
        namespace: "dev.spaceterm.desktop".into(),
        server: (!device).then_some(server()),
        reference: CredentialReference::new([if device { 9 } else { 4 }; 16]),
        public_key: None,
        route_reference: None,
        generation: LocalPairingGeneration::new(generation).unwrap(),
    }
}
fn prepared() -> (JournalEnrollment, SavedBinding) {
    let binding = binding(2);
    let existing = saved(binding.clone());
    let keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: [5; 32],
        pairing_authentication_public_key: [6; 32],
    })
    .unwrap();
    let enrollment = OperationBinding {
        id: OperationId::new(
            OperationStreamId::new(
                OperationStreamKey::new(server(), 1).unwrap(),
                binding.target().pairing(),
                binding.target().client(),
            )
            .unwrap(),
            OperationSequence::new(1).unwrap(),
        ),
        digest: RequestDigest::from_bytes([7; 32]),
    };
    let prepared = JournalEnrollment {
        target: SavedTarget::from_target(binding.target()),
        keys: keys.clone(),
        route_reference: RevocationRouteReference::new([8; 16]),
        enrollment,
        reservation: Some(JournalEnrollmentReservation {
            ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
                server: server(),
                nonce: [10; 16],
            })
            .unwrap(),
            claim_digest: RequestDigest::from_bytes([11; 32]),
        }),
    };
    let proof = SignedPairingReceipt::new(SignedPairingReceiptParts {
        schema: 1,
        claims: PairingReceiptBody::Enrollment {
            target: PairingTarget::new(PairingTargetParts {
                server: server(),
                pairing: binding.target().pairing(),
                device: binding.target().device(),
            })
            .unwrap(),
            keys,
            client: existing.streams().clone(),
            enrollment,
            revision: spaceterm_core::authority::StoreRevision::new(5),
        },
        canonical_body: vec![12],
        signature: vec![1; 64],
    })
    .unwrap();
    (
        prepared,
        SavedBinding::verified(binding, existing.streams().clone(), proof, &Verifier).unwrap(),
    )
}
async fn reserve(journal: &JournalController) {
    journal
        .transact(vec![ClientJournalChange::ReserveCredential(slot(1, true))])
        .await
        .unwrap();
    journal
        .transact(vec![ClientJournalChange::RecordCredentialPublicKey {
            reference: slot(1, true).reference,
            public_key: [5; 32],
        }])
        .await
        .unwrap();
    journal
        .transact(vec![ClientJournalChange::ReserveCredential(slot(2, false))])
        .await
        .unwrap();
    journal
        .transact(vec![ClientJournalChange::RecordCredentialPublicKey {
            reference: slot(2, false).reference,
            public_key: [6; 32],
        }])
        .await
        .unwrap();
}

#[tokio::test]
async fn pairing_native_slot_is_reserved_before_enrollment_and_never_reused() {
    let journal = empty_journal().await;
    reserve(&journal).await;
    let (prepared, _) = prepared();
    journal
        .transact(vec![ClientJournalChange::PrepareEnrollment(Box::new(
            prepared.clone(),
        ))])
        .await
        .unwrap();
    let state = journal.snapshot().unwrap();
    assert_eq!(state.pairing().generation_high_water(), 2);
    assert_eq!(state.pairing().prepared()[0], prepared);
    assert_eq!(
        state.pairing().credential_preparations(),
        &[slot(1, true).reference]
    );
    let mut reused = slot(3, false);
    reused.reference = slot(2, false).reference;
    assert!(
        journal
            .transact(vec![ClientJournalChange::ReserveCredential(reused)])
            .await
            .is_err()
    );
    assert_eq!(
        journal
            .snapshot()
            .unwrap()
            .pairing()
            .generation_high_water(),
        2
    );
}

#[tokio::test]
async fn enrollment_binding_and_restricted_acknowledgement_share_one_journal_commit() {
    let journal = empty_journal().await;
    reserve(&journal).await;
    let (prepared, saved) = prepared();
    journal
        .transact(vec![ClientJournalChange::PrepareEnrollment(Box::new(
            prepared.clone(),
        ))])
        .await
        .unwrap();
    let before = journal.snapshot().unwrap().revision();
    let receipt = journal
        .transact(vec![ClientJournalChange::SaveEnrollment {
            binding: Box::new(saved.clone()),
            acknowledgement: Box::new(JournalEnrollmentAcknowledgement {
                prepared: prepared.clone(),
                device_credential: slot(1, true).reference,
                receipt: saved.proof().clone(),
            }),
        }])
        .await
        .unwrap();
    let state = journal.snapshot().unwrap();
    assert_eq!(receipt.revision(), before.checked_successor().unwrap());
    assert!(state.ordinary_use_allowed(&saved.binding().unwrap()));
    assert!(state.pairing().prepared().is_empty());
    assert_eq!(state.pairing().acknowledgements()[0].prepared, prepared);
    journal
        .transact(vec![ClientJournalChange::FinishEnrollmentAcknowledgement(
            prepared.enrollment.id,
        )])
        .await
        .unwrap();
    assert!(
        journal
            .snapshot()
            .unwrap()
            .pairing()
            .acknowledgements()
            .is_empty()
    );
    assert_eq!(journal.snapshot().unwrap().pairing().slots().len(), 2);
}

#[tokio::test]
async fn enrollment_activation_transfers_reserved_capacity_without_another_task_slot() {
    let codec = Arc::new(Codec::default());
    let journal = JournalController::recover(
        Arc::new(MemoryJournal::new(codec.clone())),
        codec,
        Arc::new(Verifier),
        JournalLimits {
            pending_revocations: 2,
            ..JournalLimits::default()
        },
    )
    .await
    .unwrap();
    reserve(&journal).await;
    let (prepared, saved) = prepared();
    journal
        .transact(vec![ClientJournalChange::PrepareEnrollment(Box::new(
            prepared.clone(),
        ))])
        .await
        .unwrap();
    journal
        .transact(vec![ClientJournalChange::SaveEnrollment {
            binding: Box::new(saved.clone()),
            acknowledgement: Box::new(JournalEnrollmentAcknowledgement {
                prepared,
                device_credential: slot(1, true).reference,
                receipt: saved.proof().clone(),
            }),
        }])
        .await
        .unwrap();
    assert!(
        journal
            .snapshot()
            .unwrap()
            .ordinary_use_allowed(&saved.binding().unwrap())
    );
}
