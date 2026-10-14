//! Closed guest producer regressions using the same real canonical paired fixture.

use super::*;
use spaceterm_host::account_service::{
    STOPPED_NATIVE_WIRE_LIMIT, StoppedNativeAccountReply as Reply,
    StoppedNativeAccountRequest as Request, StoppedNativeArtifactTransfer,
    StoppedNativeChannelRequest,
};
use spaceterm_server::account_native_maintenance::producer::{
    StoppedNativeAccountOutcome, handle_account_request, validate_artifact_input,
};

fn channel(
    fixture: &Fixture,
    request: spaceterm_host::native_deployment::contracts::NativeRequestId,
    action: StoppedNativeAction,
    artifact: Option<&VerifiedAccountServerArtifact>,
) -> Request {
    Request::Channel {
        capture: StoppedNativeChannelRequest {
            expected_key: fixture.target.key,
            expected_pin: fixture.target.pin,
            principal: fixture.target.principal,
            registration_generation: NonZeroU64::new(
                fixture.supervisor.discover().unwrap().unwrap().generation(),
            )
            .unwrap(),
            request,
            action,
        },
        artifact: artifact
            .map(|artifact| StoppedNativeArtifactTransfer::from_artifact(artifact).unwrap()),
    }
}
async fn reply(
    fixture: &Fixture,
    request: &Request,
    artifact: Option<VerifiedAccountServerArtifact>,
) -> Reply {
    let StoppedNativeAccountOutcome::Reply(reply) = handle_account_request(
        fixture.supervisor.clone(),
        fixture.home.clone(),
        request,
        artifact,
    )
    .await
    else {
        panic!("native reply")
    };
    reply
}
async fn open_channel(
    fixture: &Fixture,
    request: &Request,
    artifact: Option<VerifiedAccountServerArtifact>,
) -> (StoppedNativeRequest, AccountNativeMaintenanceExecutor) {
    let StoppedNativeAccountOutcome::Channel { original, executor } = handle_account_request(
        fixture.supervisor.clone(),
        fixture.home.clone(),
        request,
        artifact,
    )
    .await
    else {
        panic!("native channel")
    };
    (original, *executor)
}

#[tokio::test]
async fn stopped_account_grammar_is_bounded_closed_and_cannot_resolve_unsigned_capture() {
    let fixture = Fixture::new(false).await;
    let id = fixture.supervisor.allocate_request().unwrap();
    let request = channel(&fixture, id, StoppedNativeAction::Start, None);
    let encoded = request.encode().unwrap();
    assert_eq!(Request::decode(&encoded).unwrap(), request);
    let mut unknown: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    unknown["raw_path"] = serde_json::Value::String("/unexpected".into());
    assert_eq!(
        Request::decode(&serde_json::to_vec(&unknown).unwrap()).err(),
        Some(ServiceFailure::InvalidRecord)
    );
    assert_eq!(
        Request::decode(&vec![b' '; STOPPED_NATIVE_WIRE_LIMIT + 1]).err(),
        Some(ServiceFailure::LimitExceeded)
    );
    validate_artifact_input(&fixture.supervisor, &request).unwrap();
    let original = fixture.supervisor.restore_stopped_native(id).unwrap();
    let restore = Request::Restore {
        expected_key: fixture.target.key,
        expected_pin: fixture.target.pin,
        request: id,
        action: StoppedNativeAction::Start,
    };
    assert_eq!(
        reply(&fixture, &restore, None).await,
        Reply::Restored {
            original,
            assessment: None
        }
    );
    let wrong = Request::Restore {
        expected_key: fixture.target.key,
        expected_pin: fixture.target.pin,
        request: id,
        action: StoppedNativeAction::Uninstall,
    };
    assert_eq!(
        reply(&fixture, &wrong, None).await,
        Reply::Refused(ServiceFailure::RequestConflict)
    );
    let resolve = Request::Resolve {
        expected_key: fixture.target.key,
        expected_pin: fixture.target.pin,
        request: id,
        artifact: None,
    };
    assert_eq!(
        reply(&fixture, &resolve, None).await,
        Reply::Refused(ServiceFailure::Pending)
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stopped_account_channel_proves_saved_client_and_retains_exact_restore_resolve_ack() {
    for major in [1, 2] {
        let fixture = Fixture::new(false).await;
        let id = fixture.supervisor.allocate_request().unwrap();
        let request = channel(&fixture, id, StoppedNativeAction::Start, None);
        let (original, executor) = open_channel(
            &fixture,
            &Request::decode(&request.encode().unwrap()).unwrap(),
            None,
        )
        .await;
        let (mut server, mut client) = connect(executor, fixture.target, major).await;
        let reference = assess(&mut server, &mut client, original).await;
        drop(server);
        let restore = Request::Restore {
            expected_key: fixture.target.key,
            expected_pin: fixture.target.pin,
            request: id,
            action: StoppedNativeAction::Start,
        };
        assert_eq!(
            reply(&fixture, &restore, None).await,
            Reply::Restored {
                original,
                assessment: Some(reference.clone())
            }
        );
        let (again, executor) = open_channel(&fixture, &request, None).await;
        assert_eq!(again, original);
        let (mut server, mut client) = connect(executor, fixture.target, major).await;
        assert!(matches!(
            call(
                &mut server,
                &mut client,
                StoppedNativeCall::Admit(reference.clone())
            )
            .await,
            StoppedNativeReply::Accepted(_)
        ));
        let completed = server.finish().await.unwrap();
        let resolve = Request::Resolve {
            expected_key: fixture.target.key,
            expected_pin: fixture.target.pin,
            request: id,
            artifact: None,
        };
        assert_eq!(
            reply(&fixture, &resolve, None).await,
            Reply::Progress(completed.clone())
        );
        let ack = Request::Acknowledge {
            expected_key: fixture.target.key,
            expected_pin: fixture.target.pin,
            progress: completed.clone(),
        };
        assert_eq!(
            reply(&fixture, &ack, None).await,
            Reply::Acknowledged {
                progress: completed.clone()
            }
        );
        assert_eq!(
            reply(&fixture, &ack, None).await,
            Reply::Acknowledged {
                progress: completed
            }
        );
        assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn stopped_account_stage_resupply_requires_signed_accepted_original_sha_and_version() {
    let fixture = Fixture::new(false).await;
    let before = fixture.recovered();
    let (_, package) = artifact::fixture("1.2.4", 91, artifact::native_machine());
    let metadata = StoppedNativeArtifactTransfer::from_artifact(&package).unwrap();
    let id = fixture.supervisor.allocate_request().unwrap();
    let action = StoppedNativeAction::Stage {
        artifact_sha256: package.artifact_sha256(),
    };
    let request = channel(&fixture, id, action, Some(&package));
    validate_artifact_input(&fixture.supervisor, &request).unwrap();
    let resupply = Request::Resolve {
        expected_key: fixture.target.key,
        expected_pin: fixture.target.pin,
        request: id,
        artifact: Some(metadata.clone()),
    };
    assert_eq!(
        validate_artifact_input(&fixture.supervisor, &resupply).err(),
        Some(ServiceFailure::Pending)
    );
    let (original, executor) = open_channel(&fixture, &request, Some(package)).await;
    let (mut server, mut client) = connect(executor, fixture.target, 2).await;
    let reference = assess(&mut server, &mut client, original).await;
    drop(server);
    // A valid fresh ordinary handshake can admit the retained assessment without
    // keeping the first channel's package. The original then remains input-pending.
    let reopen = channel(&fixture, id, action, None);
    let (same, executor) = open_channel(&fixture, &reopen, None).await;
    assert_eq!(same, original);
    let (mut server, mut client) = connect(executor, fixture.target, 2).await;
    assert!(matches!(
        call(
            &mut server,
            &mut client,
            StoppedNativeCall::Admit(reference.clone())
        )
        .await,
        StoppedNativeReply::Accepted(_)
    ));
    let pending = server.finish().await.unwrap();
    assert_eq!(pending.phase, ServicePhase::Accepted);
    assert_eq!(pending.state, ServiceRequestState::Pending);
    let mut wrong_version = metadata.clone();
    wrong_version.version = spaceterm_distribution::BuildVersion::parse("1.2.5").unwrap();
    let wrong = Request::Resolve {
        expected_key: fixture.target.key,
        expected_pin: fixture.target.pin,
        request: id,
        artifact: Some(wrong_version),
    };
    assert_eq!(
        validate_artifact_input(&fixture.supervisor, &wrong).err(),
        Some(ServiceFailure::RequestConflict)
    );
    let (_, wrong_package) = artifact::fixture("1.2.5", 91, artifact::native_machine());
    assert_eq!(
        reply(&fixture, &resupply, Some(wrong_package)).await,
        Reply::Refused(ServiceFailure::RequestConflict)
    );
    let (_, package) = artifact::fixture("1.2.4", 91, artifact::native_machine());
    let Reply::Progress(completed) = reply(&fixture, &resupply, Some(package)).await else {
        panic!("original stage")
    };
    assert_eq!(completed.request, id);
    assert_eq!(completed.request_digest, reference.request_digest);
    assert!(matches!(
        completed.state,
        ServiceRequestState::Completed(ServiceOutcome::Staged(_))
    ));
    assert_eq!(
        fixture
            .supervisor
            .restore_stopped_native_reference(original)
            .unwrap(),
        reference
    );
    assert_eq!(
        fixture
            .supervisor
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
    let after = fixture.recovered();
    assert_eq!(after.last_epoch, before.last_epoch);
    assert_eq!(after.organization, before.organization);
    assert_eq!(after.pairing, before.pairing);
}

#[test]
fn stopped_artifact_section_reuses_existing_signed_grammar_without_consuming_handshake() {
    use std::io::{Cursor, Read as _};
    let (trust, package) = artifact::fixture("1.2.4", 91, artifact::native_machine());
    let metadata = StoppedNativeArtifactTransfer::from_artifact(&package).unwrap();
    let mut bytes = Vec::new();
    spaceterm_host::bootstrap::artifact::write_artifact(&package, &mut bytes).unwrap();
    assert_eq!(bytes.len() as u64, metadata.wire_bytes().unwrap());
    let following_handshake = b"following ordinary handshake";
    bytes.extend_from_slice(following_handshake);
    let mut reader = Cursor::new(bytes);
    let received = spaceterm_host::bootstrap::artifact::receive_artifact_with_key(
        &mut (&mut reader).take(metadata.wire_bytes().unwrap()),
        &trust,
        spaceterm_distribution::BuildChannel::Development,
        if cfg!(target_os = "macos") {
            spaceterm_distribution::ServerPlatform::DarwinArm64
        } else {
            spaceterm_distribution::ServerPlatform::LinuxX86_64
        },
        metadata.artifact_sha256,
        1_800_000_001,
    )
    .unwrap();
    metadata.matches(&received).unwrap();
    let mut remaining = Vec::new();
    reader.read_to_end(&mut remaining).unwrap();
    assert_eq!(remaining, following_handshake);
}
