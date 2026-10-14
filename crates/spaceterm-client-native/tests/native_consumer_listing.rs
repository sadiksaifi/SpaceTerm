//! Public inventory reads the real account actor through authenticated private
//! IPC, actor-issued additional-Client receipts and the durable native journal.
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding},
    failure::{ClientFailure, ClientFailureClass},
    journal::{
        ClientJournalChange, JournalController, JournalCredentialSlot, JournalLimits,
        ReceiptVerifier, SavedBinding,
    },
    management_cli::ManagementFailure,
    pairing::credentials::*,
};
use spaceterm_client_native::{
    NativeClientProvider,
    cli::bindings::{InventoryCursor, PairingCommandBindings, PublicPairedDevice},
    journal::NativeClientJournal,
    native::consumer::{NativeConsumer, NativeConsumerKind},
    route::LocalRouteAuthority,
};
use spaceterm_core::{
    authority::*, identity::*, operation_ledger::canonical::CanonicalCodec, pairing::*,
};
use spaceterm_host::{
    account_service::{
        InstallationStatus, RegisteredServer, ServiceSupervisor,
        test_support::AccountServiceFixture,
    },
    ipc::IpcListener,
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits,
        bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
        migrations, schema,
    },
    server_keys::{ServerIdentity, ServerKeyStore},
    shell_launch::ShellLaunchPlanner,
    startup::{BuildIdentity, DesiredServerState, InstallationId},
    storage::Database,
    transport::{AdmissionLimits, MessageLimit, MessageTransport},
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{Established, HandshakeConfig, ServerHandshake, ServerPin, SigningKey},
    codec::Payload,
    convert::organization,
    negotiation::{
        CAPABILITY_CANONICAL_DOMAIN, CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
        CAPABILITY_ORGANIZATION_COMPARISONS,
    },
    pairing_adapter::*,
    wire::{v1, v2},
};
use spaceterm_server::{
    account_startup::{
        AccountOwnerEnrollment, AccountStartup, RecoveredAccountServer,
        test_support::{StartupAbsence, supervisor},
    },
    adapters::{store::production_services, terminal::AccountLaunchFacts},
    api::{ServerHandle, VerifiedPrincipal},
    pairing::{dispatch::PairingOrdinaryDispatch, protocol_lookup::PairingProtocolLookup},
    protocol_dispatch::{ActorOrdinaryDispatch, OrdinaryDispatchPort},
};
use std::{sync::Arc, time::Duration};

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
struct Credentials(Arc<Key>);
impl NativeDeviceCredentials for Credentials {
    fn create(
        &self,
        _: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async { panic!("listing must not create a key") })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            let public_key = match scope.parts().purpose {
                CredentialPurpose::DeviceIdentity => Key::new(31).public_key(),
                CredentialPurpose::PairingAuthentication { .. } => self.0.public_key(),
            };
            Ok(Some(PreparedCredential {
                scope: scope.clone(),
                public_key,
            }))
        })
    }
    fn signing_key<'a>(
        &'a self,
        prepared: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&prepared.scope)?;
            if prepared.public_key == self.0.public_key() {
                Ok(self.0.clone() as Arc<dyn SigningKey>)
            } else if prepared.public_key == Key::new(31).public_key() {
                Ok(Arc::new(Key::new(31)) as Arc<dyn SigningKey>)
            } else {
                Err(PairingFailure::CredentialUnavailable)
            }
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async { panic!("listing must not delete a key") })
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
fn root() -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    let parent = "/private/tmp";
    #[cfg(not(target_os = "macos"))]
    let parent = "/var/tmp";
    tempfile::Builder::new()
        .prefix("st-list-")
        .tempdir_in(parent)
        .unwrap()
}
fn registration(account: &AccountServiceFixture, desired: DesiredServerState) -> RegisteredServer {
    account
        .initialize_with_installation(
            desired,
            Some(InstallationStatus {
                installation: InstallationId(std::num::NonZeroU64::new(9).unwrap()),
                version: spaceterm_distribution::BuildVersion::parse("9.0.0").unwrap(),
                artifact_sha256: [8; 32],
            }),
        )
        .unwrap()
}
async fn journal(directory: std::path::PathBuf) -> Arc<JournalController> {
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
    Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    )
}
async fn existing_journal(directory: std::path::PathBuf) -> Arc<JournalController> {
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
    Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    )
}
fn handshake(major: u32) -> HandshakeConfig {
    HandshakeConfig {
        majors: vec![major],
        capabilities: vec![
            CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
            CAPABILITY_CANONICAL_DOMAIN,
            CAPABILITY_ORGANIZATION_COMPARISONS,
        ],
        ..HandshakeConfig::default()
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    registration: RegisteredServer,
    supervisor: Arc<ServiceSupervisor>,
    server: RecoveredAccountServer,
    signer: Arc<ServerIdentity>,
}
impl Fixture {
    async fn new() -> Self {
        let root = root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registration = registration(&account, DesiredServerState::Running);
        let pin = registration.server_pin();
        let identity = StoreIdentity {
            server: ServerId::new(pin.identity),
            account: registration.key(),
            server_public_key: pin.public_key,
        };
        let home = spaceterm_core::organization::ServerDirectory::new(
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
        let signer = ServerKeyStore::open(&registration)
            .unwrap()
            .load_identity(pin)
            .unwrap();
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
        Self {
            _root: root,
            registration,
            supervisor,
            server,
            signer,
        }
    }
    // Issuance, enrollment and receipt signing run through the production actor.
    async fn enroll(&self, seed: u8, major: u32) -> SignedPairingReceipt {
        let pin = self.registration.server_pin();
        let device = Arc::new(Key::new(seed));
        let authentication = Arc::new(Key::new(seed + 1));
        let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
            server: ServerId::new(pin.identity),
            device: DeviceId::new([seed; 16]),
            claim_nonce: [seed; 32],
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: device.public_key(),
                pairing_authentication_public_key: authentication.public_key(),
            })
            .unwrap(),
        })
        .unwrap();
        let proofs =
            VerifiedDeviceClaim::proofs(&proposal, device.as_ref(), authentication.as_ref())
                .unwrap();
        let claim = VerifiedDeviceClaim::verify(proposal.clone(), &proofs.0, &proofs.1).unwrap();
        let request = self.supervisor.allocate_request().unwrap();
        let digest = AccountOwnerEnrollment::new(self.supervisor.clone())
            .record_authorization(request, &proposal)
            .unwrap();
        let service = self.server.pairing();
        let issued = service
            .issue_owner_authorization(request, digest, claim)
            .await
            .unwrap();
        let PairingOutcome::AuthorizationReserved { ticket, .. } = issued.outcome else {
            panic!("reservation required")
        };
        let snapshot = self.server.handle().pairing_snapshot().unwrap();
        let reservation = snapshot.reservation(&ticket).unwrap();
        let lookup = PairingProtocolLookup::new(self.server.handle());
        let public =
            PublicEnrollmentReservation::from_wire(&reservation_to_wire(reservation)).unwrap();
        let (client, hello) =
            EnrollmentClient::begin(pin, public, vec![major], device, authentication.clone())
                .unwrap();
        let (server, hello) = EnrollmentServer::accept(
            pin.identity,
            vec![major],
            self.signer.clone(),
            &lookup,
            &hello,
        )
        .unwrap();
        let (client, proof) = client.receive_server(&hello).unwrap();
        let (mut server, ready) = server.receive_proof(&lookup, &proof).unwrap();
        let mut client = client.receive_ready(&ready).unwrap();
        let frame = client
            .submission_message(issued.authorization.as_ref().unwrap(), &Key::new(seed))
            .unwrap();
        let response = service.enrollment_frame(&mut server, &frame).await.unwrap();
        let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
            client.decode(&response).unwrap().kind.unwrap()
        else {
            panic!("enrollment result required")
        };
        ReceiptCodec::new(pin)
            .from_wire(result.receipt.as_ref().unwrap())
            .unwrap()
    }
    async fn provision(self, receipt: &SignedPairingReceipt) -> Self {
        let Self {
            _root,
            registration,
            supervisor,
            server,
            signer,
        } = self;
        let directory = _root.path().join("desktop");
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let origin = journal(directory).await;
        let route = LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap();
        save(&origin, registration.server_pin(), receipt.clone(), &route).await;
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let running = tokio::spawn(server.run_until(async {
            let _ = stopped.await;
        }));
        let endpoint = registration.endpoint().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(stream) = tokio::net::UnixStream::connect(endpoint.path()).await {
                    drop(stream);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let provider = NativeClientProvider::from_supervisor((*supervisor).clone());
        let consumer = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
            .with_credentials(Arc::new(Credentials(Arc::new(Key::new(32)))));
        // Production dispatch reserves the real reliable reply slot for the
        // additional-Client operation, then saves its actor-issued receipt.
        consumer.enroll_journal(origin).await.unwrap();
        consumer.shutdown().await.unwrap();
        stop.send(()).unwrap();
        let report = tokio::time::timeout(Duration::from_secs(15), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(report.cleanup_confirmed && report.listeners_closed && !report.recovery_required);
        let server = RecoveredAccountServer::recover(
            supervisor.clone(),
            AccountStartup::admit(&supervisor).unwrap(),
            AccountLaunchFacts {
                uid: registration.key().account_uid,
                home: _root.path().to_owned(),
                planner: ShellLaunchPlanner::for_test(
                    _root.path().join("shell"),
                    _root.path().to_owned(),
                ),
                machine: Default::default(),
            },
        )
        .await
        .unwrap();
        Self {
            _root,
            registration,
            supervisor,
            server,
            signer,
        }
    }
    async fn close(self) {
        let report = tokio::time::timeout(Duration::from_secs(15), self.server.run_until(async {}))
            .await
            .unwrap()
            .unwrap();
        assert!(report.cleanup_confirmed && report.listeners_closed && !report.recovery_required);
    }
}
async fn save(
    journal: &JournalController,
    pin: ServerPin,
    receipt: SignedPairingReceipt,
    route: &LocalRouteAuthority,
) {
    let (PairingReceiptBody::ClientBinding {
        target,
        keys,
        client,
        ..
    }
    | PairingReceiptBody::Enrollment {
        target,
        keys,
        client,
        ..
    }) = &receipt.parts().claims
    else {
        panic!("paired Client required")
    };
    let scope = CredentialScope::new(CredentialScopeParts {
        namespace: CredentialNamespace::new("spaceterm-native-development".into()).unwrap(),
        purpose: CredentialPurpose::PairingAuthentication {
            server: target.parts().server,
        },
        reference: CredentialReference::new([9; 16]),
    })
    .unwrap();
    let generation = LocalPairingGeneration::new(2).unwrap();
    let binding = PairedServerBinding::new(
        ConnectionTarget::new(
            pin,
            target.parts().device,
            target.parts().pairing,
            client.parts().client.id,
            route.description().clone(),
            scope.reference(),
            generation,
        )
        .unwrap(),
    );
    let saved =
        SavedBinding::verified(binding, client.clone(), receipt.clone(), &Verifier).unwrap();
    journal
        .transact(vec![
            ClientJournalChange::ReserveCredential(JournalCredentialSlot {
                namespace: scope.parts().namespace.as_str().to_owned(),
                server: None,
                reference: CredentialReference::new([10; 16]),
                public_key: None,
                route_reference: None,
                route: None,
                ssh_capture: None,
                generation: LocalPairingGeneration::new(1).unwrap(),
            }),
            ClientJournalChange::RecordCredentialPublicKey {
                reference: CredentialReference::new([10; 16]),
                public_key: keys.parts().device_identity_public_key,
            },
            ClientJournalChange::ReserveCredential(JournalCredentialSlot {
                namespace: scope.parts().namespace.as_str().to_owned(),
                server: Some(target.parts().server),
                reference: scope.reference(),
                public_key: None,
                route_reference: None,
                route: None,
                ssh_capture: None,
                generation,
            }),
            ClientJournalChange::RecordCredentialPublicKey {
                reference: scope.reference(),
                public_key: keys.parts().pairing_authentication_public_key,
            },
            ClientJournalChange::BindCredentialRoute {
                reference: scope.reference(),
                route_reference: RevocationRouteReference::new([8; 16]),
                route: route.description().clone(),
            },
            ClientJournalChange::SaveBinding(Box::new(saved)),
        ])
        .await
        .unwrap();
}
async fn serve(
    listener: IpcListener,
    handle: ServerHandle,
    signer: Arc<ServerIdentity>,
    dispatch: PairingOrdinaryDispatch,
    major: u32,
) {
    let pending = listener.accept().await.unwrap();
    let lookup = PairingProtocolLookup::new(handle.clone());
    let hello = pending.recv().await.unwrap().unwrap();
    let (handshake, hello) = ServerHandshake::accept(
        handle.snapshot().state().server.bytes(),
        handle.snapshot().epoch().get(),
        handshake(major),
        signer,
        &lookup,
        &hello,
    )
    .unwrap();
    pending.send(&hello).await.unwrap();
    let proof = pending.recv().await.unwrap().unwrap();
    let (Established::Ordinary(mut codec), ready) =
        handshake.receive_device(&lookup, &proof).unwrap()
    else {
        panic!("ordinary connection required")
    };
    let principal =
        VerifiedPrincipal::from_authenticated(&codec, ConnectionGeneration::new(2)).unwrap();
    handle.register(principal).await.unwrap();
    pending.send(&ready).await.unwrap();
    let transport = pending.finish_authentication().unwrap();
    while let Some(bytes) = transport.recv().await.unwrap() {
        let payload = codec.decode(&bytes).unwrap();
        // Publish the actor's actual empty organization. All reconciliation,
        // acknowledgement and inventory replies use production dispatch.
        macro_rules! snapshot {
            ($p:ident, $v:ident, $query:ident) => {{
                let snapshot = handle.snapshot();
                organization::$p::snapshot_payload(
                    &snapshot,
                    0..0,
                    organization::PublicationPart {
                        request: $query.request,
                        generation: $query.request_generation,
                        transfer: $query.request,
                        index: 0,
                        total: 1,
                        epoch: snapshot.epoch(),
                        base: OrganizationRevision::new(0),
                        revision: snapshot.state().revision,
                    },
                )
                .unwrap()
            }};
        }
        let response = match &payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::OrganizationQuery(query)),
            }) => Some(snapshot!(v1, Major1, query)),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationQuery(query)),
            }) => Some(snapshot!(v2, Major2, query)),
            _ => dispatch
                .dispatch(principal, payload, handle.clone())
                .await
                .unwrap(),
        };
        if let Some(response) = response {
            transport
                .send(&codec.encode(&response).unwrap())
                .await
                .unwrap();
        }
    }
    handle.disconnect(principal).await.unwrap();
    transport.close().await.unwrap();
    listener.close().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_list_reads_server_devices_not_the_local_saved_binding_in_both_majors() {
    for major in [1, 2] {
        let fixture = Fixture::new().await;
        let own = fixture.enroll(31, major).await;
        let other = fixture.enroll(41, major).await;
        let fixture = fixture.provision(&own).await;
        let cli = existing_journal(
            fixture
                .supervisor
                .client_connections_directory()
                .join("cli"),
        )
        .await;
        assert!(cli.snapshot().unwrap().bindings().len() == 1);
        drop(cli);
        let before = fixture
            .server
            .handle()
            .pairing_snapshot()
            .unwrap()
            .recovery()
            .clone();
        let listener = IpcListener::bind(
            fixture.registration.endpoint().unwrap(),
            MessageLimit::envelope(),
            AdmissionLimits::new(4, Duration::from_secs(5)).unwrap(),
        )
        .unwrap();
        let dispatch = PairingOrdinaryDispatch::new(
            fixture.server.pairing().clone(),
            Arc::new(ActorOrdinaryDispatch::new(Arc::new(CanonicalCodec))),
        );
        let served = tokio::spawn(serve(
            listener,
            fixture.server.handle(),
            fixture.signer.clone(),
            dispatch,
            major,
        ));
        let provider = NativeClientProvider::from_supervisor((*fixture.supervisor).clone());
        let consumer = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
            .with_credentials(Arc::new(Credentials(Arc::new(Key::new(32)))));
        let result = consumer.list_page(None).await;
        let malformed = consumer
            .list_page(Some(InventoryCursor::new("invalid".into()).unwrap()))
            .await;
        let after = fixture
            .server
            .handle()
            .pairing_snapshot()
            .unwrap()
            .recovery()
            .clone();
        consumer.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), served)
            .await
            .unwrap()
            .unwrap();
        fixture.close().await;
        let page = result.unwrap();
        let targets = [&own, &other].map(|receipt| {
            let PairingReceiptBody::Enrollment { target, .. } = &receipt.parts().claims else {
                panic!("enrollment required")
            };
            PublicPairedDevice {
                server: target.parts().server,
                device: target.parts().device,
                pairing: target.parts().pairing,
            }
        });
        assert!(page.devices == targets && page.next.is_none());
        assert!(before == after);
        assert!(matches!(malformed, Err(ManagementFailure::Usage)));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_list_reports_offline_for_retained_pairing_without_a_listener_in_both_majors() {
    for major in [1, 2] {
        let fixture = Fixture::new().await;
        let own = fixture.enroll(31, major).await;
        let fixture = fixture.provision(&own).await;
        let journal = existing_journal(
            fixture
                .supervisor
                .client_connections_directory()
                .join("cli"),
        )
        .await;
        let before = fixture
            .server
            .handle()
            .pairing_snapshot()
            .unwrap()
            .recovery()
            .clone();
        let provider = NativeClientProvider::from_supervisor((*fixture.supervisor).clone());
        let consumer = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
            .with_credentials(Arc::new(Credentials(Arc::new(Key::new(32)))));
        let result = consumer.list_page(None).await;
        consumer.shutdown().await.unwrap();
        assert!(
            fixture
                .server
                .handle()
                .pairing_snapshot()
                .unwrap()
                .recovery()
                .clone()
                == before
        );
        assert!(journal.snapshot().unwrap().bindings().len() == 1);
        fixture.close().await;
        assert!(matches!(result, Err(ManagementFailure::Offline)));
    }
}

#[tokio::test]
async fn pair_list_classifies_missing_registration_without_creating_storage() {
    let root = root();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let provider = NativeClientProvider::from_supervisor(account.reopen());
    let consumer = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli);
    assert!(matches!(
        consumer.list_page(None).await,
        Err(ManagementFailure::NativeUnavailable)
    ));
    assert!(
        !provider
            .management()
            .supervisor()
            .client_connections_directory()
            .exists()
    );
}

#[tokio::test]
async fn pair_list_classifies_empty_native_journal_as_pairing_required() {
    let root = root();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    registration(&account, DesiredServerState::Running);
    let supervisor = account.reopen();
    let admission = supervisor.admit_client_provisioning().unwrap();
    let (directory, create) = admission.journal_directory("cli").unwrap();
    assert!(create);
    let empty = journal(directory).await;
    drop(admission);
    drop(empty);
    let provider = NativeClientProvider::from_supervisor(supervisor);
    let consumer = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli);
    assert!(matches!(
        consumer.list_page(None).await,
        Err(ManagementFailure::PairingRequired)
    ));
}

#[tokio::test]
async fn pair_list_classifies_stopped_server_without_starting_or_pairing() {
    let root = root();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    registration(&account, DesiredServerState::Stopped);
    let provider = NativeClientProvider::from_supervisor(account.reopen());
    let consumer = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli);
    assert!(matches!(
        consumer.list_page(None).await,
        Err(ManagementFailure::NativeUnavailable)
    ));
    assert!(
        provider
            .management()
            .supervisor()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state()
            == DesiredServerState::Stopped
    );
    assert!(
        !provider
            .management()
            .supervisor()
            .client_connections_directory()
            .exists()
    );
}
