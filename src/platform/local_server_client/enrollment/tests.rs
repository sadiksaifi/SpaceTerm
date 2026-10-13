use super::*;
use crate::platform::local_server_client::tests::Root;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use spaceterm_client::{
    journal::{JournalLimits, ReceiptVerifier},
    pairing::credentials::{
        CredentialCreationReservation, CredentialDeletion, CredentialScope, CredentialUse,
        PreparedCredential,
    },
};
use spaceterm_core::{operation_ledger::canonical::CanonicalCodec, pairing::SignedPairingReceipt};
use spaceterm_host::{
    account_service::{InstallationStatus, test_support::AccountServiceFixture},
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits,
        bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
        migrations, schema,
    },
    shell_launch::ShellLaunchPlanner,
    startup::InstallationId,
    storage::Database,
};
use spaceterm_protocol::{
    ProtocolError, authentication::SigningKey, pairing_adapter::ReceiptCodec,
};
use spaceterm_server::{
    account_startup::{
        AccountStartup, RecoveredAccountServer,
        test_support::{StartupAbsence, supervisor},
    },
    adapters::{store::production_services, terminal::AccountLaunchFacts},
};
use std::{os::unix::fs::DirBuilderExt as _, sync::Mutex};

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(Into::into)
    }
}
#[derive(Clone, Default)]
struct Credentials(Arc<Mutex<Vec<(PreparedCredential, u8)>>>);
struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
}
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
impl NativeDeviceCredentials for Credentials {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            let mut items = self.0.lock().unwrap();
            let seed = u8::try_from(items.len() + 30).unwrap();
            let prepared = PreparedCredential {
                scope: reservation.scope().clone(),
                public_key: Key::new(seed).public_key(),
            };
            items.push((prepared.clone(), seed));
            Ok(prepared)
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|(credential, _)| &credential.scope == scope)
                .map(|(credential, _)| credential.clone()))
        })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            let items = self.0.lock().unwrap();
            let (_, seed) = items
                .iter()
                .find(|(prepared, _)| prepared == credential)
                .ok_or(PairingFailure::CredentialUnavailable)?;
            Ok(Arc::new(Key::new(*seed)) as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async { panic!("foreground enrollment must not delete another credential") })
    }
}

async fn journal(root: &Root) -> Arc<JournalController> {
    let directory = root.0.join("client");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(Verifier);
    let storage = spaceterm_client_native::journal::NativeClientJournal::create(
        directory,
        codec.clone(),
        verifier.clone(),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_foreground_owner_pairing_publishes_verified_binding_from_running_account_actor() {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
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
    let identity = StoreIdentity {
        server: ServerId::new(pin.identity),
        account: registration.key(),
        server_public_key: pin.public_key,
    };
    let home = spaceterm_core::organization::ServerDirectory::new(
        identity.server,
        root.0.as_os_str().as_encoded_bytes().to_vec(),
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
    let supervisor = supervisor(&root.0, Arc::new(StartupAbsence)).unwrap();
    let startup = AccountStartup::admit(&supervisor).unwrap();
    let server = RecoveredAccountServer::recover(
        supervisor.clone(),
        startup,
        AccountLaunchFacts {
            uid: registration.key().account_uid,
            home: root.0.clone(),
            planner: ShellLaunchPlanner::for_test(root.0.join("shell"), root.0.clone()),
            machine: spaceterm_terminal::metadata::LocalMachine::default(),
        },
    )
    .await
    .unwrap();
    let handle = server.handle();
    let before = handle.snapshot();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(server.run_until(async {
        let _ = stopped.await;
    }));
    let journal = journal(&root).await;
    let credentials = Credentials::default();
    let native = NativeClientProvider::from_supervisor((*supervisor).clone());
    let result = enroll_with_credentials(&native, journal.clone(), credentials.clone()).await;
    let _ = stop.send(());
    let report = running.await.unwrap();
    assert!(
        result.is_ok(),
        "owner enrollment {result:?}; joined server shutdown {report:?}"
    );
    // Dropping the test runtime's owning composition uses the documented
    // owner-loss recovery path, not a host-authorized native Stop.
    assert!(
        matches!(report, Err(spaceterm_server::StartupFailure::Runtime(error))
        if error.class == spaceterm_server::failure::ServerFailureClass::RecoveryRequired)
    );
    let snapshot = journal.snapshot().unwrap();
    assert_eq!(snapshot.bindings().len(), 1);
    let binding = snapshot.bindings()[0].binding().unwrap();
    assert_eq!(binding.target().pin(), pin);
    assert!(journal.ordinary_use_allowed(&binding).unwrap());
    assert!(snapshot.pairing().acknowledgements().is_empty());
    assert_eq!(credentials.0.lock().unwrap().len(), 2);
    assert_eq!(handle.snapshot().state(), before.state());
    assert_eq!(
        fixture
            .reopen()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Running
    );
    assert_eq!(
        fixture.reopen().discover().unwrap().unwrap().server_pin(),
        pin
    );
}
