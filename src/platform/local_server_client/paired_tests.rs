use super::tests::Root;
use super::*;
use spaceterm_client::{
    api::ConnectionTarget,
    journal::{ClientJournalChange, JournalLimits, ReceiptVerifier, SavedBinding},
};
use spaceterm_client_native::{NativeClientProvider, journal::NativeClientJournal};
use spaceterm_core::{
    authority::StoreRevision,
    identity::{ClientId, DeviceId, OperationStreamId, OperationStreamKey, PairingId, ServerId},
    operation_ledger::canonical::CanonicalCodec,
    operations::{OperationBinding, OperationId, OperationSequence, RequestDigest},
    pairing::{
        ClientRecord, CredentialReference, LocalPairingGeneration, PairingClientStreams,
        PairingClientStreamsParts, PairingKeys, PairingKeysParts, PairingReceiptBody,
        PairingTarget, PairingTargetParts,
    },
};
use spaceterm_host::{
    account_service::test_support::AccountServiceFixture, server_keys::ServerKeyStore,
    startup::BuildIdentity,
};
use spaceterm_protocol::{authentication::ServerPin, pairing_adapter::ReceiptCodec};
use std::sync::Arc;

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(
        &self,
        pin: ServerPin,
        receipt: &spaceterm_core::pairing::SignedPairingReceipt,
    ) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(Into::into)
    }
}

async fn paired_journal(root: &Root, registration: &RegisteredServer) -> Arc<JournalController> {
    use spaceterm_protocol::authentication::SigningKey;
    use std::os::unix::fs::PermissionsExt;
    let directory = root.0.join("client-journal");
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
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
        JournalController::recover(
            Arc::new(storage),
            codec,
            verifier.clone(),
            JournalLimits::default(),
        )
        .await
        .unwrap(),
    );
    let identity = ServerKeyStore::open(registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let pairing = PairingId::new(server, 1).unwrap();
    let client = ClientId::new(server, 1).unwrap();
    let device = DeviceId::new([91; 16]);
    let ordinary =
        OperationStreamId::new(OperationStreamKey::new(server, 1).unwrap(), pairing, client)
            .unwrap();
    let self_revocation =
        OperationStreamId::new(OperationStreamKey::new(server, 2).unwrap(), pairing, client)
            .unwrap();
    let streams = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: client,
            pairing,
        },
        ordinary,
        self_revocation,
    })
    .unwrap();
    let route = LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap();
    let binding = PairedServerBinding::new(
        ConnectionTarget::new(
            registration.server_pin(),
            device,
            pairing,
            client,
            route.description().clone(),
            CredentialReference::new([91; 16]),
            LocalPairingGeneration::new(1).unwrap(),
        )
        .unwrap(),
    );
    let proof = ReceiptCodec::new(registration.server_pin())
        .sign(
            PairingReceiptBody::ClientBinding {
                target: PairingTarget::new(PairingTargetParts {
                    server,
                    pairing,
                    device,
                })
                .unwrap(),
                keys: PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: spaceterm_client::testing::DeterministicKey::new(
                        91,
                    )
                    .unwrap()
                    .public_key(),
                    pairing_authentication_public_key:
                        spaceterm_client::testing::DeterministicKey::new(92)
                            .unwrap()
                            .public_key(),
                })
                .unwrap(),
                client: streams.clone(),
                operation: OperationBinding {
                    id: OperationId::new(ordinary, OperationSequence::new(1).unwrap()),
                    digest: RequestDigest::from_bytes([1; 32]),
                },
                revision: StoreRevision::new(1),
            },
            identity.as_ref(),
        )
        .unwrap();
    let saved = SavedBinding::verified(binding, streams, proof, verifier.as_ref()).unwrap();
    journal
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(saved))])
        .await
        .unwrap();
    journal
}

#[test]
fn desktop_local_discovery_reuses_the_verified_binding_and_original_private_route() {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let journal = runtime.block_on(paired_journal(&root, &registration));
    let before = journal.snapshot().unwrap().revision();
    let provider = NativeClientProvider::from_supervisor(fixture.reopen());
    let found = discover(&provider, &journal).unwrap().unwrap();
    let binding = found.binding.unwrap();
    assert_eq!(binding.target().pin(), registration.server_pin());
    assert_eq!(found.route.unwrap().description(), binding.target().route());
    assert_eq!(journal.snapshot().unwrap().revision(), before);
    assert_eq!(found.registration.generation(), registration.generation());
}

#[test]
fn desktop_local_stopped_binding_never_opens_missing_runtime_directory_or_starts_service() {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let journal = runtime.block_on(paired_journal(&root, &registration));
    let endpoint = registration.endpoint().unwrap();
    std::fs::remove_dir(endpoint.path().parent().unwrap()).unwrap();
    let provider = NativeClientProvider::from_supervisor(fixture.reopen());
    let found = discover(&provider, &journal).unwrap().unwrap();
    let binding = found.binding.unwrap();
    assert!(found.route.is_none());
    assert!(found.route_failure.is_none());
    assert_eq!(
        found.registration.desired_state(),
        DesiredServerState::Stopped
    );
    let discovery = trusted_discovery(provider);
    assert_eq!(
        runtime
            .block_on(discovery.status(binding.target()))
            .unwrap(),
        spaceterm_client::ports::DiscoveryState::Stopped
    );
}

#[test]
fn desktop_local_changed_route_is_rejected_without_replacing_verified_binding() {
    use spaceterm_client::api::RouteParts;
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let journal = runtime.block_on(paired_journal(&root, &registration));
    use std::os::unix::fs::PermissionsExt;
    let saved_route = journal.snapshot().unwrap().bindings()[0]
        .binding()
        .unwrap()
        .target()
        .route()
        .clone();
    assert!(matches!(saved_route.parts(), RouteParts::Ipc { .. }));
    let before = journal.snapshot().unwrap().revision();
    let changed_runtime = root.0.join("changed-runtime");
    std::fs::create_dir(&changed_runtime).unwrap();
    std::fs::set_permissions(&changed_runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = fixture.anchor().join("registration");
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["runtime"] = serde_json::to_value(changed_runtime).unwrap();
    record.sort_all_objects();
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    let provider = NativeClientProvider::from_supervisor(fixture.reopen());
    let error = match discover(&provider, &journal) {
        Ok(_) => panic!("changed endpoint admitted"),
        Err(error) => error,
    };
    assert_eq!(error.class(), ClientFailureClass::StaleAuthority);
    assert_eq!(journal.snapshot().unwrap().revision(), before);
    assert_eq!(
        journal.snapshot().unwrap().bindings()[0]
            .binding()
            .unwrap()
            .target()
            .route(),
        &saved_route
    );
}

#[test]
fn desktop_local_missing_endpoint_keeps_registered_identity_and_saved_binding() {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let journal = runtime.block_on(paired_journal(&root, &registration));
    let endpoint = registration.endpoint().unwrap();
    std::fs::remove_dir(endpoint.path().parent().unwrap()).unwrap();
    let provider = NativeClientProvider::from_supervisor(fixture.reopen());
    let found = discover(&provider, &journal).unwrap().unwrap();
    assert_eq!(found.registration.server_pin(), registration.server_pin());
    assert!(found.binding.is_some());
    assert!(found.route.is_none());
    assert_eq!(
        found.route_failure.unwrap().class(),
        ClientFailureClass::Unavailable
    );
}

#[test]
fn desktop_native_start_retries_original_local_route_after_stopped_launch() {
    use spaceterm_client::ports::ConnectionPurpose;
    use spaceterm_host::{
        ipc::IpcListener,
        transport::{AdmissionLimits, MessageLimit},
    };
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let journal = runtime.block_on(paired_journal(&root, &registration));
    let provider = NativeClientProvider::from_supervisor(fixture.reopen());
    let target = journal.snapshot().unwrap().bindings()[0].binding().unwrap();
    let connector = crate::platform::server_client_route::refreshing_connector(
        Some(provider),
        journal.clone(),
        crate::platform::server_client_route::connector(vec![], None).unwrap(),
    );
    assert!(
        runtime
            .block_on(connector.open(target.target(), ConnectionPurpose::Ordinary))
            .is_err()
    );
    // Inject the authorized service-manager boundary publishing explicit Start, not saved-path authority.
    let path = fixture.anchor().join("registration");
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["desired"] = serde_json::to_value(DesiredServerState::Running).unwrap();
    record.sort_all_objects();
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    let _entered = runtime.enter();
    let listener = IpcListener::bind(
        registration.endpoint().unwrap(),
        MessageLimit::new(4096).unwrap(),
        AdmissionLimits::new(8, std::time::Duration::from_secs(5)).unwrap(),
    )
    .unwrap();
    let route = runtime
        .block_on(connector.open(target.target(), ConnectionPurpose::Ordinary))
        .unwrap();
    assert!(route.peer.local_account_verified);
    assert_eq!(
        journal.snapshot().unwrap().bindings()[0].binding().unwrap(),
        target
    );
    runtime.block_on(route.sender.close()).unwrap();
    listener.close().unwrap();
}
