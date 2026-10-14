#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{fs, sync::Arc};

use spaceterm_core::{
    identity::{DeviceId, ServerId},
    operations::RequestDigest,
    pairing::{
        EnrollmentProposal, EnrollmentProposalParts, PairingFailure, PairingKeys, PairingKeysParts,
    },
};
use spaceterm_host::{
    account_service::test_support::AccountServiceFixture,
    startup::{BuildIdentity, DesiredServerState},
};
use spaceterm_protocol::pairing_adapter::enrollment_proposal_digest;
use spaceterm_server::{
    account_startup::AccountOwnerEnrollment, pairing::management::AccountOwnerEnrollmentPort,
};

fn proposal(server: ServerId, nonce: u8) -> EnrollmentProposal {
    EnrollmentProposal::new(EnrollmentProposalParts {
        server,
        device: DeviceId::new([2; 16]),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [3; 32],
            pairing_authentication_public_key: [4; 32],
        })
        .unwrap(),
        claim_nonce: [nonce; 32],
    })
    .unwrap()
}

#[test]
fn retained_owner_authority_validates_after_provider_reconstruction_without_reissuance() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let supervisor = Arc::new(fixture.reopen());
    let request = supervisor.allocate_request().unwrap();
    let proposal = proposal(server, 1);
    let port = AccountOwnerEnrollment::new(supervisor.clone());
    let digest = port.record_authorization(request, &proposal).unwrap();
    drop(port);
    drop(supervisor);

    let reconstructed = Arc::new(fixture.reopen());
    let port = AccountOwnerEnrollment::new(reconstructed.clone());
    let claim_digest = enrollment_proposal_digest(&proposal).unwrap();
    port.validate_owner_request(&request, digest, server, &proposal)
        .unwrap();
    port.revalidate_owner_issuance(&request, digest, server, claim_digest)
        .unwrap();
    // Validation never invokes Start or replaces the retained request authority.
    assert_eq!(
        reconstructed.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
    reconstructed.acknowledge_request(request).unwrap();
    let port = AccountOwnerEnrollment::new(Arc::new(fixture.reopen()));
    port.validate_owner_request(&request, digest, server, &proposal)
        .unwrap();
    port.revalidate_owner_issuance(&request, digest, server, claim_digest)
        .unwrap();
}

#[test]
fn missing_owner_record_is_not_created_by_either_validation_callback() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let supervisor = Arc::new(fixture.reopen());
    let request = supervisor.allocate_request().unwrap();
    let proposal = proposal(server, 1);
    let claim_digest = enrollment_proposal_digest(&proposal).unwrap();
    let digest = supervisor
        .owner_enrollment_digest(request, server, claim_digest)
        .unwrap();
    let port = AccountOwnerEnrollment::new(supervisor.clone());
    for _ in 0..2 {
        assert_eq!(
            port.validate_owner_request(&request, digest, server, &proposal),
            Err(PairingFailure::AuthorizationRequired)
        );
        assert_eq!(
            port.revalidate_owner_issuance(&request, digest, server, claim_digest),
            Err(PairingFailure::AuthorizationRequired)
        );
    }
    assert!(
        supervisor
            .validate_owner_enrollment(&request, digest, server, claim_digest)
            .is_err()
    );
}

#[test]
fn owner_authority_rejects_changed_claim_server_or_request_digest() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let port = AccountOwnerEnrollment::new(Arc::new(fixture.reopen()));
    let request = fixture.reopen().allocate_request().unwrap();
    let proposal = proposal(server, 1);
    let digest = port.record_authorization(request, &proposal).unwrap();
    let changed_claim = self::proposal(server, 2);
    assert!(
        port.validate_owner_request(&request, digest, server, &changed_claim)
            .is_err()
    );
    assert!(
        port.validate_owner_request(
            &request,
            RequestDigest::from_bytes([9; 32]),
            server,
            &proposal
        )
        .is_err()
    );
    assert!(
        port.validate_owner_request(&request, digest, ServerId::new([8; 16]), &proposal)
            .is_err()
    );
    port.validate_owner_request(&request, digest, server, &proposal)
        .unwrap();
}

#[test]
fn owner_issuance_retries_original_claim_but_cannot_replace_it() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let supervisor = Arc::new(fixture.reopen());
    let request = supervisor.allocate_request().unwrap();
    let port = AccountOwnerEnrollment::new(supervisor.clone());
    let original = proposal(server, 1);
    let digest = port.record_authorization(request, &original).unwrap();
    let retained = fs::read(fixture.anchor().join("requests")).unwrap();

    assert_eq!(
        port.record_authorization(request, &original).unwrap(),
        digest
    );
    assert_eq!(
        port.record_authorization(request, &proposal(server, 2)),
        Err(PairingFailure::RequestIdentityReused)
    );
    assert_eq!(
        fs::read(fixture.anchor().join("requests")).unwrap(),
        retained
    );
    port.validate_owner_request(&request, digest, server, &original)
        .unwrap();
    let current = supervisor.discover().unwrap().unwrap();
    assert_eq!(current.server_pin(), registration.server_pin());
    assert_eq!(current.generation(), registration.generation());
    assert_eq!(current.desired_state(), DesiredServerState::Stopped);
}

#[test]
fn foreign_build_cannot_consume_original_owner_request_or_create_authority() {
    let root = tempfile::tempdir().unwrap();
    let original = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = original.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let request = original.reopen().allocate_request().unwrap();
    let claim = proposal(server, 1);
    let issuer = AccountOwnerEnrollment::new(Arc::new(original.reopen()));
    let digest = issuer.record_authorization(request, &claim).unwrap();
    let other = AccountServiceFixture::new(root.path(), BuildIdentity::Preflight).unwrap();
    let foreign = AccountOwnerEnrollment::new(Arc::new(other.reopen()));

    assert_eq!(
        foreign.validate_owner_request(&request, digest, server, &claim),
        Err(PairingFailure::AuthorizationRequired)
    );
    assert_eq!(
        foreign.revalidate_owner_issuance(
            &request,
            digest,
            server,
            enrollment_proposal_digest(&claim).unwrap()
        ),
        Err(PairingFailure::AuthorizationRequired)
    );
    assert!(!other.anchor().exists());
    assert!(!other.data_directory().exists());
    issuer
        .validate_owner_request(&request, digest, server, &claim)
        .unwrap();
}

#[test]
fn unavailable_owner_journal_fails_validation_without_repair_or_reissuance() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let request = fixture.reopen().allocate_request().unwrap();
    let claim = proposal(server, 1);
    let port = AccountOwnerEnrollment::new(Arc::new(fixture.reopen()));
    let digest = port.record_authorization(request, &claim).unwrap();
    let journal = fixture.anchor().join("requests");
    fs::remove_file(&journal).unwrap();
    let port = AccountOwnerEnrollment::new(Arc::new(fixture.reopen()));

    assert_eq!(
        port.validate_owner_request(&request, digest, server, &claim),
        Err(PairingFailure::AuthorizationRequired)
    );
    assert_eq!(
        port.revalidate_owner_issuance(
            &request,
            digest,
            server,
            enrollment_proposal_digest(&claim).unwrap()
        ),
        Err(PairingFailure::AuthorizationRequired)
    );
    assert!(!journal.exists());
    assert_eq!(
        fixture
            .reopen()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
}

#[path = "support/job_a.rs"]
mod native;

use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{organization::ServerDirectory, pairing::PairingOutcome};
use spaceterm_host::{
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits,
        bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
        migrations, schema,
    },
    storage::Database,
};
use spaceterm_protocol::{authentication::SigningKey, pairing_adapter::VerifiedDeviceClaim};
use spaceterm_server::account_owner_enrollment::{
    AccountOwnerRequestExecutor, StoppedOwnerProviders,
};

struct DeviceKey(Ed25519KeyPair);
impl DeviceKey {
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
}
impl SigningKey for DeviceKey {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], spaceterm_protocol::ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
fn verified_claim(server: ServerId, seed: u8) -> VerifiedDeviceClaim {
    let device = DeviceKey::new(seed);
    let auth = DeviceKey::new(seed + 1);
    let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
        server,
        device: DeviceId::new([seed; 16]),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key(),
            pairing_authentication_public_key: auth.public_key(),
        })
        .unwrap(),
        claim_nonce: [seed; 32],
    })
    .unwrap();
    let (device_proof, auth_proof) =
        VerifiedDeviceClaim::proofs(&proposal, &device, &auth).unwrap();
    VerifiedDeviceClaim::verify(proposal, &device_proof, &auth_proof).unwrap()
}
fn initialized_account(
    desired: DesiredServerState,
) -> (tempfile::TempDir, AccountServiceFixture, ServerDirectory) {
    initialized_account_with_installation(desired, None)
}
fn initialized_account_with_installation(
    desired: DesiredServerState,
    installation: Option<spaceterm_host::account_service::InstallationStatus>,
) -> (tempfile::TempDir, AccountServiceFixture, ServerDirectory) {
    #[cfg(target_os = "macos")]
    let parent = std::path::PathBuf::from("/private/tmp");
    // Keep real SQLite synchronization off the shared Linux build filesystem.
    #[cfg(target_os = "linux")]
    let parent = std::path::PathBuf::from("/var/tmp");
    let root = tempfile::tempdir_in(parent).unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture
        .initialize_with_installation(desired, installation)
        .unwrap();
    let identity = StoreIdentity {
        server: ServerId::new(registration.server_pin().identity),
        account: registration.key(),
        server_public_key: registration.server_pin().public_key,
    };
    let home = ServerDirectory::new(
        identity.server,
        root.path().as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let database = Database::open_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    let services =
        spaceterm_server::adapters::store::production_services(identity, home.clone()).unwrap();
    assert!(matches!(
        SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity,
                origin: NewServerOrigin::LocalHeadless
            },
            StoreLimits::production(),
            services
        ),
        InitializationDisposition::Durable { .. }
    ));
    (root, fixture, home)
}
fn stopped_providers() -> (
    StoppedOwnerProviders,
    Arc<native::Factory>,
    Arc<native::External>,
) {
    let boundary = native::Fixture::new();
    let runtime = boundary.factory.clone();
    let external = boundary.external.clone();
    (
        StoppedOwnerProviders {
            config: boundary.config,
            host_management: boundary.dependencies.host_management,
            external_work: boundary.dependencies.external_work,
            clock: boundary.dependencies.clock,
        },
        runtime,
        external,
    )
}

#[tokio::test]
async fn stopped_owner_request_uses_exclusive_real_actor_without_start_or_second_store() {
    let (_root, fixture, home) = initialized_account(DesiredServerState::Stopped);
    let supervisor = Arc::new(fixture.reopen());
    let registration = supervisor.discover().unwrap().unwrap();
    let pin = registration.server_pin();
    let generation = registration.generation();
    let request = supervisor.allocate_request().unwrap();
    let claim = verified_claim(ServerId::new(pin.identity), 41);
    let (providers, runtime, external) = stopped_providers();
    let executor = AccountOwnerRequestExecutor::stopped(
        supervisor.clone(),
        home.clone(),
        request,
        &claim,
        providers,
    )
    .await
    .unwrap();
    let issued = executor.issue(request, claim).await.unwrap();
    assert!(matches!(
        issued.outcome,
        PairingOutcome::AuthorizationReserved { .. }
    ));
    assert!(issued.authorization.is_some());
    let different_request = supervisor.allocate_request().unwrap();
    assert_eq!(
        executor
            .issue(
                different_request,
                verified_claim(ServerId::new(pin.identity), 42)
            )
            .await
            .err(),
        Some(PairingFailure::RequestIdentityReused)
    );
    let (providers, _, _) = stopped_providers();
    assert!(
        AccountOwnerRequestExecutor::stopped(
            supervisor.clone(),
            home,
            request,
            &verified_claim(ServerId::new(pin.identity), 41),
            providers
        )
        .await
        .is_err()
    );
    assert!(runtime.starts.lock().unwrap().is_empty());
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
    let report = executor.close().await.unwrap().unwrap();
    assert!(report.cleanup_confirmed && report.listeners_closed && report.started_writes_finished);
    assert_eq!(
        external.finished.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let current = supervisor.discover().unwrap().unwrap();
    assert_eq!(current.server_pin(), pin);
    assert_eq!(current.generation(), generation);
    assert_eq!(current.desired_state(), DesiredServerState::Stopped);
}

#[tokio::test]
async fn stopped_owner_lost_reply_replays_original_request_after_reconstruction() {
    let (_root, fixture, home) = initialized_account(DesiredServerState::Stopped);
    let supervisor = Arc::new(fixture.reopen());
    let server = ServerId::new(
        supervisor
            .discover()
            .unwrap()
            .unwrap()
            .server_pin()
            .identity,
    );
    let request = supervisor.allocate_request().unwrap();
    let (providers, _, _) = stopped_providers();
    let executor = AccountOwnerRequestExecutor::stopped(
        supervisor.clone(),
        home.clone(),
        request,
        &verified_claim(server, 41),
        providers,
    )
    .await
    .unwrap();
    let original = executor
        .issue(request, verified_claim(server, 41))
        .await
        .unwrap();
    let epoch = spaceterm_core::authority::ServerEpoch::new(1);
    executor.close().await.unwrap();
    let registration = supervisor.discover().unwrap().unwrap();
    use spaceterm_host::account_service::ServiceProcesses;
    spaceterm_host::account_service::runtime_inventory::RetainedRuntimeProcesses
        .previous_instance_cleanup(&registration, epoch, &[]).await
        .expect("stopped enrollment must publish its committed epoch for reconstruction and explicit Start");
    let (providers, _, _) = stopped_providers();
    let executor = AccountOwnerRequestExecutor::stopped(
        Arc::new(fixture.reopen()),
        home,
        request,
        &verified_claim(server, 41),
        providers,
    )
    .await
    .unwrap();
    let replay = executor
        .issue(request, verified_claim(server, 41))
        .await
        .unwrap();
    assert_eq!(replay.outcome, original.outcome);
    assert!(replay.authorization.is_none());
    assert_eq!(
        executor
            .issue(request, verified_claim(server, 42))
            .await
            .err(),
        Some(PairingFailure::RequestIdentityReused)
    );
    executor.close().await.unwrap();
}

#[tokio::test]
async fn interrupted_stopped_owner_epoch_recovers_only_the_original_request() {
    use spaceterm_host::account_service::{
        ServiceProcesses, runtime_inventory::RetainedRuntimeProcesses,
    };
    let (_root, fixture, home) = initialized_account(DesiredServerState::Stopped);
    let supervisor = Arc::new(fixture.reopen());
    let registration = supervisor.discover().unwrap().unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let request = supervisor.allocate_request().unwrap();
    let (providers, _, _) = stopped_providers();
    let executor = AccountOwnerRequestExecutor::stopped(
        supervisor.clone(),
        home.clone(),
        request,
        &verified_claim(server, 41),
        providers,
    )
    .await
    .unwrap();
    executor
        .issue(request, verified_claim(server, 41))
        .await
        .unwrap();
    assert!(
        RetainedRuntimeProcesses
            .previous_instance_cleanup(
                &registration,
                spaceterm_core::authority::ServerEpoch::new(1),
                &[]
            )
            .await
            .is_err()
    );
    drop(executor);
    let recovered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let (providers, _, _) = stopped_providers();
            match AccountOwnerRequestExecutor::stopped(
                supervisor.clone(),
                home.clone(),
                request,
                &verified_claim(server, 41),
                providers,
            )
            .await
            {
                Ok(executor) => break executor,
                Err(
                    PairingFailure::RecoveryRequired | PairingFailure::AuthorizationUnavailable,
                ) => tokio::task::yield_now().await,
                Err(error) => panic!("original stopped epoch reconstruction: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
    recovered.close().await.unwrap();
    RetainedRuntimeProcesses
        .previous_instance_cleanup(
            &registration,
            spaceterm_core::authority::ServerEpoch::new(2),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
}

#[tokio::test]
async fn explicit_start_recovers_an_interrupted_stopped_owner_epoch_with_native_custody() {
    use spaceterm_host::account_service::{
        TrustedAccountFacts, runtime_inventory::RetainedRuntimeProcesses,
    };
    use spaceterm_server::account_startup::{AccountStartup, RecoveredAccountServer};
    let installation = spaceterm_host::account_service::InstallationStatus {
        installation: spaceterm_host::startup::InstallationId(
            std::num::NonZeroU64::new(1).unwrap(),
        ),
        version: spaceterm_distribution::BuildVersion::parse("1.0.0").unwrap(),
        artifact_sha256: [1; 32],
    };
    let (root, fixture, home) =
        initialized_account_with_installation(DesiredServerState::Stopped, Some(installation));
    let supervisor = spaceterm_server::account_startup::test_support::supervisor(
        root.path(),
        Arc::new(RetainedRuntimeProcesses),
    )
    .unwrap();
    let registration = supervisor.discover().unwrap().unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let request = supervisor.allocate_request().unwrap();
    let (providers, _, _) = stopped_providers();
    let executor = AccountOwnerRequestExecutor::stopped(
        supervisor.clone(),
        home,
        request,
        &verified_claim(server, 41),
        providers,
    )
    .await
    .unwrap();
    executor
        .issue(request, verified_claim(server, 41))
        .await
        .unwrap();
    drop(executor);
    let start = supervisor.allocate_request().unwrap();
    supervisor.start(start).await.unwrap();
    let startup = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match AccountStartup::admit(&supervisor) {
                Ok(startup) => break startup,
                Err(spaceterm_server::StartupFailure::Account(
                    spaceterm_host::account_service::ServiceFailure::Pending,
                )) => tokio::task::yield_now().await,
                Err(error) => panic!("native startup admission: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
    let facts = TrustedAccountFacts::current().unwrap();
    let launch = spaceterm_server::adapters::terminal::AccountLaunchFacts {
        uid: facts.uid(),
        home: root.path().to_owned(),
        planner: spaceterm_host::shell_launch::ShellLaunchPlanner::for_test(
            root.path().join("shell"),
            root.path().to_owned(),
        ),
        machine: spaceterm_terminal::metadata::LocalMachine::default(),
    };
    let runtime = RecoveredAccountServer::recover(supervisor.clone(), startup, launch).await.expect("explicit Start must reconcile the retained stopped epoch without repeating owner issuance");
    assert_eq!(
        runtime.handle().snapshot().epoch(),
        spaceterm_core::authority::ServerEpoch::new(2)
    );
    assert_eq!(
        fixture.reopen().discover().unwrap().unwrap().server_pin(),
        registration.server_pin()
    );
    drop(runtime);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match supervisor.admit_startup() {
                Ok(spaceterm_host::account_service::AccountStartupAdmission::Ready(admission)) => {
                    drop(admission);
                    break;
                }
                Err(spaceterm_host::account_service::ServiceFailure::Pending) => {
                    tokio::task::yield_now().await
                }
                other => panic!("started actor must release its native lease: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
}

async fn enroll_owner(
    executor: &AccountOwnerRequestExecutor,
    request: spaceterm_host::native_deployment::contracts::NativeRequestId,
    pin: spaceterm_protocol::authentication::ServerPin,
    seed: u8,
    major: u32,
) -> spaceterm_core::pairing::SignedPairingReceipt {
    use spaceterm_protocol::pairing_adapter::{EnrollmentClient, ReceiptCodec};
    let issued = executor
        .issue(request, verified_claim(ServerId::new(pin.identity), seed))
        .await
        .unwrap();
    let PairingOutcome::AuthorizationReserved { ticket, .. } = issued.outcome else {
        panic!("owner reservation");
    };
    let public = executor.enrollment_reservation(&ticket).unwrap();
    let (client, hello) = EnrollmentClient::begin(
        pin,
        public,
        vec![major],
        Arc::new(DeviceKey::new(seed)),
        Arc::new(DeviceKey::new(seed + 1)),
    )
    .unwrap();
    let (server, hello) = executor.accept_enrollment(&hello).unwrap();
    let (client, proof) = client.receive_server(&hello).unwrap();
    let (mut server, ready) = server.receive_proof(&proof).unwrap();
    let mut client = client.receive_ready(&ready).unwrap();
    let submission = client
        .submission_message(
            issued.authorization.as_ref().unwrap(),
            &DeviceKey::new(seed),
        )
        .unwrap();
    let reply = executor
        .enrollment_frame(&mut server, &submission)
        .await
        .unwrap();
    let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
        client.decode(&reply).unwrap().kind.unwrap()
    else {
        panic!("enrollment result");
    };
    let receipt = ReceiptCodec::new(pin)
        .from_wire(&result.receipt.unwrap())
        .unwrap();
    let lookup = client.lookup_message().unwrap();
    let reply = executor
        .enrollment_frame(&mut server, &lookup)
        .await
        .unwrap();
    let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
        client.decode(&reply).unwrap().kind.unwrap()
    else {
        panic!("lookup result");
    };
    assert_eq!(
        ReceiptCodec::new(pin)
            .from_wire(&result.receipt.unwrap())
            .unwrap(),
        receipt
    );
    receipt
}

#[tokio::test]
async fn stopped_owner_consumes_verified_claim_in_both_majors_and_keeps_stop() {
    for major in [1, 2] {
        let (_root, fixture, home) = initialized_account(DesiredServerState::Stopped);
        let supervisor = Arc::new(fixture.reopen());
        let registration = supervisor.discover().unwrap().unwrap();
        let pin = registration.server_pin();
        let key_before = fs::read(fixture.data_directory().join("identity")).unwrap();
        let request = supervisor.allocate_request().unwrap();
        let (providers, runtime, _) = stopped_providers();
        let executor = AccountOwnerRequestExecutor::stopped(
            supervisor.clone(),
            home.clone(),
            request,
            &verified_claim(ServerId::new(pin.identity), 41),
            providers,
        )
        .await
        .unwrap();
        enroll_owner(&executor, request, pin, 41, major).await;
        executor.close().await.unwrap();
        assert!(runtime.starts.lock().unwrap().is_empty());
        assert!(!registration.endpoint().unwrap().path().exists());
        assert!(fs::read(fixture.data_directory().join("identity")).unwrap() == key_before);
        assert_eq!(
            supervisor.discover().unwrap().unwrap().desired_state(),
            DesiredServerState::Stopped
        );
        let account = spaceterm_server::account_startup::AccountStartup::open(
            supervisor.discover().unwrap().unwrap(),
        );
        assert_eq!(
            account.unwrap_err(),
            spaceterm_server::StartupFailure::DeliberatelyStopped
        );
        let database = Database::open_existing_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        let identity = StoreIdentity {
            server: ServerId::new(pin.identity),
            account: registration.key(),
            server_public_key: pin.public_key,
        };
        let services =
            spaceterm_server::adapters::store::production_services(identity, home).unwrap();
        let mut store = SqliteOrganizationStore::open_existing(
            database,
            identity,
            StoreLimits::production(),
            services,
        )
        .unwrap();
        use spaceterm_core::storage::OrganizationStore;
        let recovered = store.recover().unwrap();
        assert_eq!(recovered.pairing.unwrap().parts().authorizations.len(), 1);
        assert!(recovered.organization.workspaces.is_empty());
    }
}

struct RunningAuthority(spaceterm_protocol::authentication::ServerPin);
impl spaceterm_server::api::StartupAuthorityPort for RunningAuthority {
    fn validate_startup(&self) -> Result<ServerId, spaceterm_server::contracts::ServerFailure> {
        Ok(ServerId::new(self.0.identity))
    }
    fn validate_credentials(
        &self,
        credentials: &dyn spaceterm_server::contracts::ServerCredentialPort,
        server: ServerId,
    ) -> Result<(), spaceterm_server::contracts::ServerFailure> {
        assert_eq!(server.bytes(), self.0.identity);
        assert_eq!(credentials.public_key(), self.0.public_key);
        Ok(())
    }
}

#[tokio::test]
async fn running_owner_issuance_reuses_actor_without_recovery_launch_or_organization_changes() {
    use spaceterm_core::storage::OrganizationStore;
    use spaceterm_server::{
        account_startup::AccountStartup, adapters::core::CorePorts, service::RecoveredServer,
    };
    let (_root, fixture, home) = initialized_account(DesiredServerState::Running);
    let supervisor = Arc::new(fixture.reopen());
    let registration = supervisor.discover().unwrap().unwrap();
    let pin = registration.server_pin();
    let identity = StoreIdentity {
        server: ServerId::new(pin.identity),
        account: registration.key(),
        server_public_key: pin.public_key,
    };
    let account = AccountStartup::open(registration)
        .unwrap()
        .open_store(home.clone())
        .unwrap();
    let mut boundary = native::Fixture::new();
    let runtime = boundary.factory.clone();
    let host = boundary.host.clone();
    boundary.config.host.authority = Arc::new(RunningAuthority(pin));
    boundary.config.core =
        CorePorts::production(identity, home, boundary.dependencies.clock.clone()).unwrap();
    boundary.dependencies.store = Box::new(account.store);
    let credentials = account.credentials.clone();
    boundary.dependencies.credentials = account.credentials;
    let server = RecoveredServer::recover(boundary.config, boundary.dependencies)
        .await
        .unwrap();
    let executor =
        AccountOwnerRequestExecutor::running(supervisor.clone(), server.handle()).unwrap();
    let initial_request = supervisor.allocate_request().unwrap();
    let enrollment = enroll_owner(&executor, initial_request, pin, 42, 2).await;
    let spaceterm_core::pairing::PairingReceiptBody::Enrollment { target, client, .. } =
        &enrollment.parts().claims
    else {
        panic!("enrollment");
    };
    let (_, spaceterm_protocol::authentication::Established::Ordinary(codec)) =
        authenticated_pairing(
            &server.handle(),
            credentials.clone(),
            client,
            42,
            spaceterm_protocol::authentication::Purpose::Ordinary,
        )
    else {
        panic!("ordinary authentication");
    };
    let principal = spaceterm_server::api::VerifiedPrincipal::from_authenticated(
        &codec,
        spaceterm_core::authority::ConnectionGeneration::new(1),
    )
    .unwrap();
    let session = start_owner_session(&server.handle(), principal, client.parts().ordinary).await;
    let snapshot = server.handle().snapshot();
    let request = supervisor.allocate_request().unwrap();
    let original = executor
        .issue(request, verified_claim(identity.server, 41))
        .await
        .unwrap();
    let replay = executor
        .issue(request, verified_claim(identity.server, 41))
        .await
        .unwrap();
    assert_eq!(original.outcome, replay.outcome);
    assert!(replay.authorization.is_none());
    let after = server.handle().snapshot();
    assert_eq!(after.state(), snapshot.state());
    assert_eq!(after.runtime(), snapshot.runtime());
    assert_eq!(after.epoch(), snapshot.epoch());
    assert_eq!(runtime.starts.lock().unwrap().len(), 1);
    assert_eq!(host.requests.lock().unwrap().len(), 1);
    revoke_owner_pairing(
        &server.handle(),
        credentials,
        supervisor.clone(),
        target.clone(),
        client,
        42,
    )
    .await;
    let revoked = server.handle().snapshot();
    assert_eq!(revoked.state().workspaces, snapshot.state().workspaces);
    assert_eq!(revoked.state().tabs, snapshot.state().tabs);
    assert_eq!(revoked.state().panes, snapshot.state().panes);
    assert_eq!(revoked.state().settings, snapshot.state().settings);
    assert!(server.handle().register(principal).await.is_err());
    let request = supervisor.allocate_request().unwrap();
    let reenrollment = enroll_owner(&executor, request, pin, 42, 2).await;
    let spaceterm_core::pairing::PairingReceiptBody::Enrollment {
        target: replacement,
        client: new_client,
        ..
    } = &reenrollment.parts().claims
    else {
        panic!("reenrollment");
    };
    assert_ne!(replacement.parts().pairing, target.parts().pairing);
    assert_ne!(new_client.parts().client.id, client.parts().client.id);
    let recovered_view = server.handle().snapshot();
    assert_eq!(recovered_view.state(), revoked.state());
    assert!(
        matches!(recovered_view.runtime()[0].phase, spaceterm_core::organization::SessionPhase::Running { generation } if generation == session.generation)
    );
    assert_eq!(
        recovered_view.runtime()[0].control,
        spaceterm_core::organization::ControlRecord::Unclaimed
    );
    assert_eq!(runtime.starts.lock().unwrap().len(), 1);
    assert!(executor.close().await.unwrap().is_none());
    assert_eq!(
        server.handle().phase(),
        spaceterm_server::api::ServicePhase::Ready
    );
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
    server
        .handle()
        .shutdown(spaceterm_server::shutdown::TrustedShutdownRequest { authority: host })
        .await
        .unwrap();
    server.wait().await.unwrap();
    let registration = supervisor.discover().unwrap().unwrap();
    let mut store = AccountStartup::open(registration)
        .unwrap()
        .open_store(
            ServerDirectory::new(
                identity.server,
                _root.path().as_os_str().as_encoded_bytes().to_vec(),
            )
            .unwrap(),
        )
        .unwrap()
        .store;
    let recovered = store.recover().unwrap();
    assert_eq!(recovered.pairing.unwrap().parts().authorizations.len(), 2);
}

#[test]
fn archived_owner_request_replay_never_allocates_or_rewrites_authority() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let supervisor = Arc::new(fixture.reopen());
    let request = supervisor.allocate_request().unwrap();
    let original = proposal(ServerId::new(registration.server_pin().identity), 1);
    let port = AccountOwnerEnrollment::new(supervisor.clone());
    let digest = port.record_authorization(request, &original).unwrap();
    supervisor.acknowledge_request(request).unwrap();
    let journal = fs::read(fixture.anchor().join("requests")).unwrap();
    let reconstructed = AccountOwnerEnrollment::new(Arc::new(fixture.reopen()));
    assert_eq!(
        reconstructed
            .record_authorization(request, &original)
            .unwrap(),
        digest
    );
    assert_eq!(
        reconstructed.record_authorization(request, &proposal(original.parts().server, 2)),
        Err(PairingFailure::RequestIdentityReused)
    );
    assert_eq!(
        fs::read(fixture.anchor().join("requests")).unwrap(),
        journal
    );
}

#[tokio::test]
async fn consumed_stopped_owner_receipt_survives_restart_and_native_acknowledgement() {
    use spaceterm_protocol::pairing_adapter::{EnrollmentClient, ReceiptCodec};
    for major in [1, 2] {
        let (_root, fixture, home) = initialized_account(DesiredServerState::Stopped);
        let supervisor = Arc::new(fixture.reopen());
        let registration = supervisor.discover().unwrap().unwrap();
        let pin = registration.server_pin();
        let request = supervisor.allocate_request().unwrap();
        let (providers, _, _) = stopped_providers();
        let executor = AccountOwnerRequestExecutor::stopped(
            supervisor.clone(),
            home.clone(),
            request,
            &verified_claim(ServerId::new(pin.identity), 41),
            providers,
        )
        .await
        .unwrap();
        let original_receipt = enroll_owner(&executor, request, pin, 41, major).await;
        let original = executor
            .issue(request, verified_claim(ServerId::new(pin.identity), 41))
            .await
            .unwrap();
        let PairingOutcome::AuthorizationReserved { ticket, .. } = &original.outcome else {
            panic!("owner reservation");
        };
        let public = executor.enrollment_reservation(ticket).unwrap();
        executor.close().await.unwrap();
        supervisor.acknowledge_request(request).unwrap();
        let journal = fs::read(fixture.anchor().join("requests")).unwrap();
        assert_eq!(
            supervisor.owner_enrollment_digest(
                request,
                ServerId::new(pin.identity),
                enrollment_proposal_digest(
                    verified_claim(ServerId::new(pin.identity), 41).proposal()
                )
                .unwrap(),
            ),
            Err(spaceterm_host::account_service::ServiceFailure::RequestRetired)
        );
        let (providers, runtime, _) = stopped_providers();
        let executor = AccountOwnerRequestExecutor::stopped(
            Arc::new(fixture.reopen()),
            home,
            request,
            &verified_claim(ServerId::new(pin.identity), 41),
            providers,
        )
        .await
        .unwrap();
        let replay = executor
            .issue(request, verified_claim(ServerId::new(pin.identity), 41))
            .await
            .unwrap();
        assert_eq!(replay.outcome, original.outcome);
        assert!(replay.authorization.is_none());
        let (client, hello) = EnrollmentClient::begin(
            pin,
            public,
            vec![major],
            Arc::new(DeviceKey::new(41)),
            Arc::new(DeviceKey::new(42)),
        )
        .unwrap();
        let (server, hello) = executor.accept_enrollment(&hello).unwrap();
        let (client, proof) = client.receive_server(&hello).unwrap();
        let (mut server, ready) = server.receive_proof(&proof).unwrap();
        let mut client = client.receive_ready(&ready).unwrap();
        let lookup = client.lookup_message().unwrap();
        let reply = executor
            .enrollment_frame(&mut server, &lookup)
            .await
            .unwrap();
        let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
            client.decode(&reply).unwrap().kind.unwrap()
        else {
            panic!("lookup result");
        };
        assert_eq!(
            ReceiptCodec::new(pin)
                .from_wire(&result.receipt.unwrap())
                .unwrap(),
            original_receipt
        );
        executor.close().await.unwrap();
        assert_eq!(
            fs::read(fixture.anchor().join("requests")).unwrap(),
            journal
        );
        assert!(runtime.starts.lock().unwrap().is_empty());
        assert!(!registration.endpoint().unwrap().path().exists());
        let retained = supervisor.discover().unwrap().unwrap();
        assert_eq!(retained.server_pin(), pin);
        assert_eq!(retained.generation(), registration.generation());
        assert_eq!(retained.desired_state(), DesiredServerState::Stopped);
    }
}

#[tokio::test]
async fn owner_consumption_revalidates_stopped_registration_inside_actor() {
    use spaceterm_protocol::pairing_adapter::EnrollmentClient;
    let (_root, fixture, home) = initialized_account(DesiredServerState::Stopped);
    let supervisor = Arc::new(fixture.reopen());
    let retained_registration = supervisor.discover().unwrap().unwrap();
    let pin = retained_registration.server_pin();
    let request = supervisor.allocate_request().unwrap();
    let (providers, _, _) = stopped_providers();
    let executor = AccountOwnerRequestExecutor::stopped(
        supervisor.clone(),
        home,
        request,
        &verified_claim(ServerId::new(pin.identity), 41),
        providers,
    )
    .await
    .unwrap();
    let issued = executor
        .issue(request, verified_claim(ServerId::new(pin.identity), 41))
        .await
        .unwrap();
    let PairingOutcome::AuthorizationReserved { ticket, .. } = issued.outcome else {
        panic!("reservation");
    };
    let (client, hello) = EnrollmentClient::begin(
        pin,
        executor.enrollment_reservation(&ticket).unwrap(),
        vec![2],
        Arc::new(DeviceKey::new(41)),
        Arc::new(DeviceKey::new(42)),
    )
    .unwrap();
    let (server, hello) = executor.accept_enrollment(&hello).unwrap();
    let (client, proof) = client.receive_server(&hello).unwrap();
    let (mut server, ready) = server.receive_proof(&proof).unwrap();
    let mut client = client.receive_ready(&ready).unwrap();
    let submit = client
        .submission_message(issued.authorization.as_ref().unwrap(), &DeviceKey::new(41))
        .unwrap();
    // Simulate a native publisher committing Running while this owner actor owns
    // the instance lease. The replacement server still cannot open its store.
    let registration = fixture.anchor().join("registration");
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&registration).unwrap()).unwrap();
    record["desired"] = serde_json::json!("Running");
    record["generation"] = serde_json::json!(record["generation"].as_u64().unwrap() + 1);
    fs::write(&registration, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(
        executor
            .enrollment_frame(&mut server, &submit)
            .await
            .is_err()
    );
    // Lost native admission cannot publish a positive stopped-instance proof,
    // even though the actor and physical store have joined.
    assert_eq!(
        executor.close().await.unwrap_err().class,
        spaceterm_server::contracts::ServerFailureClass::RecoveryRequired
    );
    use spaceterm_host::account_service::ServiceProcesses;
    assert!(
        spaceterm_host::account_service::runtime_inventory::RetainedRuntimeProcesses
            .previous_instance_cleanup(
                &retained_registration,
                spaceterm_core::authority::ServerEpoch::new(1),
                &[]
            )
            .await
            .is_err()
    );
}

fn authenticated_pairing(
    handle: &spaceterm_server::api::ServerHandle,
    signer: Arc<spaceterm_host::server_keys::ServerIdentity>,
    streams: &spaceterm_core::pairing::PairingClientStreams,
    seed: u8,
    purpose: spaceterm_protocol::authentication::Purpose,
) -> (
    spaceterm_protocol::authentication::Established,
    spaceterm_protocol::authentication::Established,
) {
    use spaceterm_protocol::authentication as auth;
    let pin = signer.pin();
    let lookup =
        spaceterm_server::pairing::protocol_lookup::PairingProtocolLookup::new(handle.clone());
    let principal = auth::Principal {
        server: pin.identity,
        device: [seed; 16],
        pairing: native::local(streams.parts().client.pairing.local()),
        client: native::local(streams.parts().client.id.local()),
    };
    let config = auth::HandshakeConfig {
        majors: vec![2],
        capabilities: vec![spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN],
        ..Default::default()
    };
    let (client, hello) = auth::ClientHandshake::begin(
        pin,
        principal,
        purpose,
        config.clone(),
        Arc::new(DeviceKey::new(seed + 1)),
    )
    .unwrap();
    let (server, hello) = auth::ServerHandshake::accept(
        pin.identity,
        handle.snapshot().epoch().get(),
        config,
        signer,
        &lookup,
        &hello,
    )
    .unwrap();
    let (client, proof) = client.receive_server(&hello).unwrap();
    let (server, ready) = server.receive_device(&lookup, &proof).unwrap();
    (client.receive_ready(&ready).unwrap(), server)
}

async fn start_owner_session(
    handle: &spaceterm_server::api::ServerHandle,
    principal: spaceterm_server::api::VerifiedPrincipal,
    stream: spaceterm_core::identity::OperationStreamId,
) -> spaceterm_core::authority::SessionKey {
    use spaceterm_core::{
        authority::*, control::*, geometry::*, operation_ledger::canonical::CanonicalCodec,
        operation_ledger_contracts::CanonicalOperationRequest, operations::*, organization::*,
    };
    use spaceterm_server::api::OperationResponse;
    native::connect(handle, principal).await;
    let request = |sequence, command| {
        CanonicalOperationRequest::new(
            OperationId::new(stream, OperationSequence::new(sequence).unwrap()),
            command,
            &CanonicalCodec,
        )
        .unwrap()
    };
    let OperationResponse::Completed {
        outcome: Outcome::CreatedWithControl(created),
        ..
    } = handle
        .submit(
            principal,
            request(
                1,
                Command::CreateWorkspace {
                    server: stream.server(),
                    source: WorkspaceCreationSource::ServerHome {
                        server: stream.server(),
                    },
                    name: NameDraft::Custom(NameText::new("Owner recovery".into()).unwrap()),
                },
            ),
        )
        .await
        .unwrap()
    else {
        panic!("workspace");
    };
    let grant = created.control().grant();
    let generation = handle
        .snapshot()
        .runtime()
        .iter()
        .find(|p| p.pane == grant.pane())
        .unwrap()
        .session_generation;
    let fence = EffectFence::new(
        SessionKey {
            epoch: principal.epoch(),
            pane: grant.pane(),
            generation,
        },
        grant,
        principal.binding(),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    let intent = StartupIntent::new(
        OperationId::new(stream, OperationSequence::new(2).unwrap()),
        fence,
        StartupSource::StartSession,
    )
    .unwrap();
    handle
        .submit(principal, request(2, Command::BeginStartup(intent)))
        .await
        .unwrap();
    let OperationResponse::Completed {
        outcome: Outcome::SessionStarted(session),
        ..
    } = handle
        .submit(
            principal,
            request(
                3,
                Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
                    intent: intent.id(),
                    fence,
                    grid: MeasuredGrid::new(
                        37,
                        113,
                        CellPixels::new(9, 18).unwrap(),
                        SUPPORTED_GRID_LIMITS,
                    )
                    .unwrap(),
                }),
            ),
        )
        .await
        .unwrap()
    else {
        panic!("session");
    };
    let mut snapshot = handle.subscribe_snapshot();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !snapshot.borrow().runtime().iter().any(|p| p.pane == session.pane && matches!(p.phase, SessionPhase::Running { generation } if generation == session.generation)) {
            snapshot.changed().await.unwrap();
        }
    }).await.unwrap();
    session
}

async fn revoke_owner_pairing(
    handle: &spaceterm_server::api::ServerHandle,
    signer: Arc<spaceterm_host::server_keys::ServerIdentity>,
    supervisor: Arc<spaceterm_host::account_service::ServiceSupervisor>,
    target: spaceterm_core::pairing::PairingTarget,
    client: &spaceterm_core::pairing::PairingClientStreams,
    seed: u8,
) {
    use spaceterm_core::{
        operation_ledger::canonical::CanonicalCodec,
        operation_ledger_contracts::CanonicalOperationRequest, operations::*,
        pairing::PairingCommand,
    };
    use spaceterm_protocol::{
        authentication::{Established, Purpose},
        pairing_adapter::{operation_to_wire, target_to_wire},
    };
    let (_, Established::Restricted(connection)) = authenticated_pairing(
        handle,
        signer.clone(),
        client,
        seed,
        Purpose::RevocationStatus,
    ) else {
        panic!("restricted authentication");
    };
    let request = CanonicalOperationRequest::new(
        OperationId::new(
            client.parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        ),
        Command::Pairing(PairingCommand::SelfRevoke {
            target: target.clone(),
        }),
        &CanonicalCodec,
    )
    .unwrap();
    let service = spaceterm_server::pairing::service::PairingService::new(
        Arc::new(handle.clone()),
        signer.clone(),
        Arc::new(CanonicalCodec),
        Arc::new(AccountOwnerEnrollment::new(supervisor)),
        signer.pin(),
    )
    .unwrap();
    service
        .restricted_exchange(
            &connection,
            spaceterm_protocol::wire::pairing::RestrictedExtension {
                kind: Some(
                    spaceterm_protocol::wire::pairing::restricted_extension::Kind::Revoke(
                        spaceterm_protocol::wire::pairing::SelfRevocation {
                            operation: Some(operation_to_wire(request.binding().id)),
                            request_digest: request.binding().digest.bytes().to_vec(),
                            target: Some(target_to_wire(&target)),
                        },
                    ),
                ),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn dropped_stopped_executor_keeps_instance_custody_until_external_work_is_drained() {
    let (_root, fixture, home) = initialized_account(DesiredServerState::Stopped);
    let supervisor = Arc::new(fixture.reopen());
    let server = ServerId::new(
        supervisor
            .discover()
            .unwrap()
            .unwrap()
            .server_pin()
            .identity,
    );
    let request = supervisor.allocate_request().unwrap();
    let (providers, _, external) = stopped_providers();
    let executor = AccountOwnerRequestExecutor::stopped(
        supervisor.clone(),
        home.clone(),
        request,
        &verified_claim(server, 41),
        providers,
    )
    .await
    .unwrap();
    let gate = native::Gate::new();
    *external.gate.lock().unwrap() = Some(gate.clone());
    drop(executor);
    tokio::time::timeout(std::time::Duration::from_secs(30), gate.entered())
        .await
        .unwrap();
    let (providers, _, _) = stopped_providers();
    assert!(
        AccountOwnerRequestExecutor::stopped(
            supervisor.clone(),
            home.clone(),
            request,
            &verified_claim(server, 41),
            providers
        )
        .await
        .is_err()
    );
    gate.release();
    let replacement = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let (providers, _, _) = stopped_providers();
            match AccountOwnerRequestExecutor::stopped(
                supervisor.clone(),
                home.clone(),
                request,
                &verified_claim(server, 41),
                providers,
            )
            .await
            {
                Ok(executor) => break executor,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        external.finished.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    replacement.close().await.unwrap();
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
}

#[test]
fn foreign_account_registration_cannot_validate_retained_owner_authority() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let supervisor = Arc::new(fixture.reopen());
    let request = supervisor.allocate_request().unwrap();
    let proposal = proposal(ServerId::new(registration.server_pin().identity), 1);
    let port = AccountOwnerEnrollment::new(supervisor.clone());
    let digest = port.record_authorization(request, &proposal).unwrap();
    let journal = fs::read(fixture.anchor().join("requests")).unwrap();
    let path = fixture.anchor().join("registration");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["key"]["account_uid"] = serde_json::json!(registration.key().account_uid + 1);
    fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert_eq!(
        port.validate_owner_request(&request, digest, proposal.parts().server, &proposal),
        Err(PairingFailure::IdentityMismatch)
    );
    assert_eq!(
        fs::read(fixture.anchor().join("requests")).unwrap(),
        journal
    );
}

#[cfg(feature = "test-support")]
#[path = "support/native_owner_endpoint.rs"]
mod native_endpoint;
