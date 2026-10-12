#[path = "support/operation_ledger.rs"]
mod support;
use spaceterm_core::operation_ledger::canonical::CanonicalCodec;
use spaceterm_core::operation_ledger::model::*;
use spaceterm_core::operation_ledger::*;
use spaceterm_core::operations::*;
use spaceterm_core::storage::*;
use support::*;

#[test]
fn disconnect_and_permit_drop_do_not_release_pending_reservations() {
    let mut h = Harness::new();
    h.register(binding(1));
    for n in 1..=3 {
        let _permit = h.admit(binding(1), n, &create());
    }
    assert_class(
        h.prepare(binding(1), 4, &create()),
        LedgerFailureClass::QuotaExceeded,
    );
    assert_eq!(h.rows.entries.len(), 3);
}
#[test]
fn ordinary_stream_quota_preserves_safety_reserve() {
    let mut h = Harness::new();
    h.register(binding(1));
    for n in 1..=3 {
        h.admit(binding(1), n, &create());
    }
    assert_class(
        h.prepare(binding(1), 4, &create()),
        LedgerFailureClass::QuotaExceeded,
    );
    h.admit(binding(1), 4, &safety());
    assert_class(
        h.prepare(binding(1), 5, &safety()),
        LedgerFailureClass::QuotaExceeded,
    );
}
#[test]
fn ordinary_global_quota_preserves_safety_reserve() {
    let mut h = Harness::new();
    for k in 1..=4 {
        h.register(binding(k));
        for n in 1..=if k == 4 { 1 } else { 3 } {
            h.admit(binding(k), n, &create());
        }
    }
    assert_class(
        h.prepare(binding(4), 2, &create()),
        LedgerFailureClass::QuotaExceeded,
    );
    h.admit(binding(4), 2, &safety());
    h.admit(binding(4), 3, &safety());
    assert_class(
        h.prepare(binding(4), 4, &safety()),
        LedgerFailureClass::QuotaExceeded,
    );
}
#[test]
fn completion_and_acknowledgement_use_reserved_capacity_when_all_slots_full() {
    let mut h = Harness::new();
    h.register(binding(1));
    for n in 1..=4 {
        h.admit(binding(1), n, &safety());
    }
    for n in 1..=4 {
        h.complete(binding(1), n, &Outcome::Applied, false);
    }
    assert_class(
        h.prepare(binding(1), 5, &safety()),
        LedgerFailureClass::QuotaExceeded,
    );
    h.ack(binding(1), 4);
    h.admit(binding(1), 5, &create());
}
#[test]
fn completed_results_remain_until_durable_acknowledgement() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.complete(binding(1), 1, &Outcome::Applied, true);
    let p = h
        .ledger
        .prepare_acknowledgement(&binding(1), 1, h.rows.context())
        .unwrap()
        .unwrap();
    assert!(matches!(
        h.ledger
            .resolve_commit(p, CommitDisposition::NotApplied(StoreFailure::Io))
            .unwrap(),
        TransitionResolution::NotApplied(StoreFailure::Io)
    ));
    assert!(matches!(
        h.ledger
            .lookup(&binding(1), op(binding(1), 1), None)
            .unwrap(),
        OperationLookup::Completed { .. }
    ));
    h.ack(binding(1), 1);
    assert!(h.rows.entries.is_empty());
}
#[test]
fn acknowledgement_cannot_cross_pending_completion_gap() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.admit(binding(1), 2, &create());
    h.complete(binding(1), 2, &Outcome::Applied, true);
    assert_class(
        h.ledger
            .prepare_acknowledgement(&binding(1), 2, h.rows.context()),
        LedgerFailureClass::AcknowledgementGap,
    );
    assert_class(
        h.ledger
            .prepare_acknowledgement(&binding(1), 3, h.rows.context()),
        LedgerFailureClass::AcknowledgementAhead,
    );
    h.complete(binding(1), 1, &Outcome::Applied, true);
    h.ack(binding(1), 1);
    assert_eq!(h.rows.entries.len(), 1);
    h.ack(binding(1), 2);
    assert!(h.rows.entries.is_empty());
}
#[test]
fn lost_acknowledgement_reply_is_idempotent_and_store_only() {
    let mut h = Harness::new();
    h.register(binding(1));
    h.admit(binding(1), 1, &create());
    h.complete(binding(1), 1, &Outcome::Applied, true);
    let revision = h.rows.organization;
    h.ack(binding(1), 1);
    let store = h.rows.store;
    h.reopen();
    h.ack(binding(1), 1);
    assert_eq!(h.rows.store, store);
    assert_eq!(h.rows.organization, revision);
}
#[test]
fn retirement_and_expiry_preserve_unacknowledged_results() {
    for reason in [
        StreamRetirementReason::Expired,
        StreamRetirementReason::PairingRevoked,
        StreamRetirementReason::ExplicitRetirement,
    ] {
        let mut h = Harness::new();
        h.register(binding(1));
        h.admit(binding(1), 1, &create());
        h.complete(binding(1), 1, &Outcome::Applied, true);
        h.retire(binding(1), reason);
        h.reopen();
        assert!(matches!(
            h.prepare(binding(1), 1, &create()).unwrap(),
            AdmissionPreparation::Existing(OperationLookup::Completed { .. })
        ));
        assert!(matches!(
            h.prepare(binding(1), 2, &create()).unwrap(),
            AdmissionPreparation::Existing(OperationLookup::AcknowledgedRetired)
        ));
        h.ack(binding(1), 1);
        assert_eq!(h.rows.streams.len(), 1);
    }
}
#[test]
fn stream_header_limit_includes_tombstones() {
    let policy = LedgerLimits::new(2, 2, 2, 4, 1, 1).unwrap();
    let mut h = Harness::with_limits(policy);
    for k in 1..=2 {
        h.register(binding(k));
        h.retire(binding(k), StreamRetirementReason::ExplicitRetirement);
    }
    assert_class(
        h.ledger
            .prepare_register_stream(binding(3), h.rows.context()),
        LedgerFailureClass::StreamLimit,
    );
}
#[test]
fn purpose_capacity_is_reserved_before_activation_and_consumed_once() {
    let policy = LedgerLimits::new(4, 4, 2, 4, 1, 1).unwrap();
    let mut h = Harness::with_limits(policy);
    h.register(binding(1));
    h.register_purpose(binding(2), StreamPurpose::SelfRevocation);
    h.register_purpose(binding(3), StreamPurpose::SelfRevocation);
    h.register_purpose(binding(4), StreamPurpose::SelfRevocation);
    h.admit(binding(1), 1, &safety());
    assert_class(
        h.prepare(binding(1), 2, &safety()),
        LedgerFailureClass::QuotaExceeded,
    );
    assert_class(
        h.prepare(binding(2), 1, &create()),
        LedgerFailureClass::PurposeMismatch,
    );
    h.admit(binding(2), 1, &safety());
    h.complete(binding(2), 1, &Outcome::Applied, false);
    h.ack(binding(2), 1);
    assert_class(
        h.prepare(binding(2), 2, &safety()),
        LedgerFailureClass::PurposeMismatch,
    );
    h.admit(binding(1), 2, &safety());
}
#[test]
fn dedicated_registration_rejects_before_capacity_overcommit() {
    let mut h = Harness::with_limits(LedgerLimits::new(8, 4, 2, 2, 1, 1).unwrap());
    h.register_purpose(binding(1), StreamPurpose::SelfRevocation);
    h.register_purpose(binding(2), StreamPurpose::SelfRevocation);
    assert_class(
        h.ledger.prepare_register_stream_with_purpose(
            binding_for(3, 2, 2),
            StreamPurpose::SelfRevocation,
            h.rows.context(),
        ),
        LedgerFailureClass::QuotaExceeded,
    );
}
#[test]
fn compact_outcome_and_request_bounds_reject_before_decoding_allocation() {
    assert!(CompactOutcome::from_encoded(vec![1; MAX_COMPACT_OUTCOME_BYTES]).is_ok());
    assert_class(
        CompactOutcome::from_encoded(vec![1; MAX_COMPACT_OUTCOME_BYTES + 1]),
        LedgerFailureClass::InvalidCompactOutcome,
    );
    assert!(
        CanonicalCodec
            .decode_compact_outcome(&vec![1; MAX_COMPACT_OUTCOME_BYTES])
            .is_err()
    );
    assert_class(
        CanonicalOperationRequest::decode(
            &vec![1; MAX_CANONICAL_REQUEST_BYTES + 1],
            &CanonicalCodec,
        ),
        LedgerFailureClass::InvalidRequest,
    );
    assert_eq!(
        LedgerLimits::production_v1().global_reserved_byte_ceiling(),
        64 * 1024 * 1024
    );
    assert_eq!(
        LedgerLimits::production_v1().per_stream_reserved_byte_ceiling(),
        4 * 1024 * 1024
    );
}

#[test]
fn enrollment_is_explicit_one_shot_and_bound_to_original_claim() {
    use spaceterm_core::pairing::*;
    let mut h = Harness::new();
    let claim_digest = RequestDigest::from_bytes([5; 32]);
    let purpose = StreamPurpose::Enrollment { claim_digest };
    h.register_purpose(binding(1), purpose);
    assert_class(
        h.ledger
            .prepare_register_stream(binding(1), h.rows.context()),
        LedgerFailureClass::PurposeMismatch,
    );
    let ticket = EnrollmentTicketId::new(EnrollmentTicketParts {
        server: server(),
        nonce: [1; 16],
    })
    .unwrap();
    let wrong = Command::Pairing(PairingCommand::Enroll {
        ticket: ticket.clone(),
        claim_digest: RequestDigest::from_bytes([6; 32]),
    });
    assert_class(
        h.prepare(binding(1), 1, &wrong),
        LedgerFailureClass::PurposeMismatch,
    );
    let command = Command::Pairing(PairingCommand::Enroll {
        ticket,
        claim_digest,
    });
    h.admit(binding(1), 1, &command);
    h.complete(
        binding(1),
        1,
        &Outcome::Failed(OperationFailure::InvalidValue),
        false,
    );
    h.ack(binding(1), 1);
    assert_class(
        h.prepare(binding(1), 2, &command),
        LedgerFailureClass::PurposeMismatch,
    );
    h.reopen();
    assert_eq!(h.ledger.stream(&binding(1)).unwrap().purpose, purpose);
}
#[test]
fn failed_dedicated_registration_does_not_consume_dormant_capacity() {
    let mut h = Harness::with_limits(LedgerLimits::new(4, 4, 2, 2, 1, 1).unwrap());
    let p = h
        .ledger
        .prepare_register_stream_with_purpose(
            binding(1),
            StreamPurpose::SelfRevocation,
            h.rows.context(),
        )
        .unwrap();
    h.ledger
        .resolve_commit(p, CommitDisposition::NotApplied(StoreFailure::Io))
        .unwrap();
    h.register_purpose(binding(1), StreamPurpose::SelfRevocation);
    h.register_purpose(binding(2), StreamPurpose::SelfRevocation);
}
