use spaceterm_core::{authority::ServerEpoch, operations::RequestDigest};
use spaceterm_host::{
    account_service::{
        InstallationStatus, NativeLifecycleAssessmentRef, NativeLifecycleRequest, ServiceAction,
        ServiceFailure, ServiceOutcome, ServicePhase, ServiceProgress, ServiceRequestState,
        ServiceTime,
        remote_wire::{
            NATIVE_LIFECYCLE_WIRE_LIMIT, NativeLifecycleAccountReply,
            NativeLifecycleAccountRequest, NativeLifecycleWireFailure,
        },
    },
    native_deployment::contracts::NativeRequestId,
    startup::{AccountBuildKey, BuildIdentity},
};
use spaceterm_protocol::authentication::ServerPin;

fn key() -> AccountBuildKey {
    AccountBuildKey {
        account_uid: 501,
        build_identity: BuildIdentity::Development,
    }
}
fn pin() -> ServerPin {
    ServerPin {
        identity: [1; 16],
        public_key: [2; 32],
    }
}
fn capture() -> NativeLifecycleRequest {
    NativeLifecycleRequest {
        request: NativeRequestId::new([3; 16]).unwrap(),
        action: ServiceAction::Stop,
        capture_digest: RequestDigest::from_bytes([4; 32]),
    }
}
fn assessment() -> NativeLifecycleAssessmentRef {
    NativeLifecycleAssessmentRef {
        original: capture(),
        request_digest: RequestDigest::from_bytes([5; 32]),
        proof_digest: RequestDigest::from_bytes([6; 32]),
        epoch: ServerEpoch::new(7),
        affected_sessions: 2,
        expires_at: ServiceTime {
            boot: [8; 32],
            millis: 30_001,
        },
    }
}
fn terminal() -> ServiceProgress {
    ServiceProgress {
        request: capture().request,
        request_digest: assessment().request_digest,
        phase: ServicePhase::Completed,
        state: ServiceRequestState::Completed(ServiceOutcome::Stopped),
    }
}
fn restore() -> NativeLifecycleAccountRequest {
    NativeLifecycleAccountRequest::Restore {
        expected_key: key(),
        expected_pin: pin(),
        original: capture(),
        assessment: Some(assessment()),
    }
}

#[test]
fn closed_wire_rejects_unknown_fields_at_every_owned_nested_record() {
    let original = serde_json::to_value(restore()).unwrap();
    for pointer in [
        "",
        "/expected_key",
        "/expected_pin",
        "/original",
        "/assessment",
        "/assessment/original",
        "/assessment/expires_at",
    ] {
        let mut value = original.clone();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), serde_json::json!(true));
        assert_eq!(
            NativeLifecycleAccountRequest::decode(&serde_json::to_vec(&value).unwrap()),
            Err(NativeLifecycleWireFailure::InvalidRecord),
            "unknown field at {pointer}",
        );
    }
    let ack = NativeLifecycleAccountRequest::Acknowledge {
        expected_key: key(),
        expected_pin: pin(),
        progress: terminal(),
    };
    let mut value = serde_json::to_value(ack).unwrap();
    value["progress"]["unexpected"] = serde_json::json!(true);
    assert_eq!(
        NativeLifecycleAccountRequest::decode(&serde_json::to_vec(&value).unwrap()),
        Err(NativeLifecycleWireFailure::InvalidRecord),
    );
    let reply = NativeLifecycleAccountReply::Restored {
        original: capture(),
        assessment: Some(assessment()),
    };
    let mut value = serde_json::to_value(reply).unwrap();
    value["value"]["assessment"]["expires_at"]["unexpected"] = serde_json::json!(true);
    assert_eq!(
        NativeLifecycleAccountReply::decode(&serde_json::to_vec(&value).unwrap()),
        Err(NativeLifecycleWireFailure::InvalidRecord),
    );
}

#[test]
fn bounded_wire_rejects_oversized_bodies_before_deserialization() {
    let mut bytes = restore().encode().unwrap();
    bytes.resize(NATIVE_LIFECYCLE_WIRE_LIMIT + 1, b' ');
    assert_eq!(
        NativeLifecycleAccountRequest::decode(&bytes),
        Err(NativeLifecycleWireFailure::LimitExceeded)
    );
    let mut bytes = NativeLifecycleAccountReply::Captured(capture())
        .encode()
        .unwrap();
    bytes.resize(NATIVE_LIFECYCLE_WIRE_LIMIT + 1, b' ');
    assert_eq!(
        NativeLifecycleAccountReply::decode(&bytes),
        Err(NativeLifecycleWireFailure::LimitExceeded)
    );
}

#[test]
fn wire_rejects_invalid_identity_and_assessment_comparisons() {
    let original = serde_json::to_value(restore()).unwrap();
    let cases = [
        ("/expected_key/account_uid", serde_json::json!(0)),
        ("/expected_key/account_uid", serde_json::json!(u32::MAX)),
        ("/expected_key/build_identity", serde_json::json!("unknown")),
        (
            "/expected_pin/identity",
            serde_json::to_value([0u8; 16]).unwrap(),
        ),
        (
            "/expected_pin/public_key",
            serde_json::to_value([0u8; 32]).unwrap(),
        ),
        (
            "/original/capture_digest",
            serde_json::to_value([0u8; 32]).unwrap(),
        ),
        (
            "/assessment/request_digest",
            serde_json::to_value([0u8; 32]).unwrap(),
        ),
        (
            "/assessment/proof_digest",
            serde_json::to_value([0u8; 32]).unwrap(),
        ),
        ("/assessment/epoch", serde_json::json!(0)),
        ("/assessment/affected_sessions", serde_json::json!(65)),
        (
            "/assessment/expires_at/boot",
            serde_json::to_value([0u8; 32]).unwrap(),
        ),
        ("/assessment/expires_at/millis", serde_json::json!(0)),
        ("/assessment/original/action", serde_json::json!("Restart")),
    ];
    for (pointer, replacement) in cases {
        let mut value = original.clone();
        *value.pointer_mut(pointer).unwrap() = replacement;
        assert_eq!(
            NativeLifecycleAccountRequest::decode(&serde_json::to_vec(&value).unwrap()),
            Err(NativeLifecycleWireFailure::InvalidRecord),
            "invalid comparison at {pointer}",
        );
    }
}

#[test]
fn acknowledgment_requires_terminal_live_progress_not_start_or_stage() {
    let mut progress = terminal();
    let installation = InstallationStatus {
        installation: spaceterm_host::startup::InstallationId(std::num::NonZeroU64::MIN),
        version: spaceterm_distribution::BuildVersion::parse("1.0.0").unwrap(),
        artifact_sha256: [10; 32],
    };
    for outcome in [
        ServiceOutcome::Started,
        ServiceOutcome::Staged(installation),
    ] {
        progress.state = ServiceRequestState::Completed(outcome);
        assert_eq!(
            NativeLifecycleAccountRequest::Acknowledge {
                expected_key: key(),
                expected_pin: pin(),
                progress: progress.clone(),
            }
            .validate(),
            Err(NativeLifecycleWireFailure::InvalidRecord),
        );
    }
    progress = terminal();
    progress.phase = ServicePhase::Accepted;
    assert_eq!(
        NativeLifecycleAccountRequest::Acknowledge {
            expected_key: key(),
            expected_pin: pin(),
            progress
        }
        .validate(),
        Err(NativeLifecycleWireFailure::InvalidRecord),
    );
    for operation in ["start", "stage", "uninstall", "admit", "confirm", "owner"] {
        let mut value = serde_json::to_value(NativeLifecycleAccountRequest::Observe {
            expected_key: key(),
            expected_pin: pin(),
        })
        .unwrap();
        value["operation"] = serde_json::json!(operation);
        assert_eq!(
            NativeLifecycleAccountRequest::decode(&serde_json::to_vec(&value).unwrap()),
            Err(NativeLifecycleWireFailure::InvalidRecord),
        );
    }
    assert_eq!(
        NativeLifecycleAccountRequest::Capture {
            expected_key: key(),
            expected_pin: pin(),
            request: capture().request,
            action: ServiceAction::Uninstall,
        }
        .validate(),
        Ok(()),
    );
}

#[test]
fn reserved_nonadmission_round_trips_without_becoming_acknowledgeable() {
    let mut progress = terminal();
    progress.phase = ServicePhase::Reserved;
    progress.state = ServiceRequestState::NotApplied(ServiceFailure::StaleAssessment);
    let reply = NativeLifecycleAccountReply::Progress(progress.clone());
    assert_eq!(
        NativeLifecycleAccountReply::decode(&reply.encode().unwrap()).unwrap(),
        reply,
    );
    assert_eq!(
        NativeLifecycleAccountRequest::Acknowledge {
            expected_key: key(),
            expected_pin: pin(),
            progress,
        }
        .validate(),
        Err(NativeLifecycleWireFailure::InvalidRecord),
    );
}

#[test]
fn wire_round_trip_preserves_original_capture_expiry_and_exact_terminal_result() {
    let request = restore();
    assert_eq!(
        NativeLifecycleAccountRequest::decode(&request.encode().unwrap()).unwrap(),
        request
    );
    for reply in [
        NativeLifecycleAccountReply::Restored {
            original: capture(),
            assessment: Some(assessment()),
        },
        NativeLifecycleAccountReply::Acknowledged {
            progress: terminal(),
        },
    ] {
        assert_eq!(
            NativeLifecycleAccountReply::decode(&reply.encode().unwrap()).unwrap(),
            reply
        );
    }
}
