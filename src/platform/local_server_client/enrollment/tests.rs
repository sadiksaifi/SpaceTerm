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

pub(super) async fn journal(root: &Root) -> Arc<JournalController> {
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
    foreground_owner_pairing(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_foreground_owner_pairing_provisions_dedicated_clients_from_running_account_actor() {
    foreground_owner_pairing(true).await;
}

async fn foreground_owner_pairing(cli: bool) {
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
    let result = if cli {
        use spaceterm_client_native::{
            cli::{
                arguments::{CliCommand, parse},
                bindings::PairingCommandBindings,
            },
            native::consumer::{NativeConsumer, NativeConsumerKind},
        };
        let invocation = parse(
            [
                "pair",
                "enroll",
                "--server-fingerprint",
                &spaceterm_client_native::cli::output::hex(&pin.public_key),
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        )
        .unwrap();
        let CliCommand::PairEnroll { fingerprint, .. } = invocation.command else {
            panic!("pair enroll required");
        };
        let consumer = NativeConsumer::new(native.management(), NativeConsumerKind::Cli)
            .with_credentials(Arc::new(credentials.clone()));
        consumer
            .enroll(
                fingerprint,
                false,
                spaceterm_host::native_management::SudoMode::NonInteractive,
                &mut PairIo,
            )
            .await
            .map(|_| ())
            .map_err(|_| {
                spaceterm_client::failure::ClientFailure::new(
                    spaceterm_client::failure::ClientFailureClass::Unavailable,
                )
            })
    } else {
        enroll_with_credentials(&native, journal.clone(), credentials.clone()).await
    };
    if result.is_ok() {
        use spaceterm_client::management_cli::{ProtocolManagement, StatusManagement};
        use spaceterm_client_native::native::consumer::{NativeConsumer, NativeConsumerKind};
        let cli = NativeConsumer::new(native.management(), NativeConsumerKind::Cli)
            .with_credentials(Arc::new(credentials.clone()));
        assert_eq!(
            ProtocolManagement::status(&cli)
                .await
                .unwrap()
                .parts()
                .server
                .bytes(),
            pin.identity
        );
        cli.shutdown().await.unwrap();
        let companion =
            NativeConsumer::new(native.management(), NativeConsumerKind::StatusCompanion)
                .with_credentials(Arc::new(credentials.clone()));
        let mut subscription = StatusManagement::subscribe_status(&companion)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while subscription.latest().unwrap().is_none() {
                subscription.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            subscription
                .latest()
                .unwrap()
                .unwrap()
                .parts()
                .server
                .bytes(),
            pin.identity
        );
        companion.shutdown().await.unwrap();
    }
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
    if !cli {
        let snapshot = journal.snapshot().unwrap();
        assert_eq!(snapshot.bindings().len(), 1);
        let binding = snapshot.bindings()[0].binding().unwrap();
        assert_eq!(binding.target().pin(), pin);
        assert!(journal.ordinary_use_allowed(&binding).unwrap());
        assert!(snapshot.pairing().acknowledgements().is_empty());
    }
    let mut clients = std::collections::BTreeSet::new();
    for consumer in ["cli", "companion", "management"] {
        let codec = Arc::new(CanonicalCodec);
        let verifier = Arc::new(Verifier);
        let storage = spaceterm_client_native::journal::NativeClientJournal::open_existing(
            native
                .management()
                .supervisor()
                .client_connections_directory()
                .join(consumer),
            codec.clone(),
            verifier.clone(),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let journal = JournalController::recover(
            Arc::new(storage),
            codec,
            verifier,
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let snapshot = journal.snapshot().unwrap();
        assert_eq!(snapshot.bindings().len(), 1);
        let saved = &snapshot.bindings()[0];
        assert!(matches!(
            saved.proof().parts().claims,
            spaceterm_core::pairing::PairingReceiptBody::ClientBinding { .. }
        ));
        assert!(!saved.additional_client_acknowledgement_pending());
        let binding = saved.binding().unwrap();
        assert!(journal.ordinary_use_allowed(&binding).unwrap());
        assert_eq!(binding.target().pin(), pin);
        assert!(clients.insert(binding.target().client()));
    }
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

struct TestChannel {
    send: tokio::sync::Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>,
    completion: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    receive: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    uid: u32,
}
impl MessageTransport for TestChannel {
    fn peer_facts(&self) -> spaceterm_host::transport::PeerFacts {
        spaceterm_host::transport::PeerFacts {
            route: spaceterm_host::transport::TransportRoute::PrivateIpc,
            local_uid: Some(self.uid),
            local_pid: None,
            pinned_server_key: None,
        }
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> spaceterm_host::transport::TransportFuture<'a, ()> {
        Box::pin(async move {
            self.send
                .lock()
                .await
                .as_ref()
                .ok_or(spaceterm_host::transport::TransportError::Closed)?
                .send(bytes.to_vec())
                .await
                .map_err(|_| spaceterm_host::transport::TransportError::Closed)
        })
    }
    fn recv(&self) -> spaceterm_host::transport::TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.receive.lock().await.recv().await) })
    }
    fn close(&self) -> spaceterm_host::transport::TransportFuture<'_, ()> {
        Box::pin(async {
            self.send.lock().await.take();
            if let Some(task) = self.completion.lock().await.take() {
                task.await
                    .map_err(|_| spaceterm_host::transport::TransportError::Unavailable)?;
            }
            Ok(())
        })
    }
}
struct InProcessOwner(Arc<spaceterm_host::account_service::ServiceSupervisor>);
impl OwnerChannelProvider for InProcessOwner {
    fn open<'a>(
        &'a self,
        registration: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_client::ports::ClientFuture<'a, Arc<dyn MessageTransport>> {
        Box::pin(async move {
            let (first, first_receiver) = tokio::sync::mpsc::channel(1);
            let (second, second_receiver) = tokio::sync::mpsc::channel(1);
            let uid = registration.key().account_uid;
            let server = TestChannel {
                send: tokio::sync::Mutex::new(Some(first)),
                completion: tokio::sync::Mutex::new(None),
                receive: tokio::sync::Mutex::new(second_receiver),
                uid,
            };
            let supervisor = self.0.clone();
            let task = tokio::spawn(async move {
                spaceterm_server::deployment::owner::execute(supervisor, &server)
                    .await
                    .unwrap();
            });
            Ok(Arc::new(TestChannel {
                send: tokio::sync::Mutex::new(Some(second)),
                completion: tokio::sync::Mutex::new(Some(task)),
                receive: tokio::sync::Mutex::new(first_receiver),
                uid,
            }) as Arc<dyn MessageTransport>)
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_foreground_owner_pairing_preserves_stopped_server() {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
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
    let journal = journal(&root).await;
    let credentials = Credentials::default();
    let native = NativeClientProvider::from_supervisor((*supervisor).clone());
    let channels = InProcessOwner(supervisor.clone());
    enroll_using(&native, journal.clone(), credentials.clone(), &channels)
        .await
        .unwrap();
    let snapshot = journal.snapshot().unwrap();
    assert_eq!(snapshot.bindings().len(), 1);
    let binding = snapshot.bindings()[0].binding().unwrap();
    assert_eq!(binding.target().pin(), pin);
    assert!(journal.ordinary_use_allowed(&binding).unwrap());
    assert!(snapshot.pairing().acknowledgements().is_empty());
    assert_eq!(credentials.0.lock().unwrap().len(), 2);
    assert_eq!(
        fixture
            .reopen()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
    assert_eq!(
        fixture.reopen().discover().unwrap().unwrap().server_pin(),
        pin
    );
}

struct PairIo;
impl spaceterm_client_native::cli::runner::CliIo for PairIo {
    fn write_stdout(&mut self, _: &[u8]) -> Result<(), spaceterm_client_native::cli::CliFailure> {
        Ok(())
    }
    fn write_stderr(&mut self, _: &[u8]) -> Result<(), spaceterm_client_native::cli::CliFailure> {
        Ok(())
    }
    fn read_confirmation(
        &mut self,
        _: &spaceterm_client::management_cli::ConfirmationPrompt,
    ) -> Result<
        spaceterm_client::management_cli::ConfirmationReply,
        spaceterm_client_native::cli::CliFailure,
    > {
        panic!("explicit fingerprint must not prompt")
    }
}

struct LostEnrollmentReceipt {
    owner: InProcessOwner,
    lost: Arc<std::sync::atomic::AtomicBool>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}
struct FaultedOwnerChannel {
    transport: Arc<dyn MessageTransport>,
    receives: std::sync::atomic::AtomicUsize,
    sends: std::sync::atomic::AtomicUsize,
    lost: Arc<std::sync::atomic::AtomicBool>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}
impl OwnerChannelProvider for LostEnrollmentReceipt {
    fn open<'a>(
        &'a self,
        registration: &'a spaceterm_host::account_service::RegisteredServer,
    ) -> spaceterm_client::ports::ClientFuture<'a, Arc<dyn MessageTransport>> {
        Box::pin(async move {
            Ok(Arc::new(FaultedOwnerChannel {
                transport: self.owner.open(registration).await?,
                receives: std::sync::atomic::AtomicUsize::new(0),
                sends: std::sync::atomic::AtomicUsize::new(0),
                lost: self.lost.clone(),
                requests: self.requests.clone(),
            }) as Arc<dyn MessageTransport>)
        })
    }
}
impl MessageTransport for FaultedOwnerChannel {
    fn peer_facts(&self) -> spaceterm_host::transport::PeerFacts {
        self.transport.peer_facts()
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> spaceterm_host::transport::TransportFuture<'a, ()> {
        Box::pin(async move {
            if self.sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                self.requests
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(bytes).unwrap());
            }
            self.transport.send(bytes).await
        })
    }
    fn recv(&self) -> spaceterm_host::transport::TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            let reply = self.transport.recv().await?;
            // Public owner result, authorization delivery, ServerHello and
            // ServerFinished precede the first encrypted enrollment receipt.
            if self
                .receives
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 4
                && !self.lost.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(spaceterm_host::transport::TransportError::Unavailable);
            }
            Ok(reply)
        })
    }
    fn close(&self) -> spaceterm_host::transport::TransportFuture<'_, ()> {
        self.transport.close()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_owner_enrollment_recovers_consumed_receipt_without_duplicate_native_request_or_pairing()
 {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
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
            production_services(identity, home).unwrap()
        ),
        InitializationDisposition::Durable { .. }
    ));
    let supervisor = supervisor(&root.0, Arc::new(StartupAbsence)).unwrap();
    let journal = journal(&root).await;
    let credentials = Credentials::default();
    let native = NativeClientProvider::from_supervisor((*supervisor).clone());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let channels = LostEnrollmentReceipt {
        owner: InProcessOwner(supervisor),
        lost: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        requests: requests.clone(),
    };
    assert!(
        enroll_using(&native, journal.clone(), credentials.clone(), &channels)
            .await
            .is_err()
    );
    assert!(journal.snapshot().unwrap().bindings().is_empty());
    // A reconstructed desktop repeats the original claim and resolves the
    // consumed reservation. The single-use bearer is never delivered again.
    let reconstructed = NativeClientProvider::from_supervisor(fixture.reopen());
    enroll_using(
        &reconstructed,
        journal.clone(),
        credentials.clone(),
        &channels,
    )
    .await
    .unwrap();
    let attempts = requests.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["request"], attempts[1]["request"]);
    assert_eq!(attempts[0]["proposal"], attempts[1]["proposal"]);
    assert_eq!(credentials.0.lock().unwrap().len(), 2);
    let snapshot = journal.snapshot().unwrap();
    assert_eq!(snapshot.bindings().len(), 1);
    assert!(snapshot.pairing().acknowledgements().is_empty());
    assert!(
        journal
            .ordinary_use_allowed(&snapshot.bindings()[0].binding().unwrap())
            .unwrap()
    );
    let database = Database::open_existing_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    let pairings: usize = database
        .query_row("SELECT count(*) FROM pairings", [], |row| row.get(0))
        .unwrap();
    let clients: usize = database
        .query_row("SELECT count(*) FROM paired_clients", [], |row| row.get(0))
        .unwrap();
    assert_eq!(pairings, 1);
    assert_eq!(clients, 1);
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
