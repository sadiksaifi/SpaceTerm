//! Public CLI authorization uses an explicitly retained account-owner claim, the
//! production private owner endpoint, and the real account actor. No bearer is
//! exposed to the runner or retained in the client journal.

#[path = "native_consumer_authorization/public_claim.rs"]
mod public_claim;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::{JournalController, JournalLimits, ReceiptVerifier},
    management_cli::ManagementFailure,
    pairing::credentials::*,
};
use spaceterm_client_native::{
    NativeClientProvider,
    cli::{arguments::parse, bindings::PairingCommandBindings},
    credentials::adapter::DeviceCredentials,
    journal::NativeClientJournal,
    native::consumer::{NativeConsumer, NativeConsumerKind},
    pairing_runtime::NativePairingRuntime,
    route::{LocalRouteAuthority, NativeRouteConnector},
};
use spaceterm_core::{
    identity::{DeviceId, ServerId},
    management::{ManagementProgressState, ManagementRequestReference},
    operation_ledger::canonical::CanonicalCodec,
    pairing::*,
};
use spaceterm_host::{
    account_service::{InstallationStatus, test_support::AccountServiceFixture},
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits,
        bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
        migrations, schema,
    },
    shell_launch::ShellLaunchPlanner,
    startup::{BuildIdentity, DesiredServerState, InstallationId},
    storage::Database,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{HandshakeConfig, ServerPin, SigningKey},
    pairing_adapter::ReceiptCodec,
};
use spaceterm_server::{
    account_startup::{
        AccountStartup, RecoveredAccountServer,
        test_support::{StartupAbsence, supervisor},
    },
    adapters::{store::production_services, terminal::AccountLaunchFacts},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Key(Ed25519KeyPair);
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
struct Credentials {
    items: Mutex<Vec<PreparedCredential>>,
    creates: AtomicUsize,
    device: Arc<Key>,
    authentication: Arc<Key>,
}
impl Credentials {
    fn new() -> Self {
        Self {
            items: Default::default(),
            creates: AtomicUsize::new(0),
            device: Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[71; 32]).unwrap())),
            authentication: Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[72; 32]).unwrap())),
        }
    }
    fn key(&self, scope: &CredentialScope) -> &Arc<Key> {
        if scope.parts().purpose == CredentialPurpose::DeviceIdentity {
            &self.device
        } else {
            &self.authentication
        }
    }
}
impl NativeDeviceCredentials for Credentials {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            self.creates.fetch_add(1, Ordering::SeqCst);
            let scope = reservation.scope().clone();
            let credential = PreparedCredential {
                public_key: self.key(&scope).public_key(),
                scope,
            };
            self.items.lock().unwrap().push(credential.clone());
            Ok(credential)
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .find(|item| &item.scope == scope)
                .cloned())
        })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authority: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authority.validate(&credential.scope)?;
            let key = self.key(&credential.scope);
            if key.public_key() != credential.public_key {
                return Err(PairingFailure::CredentialUnavailable);
            }
            Ok(key.clone() as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async { panic!("authorization must not delete credentials") })
    }
}
struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidStorage))
    }
}
#[derive(Clone, Copy)]
enum Scenario {
    Complete,
    WrongFingerprint,
    UnknownRequest,
    ChangedClaim,
    ChangedNativeClaim,
    LostDelivery,
    MissingClaim,
    StatusCompanion,
}

#[tokio::test]
async fn public_pair_authorize_consumes_single_use_authorization_with_real_account_actor_in_both_majors()
 {
    for major in [1, 2] {
        exercise(major, Scenario::Complete).await;
    }
}
#[tokio::test]
async fn public_pair_authorize_rejects_wrong_fingerprint_before_issuance_in_both_majors() {
    for major in [1, 2] {
        exercise(major, Scenario::WrongFingerprint).await;
    }
}
#[tokio::test]
async fn public_pair_authorize_rejects_unretained_request_without_creating_credentials_in_both_majors()
 {
    for major in [1, 2] {
        exercise(major, Scenario::UnknownRequest).await;
    }
}
#[tokio::test]
async fn public_pair_authorize_rejects_changed_retained_claim_in_both_majors() {
    for major in [1, 2] {
        exercise(major, Scenario::ChangedClaim).await;
    }
}
#[tokio::test]
async fn public_pair_authorize_does_not_replace_lost_single_use_delivery_in_both_majors() {
    for major in [1, 2] {
        exercise(major, Scenario::LostDelivery).await;
    }
}
#[tokio::test]
async fn public_pair_authorize_does_not_enroll_when_no_explicit_claim_exists_in_both_majors() {
    for major in [1, 2] {
        exercise(major, Scenario::MissingClaim).await;
    }
}

#[tokio::test]
async fn public_pair_authorize_rejects_changed_native_request_binding_in_both_majors() {
    for major in [1, 2] {
        exercise(major, Scenario::ChangedNativeClaim).await;
    }
}
#[tokio::test]
async fn public_pair_authorize_remains_unavailable_to_read_only_companion_in_both_majors() {
    for major in [1, 2] {
        exercise(major, Scenario::StatusCompanion).await;
    }
}

async fn exercise(major: u32, scenario: Scenario) {
    let root = tempfile::Builder::new()
        .prefix("st-authorize-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture
        .initialize_with_installation(
            DesiredServerState::Running,
            Some(InstallationStatus {
                installation: InstallationId(std::num::NonZeroU64::new(9).unwrap()),
                version: spaceterm_distribution::BuildVersion::parse("9.0.0").unwrap(),
                artifact_sha256: [8; 32],
            }),
        )
        .unwrap();
    let pin = registration.server_pin();
    let server_id = ServerId::new(pin.identity);
    let identity = StoreIdentity {
        server: server_id,
        account: registration.key(),
        server_public_key: pin.public_key,
    };
    let home = spaceterm_core::organization::ServerDirectory::new(
        server_id,
        root.path().as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let database = Database::open_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    assert!(matches!(
        SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity,
                origin: NewServerOrigin::LocalHeadless
            },
            StoreLimits::production(),
            production_services(identity, home).unwrap()
        ),
        InitializationDisposition::Durable { .. }
    ));
    let supervisor = supervisor(root.path(), Arc::new(StartupAbsence)).unwrap();
    let server = RecoveredAccountServer::recover(
        supervisor.clone(),
        AccountStartup::admit(&supervisor).unwrap(),
        AccountLaunchFacts {
            uid: registration.key().account_uid,
            home: root.path().to_owned(),
            planner: ShellLaunchPlanner::for_test(
                root.path().join("shell"),
                root.path().to_owned(),
            ),
            machine: Default::default(),
        },
    )
    .await
    .unwrap();
    let handle = server.handle();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(server.run_until(async {
        let _ = stopped.await;
    }));
    let credentials = Arc::new(Credentials::new());
    let admission = supervisor.admit_client_provisioning().unwrap();
    let (directory, create) = admission.journal_directory("enrollment").unwrap();
    assert!(create);
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(Verifier);
    let storage = NativeClientJournal::create(
        directory.clone(),
        codec.clone(),
        verifier.clone(),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let journal = Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    );
    let local = LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap();
    let connector = Arc::new(
        spaceterm_client_native::restricted_routes::NativePairingConnector::new(
            journal.clone(),
            NativeRouteConnector::new(vec![local], None).unwrap(),
        ),
    );
    let runtime = NativePairingRuntime::with_adapters(
        journal.clone(),
        credentials.clone(),
        connector.clone(),
        connector,
        Arc::new(NoInvalidation),
        HandshakeConfig::default(),
    );
    let namespace = CredentialNamespace::new("spaceterm-native-development".into()).unwrap();
    let (_, device) = runtime
        .prepare_credential(
            DeviceCredentials::new_scope(namespace.clone(), CredentialPurpose::DeviceIdentity)
                .unwrap(),
        )
        .await
        .unwrap();
    let (generation, authentication) = runtime
        .prepare_credential(
            DeviceCredentials::new_scope(
                namespace,
                CredentialPurpose::PairingAuthentication { server: server_id },
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let request = supervisor.allocate_request().unwrap();
    let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
        server: server_id,
        device: DeviceId::new([73; 16]),
        claim_nonce: [74; 32],
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key,
            pairing_authentication_public_key: authentication.public_key,
        })
        .unwrap(),
    })
    .unwrap();
    if matches!(scenario, Scenario::ChangedNativeClaim) {
        let claim_digest =
            spaceterm_protocol::pairing_adapter::enrollment_proposal_digest(&proposal).unwrap();
        let digest = supervisor
            .owner_enrollment_digest(request, server_id, claim_digest)
            .unwrap();
        supervisor
            .record_owner_enrollment(request, digest, server_id, claim_digest)
            .unwrap();
    }
    let reference = RevocationRouteReference::new([75; 16]);
    let mut plan = serde_json::json!({ "server_public_key": pin.public_key, "proposal": proposal, "request": request, "generation": generation, "device": device, "authentication": authentication, "reference": reference });
    if matches!(scenario, Scenario::ChangedClaim) {
        plan["authentication"]["public_key"] = serde_json::to_value([76u8; 32]).unwrap();
    }
    if matches!(scenario, Scenario::ChangedNativeClaim) {
        plan["proposal"]["claim_nonce"] = serde_json::to_value([78u8; 32]).unwrap();
    }
    if !matches!(scenario, Scenario::MissingClaim) {
        admission
            .retain_owner_attempt("enrollment", None, &serde_json::to_vec(&plan).unwrap())
            .unwrap();
    }
    drop(runtime);
    drop(journal);
    drop(admission);
    let provider = NativeClientProvider::from_supervisor((*supervisor).clone());
    let kind = if matches!(scenario, Scenario::StatusCompanion) {
        NativeConsumerKind::StatusCompanion
    } else {
        NativeConsumerKind::Cli
    };
    let mut consumer = NativeConsumer::new(provider.management(), kind)
        .with_credentials(credentials.clone())
        .with_owner_handshake(HandshakeConfig {
            majors: vec![major],
            ..HandshakeConfig::default()
        });
    if matches!(scenario, Scenario::LostDelivery) {
        consumer = consumer.with_owner_channels(Arc::new(LoseDelivery(
            spaceterm_client_native::native::owner_channel::NativeOwnerChannel(
                provider.management(),
            ),
        )));
    }
    let requested = if matches!(scenario, Scenario::UnknownRequest) {
        supervisor.allocate_request().unwrap()
    } else {
        request
    };
    let fingerprint = if matches!(scenario, Scenario::WrongFingerprint) {
        [77; 32]
    } else {
        credentials.device.public_key()
    };
    let arguments = vec![
        "pair".into(),
        "authorize".into(),
        hex(requested.as_bytes()),
        "--device-fingerprint".into(),
        hex(&fingerprint),
    ];
    let invocation = parse(arguments.into_iter().map(std::ffi::OsString::from)).unwrap();
    let spaceterm_client_native::cli::arguments::CliCommand::PairAuthorize {
        request: public_request,
        fingerprint,
    } = invocation.command
    else {
        panic!("expected authorization command");
    };
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        consumer.authorize(public_request, fingerprint),
    )
    .await
    .unwrap();
    match scenario {
        Scenario::Complete => {
            assert!(
                matches!(result, Ok(ref progress) if progress.state == ManagementProgressState::Completed && progress.request == ManagementRequestReference::Native(*request.as_bytes()))
            );
            let replay = tokio::time::timeout(
                Duration::from_secs(15),
                consumer.authorize(public_request, fingerprint),
            )
            .await
            .unwrap();
            assert!(
                matches!(replay, Ok(ref progress) if progress.state == ManagementProgressState::Completed)
            );
            let codec = Arc::new(CanonicalCodec);
            let verifier = Arc::new(Verifier);
            let storage = NativeClientJournal::open_existing(
                directory,
                codec.clone(),
                verifier.clone(),
                JournalLimits::default(),
            )
            .await
            .unwrap();
            let recovered = JournalController::recover(
                Arc::new(storage),
                codec,
                verifier,
                JournalLimits::default(),
            )
            .await
            .unwrap();
            let state = recovered.snapshot().unwrap();
            assert!(state.bindings().len() == 1 && state.pairing().acknowledgements().is_empty());
            assert!(
                state.bindings()[0]
                    .binding()
                    .is_ok_and(|binding| binding.target().pin() == pin)
            );
        }
        Scenario::LostDelivery => {
            assert!(matches!(result, Err(ManagementFailure::Offline)));
            let retry = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
                .with_credentials(credentials.clone())
                .with_owner_handshake(HandshakeConfig {
                    majors: vec![major],
                    ..HandshakeConfig::default()
                });
            let result = tokio::time::timeout(
                Duration::from_secs(15),
                retry.authorize(public_request, fingerprint),
            )
            .await
            .unwrap();
            assert!(matches!(result, Err(ManagementFailure::OutcomePending)));
        }
        Scenario::ChangedNativeClaim => {
            assert!(matches!(result, Err(ManagementFailure::StaleAssessment)))
        }
        _ => assert!(matches!(
            result,
            Err(ManagementFailure::AuthorizationDenied)
        )),
    }
    assert!(credentials.creates.load(Ordering::SeqCst) == 2);
    assert!(
        !supervisor
            .client_connections_directory()
            .join("cli")
            .exists()
    );
    let pairing = handle.pairing_snapshot().unwrap();
    let recovery = pairing.recovery();
    let expected = usize::from(matches!(
        scenario,
        Scenario::Complete | Scenario::LostDelivery
    ));
    assert!(recovery.parts().enrollments.len() == expected);
    assert!(
        recovery.parts().authorizations.len()
            == usize::from(matches!(scenario, Scenario::Complete))
    );
    if matches!(scenario, Scenario::Complete) {
        assert!(matches!(
            recovery.parts().enrollments[0].parts().state,
            EnrollmentReservationState::Consumed { .. }
        ));
    }
    if matches!(scenario, Scenario::LostDelivery) {
        assert!(matches!(
            recovery.parts().enrollments[0].parts().state,
            EnrollmentReservationState::Reserved
        ));
    }
    let _ = stop.send(());
    let report = tokio::time::timeout(Duration::from_secs(15), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.cleanup_confirmed && report.listeners_closed && !report.recovery_required);
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
struct NoInvalidation;
impl spaceterm_client::pending::LocalUseInvalidator for NoInvalidation {
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: spaceterm_core::identity::WorkspaceId,
    ) -> Result<Vec<spaceterm_core::authority::GrantedControl>, ClientFailure> {
        Ok(vec![])
    }
    fn invalidate_attachment(
        &self,
        _: &LocalPairingKey,
        _: spaceterm_client::view::WorkspaceAttachment,
    ) -> Result<Vec<spaceterm_core::authority::GrantedControl>, ClientFailure> {
        Ok(vec![])
    }
    fn invalidate_pairing(&self, _: &LocalPairingKey) {}
    fn remove_pairing<'a>(
        &'a self,
        _: &'a LocalPairingKey,
    ) -> spaceterm_client::ports::ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct LoseDelivery(spaceterm_client_native::native::owner_channel::NativeOwnerChannel);
impl spaceterm_client_native::native::owner_channel::OwnerChannelProvider for LoseDelivery {
    fn open<'a>(
        &'a self,
        registration: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_client::ports::ClientFuture<
        'a,
        Arc<dyn spaceterm_host::transport::MessageTransport>,
    > {
        Box::pin(async move {
            Ok(Arc::new(LosingChannel {
                inner: self.0.open(registration).await?,
                receives: AtomicUsize::new(0),
            })
                as Arc<dyn spaceterm_host::transport::MessageTransport>)
        })
    }
}
struct LosingChannel {
    inner: Arc<dyn spaceterm_host::transport::MessageTransport>,
    receives: AtomicUsize,
}
impl spaceterm_host::transport::MessageTransport for LosingChannel {
    fn peer_facts(&self) -> spaceterm_host::transport::PeerFacts {
        self.inner.peer_facts()
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> spaceterm_host::transport::TransportFuture<'a, ()> {
        self.inner.send(bytes)
    }
    fn recv(&self) -> spaceterm_host::transport::TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            let bytes = zeroize::Zeroizing::new(self.inner.recv().await?.unwrap_or_default());
            if self.receives.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err(spaceterm_host::transport::TransportError::Closed);
            }
            Ok(Some(bytes.to_vec()))
        })
    }
    fn close(&self) -> spaceterm_host::transport::TransportFuture<'_, ()> {
        self.inner.close()
    }
}

#[derive(Clone, Copy)]
enum PublicScenario {
    Complete,
    CliPipe,
    WrongFingerprint,
    ChangedProof,
    ChangedAuthenticationProof,
    ChangedRequestBinding,
    LostDelivery,
}

#[tokio::test]
async fn public_claim_producer_delivers_to_separate_native_caller_journal_in_both_majors() {
    for major in [1, 2] {
        public_exercise(major, PublicScenario::Complete).await;
    }
}

#[tokio::test]
async fn public_pair_authorize_claim_native_binding_delivers_to_public_driver_in_both_majors() {
    for major in [1, 2] {
        public_exercise(major, PublicScenario::CliPipe).await;
    }
}

#[tokio::test]
async fn public_claim_producer_rejects_wrong_fingerprint_before_retaining_authority_in_both_majors()
{
    for major in [1, 2] {
        public_exercise(major, PublicScenario::WrongFingerprint).await;
    }
}

#[tokio::test]
async fn public_claim_format_verifies_both_proofs_before_issuance_in_both_majors() {
    for major in [1, 2] {
        public_exercise(major, PublicScenario::ChangedProof).await;
        public_exercise(major, PublicScenario::ChangedAuthenticationProof).await;
    }
}

#[tokio::test]
async fn public_claim_producer_preserves_original_native_request_binding_in_both_majors() {
    for major in [1, 2] {
        public_exercise(major, PublicScenario::ChangedRequestBinding).await;
    }
}

#[tokio::test]
async fn public_claim_producer_never_reissues_a_lost_delivery_in_both_majors() {
    for major in [1, 2] {
        public_exercise(major, PublicScenario::LostDelivery).await;
    }
}

async fn public_exercise(major: u32, scenario: PublicScenario) {
    use spaceterm_client_native::native::consumer::PublicEnrollmentClaim;
    use spaceterm_core::operations::{OperationBinding, OperationId, OperationSequence};
    use spaceterm_protocol::pairing_adapter::{VerifiedDeviceClaim, enrollment_proposal_digest};
    use std::os::unix::fs::DirBuilderExt as _;

    let root = tempfile::Builder::new()
        .prefix("st-public-authorize-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture
        .initialize_with_installation(
            DesiredServerState::Running,
            Some(InstallationStatus {
                installation: InstallationId(std::num::NonZeroU64::new(9).unwrap()),
                version: spaceterm_distribution::BuildVersion::parse("9.0.0").unwrap(),
                artifact_sha256: [8; 32],
            }),
        )
        .unwrap();
    let pin = registration.server_pin();
    let server_id = ServerId::new(pin.identity);
    let identity = StoreIdentity {
        server: server_id,
        account: registration.key(),
        server_public_key: pin.public_key,
    };
    let home = spaceterm_core::organization::ServerDirectory::new(
        server_id,
        root.path().as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let database = Database::open_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    assert!(matches!(
        SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity,
                origin: NewServerOrigin::LocalHeadless
            },
            StoreLimits::production(),
            production_services(identity, home).unwrap(),
        ),
        InitializationDisposition::Durable { .. }
    ));
    let supervisor = supervisor(root.path(), Arc::new(StartupAbsence)).unwrap();
    let server = RecoveredAccountServer::recover(
        supervisor.clone(),
        AccountStartup::admit(&supervisor).unwrap(),
        AccountLaunchFacts {
            uid: registration.key().account_uid,
            home: root.path().to_owned(),
            planner: ShellLaunchPlanner::for_test(
                root.path().join("shell"),
                root.path().to_owned(),
            ),
            machine: Default::default(),
        },
    )
    .await
    .unwrap();
    let handle = server.handle();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(server.run_until(async {
        let _ = stopped.await;
    }));

    // This caller owns its journal and both private keys. The authorizer receives
    // only the public claim and cannot load or manufacture a caller credential.
    let directory = root.path().canonicalize().unwrap().join("public-caller");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(Verifier);
    let storage = NativeClientJournal::create(
        directory,
        codec.clone(),
        verifier.clone(),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let journal = Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    );
    let credentials = Arc::new(Credentials::new());
    let local = LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap();
    let route = local.description().clone();
    let restricted = Arc::new(
        spaceterm_client_native::restricted_routes::NativePairingConnector::new(
            journal.clone(),
            NativeRouteConnector::new(vec![local], None).unwrap(),
        ),
    );
    let handshake = HandshakeConfig {
        majors: vec![major],
        ..HandshakeConfig::default()
    };
    let caller = NativePairingRuntime::with_adapters(
        journal.clone(),
        credentials.clone(),
        restricted.clone(),
        restricted.clone(),
        Arc::new(NoInvalidation),
        handshake.clone(),
    );
    let namespace = CredentialNamespace::new("spaceterm-native-development".into()).unwrap();
    let (_, device) = caller
        .prepare_credential(
            DeviceCredentials::new_scope(namespace.clone(), CredentialPurpose::DeviceIdentity)
                .unwrap(),
        )
        .await
        .unwrap();
    let (generation, authentication) = caller
        .prepare_credential(
            DeviceCredentials::new_scope(
                namespace,
                CredentialPurpose::PairingAuthentication { server: server_id },
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let request = supervisor.allocate_request().unwrap();
    let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
        server: server_id,
        device: DeviceId::new([81; 16]),
        claim_nonce: [82; 32],
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key,
            pairing_authentication_public_key: authentication.public_key,
        })
        .unwrap(),
    })
    .unwrap();
    let reference = RevocationRouteReference::new([83; 16]);
    let attempt = spaceterm_client::pairing::enrollment::DirectEnrollmentAttempt::new(
        pin,
        reference,
        generation.get(),
    )
    .unwrap();
    let verified = attempt
        .confirm(pin, reference, generation.get(), &attempt.fingerprint())
        .unwrap();
    let device_key = credentials
        .signing_key(&device, &verified.credential_use(&device).unwrap())
        .await
        .unwrap();
    let auth_key = credentials
        .signing_key(
            &authentication,
            &verified.credential_use(&authentication).unwrap(),
        )
        .await
        .unwrap();
    let (mut device_proof, mut auth_proof) =
        VerifiedDeviceClaim::proofs(&proposal, device_key.as_ref(), auth_key.as_ref()).unwrap();
    if matches!(scenario, PublicScenario::ChangedProof) {
        device_proof[0] ^= 1;
    }
    if matches!(scenario, PublicScenario::ChangedAuthenticationProof) {
        auth_proof[0] ^= 1;
    }
    let encoded = serde_json::to_vec(&serde_json::json!({
        "request": request, "proposal": proposal,
        "device_proof": device_proof.to_vec(), "authentication_proof": auth_proof.to_vec(),
    }))
    .unwrap();
    let claim = PublicEnrollmentClaim::decode(&encoded);
    let provider = NativeClientProvider::from_supervisor((*supervisor).clone());
    let mut producer = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
        .with_credentials(Arc::new(ForbiddenCredentials));
    if matches!(scenario, PublicScenario::LostDelivery) {
        producer = producer.with_owner_channels(Arc::new(LoseDelivery(
            spaceterm_client_native::native::owner_channel::NativeOwnerChannel(
                provider.management(),
            ),
        )));
    }
    let producer = Arc::new(producer);
    if matches!(
        scenario,
        PublicScenario::ChangedProof | PublicScenario::ChangedAuthenticationProof
    ) {
        assert!(matches!(claim, Err(ManagementFailure::AuthorizationDenied)));
    } else {
        let claim = claim.unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&claim.encode().unwrap()).unwrap()
                == serde_json::from_slice::<serde_json::Value>(&encoded).unwrap()
        );
        if matches!(scenario, PublicScenario::ChangedRequestBinding) {
            let mut changed = proposal.parts().clone();
            changed.claim_nonce[0] ^= 1;
            let digest =
                enrollment_proposal_digest(&EnrollmentProposal::new(changed).unwrap()).unwrap();
            let native_digest = supervisor
                .owner_enrollment_digest(request, server_id, digest)
                .unwrap();
            supervisor
                .record_owner_enrollment(request, native_digest, server_id, digest)
                .unwrap();
        }
        let expected = if matches!(scenario, PublicScenario::WrongFingerprint) {
            [84; 32]
        } else {
            device.public_key
        };
        let result = if matches!(scenario, PublicScenario::CliPipe) {
            None
        } else {
            Some(producer.authorize_public_claim(&claim, expected).await)
        };
        match scenario {
            PublicScenario::Complete | PublicScenario::CliPipe => {
                let (reservation, authorization, native_request, native_digest, channel) = if matches!(
                    scenario,
                    PublicScenario::CliPipe
                ) {
                    use spaceterm_client_native::cli::{
                        bindings::{UnavailableNativeBindings, UnavailableProtocolManagement},
                        runner::{CliDependencies, Dispatch, dispatch},
                    };
                    let invocation = parse(
                        [
                            "--json",
                            "--non-interactive",
                            "pair",
                            "authorize",
                            "--claim-stdin",
                            "--authorization-stdout",
                            "--device-fingerprint",
                            &hex(&expected),
                        ]
                        .into_iter()
                        .map(std::ffi::OsString::from),
                    )
                    .unwrap();
                    let deps = CliDependencies::new(
                        Ok(registration.key()),
                        Arc::new(UnavailableProtocolManagement),
                        Arc::new(UnavailableNativeBindings::default()),
                        producer.clone(),
                    );
                    let mut io = ClaimPipeIo {
                        claim: Some(zeroize::Zeroizing::new(encoded)),
                        delivery: None,
                        ordinary_writes: 0,
                        diagnostics: Vec::new(),
                    };
                    let dispatched = dispatch(invocation, &deps, &mut io).await;
                    assert!(matches!(dispatched, Dispatch::Exit(code) if code.code() == 0));
                    assert!(io.ordinary_writes == 0 && io.diagnostics.is_empty());
                    let (reservation, authorization) = spaceterm_protocol::pairing_adapter::EnrollmentAuthorization::from_delivery(io.delivery.take().unwrap()).unwrap();
                    let native_digest = supervisor
                        .retained_owner_enrollment_digest(request, server_id, claim.digest())
                        .unwrap()
                        .unwrap();
                    (
                        reservation,
                        Some(authorization),
                        request,
                        native_digest,
                        None,
                    )
                } else {
                    let delivery = result.unwrap().unwrap();
                    assert!(delivery.pin() == pin && delivery.native_request() == request);
                    let parts = delivery.into_parts(reference);
                    (
                        parts.reservation,
                        parts.authorization,
                        parts.native_request,
                        parts.native_digest,
                        Some(parts.channel),
                    )
                };
                let public = reservation.parts();
                let prepared = spaceterm_client::pairing::journal::PreparedEnrollment {
                    local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
                        server: server_id,
                        pairing: public.target.parts().pairing,
                        generation,
                    })
                    .unwrap(),
                    device: public.target.parts().device,
                    pin,
                    keys: public.keys.clone(),
                    client: public.initial_client,
                    credential: authentication,
                    route: reference,
                    enrollment: OperationBinding {
                        id: OperationId::new(
                            public.enrollment_stream,
                            OperationSequence::new(1).unwrap(),
                        ),
                        digest: public.enrollment_request_digest,
                    },
                    reservation: Some(reservation),
                };
                if let Some(channel) = &channel {
                    use spaceterm_client::pairing::enrollment::EnrollmentConnector as _;
                    let mut wrong_pin = pin;
                    wrong_pin.public_key[0] ^= 1;
                    assert!(matches!(
                        channel.open(wrong_pin, reference).await,
                        Err(PairingFailure::IdentityMismatch)
                    ));
                    assert!(matches!(
                        channel
                            .open(pin, RevocationRouteReference::new([85; 16]))
                            .await,
                        Err(PairingFailure::IdentityMismatch)
                    ));
                }
                let enrollment_connector: Arc<
                    dyn spaceterm_client::pairing::enrollment::EnrollmentConnector,
                > = match &channel {
                    Some(channel) => channel.clone(),
                    None => restricted.clone(),
                };
                let enrolled = NativePairingRuntime::with_adapters(
                    journal.clone(),
                    credentials.clone(),
                    enrollment_connector,
                    restricted,
                    Arc::new(NoInvalidation),
                    handshake,
                );
                let saved = enrolled
                    .enroll_account_owner(
                        &supervisor,
                        spaceterm_client_native::pairing_runtime::OwnerEnrollmentRequest {
                            prepared: &prepared,
                            device: &device,
                            authorization: authorization.as_ref(),
                            native_request,
                            native_digest,
                        },
                        route,
                    )
                    .await
                    .unwrap();
                if let Some(channel) = channel {
                    use spaceterm_client::pairing::enrollment::EnrollmentConnector as _;
                    assert!(matches!(
                        channel.open(pin, reference).await,
                        Err(PairingFailure::Offline)
                    ));
                    channel.close().await.unwrap();
                }
                assert!(
                    saved.key() == &prepared.local_pairing
                        && journal.snapshot().unwrap().bindings().len() == 1
                );
                let replay = producer
                    .authorize_public_claim(&claim, expected)
                    .await
                    .unwrap();
                let replay = replay.into_parts(reference);
                assert!(replay.authorization.is_none());
                replay.channel.close().await.unwrap();
            }
            PublicScenario::WrongFingerprint => assert!(matches!(
                result.unwrap(),
                Err(ManagementFailure::AuthorizationDenied)
            )),
            PublicScenario::ChangedRequestBinding => {
                assert!(matches!(
                    result.unwrap(),
                    Err(ManagementFailure::StaleAssessment)
                ))
            }
            PublicScenario::LostDelivery => {
                assert!(matches!(result.unwrap(), Err(ManagementFailure::Offline)));
                let retry = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
                    .with_credentials(Arc::new(ForbiddenCredentials));
                assert!(matches!(
                    retry.authorize_public_claim(&claim, expected).await,
                    Err(ManagementFailure::OutcomePending)
                ));
            }
            PublicScenario::ChangedProof | PublicScenario::ChangedAuthenticationProof => {
                unreachable!()
            }
        }
    }
    assert!(credentials.creates.load(Ordering::SeqCst) == 2);
    assert!(!supervisor.client_connections_directory().exists());
    let snapshot = handle.pairing_snapshot().unwrap();
    let recovery = snapshot.recovery();
    assert!(
        recovery.parts().enrollments.len()
            == usize::from(matches!(
                scenario,
                PublicScenario::Complete | PublicScenario::CliPipe | PublicScenario::LostDelivery
            ))
    );
    let _ = stop.send(());
    let report = tokio::time::timeout(Duration::from_secs(15), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.cleanup_confirmed && report.listeners_closed && !report.recovery_required);
}

struct ForbiddenCredentials;
impl NativeDeviceCredentials for ForbiddenCredentials {
    fn create(
        &self,
        _: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async { panic!("public producer must not create caller credentials") })
    }
    fn recover_preparation<'a>(
        &'a self,
        _: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async { panic!("public producer must not recover caller credentials") })
    }
    fn signing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
        _: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async { panic!("public producer must not load caller credentials") })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async { panic!("public producer must not delete caller credentials") })
    }
}

struct ClaimPipeIo {
    claim: Option<zeroize::Zeroizing<Vec<u8>>>,
    delivery: Option<zeroize::Zeroizing<Vec<u8>>>,
    ordinary_writes: usize,
    diagnostics: Vec<u8>,
}
impl spaceterm_client_native::cli::runner::CliIo for ClaimPipeIo {
    fn write_stdout(&mut self, _: &[u8]) -> Result<(), spaceterm_client_native::cli::CliFailure> {
        self.ordinary_writes += 1;
        Err(spaceterm_client_native::cli::CliFailure::AuthorizationUnavailable)
    }
    fn write_stderr(
        &mut self,
        bytes: &[u8],
    ) -> Result<(), spaceterm_client_native::cli::CliFailure> {
        self.diagnostics.extend_from_slice(bytes);
        Ok(())
    }
    fn read_authorization_claim(
        &mut self,
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, spaceterm_client_native::cli::CliFailure> {
        self.claim
            .take()
            .ok_or(spaceterm_client_native::cli::CliFailure::AuthorizationUnavailable)
    }
    fn write_authorization_delivery(
        &mut self,
        bytes: &[u8],
    ) -> Result<(), spaceterm_client_native::cli::CliFailure> {
        assert!(self.delivery.is_none());
        self.delivery = Some(zeroize::Zeroizing::new(bytes.to_vec()));
        Ok(())
    }
    fn read_confirmation(
        &mut self,
        _: &spaceterm_client::management_cli::ConfirmationPrompt,
    ) -> Result<
        spaceterm_client::management_cli::ConfirmationReply,
        spaceterm_client_native::cli::CliFailure,
    > {
        Err(spaceterm_client_native::cli::CliFailure::ConfirmationRequired)
    }
}

#[tokio::test]
async fn public_claim_pipe_refuses_stopped_owner_before_issuance() {
    use spaceterm_client_native::native::consumer::PublicEnrollmentClaim;
    use spaceterm_protocol::pairing_adapter::VerifiedDeviceClaim;

    let root = tempfile::Builder::new()
        .prefix("st-stopped-claim-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture
        .initialize_with_installation(
            DesiredServerState::Stopped,
            Some(InstallationStatus {
                installation: InstallationId(std::num::NonZeroU64::new(9).unwrap()),
                version: spaceterm_distribution::BuildVersion::parse("9.0.0").unwrap(),
                artifact_sha256: [8; 32],
            }),
        )
        .unwrap();
    let supervisor = supervisor(root.path(), Arc::new(StartupAbsence)).unwrap();
    let request = supervisor.allocate_request().unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let device = Key(Ed25519KeyPair::from_seed_unchecked(&[91; 32]).unwrap());
    let authentication = Key(Ed25519KeyPair::from_seed_unchecked(&[92; 32]).unwrap());
    let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
        server,
        device: DeviceId::new([93; 16]),
        claim_nonce: [94; 32],
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key(),
            pairing_authentication_public_key: authentication.public_key(),
        })
        .unwrap(),
    })
    .unwrap();
    let (device_proof, auth_proof) =
        VerifiedDeviceClaim::proofs(&proposal, &device, &authentication).unwrap();
    let claim =
        PublicEnrollmentClaim::verify(request, proposal, &device_proof, &auth_proof).unwrap();
    let invocation = parse(
        [
            "pair",
            "authorize",
            &hex(request.as_bytes()),
            "--device-fingerprint",
            &hex(&device.public_key()),
        ]
        .into_iter()
        .map(std::ffi::OsString::from),
    )
    .unwrap();
    let spaceterm_client_native::cli::CliCommand::PairAuthorize { fingerprint, .. } =
        invocation.command
    else {
        unreachable!()
    };
    let provider = NativeClientProvider::from_supervisor((*supervisor).clone());
    let producer = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
        .with_credentials(Arc::new(ForbiddenCredentials));
    let public = spaceterm_client_native::pairing_runtime::PublicDeviceClaim {
        schema: 1,
        server_identity: registration.server_pin().identity,
        server_public_key: registration.server_pin().public_key,
        proposal: claim.proposal().clone(),
        device_proof: device_proof.to_vec(),
        authentication_proof: auth_proof.to_vec(),
    };
    for encoded in [
        claim.encode().unwrap(),
        serde_json::to_vec(&public).unwrap(),
    ] {
        let result = producer
            .authorize_claim(zeroize::Zeroizing::new(encoded), fingerprint)
            .await;
        assert!(
            matches!(result, Err(error) if error.failure == ManagementFailure::AuthorizationUnavailable)
        );
    }
    assert!(
        supervisor
            .retained_owner_enrollment_digest(request, server, claim.digest())
            .unwrap()
            .is_none()
    );
    assert!(!supervisor.client_connections_directory().exists());
}
