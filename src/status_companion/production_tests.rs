//! Production status composition over a real private account registration and journal.

use super::super::native::NativeCompanion;
use super::*;
use spaceterm_client::api::{ConnectionTarget, PairedServerBinding, RouteDescription, RouteParts};
use spaceterm_client::failure::{ClientFailure, ClientFailureClass};
use spaceterm_client::journal::{
    ClientJournal, ClientJournalChange, ClientJournalTransaction, JournalCommitDisposition,
    JournalCredentialSlot, JournalLimits, ReceiptVerifier, SavedBinding,
};
use spaceterm_client_native::{NativeClientProvider, journal::NativeClientJournal};
use spaceterm_core::authority::{SessionKey, StoreRevision};
use spaceterm_core::identity::DeviceId;
use spaceterm_core::native_management::{NativeManagementRequestRef, NativeRetirementAction};
use spaceterm_core::operation_ledger::canonical::CanonicalCodec;
use spaceterm_core::operations::{OperationBinding, OperationId, OperationSequence, RequestDigest};
use spaceterm_core::pairing::{
    ClientRecord, CredentialReference, LocalPairingGeneration, PairingClientStreams,
    PairingClientStreamsParts, PairingKeys, PairingKeysParts, PairingReceiptBody, PairingTarget,
    PairingTargetParts, RevocationRouteReference, SignedPairingReceipt,
};
use spaceterm_core::retirement::SessionCleanupObservation;
use spaceterm_host::account_service::{
    LingerState, RegisteredServer, RuntimeAssessment, ServiceClock, ServiceFailure, ServiceFuture,
    ServiceManager, ServiceProcesses, ServiceTime, SupervisorState,
    test_support::AccountServiceFixture,
};
use spaceterm_host::server_keys::ServerKeyStore;
use spaceterm_host::startup::BuildIdentity;
use spaceterm_protocol::pairing_adapter::ReceiptCodec;
use std::os::unix::fs::DirBuilderExt;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    root: PathBuf,
    account: AccountServiceFixture,
    boundary: Arc<ReadOnlyBoundary>,
}
impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("companion-native-{}", u64::from_be_bytes(nonce)));
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        Self {
            account: AccountServiceFixture::new(&root, BuildIdentity::Development).unwrap(),
            root,
            boundary: Arc::new(ReadOnlyBoundary(Mutex::new(SupervisorState::Running))),
        }
    }
    fn provider(&self) -> NativeClientProvider {
        NativeClientProvider::from_supervisor(self.account.with_providers(
            self.boundary.clone(),
            self.boundary.clone(),
            self.boundary.clone(),
            None,
        ))
    }
    fn requests(&self) -> Option<Vec<u8>> {
        match std::fs::read(self.account.anchor().join("requests")) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("could not inspect test requests: {error}"),
        }
    }
    fn client_directory(&self, name: &str) -> PathBuf {
        let directory = self.root.join(name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        directory
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

// Only external observation is faked. Any lifecycle, authorization, assessment or
// retirement effect panics rather than being silently accepted by the fixture.
struct ReadOnlyBoundary(Mutex<SupervisorState>);
impl ServiceManager for ReadOnlyBoundary {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async { Ok(*self.0.lock().unwrap()) })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async { Ok(cfg!(target_os = "linux").then_some(LingerState::Enabled)) })
    }
    fn configure<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        panic!("status must not configure a service")
    }
    fn start<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        panic!("status must not start a service")
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        panic!("status must not stop a service")
    }
    fn uninstall<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        panic!("status must not uninstall a service")
    }
    fn verify_installation(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        panic!("status must not admit an installation")
    }
}
impl ServiceProcesses for ReadOnlyBoundary {
    fn assess<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, RuntimeAssessment> {
        panic!("status must not assess retirement")
    }
    fn revalidate<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        panic!("status must not reserve admissions")
    }
    fn abort_reservation<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        panic!("status must not abort admissions")
    }
    fn quiesce<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: NativeManagementRequestRef,
        _: NativeRetirementAction,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        panic!("status must not retire sessions")
    }
    fn verify_absent<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        panic!("status must not run process cleanup")
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: ServerEpoch,
        _: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        panic!("status must not run startup cleanup")
    }
}
impl ServiceClock for ReadOnlyBoundary {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        panic!("status must not issue an assessment")
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

async fn save_client(
    directory: PathBuf,
    pin: ServerPin,
    issuer: &dyn SigningKey,
    client_local: u64,
) -> SavedBinding {
    let server = ServerId::new(pin.identity);
    let client = ClientId::new(server, client_local).unwrap();
    let pairing = PairingId::new(server, 3).unwrap();
    let stream = |number| {
        OperationStreamId::new(
            OperationStreamKey::new(server, number).unwrap(),
            pairing,
            client,
        )
        .unwrap()
    };
    let streams = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: client,
            pairing,
        },
        ordinary: stream(5),
        self_revocation: stream(6),
    })
    .unwrap();
    let reference = CredentialReference::new(local(client_local));
    let target = ConnectionTarget::new(
        pin,
        DeviceId::new([2; 16]),
        pairing,
        client,
        RouteDescription::new(RouteParts::Ipc {
            directory: "/explicit-local-fixture".into(),
            name: "server".into(),
        })
        .unwrap(),
        reference,
        LocalPairingGeneration::new(1).unwrap(),
    )
    .unwrap();
    let keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: [7; 32],
        pairing_authentication_public_key: Key(
            Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap()
        )
        .public_key(),
    })
    .unwrap();
    let receipt = ReceiptCodec::new(pin)
        .sign(
            PairingReceiptBody::ClientBinding {
                target: PairingTarget::new(PairingTargetParts {
                    server,
                    device: target.device(),
                    pairing,
                })
                .unwrap(),
                keys: keys.clone(),
                client: streams.clone(),
                operation: OperationBinding {
                    id: OperationId::new(stream(8), OperationSequence::new(1).unwrap()),
                    digest: RequestDigest::from_bytes([8; 32]),
                },
                revision: StoreRevision::new(1),
            },
            issuer,
        )
        .unwrap();
    let binding = SavedBinding::verified(
        PairedServerBinding::new(target),
        streams,
        receipt,
        &Verifier,
    )
    .unwrap();
    let journal = NativeClientJournal::create(
        directory.clone(),
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let state = journal.recover().await.unwrap();
    let changes = vec![
        ClientJournalChange::ReserveCredential(JournalCredentialSlot {
            namespace: "dev.spaceterm.companion-test".into(),
            server: Some(server),
            reference,
            public_key: None,
            route_reference: None,
            route: None,
            ssh_capture: None,
            generation: LocalPairingGeneration::new(1).unwrap(),
        }),
        ClientJournalChange::RecordCredentialPublicKey {
            reference,
            public_key: keys.parts().pairing_authentication_public_key,
        },
        ClientJournalChange::BindCredentialRoute {
            reference,
            route_reference: RevocationRouteReference::new([9; 16]),
            route: binding.binding().unwrap().target().route().clone(),
        },
        ClientJournalChange::SaveBinding(Box::new(binding)),
    ];
    assert!(matches!(
        journal
            .commit(&ClientJournalTransaction {
                expected_revision: state.revision(),
                changes,
            })
            .await
            .unwrap(),
        JournalCommitDisposition::Durable(_)
    ));
    drop(journal);
    let journal = NativeClientJournal::open_existing(
        directory,
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    journal.recover().await.unwrap().bindings()[0].clone()
}

struct Backend {
    context: AuthenticatedManagementContext,
    reads: Arc<AtomicUsize>,
}
impl StatusBackend for Backend {
    fn context(&self) -> Result<AuthenticatedManagementContext, ManagementFailure> {
        Ok(self.context)
    }
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let server = self.context.server();
        Box::pin(async move {
            let mut parts = running(1, "1.2.3").parts().clone();
            parts.server = server;
            parts.remote_access.value = RemoteAccessPolicy::disabled(server);
            Ok(ServerStatus::new(parts).unwrap())
        })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(async {
            let subscription = StatusSubscription::new(
                self.context.server(),
                StatusQuery {
                    connection: self.context.connection_binding()?,
                    ..query()
                },
                Arc::new(Resync::default()),
            )?;
            subscription.apply(snapshot(self.status().await?))?;
            Ok(subscription)
        })
    }
}

#[tokio::test]
async fn native_composition_reads_reopened_companion_identity_and_rejects_desktop_or_cli_borrowing()
{
    for major in [1, 2] {
        let fixture = Fixture::new();
        let registration = fixture
            .account
            .initialize(DesiredServerState::Running)
            .unwrap();
        let pin = registration.server_pin();
        let issuer = ServerKeyStore::open(&registration)
            .unwrap()
            .load_identity(pin)
            .unwrap();
        let desktop =
            save_client(fixture.client_directory("desktop"), pin, issuer.as_ref(), 4).await;
        let cli = save_client(fixture.client_directory("cli"), pin, issuer.as_ref(), 8).await;
        let companion =
            save_client(fixture.client_directory("status"), pin, issuer.as_ref(), 9).await;
        assert_ne!(
            companion.streams().parts().client.id,
            desktop.streams().parts().client.id
        );
        assert_ne!(
            companion.streams().parts().client.id,
            cli.streams().parts().client.id
        );
        let reads = Arc::new(AtomicUsize::new(0));
        let backend = |client, kind| {
            let (connection, stream) =
                authenticated_on_server(client, ServerId::new(pin.identity), issuer.clone(), major);
            Backend {
                context: AuthenticatedManagementContext::from_connection(&connection, stream, kind)
                    .unwrap()
                    .with_connection_binding(ConnectionBinding::new(
                        stream.client(),
                        ConnectionGeneration::new(1),
                    ))
                    .unwrap(),
                reads: reads.clone(),
            }
        };
        for (client, kind) in [
            (4, ManagementClientKind::StatusCompanion),
            (8, ManagementClientKind::Cli),
        ] {
            assert!(matches!(
                fixture.provider().with_companion_backend(
                    companion.clone(),
                    &Verifier,
                    backend(client, kind),
                ),
                Err(ManagementFailure::AuthorizationDenied)
            ));
        }
        let provider = fixture
            .provider()
            .with_companion_backend(
                companion,
                &Verifier,
                backend(9, ManagementClientKind::StatusCompanion),
            )
            .unwrap();
        let selected = Arc::new(Mutex::new(fixture.provider()));
        let current = selected.clone();
        let factories = Arc::new(AtomicUsize::new(0));
        let calls = factories.clone();
        let native = NativeCompanion::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(current.lock().unwrap().clone())
        });
        assert!(matches!(
            native.status().await,
            Err(ManagementFailure::PairingRequired)
        ));
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        *selected.lock().unwrap() = provider;
        let requests_before = fixture.requests();
        let read = native.status().await.unwrap();
        assert_eq!(read.parts().running.as_ref().unwrap().as_str(), "1.2.3");
        let subscription = native.subscribe_status().await.unwrap();
        assert_eq!(subscription.latest().unwrap(), Some(read.clone()));
        assert_eq!(factories.load(Ordering::SeqCst), 3);
        assert_eq!(native.status().await.unwrap(), read);
        assert_eq!(
            factories.load(Ordering::SeqCst),
            3,
            "liveness must reuse the subscription's client worker"
        );
        *fixture.boundary.0.lock().unwrap() = SupervisorState::Suppressed;
        assert_eq!(
            native.status().await,
            Err(ManagementFailure::NativeUnavailable)
        );
        assert_eq!(factories.load(Ordering::SeqCst), 3);
        *fixture.boundary.0.lock().unwrap() = SupervisorState::Running;
        drop(subscription);
        drop(native);
        assert_eq!(reads.load(Ordering::SeqCst), 3);
        assert_eq!(fixture.requests(), requests_before);
        assert_eq!(
            fixture
                .provider()
                .management()
                .supervisor()
                .discover()
                .unwrap()
                .unwrap()
                .desired_state(),
            DesiredServerState::Running
        );
    }
}

#[tokio::test]
async fn native_composition_rediscovers_missing_setup_stop_suppression_and_missing_binding() {
    let fixture = Fixture::new();
    let provider = fixture.provider();
    let native = NativeCompanion::new(move || Ok(provider.clone()));
    assert!(matches!(
        native.subscribe_status().await,
        Err(ManagementFailure::NativeUnavailable)
    ));
    assert!(!fixture.account.anchor().exists());

    fixture
        .account
        .initialize(DesiredServerState::Running)
        .unwrap();
    assert!(matches!(
        native.subscribe_status().await,
        Err(ManagementFailure::PairingRequired)
    ));
    let requests = fixture.requests();
    *fixture.boundary.0.lock().unwrap() = SupervisorState::Suppressed;
    assert!(matches!(
        native.subscribe_status().await,
        Err(ManagementFailure::NativeUnavailable)
    ));
    assert_eq!(fixture.requests(), requests);

    let stopped = Fixture::new();
    let registration = stopped
        .account
        .initialize(DesiredServerState::Stopped)
        .unwrap();
    *stopped.boundary.0.lock().unwrap() = SupervisorState::Disabled;
    let provider = stopped.provider();
    let native = NativeCompanion::new(move || Ok(provider.clone()));
    assert!(matches!(
        native.subscribe_status().await,
        Err(ManagementFailure::PairingRequired)
    ));

    let pin = registration.server_pin();
    let issuer = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(pin)
        .unwrap();
    let saved = save_client(stopped.client_directory("status"), pin, issuer.as_ref(), 9).await;
    let (connection, stream) = authenticated_on_server(9, ServerId::new(pin.identity), issuer, 2);
    let reads = Arc::new(AtomicUsize::new(0));
    let provider = stopped
        .provider()
        .with_companion_backend(
            saved,
            &Verifier,
            Backend {
                context: AuthenticatedManagementContext::from_connection(
                    &connection,
                    stream,
                    ManagementClientKind::StatusCompanion,
                )
                .unwrap()
                .with_connection_binding(ConnectionBinding::new(
                    stream.client(),
                    ConnectionGeneration::new(1),
                ))
                .unwrap(),
                reads: reads.clone(),
            },
        )
        .unwrap();
    let native = NativeCompanion::new(move || Ok(provider.clone()));
    assert!(matches!(
        native.subscribe_status().await,
        Err(ManagementFailure::Offline)
    ));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let observation = native.inspect().await.unwrap();
    assert_eq!(
        CompanionModel::disconnected(ManagementFailure::Offline, Some(&observation)).state(),
        ServerState::Stopped
    );
}
