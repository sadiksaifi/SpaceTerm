#![cfg(any(target_os = "macos", target_os = "linux"))]

//! Public CLI enrollment through authenticated installed code and the production
//! command transport. Every account, journal and native home belongs to this test.

#[path = "support/owner_credentials.rs"]
mod owner_credentials;
#[expect(
    dead_code,
    reason = "this suite signs runnable bytes rather than dummy architecture fixtures"
)]
#[path = "../../spaceterm-host/tests/support/signed_installation.rs"]
mod signed_installation;

use owner_credentials::MemoryCredentials;
use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::{JournalController, JournalLimits, ReceiptVerifier},
    management_cli::{ConfirmationPrompt, ConfirmationReply},
    ports::ClientFuture,
};
use spaceterm_client_native::{
    NativeClientProvider,
    cli::{
        CliFailure,
        arguments::parse,
        output::hex,
        runner::{CliDependencies, CliIo, Dispatch, dispatch},
    },
    journal::NativeClientJournal,
    native::{
        consumer::{NativeConsumer, NativeConsumerKind},
        owner_channel::{OwnerChannelProvider, test_support::installed_owner_channel},
    },
};
use spaceterm_core::{
    identity::ServerId,
    operation_ledger::canonical::CanonicalCodec,
    organization::ServerDirectory,
    pairing::{PairingReceiptBody, SignedPairingReceipt},
};
use spaceterm_host::{
    account_service::{InstallationStatus, RegisteredServer, test_support::AccountServiceFixture},
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits,
        bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
        migrations, schema,
    },
    server_installation::test_support as installations,
    startup::{BuildIdentity, DesiredServerState, InstallationId},
    storage::Database,
    transport::{MessageTransport, PeerFacts, TransportFuture},
};
use spaceterm_protocol::{
    authentication::{HandshakeConfig, ServerPin},
    pairing_adapter::ReceiptCodec,
};
use spaceterm_server::deployment::owner::test_support as stopped_owner;
use std::{
    fs,
    num::NonZeroU64,
    os::unix::fs::DirBuilderExt as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidStorage))
    }
}

#[derive(Default)]
struct Io(Vec<u8>);
impl CliIo for Io {
    fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn write_stderr(&mut self, _: &[u8]) -> Result<(), CliFailure> {
        panic!("signed stopped-owner enrollment must not produce a CLI failure")
    }
    fn read_confirmation(
        &mut self,
        _: &ConfirmationPrompt,
    ) -> Result<ConfirmationReply, CliFailure> {
        panic!("the public noninteractive CLI has an exact server fingerprint")
    }
}

struct ObservedOwnerChannel {
    installed: Arc<dyn OwnerChannelProvider>,
    opened: AtomicUsize,
    closed: Arc<AtomicUsize>,
}
impl OwnerChannelProvider for ObservedOwnerChannel {
    fn open<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ClientFuture<'a, Arc<dyn MessageTransport>> {
        Box::pin(async move {
            assert_eq!(registration.desired_state(), DesiredServerState::Stopped);
            let endpoint = registration.endpoint().unwrap().path().to_owned();
            assert!(!endpoint.exists());
            let inner = self.installed.open(registration).await?;
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(ObservedTransport {
                inner,
                endpoint,
                closed: self.closed.clone(),
            }) as Arc<dyn MessageTransport>)
        })
    }
}
struct ObservedTransport {
    inner: Arc<dyn MessageTransport>,
    endpoint: PathBuf,
    closed: Arc<AtomicUsize>,
}
impl MessageTransport for ObservedTransport {
    fn peer_facts(&self) -> PeerFacts {
        self.inner.peer_facts()
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            assert!(!self.endpoint.exists());
            let result = self.inner.send(bytes).await;
            assert!(!self.endpoint.exists());
            result
        })
    }
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            assert!(!self.endpoint.exists());
            let result = self.inner.recv().await;
            assert!(!self.endpoint.exists());
            result
        })
    }
    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async move {
            self.inner.close().await?;
            assert!(!self.endpoint.exists());
            self.closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

async fn enrollment_receipt(directory: &Path, pin: ServerPin, client: u64) -> SignedPairingReceipt {
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(Verifier);
    let storage = NativeClientJournal::open_existing(
        directory.to_owned(),
        codec.clone(),
        verifier.clone(),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let journal =
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap();
    let state = journal.snapshot().unwrap();
    let [saved] = state.bindings() else {
        panic!("one acknowledged owner binding required")
    };
    let binding = saved.binding().unwrap();
    assert_eq!(binding.target().pin(), pin);
    assert_eq!(binding.target().client().local(), client);
    assert!(journal.ordinary_use_allowed(&binding).unwrap());
    let receipt = saved.proof().clone();
    ReceiptCodec::new(pin).verify(&receipt).unwrap();
    assert!(matches!(
        receipt.parts().claims,
        PairingReceiptBody::Enrollment { .. }
    ));
    receipt
}

#[tokio::test]
async fn public_cli_enrolls_through_signed_installed_stopped_owner_in_both_majors() {
    let binary = fs::read(env!("CARGO_BIN_EXE_spaceterm-server")).unwrap();
    let commit = env!("SPACETERM_SERVER_COMMIT");
    assert_eq!(commit.len(), 40);
    let mut build_commit = [0; 20];
    for (index, byte) in build_commit.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&commit[index * 2..index * 2 + 2], 16).unwrap();
    }
    let (key, artifact) = signed_installation::fixture_with_binary_and_commit(
        env!("SPACETERM_SERVER_VERSION"),
        7,
        &binary,
        build_commit,
    );
    for major in [1, 2] {
        #[cfg(target_os = "macos")]
        let parent = Path::new("/private/tmp");
        #[cfg(target_os = "linux")]
        let parent = Path::new("/var/tmp");
        let root = tempfile::Builder::new()
            .prefix("st-signed-owner-")
            .tempdir_in(parent)
            .unwrap();
        let home = root.path().join("home");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let approved = InstallationStatus {
            installation: InstallationId(NonZeroU64::new(1).unwrap()),
            version: artifact.manifest().version.clone(),
            artifact_sha256: artifact.artifact_sha256(),
        };
        let registration = fixture
            .initialize_with_installation(DesiredServerState::Stopped, Some(approved.clone()))
            .unwrap();
        {
            let store = installations::store(&registration, key.clone()).unwrap();
            let id = store.stage(&artifact).unwrap();
            assert_eq!(id, approved.installation);
            store
                .activate(
                    id,
                    store.snapshot().unwrap().generation,
                    schema::SCHEMA_VERSION,
                )
                .unwrap();
            let executable = store
                .retain_compatible_approved(schema::SCHEMA_VERSION)
                .unwrap();
            assert!(executable.path().starts_with(root.path()));
            assert_ne!(
                executable.path(),
                Path::new(env!("CARGO_BIN_EXE_spaceterm-server"))
            );
            assert!(fs::read(executable.path()).unwrap() == binary);
        }
        let pin = registration.server_pin();
        let server = ServerId::new(pin.identity);
        let identity = StoreIdentity {
            server,
            account: registration.key(),
            server_public_key: pin.public_key,
        };
        let directory =
            ServerDirectory::new(server, home.as_os_str().as_encoded_bytes().to_vec()).unwrap();
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
                spaceterm_server::adapters::store::production_services(identity, directory)
                    .unwrap(),
            ),
            InitializationDisposition::Durable { .. }
        ));
        let identity_bytes = fs::read(fixture.data_directory().join("identity")).unwrap();
        let generation = registration.generation();
        let registration_bytes = fs::read(fixture.anchor().join("registration")).unwrap();
        let credentials = Arc::new(MemoryCredentials::new());
        let mut original_result = None;
        let mut original_receipt = None;
        for attempt in 0..6 {
            let supervisor = Arc::try_unwrap(stopped_owner::supervisor(root.path()).unwrap())
                .ok()
                .unwrap();
            let provider = NativeClientProvider::from_supervisor(
                fixture.with_installation_trust(supervisor, key.clone()),
            );
            let channels = Arc::new(ObservedOwnerChannel {
                installed: installed_owner_channel(
                    provider.management(),
                    root.path(),
                    &home,
                    key.clone(),
                )
                .unwrap(),
                opened: AtomicUsize::new(0),
                closed: Arc::new(AtomicUsize::new(0)),
            });
            let consumer = Arc::new(
                NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
                    .with_credentials(credentials.clone())
                    .with_owner_handshake(HandshakeConfig {
                        majors: vec![major],
                        ..HandshakeConfig::default()
                    })
                    .with_owner_channels(channels.clone()),
            );
            let previous_client = original_result
                .as_ref()
                .map(|result: &serde_json::Value| result["result"]["client"].as_u64().unwrap());
            if attempt == 2 {
                credentials.lose_authentication_key();
                original_result = None;
                original_receipt = None;
            }
            if attempt == 4 {
                credentials.lose_device_key();
                original_result = None;
                original_receipt = None;
            }
            let fingerprint = hex(&pin.public_key);
            let dependencies = CliDependencies::new(
                Ok(provider.key()),
                consumer.clone(),
                provider.management(),
                consumer.clone(),
            );
            let mut arguments = vec![
                "--json",
                "--non-interactive",
                "pair",
                "enroll",
                "--server-fingerprint",
                &fingerprint,
            ];
            if attempt >= 2 {
                arguments.push("--recover-account");
            }
            let invocation = parse(arguments.into_iter().map(std::ffi::OsString::from)).unwrap();
            let mut io = Io::default();
            let Dispatch::Exit(exit) = tokio::time::timeout(
                Duration::from_secs(60),
                dispatch(invocation, &dependencies, &mut io),
            )
            .await
            .unwrap() else {
                panic!("pair enroll must exit without starting serve")
            };
            assert_eq!(exit.code(), 0);
            let result: serde_json::Value = serde_json::from_slice(&io.0).unwrap();
            assert_eq!(result["result"]["kind"], "enrolled");
            assert_eq!(result["result"]["server"], hex(&pin.identity));
            let client = result["result"]["client"].as_u64().unwrap();
            assert!(client > 0);
            if attempt == 2 || attempt == 4 {
                assert_ne!(Some(client), previous_client);
            }
            if let Some(original) = &original_result {
                assert_eq!(&result, original);
            } else {
                original_result = Some(result);
            }
            assert_eq!(
                channels.opened.load(Ordering::SeqCst),
                usize::from(attempt == 0 || attempt == 2 || attempt == 4)
            );
            assert_eq!(
                channels.closed.load(Ordering::SeqCst),
                usize::from(attempt == 0 || attempt == 2 || attempt == 4)
            );
            drop(dependencies);
            Arc::try_unwrap(consumer)
                .ok()
                .unwrap()
                .shutdown()
                .await
                .unwrap();
            let receipt = enrollment_receipt(
                &provider
                    .management()
                    .supervisor()
                    .client_connections_directory()
                    .join("enrollment"),
                pin,
                client,
            )
            .await;
            if let Some(original) = &original_receipt {
                assert_eq!(&receipt, original);
            } else {
                original_receipt = Some(receipt);
            }
            assert_eq!(credentials.created(), 2 * (attempt / 2 + 1));
            let current = fixture.reopen().discover().unwrap().unwrap();
            assert_eq!(current.desired_state(), DesiredServerState::Stopped);
            assert_eq!(current.server_pin(), pin);
            assert_eq!(current.generation(), generation);
            assert_eq!(current.approved_installation(), Some(&approved));
            assert!(fs::read(fixture.data_directory().join("identity")).unwrap() == identity_bytes);
            assert_eq!(
                fs::read(fixture.anchor().join("registration")).unwrap(),
                registration_bytes
            );
            assert!(!current.endpoint().unwrap().path().exists());
            assert!(
                !current
                    .endpoint()
                    .unwrap()
                    .path()
                    .parent()
                    .unwrap()
                    .join("owner")
                    .exists()
            );
        }
    }
}

#[tokio::test]
async fn stopped_owner_rejects_signed_selection_not_approved_by_registration_before_spawn() {
    let binary = fs::read(env!("CARGO_BIN_EXE_spaceterm-server")).unwrap();
    let (key, first) = signed_installation::fixture_with_binary("1.0.0", 7, &binary);
    let (_, second) = signed_installation::fixture_with_binary("1.0.1", 7, &binary);
    #[cfg(target_os = "macos")]
    let parent = Path::new("/private/tmp");
    #[cfg(target_os = "linux")]
    let parent = Path::new("/var/tmp");
    let root = tempfile::Builder::new()
        .prefix("st-owner-selection-")
        .tempdir_in(parent)
        .unwrap();
    let home = root.path().join("home");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture
        .initialize_with_installation(
            DesiredServerState::Stopped,
            Some(InstallationStatus {
                installation: InstallationId(NonZeroU64::new(1).unwrap()),
                version: first.manifest().version.clone(),
                artifact_sha256: first.artifact_sha256(),
            }),
        )
        .unwrap();
    {
        let store = installations::store(&registration, key.clone()).unwrap();
        let first_id = store.stage(&first).unwrap();
        store
            .activate(
                first_id,
                store.snapshot().unwrap().generation,
                schema::SCHEMA_VERSION,
            )
            .unwrap();
        let second_id = store.stage(&second).unwrap();
        store
            .activate(
                second_id,
                store.snapshot().unwrap().generation,
                schema::SCHEMA_VERSION,
            )
            .unwrap();
        assert_ne!(
            store
                .retain_compatible_approved(schema::SCHEMA_VERSION)
                .unwrap()
                .status()
                .installation,
            registration.approved_installation().unwrap().installation
        );
    }
    let before = fs::read(fixture.anchor().join("registration")).unwrap();
    let supervisor = Arc::try_unwrap(stopped_owner::supervisor(root.path()).unwrap())
        .ok()
        .unwrap();
    let provider = NativeClientProvider::from_supervisor(
        fixture.with_installation_trust(supervisor, key.clone()),
    );
    let channel = installed_owner_channel(provider.management(), root.path(), &home, key).unwrap();
    let result = channel.open(&registration).await;
    assert!(matches!(result, Err(error) if error.class() == ClientFailureClass::StaleAuthority));
    assert_eq!(
        fs::read(fixture.anchor().join("registration")).unwrap(),
        before
    );
    assert!(!registration.endpoint().unwrap().path().exists());
}
