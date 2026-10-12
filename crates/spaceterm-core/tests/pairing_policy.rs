use spaceterm_core::authority::{FieldVersion, ServerEpoch, StoreRevision};
use spaceterm_core::identity::*;
use spaceterm_core::operations::{
    AssessmentRef, OperationBinding, OperationId, OperationSequence, RequestDigest,
};
use spaceterm_core::pairing::policy::{PairingPolicy, PairingSnapshot};
use spaceterm_core::pairing::*;
use spaceterm_core::pairing::{ClientRecord, PairingRecord, PairingState};

fn binding(server: ServerId, pairing: u64, client: u64, stream: u64) -> OperationStreamId {
    OperationStreamId::new(
        OperationStreamKey::new(server, stream).unwrap(),
        PairingId::new(server, pairing).unwrap(),
        ClientId::new(server, client).unwrap(),
    )
    .unwrap()
}
fn authorization(server: ServerId, pairing: u64, client: u64) -> PairingAuthorization {
    PairingAuthorization::new(PairingAuthorizationParts {
        record: PairingRecord {
            id: PairingId::new(server, pairing).unwrap(),
            device: DeviceId::new([pairing as u8; 16]),
            state: PairingState::Authorized,
        },
        authorization_version: FieldVersion::new(1),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [1; 32],
            pairing_authentication_public_key: [2; 32],
        })
        .unwrap(),
        clients: vec![
            PairingClientStreams::new(PairingClientStreamsParts {
                client: ClientRecord {
                    id: ClientId::new(server, client).unwrap(),
                    pairing: PairingId::new(server, pairing).unwrap(),
                },
                ordinary: binding(server, pairing, client, client * 2),
                self_revocation: binding(server, pairing, client, client * 2 + 1),
            })
            .unwrap(),
        ],
        revocation_receipt: None,
    })
    .unwrap()
}
fn snapshot() -> PairingSnapshot {
    let server = ServerId::new([8; 16]);
    PairingSnapshot::new(
        server,
        ServerEpoch::new(1),
        StoreRevision::new(3),
        IdentityHighWater {
            pairing: 2,
            client: 2,
            operation_stream: 5,
            ..Default::default()
        },
        PairingRecovery::new(PairingRecoveryParts {
            authorizations: vec![authorization(server, 1, 1), authorization(server, 2, 2)],
            enrollments: vec![],
        })
        .unwrap(),
    )
    .unwrap()
}
#[test]
fn client_binding_cannot_move_between_pairings() {
    let state = snapshot();
    let policy = PairingPolicy::default();
    let principal = policy
        .authorize_client(
            &state,
            PairingId::new(state.server(), 1).unwrap(),
            ClientId::new(state.server(), 1).unwrap(),
        )
        .unwrap();
    assert_eq!(principal.device(), DeviceId::new([1; 16]));
    assert_eq!(
        policy.authorize_client(
            &state,
            PairingId::new(state.server(), 1).unwrap(),
            ClientId::new(state.server(), 2).unwrap()
        ),
        Err(PairingFailure::ClientNotBound)
    );
}
#[test]
fn unpair_assessment_binds_exact_authorization_version_and_clients() {
    let state = snapshot();
    let policy = PairingPolicy::default();
    let actor = policy
        .authorize_client(
            &state,
            PairingId::new(state.server(), 1).unwrap(),
            ClientId::new(state.server(), 1).unwrap(),
        )
        .unwrap();
    let reference = AssessmentRef {
        operation: OperationId::new(
            binding(state.server(), 1, 1, 2),
            OperationSequence::new(1).unwrap(),
        ),
        digest: RequestDigest::from_bytes([3; 32]),
    };
    let target = PairingTarget::new(PairingTargetParts {
        server: state.server(),
        pairing: PairingId::new(state.server(), 2).unwrap(),
        device: DeviceId::new([2; 16]),
    })
    .unwrap();
    let assessment = policy
        .assess_unpair(&state, &actor, reference, target)
        .unwrap();
    assert_eq!(
        assessment.parts().clients,
        vec![ClientRecord {
            id: ClientId::new(state.server(), 2).unwrap(),
            pairing: PairingId::new(state.server(), 2).unwrap()
        }]
    );
    assert_eq!(
        assessment.parts().authorization_version,
        FieldVersion::new(1)
    );
}

#[test]
fn pairing_limits_accept_boundary_and_reject_boundary_plus_one() {
    use spaceterm_core::pairing::policy::PairingLimits;
    let state = snapshot();
    let limited = PairingPolicy::new(PairingLimits {
        active: 2,
        clients: 1,
        lifetimes: 2,
        outstanding: 1,
    })
    .unwrap();
    assert_eq!(
        limited.check_reservation_capacity(&state, DeviceId::new([3; 16])),
        Err(PairingFailure::LimitExceeded)
    );
    assert!(
        PairingPolicy::new(PairingLimits {
            active: MAX_ACTIVE_PAIRINGS + 1,
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        PairingPolicy::new(PairingLimits {
            clients: MAX_CLIENTS_PER_PAIRING + 1,
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        PairingPolicy::new(PairingLimits {
            lifetimes: MAX_PAIRING_LIFETIMES + 1,
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        PairingPolicy::new(PairingLimits {
            outstanding: MAX_OUTSTANDING_ENROLLMENTS + 1,
            ..Default::default()
        })
        .is_err()
    );
}
fn reserved(state: &PairingSnapshot) -> EnrollmentReservation {
    EnrollmentReservation::new(EnrollmentReservationParts {
        ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
            server: state.server(),
            nonce: [1; 16],
        })
        .unwrap(),
        target: PairingTarget::new(PairingTargetParts {
            server: state.server(),
            device: DeviceId::new([3; 16]),
            pairing: PairingId::new(state.server(), 3).unwrap(),
        })
        .unwrap(),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [3; 32],
            pairing_authentication_public_key: [4; 32],
        })
        .unwrap(),
        initial_client: ClientId::new(state.server(), 3).unwrap(),
        enrollment_stream: binding(state.server(), 3, 3, 6),
        claim_digest: RequestDigest::from_bytes([6; 32]),
        authorization_digest: RequestDigest::from_bytes([7; 32]),
        enrollment_request_digest: RequestDigest::from_bytes([8; 32]),
        issuer: EnrollmentIssuer::AccountOwner {
            native_request: [1; 16],
            native_request_digest: RequestDigest::from_bytes([9; 32]),
        },
        issued_epoch: state.epoch(),
        issued_revision: state.revision().checked_successor().unwrap(),
        issued_at_millis: 100,
        expires_at_millis: 200,
        state: EnrollmentReservationState::Reserved,
    })
    .unwrap()
}
fn with_reserved(state: PairingSnapshot, r: EnrollmentReservation) -> PairingSnapshot {
    let transition = PairingPolicy::default()
        .prepare_reservation(&state, r.clone())
        .unwrap();
    let mut recovery = state.recovery().parts().clone();
    recovery.enrollments.push(r);
    PairingSnapshot::new(
        state.server(),
        state.epoch(),
        state.revision().checked_successor().unwrap(),
        transition.high_water,
        PairingRecovery::new(recovery).unwrap(),
    )
    .unwrap()
}
#[test]
fn enrollment_ticket_is_bound_to_server_device_and_key() {
    let state = snapshot();
    let r = reserved(&state);
    let state = with_reserved(state, r.clone());
    let operation = OperationBinding {
        id: OperationId::new(
            r.parts().enrollment_stream,
            OperationSequence::new(1).unwrap(),
        ),
        digest: r.parts().enrollment_request_digest,
    };
    let policy = PairingPolicy::default();
    assert!(
        policy
            .check_enrollment_authorization(
                &state,
                &r.parts().ticket,
                operation,
                r.parts().claim_digest,
                r.parts().authorization_digest,
                199
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        policy.check_enrollment_authorization(
            &state,
            &r.parts().ticket,
            operation,
            r.parts().claim_digest,
            RequestDigest::from_bytes([0; 32]),
            199
        ),
        Err(PairingFailure::AuthorizationDenied)
    );
    assert_eq!(
        policy.check_enrollment_authorization(
            &state,
            &r.parts().ticket,
            operation,
            RequestDigest::from_bytes([0; 32]),
            r.parts().authorization_digest,
            199
        ),
        Err(PairingFailure::RequestIdentityReused)
    );
    assert_eq!(
        policy.check_enrollment_authorization(
            &state,
            &r.parts().ticket,
            operation,
            r.parts().claim_digest,
            r.parts().authorization_digest,
            200
        ),
        Err(PairingFailure::AuthorizationExpired)
    );
}
#[test]
fn enrollment_reserves_purpose_and_result_before_activation() {
    use spaceterm_core::operation_ledger_contracts::{
        OutcomeReservation, ReservationClass, StreamPurpose,
    };
    let state = snapshot();
    let r = reserved(&state);
    let transition = PairingPolicy::default()
        .prepare_reservation(&state, r.clone())
        .unwrap();
    assert_eq!(transition.register_streams.len(), 1);
    assert_eq!(
        transition.register_streams[0].purpose,
        StreamPurpose::Enrollment {
            claim_digest: r.parts().claim_digest
        }
    );
    assert_eq!(
        transition.register_streams[0].dormant_reservation,
        Some(OutcomeReservation::full(
            ReservationClass::DedicatedEnrollment
        ))
    );
    assert!(matches!(
        transition.writes.parts().writes.as_slice(),
        [PairingWrite::PutEnrollmentReservation { .. }]
    ));
}
#[test]
fn abandoned_identity_cannot_be_reserved_again() {
    let state = snapshot();
    let r = reserved(&state);
    let state = with_reserved(state, r.clone());
    assert!(matches!(
        PairingPolicy::default().prepare_reservation(&state, r),
        Err(PairingFailure::IdentityMismatch) | Err(PairingFailure::RequestIdentityReused)
    ));
}

#[test]
fn expired_and_restart_invalidated_tickets_cannot_enroll() {
    let state = snapshot();
    let r = reserved(&state);
    let state = with_reserved(state, r.clone());
    let policy = PairingPolicy::default();
    let at_boundary = policy.prepare_ticket_invalidations(&state, 200).unwrap();
    assert_eq!(at_boundary.writes.parts().writes.len(), 1);
    let value = match &at_boundary.writes.parts().writes[0] {
        PairingWrite::PutEnrollmentReservation {
            expected_kind: Some(EnrollmentReservationKind::Reserved),
            value,
        } => value,
        _ => panic!("invalid projection"),
    };
    assert_eq!(
        value.parts().state.kind(),
        EnrollmentReservationKind::Invalidated
    );
    assert_eq!(value.parts().target, r.parts().target);
    assert_eq!(
        at_boundary.retire_streams,
        vec![r.parts().enrollment_stream]
    );
    let restarted = PairingSnapshot::new(
        state.server(),
        ServerEpoch::new(2),
        state.revision(),
        state.high_water(),
        state.recovery().clone(),
    )
    .unwrap();
    assert_eq!(
        policy
            .prepare_ticket_invalidations(&restarted, 0)
            .unwrap()
            .writes
            .parts()
            .writes
            .len(),
        1
    );
}
