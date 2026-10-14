// Reuse the signed native artifact shape from the account installation suite.
use spaceterm_host::{
    account_service::{test_support::AccountServiceFixture, *},
    startup::{BuildIdentity, DesiredServerState},
};
use spaceterm_server::deployment::setup::{SetupRequest, account_setup, prepare_confirmed_setup};
use std::sync::Arc;

#[path = "account_artifact.rs"]
mod artifact;
use artifact::{fixture, native_machine};

#[tokio::test]
async fn account_setup_requires_verified_artifact_and_retained_native_request() {
    use spaceterm_host::native_deployment::contracts::NativeRequestId;
    let root = tempfile::tempdir().unwrap();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let supervisor = account.reopen();
    let request = supervisor.allocate_request().unwrap();
    let (_key, artifact) = fixture("1.2.3", 91, native_machine());
    let before = std::fs::read(account.anchor().join("requests")).unwrap();
    // A test-authenticated artifact cannot cross the production trust boundary.
    let feed = root.path().join("feed");
    let signature = root.path().join("signature");
    let archive = root.path().join("archive");
    std::fs::write(&feed, artifact.feed().bytes()).unwrap();
    std::fs::write(&signature, artifact.feed().signature()).unwrap();
    std::fs::write(&archive, artifact.bytes()).unwrap();
    assert_eq!(
        prepare_confirmed_setup(
            &supervisor,
            &SetupRequest {
                request,
                feed,
                signature,
                archive
            }
        )
        .err(),
        Some(ServiceFailure::ArtifactRejected)
    );
    assert!(supervisor.discover().unwrap().is_none());
    assert!(!account.data_directory().exists());
    assert_eq!(
        std::fs::read(account.anchor().join("requests")).unwrap(),
        before
    );
    let unreserved = NativeRequestId::new([99; 16]).unwrap();
    assert_eq!(
        account_setup(&supervisor, unreserved, account.confirm_install(artifact))
            .await
            .err(),
        Some(ServiceFailure::UnknownRequest)
    );
    assert!(!account.data_directory().exists());
    assert_eq!(
        std::fs::read(account.anchor().join("requests")).unwrap(),
        before
    );

    // Isolated test composition supplies the real signed artifact and a retained
    // installed account. Reuse must not invoke a native manager or change Stop.
    let (_key, artifact) = fixture("1.2.3", 91, native_machine());
    let installation = InstallationStatus {
        installation: spaceterm_host::startup::InstallationId(
            std::num::NonZeroU64::new(1).unwrap(),
        ),
        version: artifact.manifest().version.clone(),
        artifact_sha256: artifact.artifact_sha256(),
    };
    let registration = account
        .initialize_with_installation(DesiredServerState::Stopped, Some(installation.clone()))
        .unwrap();
    let pin = registration.server_pin();
    let generation = registration.generation();
    let result = account_setup(&supervisor, request, account.confirm_install(artifact))
        .await
        .unwrap();
    assert_eq!(result.request, request);
    assert_eq!(
        result.state,
        ServiceRequestState::Completed(ServiceOutcome::Installed {
            server: pin,
            installation,
            reused: true
        })
    );
    let (_, artifact) = fixture("1.2.3", 91, native_machine());
    let replay = account_setup(
        &Arc::new(account.reopen()),
        request,
        account.confirm_install(artifact),
    )
    .await
    .unwrap();
    assert_eq!(replay, result);
    let current = supervisor.discover().unwrap().unwrap();
    assert_eq!(current.desired_state(), DesiredServerState::Stopped);
    assert_eq!(current.server_pin(), pin);
    assert_eq!(current.generation(), generation);
    let (_, changed) = fixture("1.2.4", 92, native_machine());
    assert_eq!(
        account_setup(&supervisor, request, account.confirm_install(changed))
            .await
            .err(),
        Some(ServiceFailure::RequestConflict)
    );
}

#[tokio::test]
async fn account_native_status_authenticates_live_approved_and_staged_code_without_mutation() {
    use spaceterm_core::{identity::ServerId, management::*};
    use spaceterm_server::{
        account_startup::test_support,
        listeners::{AccountNativeStatusObserver, NativeStatusObserver},
    };
    let root = tempfile::tempdir().unwrap();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let (key, artifact) = fixture("1.2.3", 91, native_machine());
    let approved = InstallationStatus {
        installation: spaceterm_host::startup::InstallationId(std::num::NonZeroU64::MIN),
        version: artifact.manifest().version.clone(),
        artifact_sha256: artifact.artifact_sha256(),
    };
    let registration = account
        .initialize_with_installation(DesiredServerState::Stopped, Some(approved.clone()))
        .unwrap();
    let store =
        spaceterm_host::server_installation::test_support::store(&registration, key.clone())
            .unwrap();
    let id = store.stage(&artifact).unwrap();
    assert_eq!(id, approved.installation);
    store
        .activate(
            id,
            store.snapshot().unwrap().generation,
            spaceterm_host::organization_store::schema::SCHEMA_VERSION,
        )
        .unwrap();
    let (_, staged) = fixture("1.2.4", 91, native_machine());
    let staged_id = store.stage(&staged).unwrap();
    account
        .stage_installation(InstallationStatus {
            installation: staged_id,
            version: staged.manifest().version.clone(),
            artifact_sha256: staged.artifact_sha256(),
        })
        .unwrap();
    let supervisor =
        test_support::supervisor(root.path(), Arc::new(test_support::StartupAbsence)).unwrap();
    let supervisor = Arc::new(account.with_installation_trust((*supervisor).clone(), key));
    let observer = AccountNativeStatusObserver::new(supervisor.clone());
    let before = std::fs::read(account.anchor().join("registration")).unwrap();
    let status = observer
        .observe(ServerId::new(registration.server_pin().identity))
        .await
        .unwrap();
    assert_eq!(status.verification, NativeVerification::Verified);
    assert_eq!(status.approved.as_ref().unwrap().as_str(), "1.2.3");
    assert_eq!(status.staged.as_ref().unwrap().as_str(), "1.2.4");
    assert!(
        status
            .capabilities
            .iter()
            .all(|value| value.availability == CapabilityAvailability::Available)
    );
    assert!(std::fs::read(account.anchor().join("registration")).unwrap() == before);
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
    assert!(!registration.endpoint().unwrap().path().exists());
    let request = supervisor.allocate_request().unwrap();
    spaceterm_host::account_service::test_support::interrupt_after_phase(ServicePhase::Accepted);
    assert_eq!(
        supervisor.start(request).await.err(),
        Some(ServiceFailure::SupervisorUnavailable)
    );
    let journal_before = std::fs::read(account.anchor().join("requests")).unwrap();
    let pending = observer
        .observe(ServerId::new(registration.server_pin().identity))
        .await
        .unwrap();
    assert_eq!(
        pending.progress[0].request,
        ManagementRequestReference::Native(*request.as_bytes())
    );
    assert_eq!(pending.progress[0].state, ManagementProgressState::Pending);
    assert!(std::fs::read(account.anchor().join("requests")).unwrap() == journal_before);
    assert!(std::fs::read(account.anchor().join("registration")).unwrap() == before);
    let executable = store.retain_approved().unwrap();
    std::fs::write(executable.path(), b"corrupted approved artifact").unwrap();
    let failed = observer
        .observe(ServerId::new(registration.server_pin().identity))
        .await
        .unwrap();
    assert_eq!(failed.verification, NativeVerification::Unverified);
    assert!(failed.approved.is_none());
    assert!(failed.capabilities.iter().any(|value| value.capability
        == CAPABILITY_SIGNED_SERVER_INSTALLATION
        && value.availability == CapabilityAvailability::Unavailable));
    assert!(std::fs::read(account.anchor().join("registration")).unwrap() == before);
}
