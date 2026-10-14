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
    // SQLite removes completed native retirements. Startup therefore requests no
    // Sessions even though the signed original request retains its live scope.
    assert_eq!(
        RetainedRuntimeProcesses
            .previous_instance_cleanup(&reopened, epoch, &[])
            .await
            .unwrap(),
        Vec::new()
    );
    assert_eq!(
        RetainedRuntimeProcesses
            .previous_instance_cleanup(&reopened, epoch, &[session])
            .await
            .unwrap(),
        vec![observation]
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

struct ScopeManager;
impl spaceterm_host::account_service::ServiceManager for ScopeManager {
    fn status<'a>(
        &'a self,
        _: Option<&'a spaceterm_host::account_service::RegisteredServer>,
    ) -> spaceterm_host::account_service::ServiceFuture<
        'a,
        spaceterm_host::account_service::SupervisorState,
    > {
        Box::pin(async { Ok(spaceterm_host::account_service::SupervisorState::Inactive) })
    }
    fn scoped_processes<'a>(
        &'a self,
        _: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_host::account_service::ServiceFuture<'a, Vec<u32>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn linger(
        &self,
    ) -> spaceterm_host::account_service::ServiceFuture<
        '_,
        Option<spaceterm_host::account_service::LingerState>,
    > {
        Box::pin(async { Ok(None) })
    }
    fn configure<'a>(
        &'a self,
        _: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn start<'a>(
        &'a self,
        _: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn stop<'a>(
        &'a self,
        _: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn uninstall<'a>(
        &'a self,
        _: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_host::account_service::ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn verify_installation(
        &self,
        _: &spaceterm_host::account_service::RegisteredServer,
    ) -> Result<(), ServiceFailure> {
        Ok(())
    }
}
struct Executable;
impl spaceterm_host::account_service::ServiceInstallations for Executable {
    fn approved_executable(
        &self,
        _: &spaceterm_host::account_service::RegisteredServer,
    ) -> Result<std::path::PathBuf, ServiceFailure> {
        Ok(std::env::current_exe().unwrap())
    }
    fn uninstall(
        &self,
        _: &spaceterm_host::account_service::RegisteredServer,
    ) -> Result<(), ServiceFailure> {
        Ok(())
    }
}
struct BootClock([u8; 32]);
impl spaceterm_host::account_service::ServiceClock for BootClock {
    fn now(&self) -> Result<spaceterm_host::account_service::ServiceTime, ServiceFailure> {
        Ok(spaceterm_host::account_service::ServiceTime {
            boot: self.0,
            millis: 1,
        })
    }
}
fn custody_boot(registration: &spaceterm_host::account_service::RegisteredServer) -> [u8; 32] {
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(registration.data_directory().join("runtime-custody")).unwrap(),
    )
    .unwrap();
    let body: Vec<u8> = serde_json::from_value(record["body"].clone()).unwrap();
    let state: serde_json::Value = serde_json::from_slice(&body).unwrap();
    serde_json::from_value(state["owner"]["boot"].clone()).unwrap()
}
fn native_processes(boot: [u8; 32]) -> spaceterm_host::account_service::NativeServiceProcesses {
    spaceterm_host::account_service::NativeServiceProcesses::current(Arc::new(Executable))
        .unwrap()
        .with_manager(Arc::new(ScopeManager))
        .with_clock(Arc::new(BootClock(boot)))
        .with_runtime(Arc::new(RetainedRuntimeProcesses))
}

#[tokio::test]
async fn reboot_reconciles_exact_old_epoch_and_first_reasons_without_replacing_custody() {
    use spaceterm_core::{
        organization::RetirementReason,
        retirement::{RetiredSession, RetirementCleanupState},
    };
    let root = root();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let signer = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    let epoch = ServerEpoch::new(1);
    RuntimeCustody::begin(&registration, epoch, signer).unwrap();
    let boot = custody_boot(&registration);
    let session = SessionKey {
        pane: PaneId::new(ServerId::new(registration.server_pin().identity), 1).unwrap(),
        epoch,
        generation: SessionGeneration::new(1),
    };
    let expected = [RetiredSession::new(
        session,
        RetirementReason::WorktreeRemoval,
        RetirementCleanupState::AwaitingEvidence,
    )
    .unwrap()];
    let original = std::fs::read(registration.data_directory().join("runtime-custody")).unwrap();
    // Even an empty native-manager scope cannot excuse a live previous root.
    assert_eq!(
        native_processes(boot)
            .reconcile_previous_instance(&registration, epoch, &expected)
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    let mut next_boot = boot;
    next_boot[0] ^= 1;
    let observations = native_processes(next_boot)
        .reconcile_previous_instance(&registration, epoch, &expected)
        .await
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].session(), session);
    assert_eq!(
        observations[0].first_reason(),
        RetirementReason::WorktreeRemoval
    );
    assert_eq!(
        observations[0].result(),
        CleanupResult::Confirmed(ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership)
    );
    assert_eq!(
        native_processes(next_boot)
            .reconcile_previous_instance(&registration, ServerEpoch::new(2), &[])
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    assert_eq!(
        std::fs::read(registration.data_directory().join("runtime-custody")).unwrap(),
        original
    );
    assert_eq!(
        fixture.reopen().discover().unwrap().unwrap().server_pin(),
        registration.server_pin()
    );
    let listener =
        std::os::unix::net::UnixListener::bind(registration.endpoint().unwrap().path()).unwrap();
    assert_eq!(
        native_processes(next_boot)
            .reconcile_previous_instance(&registration, epoch, &expected)
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    drop(listener);
}

#[tokio::test]
async fn interrupted_recovery_reconciles_only_its_signed_prospective_epoch() {
    let root = root();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let signer = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    let custody =
        RuntimeCustody::begin(&registration, ServerEpoch::new(1), signer.clone()).unwrap();
    custody.completed_instance(&registration).unwrap();
    RuntimeCustody::prepare_start_for_epoch(&registration, signer.as_ref(), ServerEpoch::new(2))
        .unwrap();
    let boot = custody_boot(&registration);
    assert_eq!(
        native_processes(boot)
            .reconcile_previous_instance(&registration, ServerEpoch::new(2), &[])
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    let mut next_boot = boot;
    next_boot[0] ^= 1;
    let processes = native_processes(next_boot);
    // The database may be before or after the interrupted recovery's commit.
    processes
        .reconcile_previous_instance(&registration, ServerEpoch::new(1), &[])
        .await
        .unwrap();
    processes
        .reconcile_previous_instance(&registration, ServerEpoch::new(2), &[])
        .await
        .unwrap();
    assert_eq!(
        processes
            .reconcile_previous_instance(&registration, ServerEpoch::new(3), &[])
            .await,
        Err(ServiceFailure::CleanupUnresolved)
    );
    assert_eq!(
        fixture
            .reopen()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Running
    );
}
