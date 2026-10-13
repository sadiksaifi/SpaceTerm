#![cfg(all(
    feature = "test-support",
    any(target_os = "macos", target_os = "linux")
))]

use spaceterm_core::{
    authority::{ServerEpoch, SessionGeneration, SessionKey},
    identity::{PaneId, ServerId},
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    operations::RequestDigest,
    retirement::{CleanupResult, ConfirmedCleanupClass, SessionCleanupObservation},
};
use spaceterm_host::{
    account_service::{
        RuntimeAssessment, ServiceFailure, ServiceProcesses,
        runtime_inventory::{RetainedRuntimeProcesses, RuntimeCustody},
        test_support::AccountServiceFixture,
    },
    server_keys::ServerKeyStore,
    startup::{BuildIdentity, DesiredServerState},
};
use std::sync::Arc;

fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("st-ri-")
        .tempdir_in("/var/tmp")
        .unwrap()
}

#[tokio::test]
async fn signed_cleanup_reopens_exact_original_request_and_rejects_new_instance() {
    let root = root();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let signer = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    assert_eq!(
        RetainedRuntimeProcesses
            .assess(&registration)
            .await
            .unwrap()
            .epoch,
        None
    );
    let epoch = ServerEpoch::new(1);
    let custody = RuntimeCustody::begin(&registration, epoch, signer.clone()).unwrap();
    assert_eq!(
        RetainedRuntimeProcesses.verify_absent(&registration).await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    let server = ServerId::new(registration.server_pin().identity);
    let session = SessionKey {
        pane: PaneId::new(server, 1).unwrap(),
        epoch,
        generation: SessionGeneration::new(4),
    };
    let assessment = RuntimeAssessment {
        server,
        epoch: Some(epoch),
        generation: 8,
        sessions: vec![session],
        token: [9; 32],
    };
    let request =
        NativeManagementRequestRef::new(server, [2; 16], RequestDigest::from_bytes([3; 32]))
            .unwrap();
    let action = NativeRetirementAction::Stop;
    let observation = SessionCleanupObservation::new(
        session,
        action.reason(),
        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
    )
    .unwrap();
    assert_eq!(
        custody.completed(&registration, request, action, &assessment, &[]),
        Err(ServiceFailure::CleanupUnresolved)
    );
    let unrelated = SessionCleanupObservation::new(
        session,
        spaceterm_core::organization::RetirementReason::EndAllSessions,
        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
    )
    .unwrap();
    assert_eq!(
        custody.completed(&registration, request, action, &assessment, &[unrelated]),
        Err(ServiceFailure::CleanupUnresolved)
    );
    custody
        .completed(&registration, request, action, &assessment, &[observation])
        .unwrap();
    drop(registration);
    let reopened = fixture.reopen().discover().unwrap().unwrap();
    assert_eq!(
        RetainedRuntimeProcesses::cleanup(&reopened, request, action, &assessment).unwrap(),
        vec![observation]
    );
    RetainedRuntimeProcesses
        .verify_absent(&reopened)
        .await
        .unwrap();
    let other = NativeManagementRequestRef::new(server, [5; 16], request.digest()).unwrap();
    assert_eq!(
        RetainedRuntimeProcesses::cleanup(&reopened, other, action, &assessment),
        Err(ServiceFailure::CleanupUnresolved)
    );
    let absent = RetainedRuntimeProcesses.assess(&reopened).await.unwrap();
    let next = RuntimeCustody::begin(&reopened, ServerEpoch::new(2), signer).unwrap();
    assert_eq!(
        RetainedRuntimeProcesses
            .revalidate(&reopened, &absent)
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    assert_eq!(
        RetainedRuntimeProcesses::cleanup(&reopened, request, action, &assessment),
        Err(ServiceFailure::CleanupUnresolved)
    );
    assert_eq!(
        custody.completed(&reopened, request, action, &assessment, &[observation]),
        Err(ServiceFailure::IdentityMismatch)
    );
    drop(next);
}

#[tokio::test]
async fn missing_tampered_or_unclean_runtime_metadata_never_proves_absence() {
    let root = root();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let signer = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    let original = std::fs::read(registration.data_directory().join("runtime-custody")).unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
    record["signature"][0] = serde_json::json!(record["signature"][0].as_u64().unwrap() ^ 1);
    std::fs::write(
        registration.data_directory().join("runtime-custody"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert_eq!(
        RetainedRuntimeProcesses.verify_absent(&registration).await,
        Err(ServiceFailure::IdentityMismatch)
    );
    std::fs::write(
        registration.data_directory().join("runtime-custody"),
        original,
    )
    .unwrap();
    RuntimeCustody::begin(&registration, ServerEpoch::new(1), signer).unwrap();
    assert_eq!(
        RetainedRuntimeProcesses
            .previous_instance_cleanup(&registration, ServerEpoch::new(1), &[])
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    std::fs::remove_file(registration.data_directory().join("runtime-custody")).unwrap();
    assert_eq!(
        RetainedRuntimeProcesses.verify_absent(&registration).await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    // Established discovery and key reads cannot recreate empty clean custody.
    fixture.reopen().discover().unwrap().unwrap();
    assert!(
        !registration
            .data_directory()
            .join("runtime-custody")
            .exists()
    );
    let _signer: Arc<_> = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
}

#[tokio::test]
async fn absent_reservation_abort_cannot_be_reused_after_provider_reconstruction() {
    let root = root();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let assessment = RetainedRuntimeProcesses
        .assess(&registration)
        .await
        .unwrap();
    RetainedRuntimeProcesses
        .revalidate(&registration, &assessment)
        .await
        .unwrap();
    RetainedRuntimeProcesses
        .abort_reservation(&registration, &assessment)
        .await
        .unwrap();
    drop(registration);
    let reopened = fixture.reopen().discover().unwrap().unwrap();
    assert_eq!(
        RetainedRuntimeProcesses
            .revalidate(&reopened, &assessment)
            .await,
        Err(ServiceFailure::StaleAssessment)
    );
    RetainedRuntimeProcesses
        .abort_reservation(&reopened, &assessment)
        .await
        .unwrap();
    let fresh = RetainedRuntimeProcesses.assess(&reopened).await.unwrap();
    assert_ne!(fresh.token, assessment.token);
    RetainedRuntimeProcesses
        .revalidate(&reopened, &fresh)
        .await
        .unwrap();
}
