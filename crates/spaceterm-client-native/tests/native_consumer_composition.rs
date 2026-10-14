//! Real durable journals, signed additional-Client receipts, private IPC and both
//! authenticated protocol majors. Only the native credential boundary is injected.
use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding},
    failure::{ClientFailure, ClientFailureClass},
    journal::{
        ClientJournalChange, JournalController, JournalCredentialSlot, JournalLimits,
        ReceiptVerifier, SavedBinding,
    },
    management_cli::{ProtocolManagement, StatusManagement},
    pairing::credentials::*,
};
use spaceterm_client_native::{
    NativeClientProvider,
    journal::NativeClientJournal,
    native::consumer::{NativeConsumer, NativeConsumerKind},
    route::{LocalRouteAuthority, NativeRouteConnector},
};
use spaceterm_core::{
    authority::*, identity::*, management::*, operation_ledger::canonical::CanonicalCodec,
    operations::*, organization::*, organization_retained::*, pairing::*,
    remote_access::RemoteAccessPolicy,
};
use spaceterm_host::{
    account_service::test_support::AccountServiceFixture,
    ipc::IpcListener,
    server_keys::ServerKeyStore,
    startup::{BuildIdentity, DesiredServerState},
    transport::{AdmissionLimits, MessageLimit, MessageTransport},
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{
        Established, HandshakeConfig, PairingLookup, PairingRecord, Principal, ServerHandshake,
        ServerPin, SigningKey,
    },
    codec::Payload,
    convert::{management, organization},
    negotiation::{
        CAPABILITY_CANONICAL_DOMAIN, CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
        CAPABILITY_ORGANIZATION_COMPARISONS,
    },
    pairing_adapter::ReceiptCodec,
    wire::{v1, v2},
};
use std::{num::NonZeroU64, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};

struct Key(Ed25519KeyPair);
impl SigningKey for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, value: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(value).as_ref().try_into().unwrap())
    }
}
struct Credentials(Arc<Key>);
impl NativeDeviceCredentials for Credentials {
    fn create(
        &self,
        _: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async { panic!("restoration must not create credentials") })
    }
    fn recover_preparation<'a>(
        &'a self,
        _: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async { panic!("restoration must use retained preparation") })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            assert_eq!(credential.public_key, self.0.public_key());
            Ok(self.0.clone() as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async { panic!("status must not delete credentials") })
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
struct Lookup(PairingRecord);
impl PairingLookup for Lookup {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        assert_eq!(
            (server, pairing, client),
            (
                self.0.principal.server,
                self.0.principal.pairing,
                self.0.principal.client
            )
        );
        Ok(self.0)
    }
}
fn local(value: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&value.to_be_bytes());
    bytes
}
fn snapshot(server: ServerId) -> OrganizationSnapshot {
    OrganizationSnapshot::new(
        ServerEpoch::new(1),
        OrganizationState {
            server,
            revision: OrganizationRevision::new(1),
            settings: ServerSettingsRecord {
                worktree_location: Versioned {
                    version: FieldVersion::new(1),
                    value: WorktreeLocationTemplate::new("{branch}".into()).unwrap(),
                },
            },
            workspaces: vec![],
            tabs: vec![],
            panes: vec![],
            repositories: vec![],
            worktrees: vec![],
            retained: Some(
                OrganizationRetainedFacts::new(OrganizationRetainedParts {
                    server,
                    workspace_names: vec![],
                    creation_defaults: vec![],
                    pane_generations: vec![],
                    tab_membership_versions: vec![],
                    repository_inventory_versions: vec![],
                    worktree_inventory: vec![],
                    remote_access: Versioned {
                        version: FieldVersion::new(1),
                        value: RemoteAccessPolicy::disabled(server),
                    },
                })
                .unwrap(),
            ),
        },
        vec![],
    )
    .unwrap()
}
fn status(server: ServerId, revision: u64) -> ServerStatus {
    ServerStatus::new(ServerStatusParts {
        runtime: ServerRuntimeStatus::default(),
        server,
        epoch: ServerEpoch::new(1),
        revision: NonZeroU64::new(revision).unwrap(),
        running: None,
        approved: None,
        staged: None,
        native_verification: NativeVerification::Unverified,
        capabilities: vec![],
        remote_access: Versioned {
            version: FieldVersion::new(1),
            value: RemoteAccessPolicy::disabled(server),
        },
        listeners: vec![],
        progress: vec![],
    })
    .unwrap()
}
async fn controller(directory: &std::path::Path, create: bool) -> Arc<JournalController> {
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(Verifier);
    let storage = if create {
        NativeClientJournal::create(
            directory.to_owned(),
            codec.clone(),
            verifier.clone(),
            JournalLimits::default(),
        )
        .await
    } else {
        // A cancelled worker may leave its already-started blocking commit
        // draining. A process restart releases that handle; this in-process
        // fixture waits for the same release without treating locked as missing.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match NativeClientJournal::open_existing(
                    directory.to_owned(),
                    codec.clone(),
                    verifier.clone(),
                    JournalLimits::default(),
                )
                .await
                {
                    Ok(storage) => break Ok(storage),
                    Err(failure) if failure.class() == ClientFailureClass::StorageUnavailable => {
                        tokio::time::sleep(Duration::from_millis(10)).await
                    }
                    Err(failure) => break Err(failure),
                }
            }
        })
        .await
        .unwrap()
    }
    .unwrap();
    Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    )
}

struct Exercise {
    kind: NativeConsumerKind,
    major: u32,
    self_remove: bool,
    offline: Option<DesiredServerState>,
    operation_count: usize,
    restart: bool,
    processes: bool,
    recover_pairing: bool,
    missing_runtime: bool,
    confirm_obsolete_token: bool,
    publish_delta: bool,
}
async fn exercise(options: Exercise) {
    let Exercise {
        kind,
        major,
        self_remove,
        offline,
        operation_count,
        restart,
        processes,
        recover_pairing,
        missing_runtime,
        confirm_obsolete_token,
        publish_delta,
    } = options;
    use std::os::unix::fs::DirBuilderExt;
    let root = tempfile::Builder::new()
        .prefix("st-consumer-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = account
        .initialize(offline.unwrap_or(DesiredServerState::Running))
        .unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let signer = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    let device = Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[27; 32]).unwrap()));
    let client = ClientId::new(
        server,
        if kind == NativeConsumerKind::Cli {
            7
        } else {
            8
        },
    )
    .unwrap();
    let pairing = PairingId::new(server, 2).unwrap();
    let streams = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: client,
            pairing,
        },
        ordinary: OperationStreamId::new(
            OperationStreamKey::new(server, 11).unwrap(),
            pairing,
            client,
        )
        .unwrap(),
        self_revocation: OperationStreamId::new(
            OperationStreamKey::new(server, 12).unwrap(),
            pairing,
            client,
        )
        .unwrap(),
    })
    .unwrap();
    let operation = OperationBinding {
        id: OperationId::new(
            OperationStreamId::new(
                OperationStreamKey::new(server, 13).unwrap(),
                pairing,
                client,
            )
            .unwrap(),
            OperationSequence::new(1).unwrap(),
        ),
        digest: RequestDigest::from_bytes([9; 32]),
    };
    let route = LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap();
    let credential = PreparedCredential {
        scope: CredentialScope::new(CredentialScopeParts {
            namespace: CredentialNamespace::new("dev.spaceterm.consumer-test".into()).unwrap(),
            purpose: CredentialPurpose::PairingAuthentication { server },
            reference: CredentialReference::new([8; 16]),
        })
        .unwrap(),
        public_key: device.public_key(),
    };
    let binding = PairedServerBinding::new(
        ConnectionTarget::new(
            registration.server_pin(),
            DeviceId::new([6; 16]),
            pairing,
            client,
            route.description().clone(),
            credential.scope.reference(),
            LocalPairingGeneration::new(1).unwrap(),
        )
        .unwrap(),
    );
    let proof = ReceiptCodec::new(registration.server_pin())
        .sign(
            PairingReceiptBody::ClientBinding {
                target: PairingTarget::new(PairingTargetParts {
                    server,
                    device: binding.target().device(),
                    pairing,
                })
                .unwrap(),
                keys: PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: Key(
                        Ed25519KeyPair::from_seed_unchecked(&[28; 32]).unwrap()
                    )
                    .public_key(),
                    pairing_authentication_public_key: device.public_key(),
                })
                .unwrap(),
                client: streams.clone(),
                operation,
                revision: StoreRevision::new(1),
            },
            signer.as_ref(),
        )
        .unwrap();
    let saved = SavedBinding::verified(binding.clone(), streams, proof, &Verifier).unwrap();
    let journal_root = if processes && offline.is_none() {
        root.path().canonicalize().unwrap()
    } else {
        account.reopen().client_connections_directory()
    };
    let directory = journal_root.join(if kind == NativeConsumerKind::Cli {
        "cli"
    } else {
        "companion"
    });
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let journal = controller(&directory, true).await;
    journal
        .transact(vec![
            ClientJournalChange::ReserveCredential(JournalCredentialSlot {
                namespace: credential.scope.parts().namespace.as_str().to_owned(),
                server: Some(server),
                reference: credential.scope.reference(),
                public_key: None,
                route_reference: None,
                route: None,
                ssh_capture: None,
                generation: LocalPairingGeneration::new(1).unwrap(),
            }),
            ClientJournalChange::RecordCredentialPublicKey {
                reference: credential.scope.reference(),
                public_key: device.public_key(),
            },
            ClientJournalChange::BindCredentialRoute {
                reference: credential.scope.reference(),
                route_reference: RevocationRouteReference::new([4; 16]),
                route: binding.target().route().clone(),
            },
            ClientJournalChange::SaveBinding(Box::new(saved)),
        ])
        .await
        .unwrap();
    drop(journal);
    // The connection uses a physically reopened canonical journal, not a fixture
    // identity or an already authenticated desktop connection.
    let journal = controller(&directory, false).await;
    if offline.is_some() && processes {
        drop(journal);
        let assessed = run_offline_cli_process(root.path(), None).await;
        assert_eq!(assessed["exit"], 0);
        let token = assessed["output"]["result"]["token"]
            .as_str()
            .unwrap()
            .to_owned();
        let obsolete_token = if confirm_obsolete_token {
            let second = run_offline_cli_process(root.path(), None).await;
            assert_eq!(second["exit"], 0);
            let second_token = second["output"]["result"]["token"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_ne!(token, second_token);
            Some(second_token)
        } else {
            None
        };
        let other = AccountServiceFixture::new(root.path(), BuildIdentity::Preflight).unwrap();
        let other_registration = other.initialize(DesiredServerState::Stopped).unwrap();
        let other_path = other
            .reopen()
            .client_connections_directory()
            .parent()
            .unwrap()
            .join("registration");
        let other_bytes = std::fs::read(&other_path).unwrap();
        if recover_pairing {
            let registration_path = account
                .reopen()
                .client_connections_directory()
                .parent()
                .unwrap()
                .join("registration");
            std::fs::remove_file(registration_path).unwrap();
        }
        let removed_runtime = if missing_runtime {
            let endpoint = registration.endpoint().unwrap();
            let runtime = endpoint.path().parent().unwrap().to_owned();
            std::fs::remove_dir_all(&runtime).unwrap();
            assert_eq!(
                account.reopen().discover().unwrap().unwrap().server_pin(),
                registration.server_pin()
            );
            let provider = NativeClientProvider::from_supervisor(account.reopen());
            let consumer = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
                .with_credentials(Arc::new(Credentials(device.clone())));
            assert_eq!(
                ProtocolManagement::status(&consumer).await,
                Err(spaceterm_client::management_cli::ManagementFailure::NativeUnavailable)
            );
            consumer.shutdown().await.unwrap();
            Some(runtime)
        } else {
            None
        };
        if restart {
            let journal = controller(&directory, false).await;
            journal
                .transact(vec![ClientJournalChange::ReserveCredential(
                    JournalCredentialSlot {
                        namespace: "dev.spaceterm.other-server".to_owned(),
                        server: Some(ServerId::new([42; 16])),
                        reference: CredentialReference::new([42; 16]),
                        public_key: None,
                        route_reference: None,
                        route: None,
                        ssh_capture: None,
                        generation: LocalPairingGeneration::new(2).unwrap(),
                    },
                )])
                .await
                .unwrap();
        }
        let confirmed = run_offline_cli_process(root.path(), Some(token)).await;
        let mut journal = controller(&directory, false).await;
        if restart {
            assert_eq!(
                confirmed["exit"],
                spaceterm_client::management_cli::ManagementFailure::StaleAssessment.exit_code()
            );
            assert_eq!(confirmed["output"]["error"], "stale_assessment");
            assert_eq!(journal.snapshot().unwrap().bindings().len(), 1);
            assert!(journal.snapshot().unwrap().revocations().is_empty());
        } else {
            assert_eq!(confirmed["exit"], 75);
            assert_eq!(
                confirmed["output"]["result"]["local_connection_removed"],
                true
            );
            assert_eq!(confirmed["output"]["result"]["revocation"], "pending");
            assert!(journal.snapshot().unwrap().bindings().is_empty());
            assert_eq!(journal.snapshot().unwrap().revocations().len(), 1);
            if let Some(token) = obsolete_token {
                let original = journal.snapshot().unwrap().revocations()[0].task.clone();
                drop(journal);
                let stale = run_offline_cli_process(root.path(), Some(token)).await;
                assert_eq!(
                    stale["exit"],
                    spaceterm_client::management_cli::ManagementFailure::StaleAssessment
                        .exit_code()
                );
                assert_eq!(stale["output"]["error"], "stale_assessment");
                let fresh = run_offline_cli_process(root.path(), None).await;
                assert_eq!(
                    fresh["exit"],
                    spaceterm_client::management_cli::ManagementFailure::PairingRequired
                        .exit_code()
                );
                assert_eq!(fresh["output"]["error"], "pairing_required");
                journal = controller(&directory, false).await;
                let snapshot = journal.snapshot().unwrap();
                assert!(snapshot.bindings().is_empty());
                assert_eq!(snapshot.revocations().len(), 1);
                assert_eq!(snapshot.revocations()[0].task, original);
            }
        }
        if let Some(runtime) = removed_runtime {
            use spaceterm_client_native::cli::bindings::{PairingCommandBindings, RevocationState};
            let original = journal.snapshot().unwrap().revocations()[0]
                .task
                .parts()
                .original
                .operation();
            drop(journal);
            let provider = NativeClientProvider::from_supervisor(account.reopen());
            let retry = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
                .with_credentials(Arc::new(Credentials(device)));
            let reports = retry.retry_pending().await.unwrap();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].revocation, RevocationState::Pending);
            let inventory = retry
                .pairing_runtime()
                .await
                .unwrap()
                .inventory()
                .await
                .unwrap();
            assert!(inventory.saved.is_empty());
            assert_eq!(inventory.revocations.len(), 1);
            assert_eq!(
                inventory.revocations[0].task.parts().original.operation(),
                original
            );
            assert!(
                !runtime.exists(),
                "restoration must not recreate runtime storage"
            );
            retry.shutdown().await.unwrap();
        }
        assert_eq!(std::fs::read(&other_path).unwrap(), other_bytes);
        assert_eq!(
            other.reopen().discover().unwrap().unwrap().server_pin(),
            other_registration.server_pin()
        );
        return;
    }
    if offline.is_some() {
        use spaceterm_client_native::cli::bindings::{PairingCommandBindings, RevocationState};
        let provider = NativeClientProvider::from_supervisor(account.reopen());
        let consumer = NativeConsumer::with_recovered_journal(
            provider.management(),
            kind,
            journal.clone(),
            Arc::new(Credentials(device.clone())),
            NativeRouteConnector::new(
                vec![LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap()],
                None,
            )
            .unwrap(),
        );
        if offline == Some(DesiredServerState::Running) {
            // An unreachable account server may also lose its registration. Local
            // pairing removal still uses only the retained journal's authority.
            let registration_path = account
                .reopen()
                .client_connections_directory()
                .parent()
                .unwrap()
                .join("registration");
            std::fs::remove_file(registration_path).unwrap();
            assert!(account.reopen().discover().unwrap().is_none());
        }
        let assessment = consumer
            .assess_unpair(DeviceId::new([6; 16]), None)
            .await
            .unwrap();
        let token = assessment.token().clone();
        consumer.shutdown().await.unwrap();
        drop(journal);
        let journal = controller(&directory, false).await;
        let consumer = NativeConsumer::with_recovered_journal(
            provider.management(),
            kind,
            journal.clone(),
            Arc::new(Credentials(device)),
            NativeRouteConnector::new(vec![route], None).unwrap(),
        );
        assert_eq!(
            consumer
                .assess_unpair(DeviceId::new([6; 16]), Some(token.clone()))
                .await
                .unwrap(),
            assessment,
            "offline local confirmation retains its original scope across restart",
        );
        if restart {
            journal
                .transact(vec![ClientJournalChange::ReserveCredential(
                    JournalCredentialSlot {
                        namespace: "dev.spaceterm.other-server".to_owned(),
                        server: Some(ServerId::new([42; 16])),
                        reference: CredentialReference::new([42; 16]),
                        public_key: None,
                        route_reference: None,
                        route: None,
                        ssh_capture: None,
                        generation: LocalPairingGeneration::new(2).unwrap(),
                    },
                )])
                .await
                .unwrap();
            assert_eq!(
                consumer
                    .assess_unpair(DeviceId::new([6; 16]), Some(token))
                    .await,
                Err(spaceterm_client::management_cli::ManagementFailure::StaleAssessment),
                "offline confirmation must retain the original journal revision",
            );
            assert_eq!(
                consumer
                    .pairing_runtime()
                    .await
                    .unwrap()
                    .inventory()
                    .await
                    .unwrap()
                    .saved
                    .len(),
                1
            );
            consumer.shutdown().await.unwrap();
            return;
        }
        let confirmed = spaceterm_client::management_cli::confirm_exact(
            assessment,
            spaceterm_client::management_cli::ConfirmationReply::new(token),
        )
        .unwrap();
        let report = consumer.unpair(confirmed).await.unwrap();
        assert!(report.local_connection_removed);
        assert_eq!(report.revocation, RevocationState::Pending);
        let runtime = consumer.pairing_runtime().await.unwrap();
        assert!(runtime.inventory().await.unwrap().saved.is_empty());
        assert_eq!(runtime.inventory().await.unwrap().revocations.len(), 1);
        consumer.shutdown().await.unwrap();
        drop(runtime);
        drop(journal);
        let reopened = controller(&directory, false).await;
        assert!(reopened.snapshot().unwrap().bindings().is_empty());
        return;
    }
    let lookup = Lookup(PairingRecord {
        principal: Principal {
            server: server.bytes(),
            device: [6; 16],
            pairing: local(2),
            client: local(client.local()),
        },
        device_public_key: device.public_key(),
        active: true,
    });
    let listener = IpcListener::bind(
        registration.endpoint().unwrap(),
        MessageLimit::envelope(),
        AdmissionLimits::new(4, Duration::from_secs(5)).unwrap(),
    )
    .unwrap();
    let lost_pairing_operation = Arc::new(std::sync::Mutex::new(None));
    let original_pairing_operation = lost_pairing_operation.clone();
    let served = tokio::spawn(async move {
        let mut queries = 0;
        let mut writes = 0;
        for _ in 0..if operation_count > 128 || restart || processes {
            2
        } else {
            1
        } {
            let pending = listener.accept().await.unwrap();
            let hello = pending.recv().await.unwrap().unwrap();
            let (handshake, reply) = ServerHandshake::accept(
                server.bytes(),
                1,
                HandshakeConfig {
                    majors: vec![major],
                    capabilities: vec![
                        CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
                        CAPABILITY_CANONICAL_DOMAIN,
                        CAPABILITY_ORGANIZATION_COMPARISONS,
                    ],
                    ..HandshakeConfig::default()
                },
                signer.clone(),
                &lookup,
                &hello,
            )
            .unwrap();
            pending.send(&reply).await.unwrap();
            let proof = pending.recv().await.unwrap().unwrap();
            let (Established::Ordinary(mut codec), ready) =
                handshake.receive_device(&lookup, &proof).unwrap()
            else {
                panic!("ordinary connection required")
            };
            pending.send(&ready).await.unwrap();
            let transport = pending.finish_authentication().unwrap();
            let mut retained_pairing = None;
            let mut delta_sent = false;
            while let Some(bytes) = transport.recv().await.unwrap() {
                let payload = codec.decode(&bytes).unwrap();
                macro_rules! respond {($m:expr,$p:ident,$v:ident)=>{match $m.kind{
                Some($p::message::Kind::OrganizationQuery(query))=>organization::$p::snapshot_payload(&snapshot(server),0..0,organization::PublicationPart{request:query.request,generation:query.request_generation,transfer:query.request,index:0,total:1,epoch:ServerEpoch::new(1),base:OrganizationRevision::new(0),revision:OrganizationRevision::new(1)}).unwrap(),
                Some($p::message::Kind::Reconciliation(mut query))=>{query.acknowledged=true;query.connection_generation=1;Payload::$v($p::Message{kind:Some($p::message::Kind::Reconciliation(query))})},
                Some($p::message::Kind::StatusQuery(query))=>{queries+=1;let query=management::$p::decode_query(&query,ConnectionBinding{client,generation:ConnectionGeneration::new(1)}).unwrap();Payload::$v($p::Message{kind:Some($p::message::Kind::StatusPublication(management::$p::encode_publication(&StatusPublication::Snapshot{request:query.request,generation:query.generation,status:status(server,queries)})))})},
                Some($p::message::Kind::Operation(value))=>{assert_eq!(kind,NativeConsumerKind::Cli);let request=if let Some($p::operation::Action::IdentifiedPairing(inner))=&value.action {let Some($p::pairing_request::Action::AssessUnpair(action))=&inner.action else{panic!("unexpected pairing command")};let id=spaceterm_protocol::pairing_adapter::operation_from_wire(inner.operation.as_ref().unwrap()).unwrap();spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(id,Command::Pairing(PairingCommand::AssessUnpair{target:spaceterm_protocol::pairing_adapter::target_from_wire(action.target.as_ref().unwrap()).unwrap()}),&CanonicalCodec).unwrap()}else{spaceterm_protocol::convert::operations::$p::decode_request(&value,pairing,client,&CanonicalCodec).unwrap()};let outcome=match request.command(){Command::SetRemoteAccess{expected_version,value,..}=>{assert_eq!(expected_version.get(),1);assert_eq!(value.endpoints(),&["127.0.0.1:443".parse().unwrap()]);writes+=1;Outcome::Applied},Command::Pairing(PairingCommand::AssessUnpair{target})=>Outcome::Pairing(Box::new(PairingOutcome::UnpairAssessed(spaceterm_core::pairing::UnpairAssessment::new(UnpairAssessmentParts{reference:AssessmentRef{operation:request.binding().id,digest:request.binding().digest},target:target.clone(),authorization_version:FieldVersion::new(1),clients:vec![ClientRecord{id:client,pairing}]}).unwrap()))),_=>panic!("unexpected management mutation")};Payload::$v($p::Message{kind:Some($p::message::Kind::Outcome(if let Outcome::Pairing(outcome)=&outcome{let PairingOutcome::UnpairAssessed(value)=outcome.as_ref() else{panic!("unpair assessment required")};let parts=value.parts();$p::Outcome{operation:Some(spaceterm_protocol::pairing_adapter::operation_to_wire(request.binding().id)),publication_revision:2,result:Some($p::outcome::Result::PairingResult(Box::new($p::PairingOutcome{operation:Some(Box::new(spaceterm_protocol::pairing_adapter::operation_to_wire(request.binding().id))),disposition:4,unpair_assessment:Some(Box::new(spaceterm_protocol::wire::pairing::UnpairAssessment{assessment_operation:Some(spaceterm_protocol::pairing_adapter::operation_to_wire(parts.reference.operation)),assessment_digest:parts.reference.digest.bytes().to_vec(),target:Some(spaceterm_protocol::pairing_adapter::target_to_wire(&parts.target)),authorization_version:parts.authorization_version.get(),clients:parts.clients.iter().map(|client|spaceterm_protocol::convert::identity::encode_client(client.id)).collect()})),..Default::default()})))}}else{spaceterm_protocol::convert::operations::$p::encode_outcome(request.binding().id,OrganizationRevision::new(2),&outcome).unwrap()}))})},
                Some($p::message::Kind::OperationLookupRequest(value))=>{
                    let Some(Payload::$v($p::Message{kind:Some($p::message::Kind::Outcome(outcome))})) = &retained_pairing else {panic!("lost pairing outcome required")};
                    assert_eq!(value.operation,outcome.operation);
                    let Some($p::outcome::Result::PairingResult(pairing_result))=&outcome.result else {panic!("pairing result required")};
                    assert_eq!(value.request_digest,pairing_result.unpair_assessment.as_ref().unwrap().assessment_digest);
                    Payload::$v($p::Message{kind:Some($p::message::Kind::OperationLookupResult($p::OperationLookupResult{operation:value.operation,request_digest:value.request_digest,disposition:Some($p::operation_lookup_result::Disposition::Completed(Box::new(outcome.clone())))}))})
                },
                Some($p::message::Kind::Acknowledgement(value))=>{let(stream,through)=spaceterm_protocol::convert::operations::$p::decode_acknowledgement(&value,pairing,client).unwrap();Payload::$v($p::Message{kind:Some($p::message::Kind::AcknowledgementConfirmation($p::AcknowledgementConfirmation{stream:Some(spaceterm_protocol::convert::identity::encode_stream(stream)),acknowledged_through:through.get()}))})},
                _=>panic!("unexpected consumer request"),
            }};}
                let response = match payload {
                    Payload::Major1(m) => respond!(m, v1, Major1),
                    Payload::Major2(m) => respond!(m, v2, Major2),
                };
                if publish_delta
                    && !delta_sent
                    && matches!(
                        &response,
                        Payload::Major1(v1::Message {
                            kind: Some(v1::message::Kind::StatusPublication(_))
                        }) | Payload::Major2(v2::Message {
                            kind: Some(v2::message::Kind::StatusPublication(_))
                        })
                    )
                {
                    // Deliver a contiguous multipart delta before the pending query
                    // reply. The same authenticated worker must remain usable.
                    for index in 0..2 {
                        macro_rules! delta {
                            ($p:ident, $v:ident) => {
                                Payload::$v($p::Message { kind: Some($p::message::Kind::OrganizationDelta($p::OrganizationDelta {
                                    server: Some(spaceterm_protocol::convert::identity::encode_global(server.bytes())),
                                    epoch: 1, base_revision: 1, revision: 2, complete: index == 1,
                                    part: Some(organization::$p::encode_part(organization::PublicationPart {
                                        request: 1, generation: 1, transfer: 9, index, total: 2,
                                        epoch: ServerEpoch::new(1), base: OrganizationRevision::new(1), revision: OrganizationRevision::new(2),
                                    })), ..Default::default()
                                })) })
                            };
                        }
                        let publication = match major {
                            1 => delta!(v1, Major1),
                            2 => delta!(v2, Major2),
                            _ => unreachable!(),
                        };
                        transport
                            .send(&codec.encode(&publication).unwrap())
                            .await
                            .unwrap();
                    }
                    delta_sent = true;
                }
                if recover_pairing {
                    let operation = match &response {
                        Payload::Major1(v1::Message {
                            kind: Some(v1::message::Kind::Outcome(value)),
                        }) if matches!(
                            value.result,
                            Some(v1::outcome::Result::PairingResult(_))
                        ) =>
                        {
                            Some(
                                spaceterm_protocol::convert::identity::decode_operation(
                                    value.operation.as_ref().unwrap(),
                                )
                                .unwrap(),
                            )
                        }
                        Payload::Major2(v2::Message {
                            kind: Some(v2::message::Kind::Outcome(value)),
                        }) if matches!(
                            value.result,
                            Some(v2::outcome::Result::PairingResult(_))
                        ) =>
                        {
                            Some(
                                spaceterm_protocol::convert::identity::decode_operation(
                                    value.operation.as_ref().unwrap(),
                                )
                                .unwrap(),
                            )
                        }
                        _ => None,
                    };
                    if let Some(operation) = operation {
                        assert!(
                            retained_pairing.is_none(),
                            "the client must not submit a replacement assessment"
                        );
                        *original_pairing_operation.lock().unwrap() = Some(operation);
                        retained_pairing = Some(response);
                        continue;
                    }
                }
                if transport
                    .send(&codec.encode(&response).unwrap())
                    .await
                    .is_err()
                {
                    assert!(
                        matches!(
                            response,
                            Payload::Major1(v1::Message {
                                kind: Some(v1::message::Kind::AcknowledgementConfirmation(_))
                            }) | Payload::Major2(v2::Message {
                                kind: Some(v2::message::Kind::AcknowledgementConfirmation(_))
                            })
                        ),
                        "only a one-shot client's final acknowledgement may race its disconnect"
                    );
                    break;
                }
            }
        }
        listener.close().unwrap();
        (queries, writes)
    });
    if processes {
        drop(journal);
        run_cli_process(root.path(), None).await;
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("cli-result.json")).unwrap())
                .unwrap();
        let token = result["result"]["token"].as_str().unwrap().to_owned();
        run_cli_process(root.path(), Some(token)).await;
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("cli-result.json")).unwrap())
                .unwrap();
        assert_eq!(result["result"]["state"], "Completed");
        assert_eq!(served.await.unwrap(), (3, 1));
        return;
    }
    let provider = NativeClientProvider::from_supervisor(account.reopen());
    // The registered runtime lives outside the fixed authority directory. Fresh
    // CLI and companion construction must restore its locally approved endpoint.
    let mut consumer = NativeConsumer::new(provider.management(), kind)
        .with_credentials(Arc::new(Credentials(device.clone())));
    let mut original_result = None;
    if kind == NativeConsumerKind::Cli {
        // Native lifecycle comparisons must reuse the saved CLI principal and
        // its canonical journal, not an unprovisioned management identity.
        let runtime = spaceterm_client_native::native::runtime::RuntimeClientIdentity::access(
            &consumer,
            &registration,
        )
        .await
        .unwrap();
        assert_eq!(runtime.binding, binding);
        assert_eq!(runtime.signer.public_key(), device.public_key());
        assert!(
            runtime
                .journal
                .ordinary_use_allowed(&binding)
                .await
                .unwrap()
        );
        assert_eq!(
            ProtocolManagement::status(&consumer)
                .await
                .unwrap()
                .parts()
                .server,
            server
        );
        assert_eq!(consumer.negotiated_major().unwrap(), major);
        let assessment = consumer
            .assess(
                spaceterm_client::management_cli::ManagementAction::RemoteAccess {
                    policy: RemoteAccessPolicy::enabled(
                        server,
                        vec!["127.0.0.1:443".parse().unwrap()],
                    )
                    .unwrap(),
                },
            )
            .await
            .unwrap();
        let assessment = if restart {
            let action = assessment.action().clone();
            let token = assessment.token().clone();
            consumer.shutdown().await.unwrap();
            drop(journal);
            consumer = NativeConsumer::restore(
                provider.management(),
                kind,
                controller(&directory, false).await,
                Arc::new(Credentials(device.clone())),
                NativeRouteConnector::new(
                    vec![LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap()],
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(
                consumer
                    .restore_assessment(
                        spaceterm_client::management_cli::ManagementAction::RemoteAccess {
                            policy: RemoteAccessPolicy::disabled(server)
                        },
                        token.clone(),
                    )
                    .await,
                Err(spaceterm_client::management_cli::ManagementFailure::StaleAssessment)
            );
            let restored = consumer.restore_assessment(action, token).await.unwrap();
            assert_eq!(
                restored, assessment,
                "restoration must preserve the original scope and expiry"
            );
            restored
        } else {
            assessment
        };
        let duplicate = assessment.clone();
        let token = assessment.token().clone();
        let confirmed = spaceterm_client::management_cli::confirm_exact(
            assessment,
            spaceterm_client::management_cli::ConfirmationReply::new(token),
        )
        .unwrap();
        let progress = consumer.execute(confirmed).await.unwrap();
        assert_eq!(progress.state, ManagementProgressState::Completed);
        original_result = Some(progress);
        let repeated = spaceterm_client::management_cli::confirm_exact(
            duplicate.clone(),
            spaceterm_client::management_cli::ConfirmationReply::new(duplicate.token().clone()),
        )
        .unwrap();
        assert_eq!(
            consumer.execute(repeated).await,
            Err(spaceterm_client::management_cli::ManagementFailure::StaleAssessment)
        );
        assert_eq!(
            progress.request.operation_id().unwrap().stream().client(),
            client
        );
        for completed in 1..operation_count {
            if operation_count > 128 && completed == 64 {
                consumer.shutdown().await.unwrap();
                consumer = NativeConsumer::restore(
                    provider.management(),
                    kind,
                    controller(&directory, false).await,
                    Arc::new(Credentials(device.clone())),
                    NativeRouteConnector::new(
                        vec![LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap()],
                        None,
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            }
            let assessment = consumer
                .assess(
                    spaceterm_client::management_cli::ManagementAction::RemoteAccess {
                        policy: RemoteAccessPolicy::enabled(
                            server,
                            vec!["127.0.0.1:443".parse().unwrap()],
                        )
                        .unwrap(),
                    },
                )
                .await
                .unwrap();
            let token = assessment.token().clone();
            let confirmed = spaceterm_client::management_cli::confirm_exact(
                assessment,
                spaceterm_client::management_cli::ConfirmationReply::new(token),
            )
            .unwrap();
            assert_eq!(
                consumer.execute(confirmed).await.unwrap().state,
                ManagementProgressState::Completed
            );
        }
        if operation_count > 128 {
            let runtime = consumer.pairing_runtime().await.unwrap();
            let state = runtime.journal().snapshot().unwrap();
            assert!(state.operations().len() < 128);
        }
        if recover_pairing {
            assert_eq!(
                consumer
                    .assess(spaceterm_client::management_cli::ManagementAction::Unpair {
                        target: PairingTarget::new(PairingTargetParts {
                            server,
                            device: DeviceId::new([6; 16]),
                            pairing,
                        })
                        .unwrap(),
                    })
                    .await,
                Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
            );
            let operation = lost_pairing_operation.lock().unwrap().unwrap();
            let recovered = consumer.resolve(operation).await.unwrap();
            assert_eq!(recovered.request.operation_id(), Some(operation));
            assert_eq!(recovered.state, ManagementProgressState::Completed);
        }
        if self_remove {
            let runtime = consumer.pairing_runtime().await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let state = runtime.journal().snapshot().unwrap();
                    let header = state
                        .stream(progress.request.operation_id().unwrap().stream())
                        .unwrap();
                    if header.confirmed_prefix == header.durable_prefix {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            use spaceterm_client_native::cli::bindings::{PairingCommandBindings, RevocationState};
            let result = consumer.assess_unpair(DeviceId::new([6; 16]), None).await;
            let assessment = result.unwrap();
            let runtime = consumer.pairing_runtime().await.unwrap();
            let token = assessment.token().clone();
            let confirmed = spaceterm_client::management_cli::confirm_exact(
                assessment,
                spaceterm_client::management_cli::ConfirmationReply::new(token),
            )
            .unwrap();
            let report = consumer.unpair(confirmed).await.unwrap();
            assert!(report.local_connection_removed);
            assert_eq!(report.revocation, RevocationState::Pending);
            let inventory = runtime.inventory().await.unwrap();
            assert!(inventory.saved.is_empty());
            assert_eq!(inventory.revocations.len(), 1);
            assert_eq!(
                inventory.revocations[0]
                    .task
                    .parts()
                    .original
                    .operation()
                    .stream()
                    .key()
                    .local(),
                12
            );
        }
    } else {
        assert_eq!(
            consumer
                .assess(
                    spaceterm_client::management_cli::ManagementAction::RemoteAccess {
                        policy: RemoteAccessPolicy::disabled(server)
                    }
                )
                .await,
            Err(spaceterm_client::management_cli::ManagementFailure::AuthorizationDenied)
        );
        assert!(matches!(
            consumer.pairing_runtime().await,
            Err(spaceterm_client::management_cli::ManagementFailure::AuthorizationDenied)
        ));
        assert_eq!(
            StatusManagement::status(&consumer)
                .await
                .unwrap()
                .parts()
                .server,
            server
        );
        let mut subscription = StatusManagement::subscribe_status(&consumer).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while subscription.latest().unwrap().is_none() {
                subscription.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(subscription.connection().client, client);
        assert_eq!(
            subscription
                .latest()
                .unwrap()
                .unwrap()
                .parts()
                .revision
                .get(),
            2
        );
    }
    consumer.shutdown().await.unwrap();
    assert_eq!(
        served.await.unwrap(),
        if kind == NativeConsumerKind::Cli {
            ((operation_count + 1) as u64, operation_count as u64)
        } else {
            (2, 0)
        }
    );
    if operation_count > 128 {
        // Lost stdout is recoverable after dropping both the worker and SQLite
        // handles. There is no listener, so this also forbids a new network request.
        let recovered = NativeConsumer::with_recovered_journal(
            provider.management(),
            kind,
            controller(&directory, false).await,
            Arc::new(Credentials(device)),
            NativeRouteConnector::new(vec![route], None).unwrap(),
        );
        let original = original_result.unwrap();
        assert_eq!(
            recovered
                .resolve(original.request.operation_id().unwrap())
                .await
                .unwrap(),
            original
        );
        recovered.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn production_cli_reopens_saved_additional_client_and_executes_authenticated_management_in_both_majors()
 {
    for major in [1, 2] {
        exercise(Exercise {
            kind: NativeConsumerKind::Cli,
            major,
            self_remove: false,
            offline: None,
            operation_count: 1,
            restart: false,
            processes: false,
            recover_pairing: false,
            missing_runtime: false,
            confirm_obsolete_token: false,
            publish_delta: false,
        })
        .await;
    }
}
#[tokio::test]
async fn production_companion_reopens_saved_additional_client_and_subscribes_over_authenticated_ipc()
 {
    for major in [1, 2] {
        exercise(Exercise {
            kind: NativeConsumerKind::StatusCompanion,
            major,
            self_remove: false,
            offline: None,
            operation_count: 1,
            restart: false,
            processes: false,
            recover_pairing: false,
            missing_runtime: false,
            confirm_obsolete_token: false,
            publish_delta: false,
        })
        .await;
    }
}

#[tokio::test]
async fn native_cli_and_companion_accept_authenticated_multipart_deltas_in_both_majors() {
    for major in [1, 2] {
        for kind in [NativeConsumerKind::Cli, NativeConsumerKind::StatusCompanion] {
            exercise(Exercise {
                kind,
                major,
                self_remove: false,
                offline: None,
                operation_count: 1,
                restart: false,
                processes: false,
                recover_pairing: false,
                missing_runtime: false,
                confirm_obsolete_token: false,
                publish_delta: true,
            })
            .await;
        }
    }
}

#[tokio::test]
async fn production_cli_self_unpair_closes_ordinary_worker_before_restricted_retry_in_both_majors()
{
    for major in [1, 2] {
        exercise(Exercise {
            kind: NativeConsumerKind::Cli,
            major,
            self_remove: true,
            offline: None,
            operation_count: 1,
            restart: false,
            processes: false,
            recover_pairing: false,
            missing_runtime: false,
            confirm_obsolete_token: false,
            publish_delta: false,
        })
        .await;
    }
}

#[tokio::test]
async fn cli_assessment_survives_consumer_and_journal_restart_without_refreshing_scope() {
    for major in [1, 2] {
        exercise(Exercise {
            kind: NativeConsumerKind::Cli,
            major,
            self_remove: false,
            offline: None,
            operation_count: 1,
            restart: true,
            processes: false,
            recover_pairing: false,
            missing_runtime: false,
            confirm_obsolete_token: false,
            publish_delta: false,
        })
        .await;
    }
}

async fn run_cli_process(root: &std::path::Path, token: Option<String>) {
    let root = root.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("cli_assess_and_confirm_work_in_separate_processes")
            .arg("--nocapture")
            .env("SPACETERM_ASSESSMENT_TEST_ROOT", root);
        if let Some(token) = token {
            command.env("SPACETERM_ASSESSMENT_TEST_TOKEN", token);
        }
        command.output().unwrap()
    })
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "child CLI dispatch failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn cli_assess_and_confirm_work_in_separate_processes() {
    use spaceterm_client::management_cli::{ConfirmationPrompt, ConfirmationReply};
    use spaceterm_client_native::cli::{
        CliFailure,
        arguments::parse,
        runner::{CliDependencies, CliIo, Dispatch, dispatch},
    };
    if let Some(root) = std::env::var_os("SPACETERM_ASSESSMENT_TEST_ROOT") {
        struct Io(Vec<u8>);
        impl CliIo for Io {
            fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
                self.0.extend_from_slice(bytes);
                Ok(())
            }
            fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
                panic!("{}", String::from_utf8_lossy(bytes));
            }
            fn read_confirmation(
                &mut self,
                _: &ConfirmationPrompt,
            ) -> Result<ConfirmationReply, CliFailure> {
                panic!("noninteractive CLI must not prompt")
            }
        }
        let root = std::path::PathBuf::from(root).canonicalize().unwrap();
        let account = AccountServiceFixture::new(&root, BuildIdentity::Development).unwrap();
        let registration = account.initialize(DesiredServerState::Running).unwrap();
        let provider = NativeClientProvider::from_supervisor(account.reopen());
        let consumer = Arc::new(NativeConsumer::with_recovered_journal(
            provider.management(),
            NativeConsumerKind::Cli,
            controller(&root.join("cli"), false).await,
            Arc::new(Credentials(Arc::new(Key(
                Ed25519KeyPair::from_seed_unchecked(&[27; 32]).unwrap(),
            )))),
            NativeRouteConnector::new(
                vec![LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap()],
                None,
            )
            .unwrap(),
        ));
        let dependencies = CliDependencies::new(
            Ok(provider.key()),
            consumer.clone(),
            provider.management(),
            consumer.clone(),
        );
        let mut args = vec![
            "--json".to_owned(),
            "--non-interactive".to_owned(),
            "remote-access".to_owned(),
            "enable".to_owned(),
            "--bind".to_owned(),
            "127.0.0.1:443".to_owned(),
        ];
        if let Ok(token) = std::env::var("SPACETERM_ASSESSMENT_TEST_TOKEN") {
            args.extend(["--confirm".to_owned(), token]);
        } else {
            args.push("--assess".to_owned());
        }
        let invocation = parse(args.into_iter().map(std::ffi::OsString::from)).unwrap();
        let mut io = Io(Vec::new());
        let Dispatch::Exit(exit) = dispatch(invocation, &dependencies, &mut io).await else {
            panic!("client only")
        };
        assert_eq!(exit.code(), 0);
        std::fs::write(root.join("cli-result.json"), io.0).unwrap();
        drop(dependencies);
        Arc::try_unwrap(consumer)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    } else {
        for major in [1, 2] {
            exercise(Exercise {
                kind: NativeConsumerKind::Cli,
                major,
                self_remove: false,
                offline: None,
                operation_count: 1,
                restart: false,
                processes: true,
                recover_pairing: false,
                missing_runtime: false,
                confirm_obsolete_token: false,
                publish_delta: false,
            })
            .await;
        }
    }
}

#[tokio::test]
async fn lost_assess_unpair_reply_recovers_original_operation_over_authenticated_ipc_in_both_majors()
 {
    for major in [1, 2] {
        exercise(Exercise {
            kind: NativeConsumerKind::Cli,
            major,
            self_remove: false,
            offline: None,
            operation_count: 1,
            restart: false,
            processes: false,
            recover_pairing: true,
            missing_runtime: false,
            confirm_obsolete_token: false,
            publish_delta: false,
        })
        .await;
    }
}

async fn run_offline_cli_process(
    root: &std::path::Path,
    token: Option<String>,
) -> serde_json::Value {
    let root = root.to_owned();
    let child_root = root.clone();
    let output = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("offline_cli_assess_and_confirm_work_in_separate_processes")
            .arg("--nocapture")
            .env("SPACETERM_OFFLINE_TEST_ROOT", child_root);
        if let Some(token) = token {
            command.env("SPACETERM_OFFLINE_TEST_TOKEN", token);
        }
        command.output().unwrap()
    })
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "child offline CLI failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&std::fs::read(root.join("offline-cli-result.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn offline_cli_assess_and_confirm_work_in_separate_processes() {
    use spaceterm_client::management_cli::{ConfirmationPrompt, ConfirmationReply};
    use spaceterm_client_native::cli::{
        CliFailure,
        arguments::parse,
        runner::{CliDependencies, CliIo, Dispatch, dispatch},
    };
    if let Some(root) = std::env::var_os("SPACETERM_OFFLINE_TEST_ROOT") {
        struct Io(Vec<u8>);
        impl CliIo for Io {
            fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
                self.0.extend_from_slice(bytes);
                Ok(())
            }
            fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
                self.0.extend_from_slice(bytes);
                Ok(())
            }
            fn read_confirmation(
                &mut self,
                _: &ConfirmationPrompt,
            ) -> Result<ConfirmationReply, CliFailure> {
                panic!("noninteractive CLI must not prompt")
            }
        }
        let root = std::path::PathBuf::from(root).canonicalize().unwrap();
        let account = AccountServiceFixture::new(&root, BuildIdentity::Development).unwrap();
        let provider = NativeClientProvider::from_supervisor(account.reopen());
        let consumer = Arc::new(
            NativeConsumer::new(provider.management(), NativeConsumerKind::Cli).with_credentials(
                Arc::new(Credentials(Arc::new(Key(
                    Ed25519KeyPair::from_seed_unchecked(&[27; 32]).unwrap(),
                )))),
            ),
        );
        let dependencies = CliDependencies::new(
            Ok(provider.key()),
            consumer.clone(),
            provider.management(),
            consumer.clone(),
        );
        let mut args = vec![
            "--json".to_owned(),
            "--non-interactive".to_owned(),
            "unpair".to_owned(),
            "--device".to_owned(),
            "06060606060606060606060606060606".to_owned(),
        ];
        if let Ok(token) = std::env::var("SPACETERM_OFFLINE_TEST_TOKEN") {
            args.extend(["--confirm".to_owned(), token]);
        } else {
            args.push("--assess".to_owned());
        }
        let invocation = parse(args.into_iter().map(std::ffi::OsString::from)).unwrap();
        let mut io = Io(Vec::new());
        let Dispatch::Exit(exit) = dispatch(invocation, &dependencies, &mut io).await else {
            panic!("client only")
        };
        let output: serde_json::Value = serde_json::from_slice(&io.0).unwrap();
        std::fs::write(
            root.join("offline-cli-result.json"),
            serde_json::to_vec(&serde_json::json!({"exit":exit.code(), "output":output})).unwrap(),
        )
        .unwrap();
        drop(dependencies);
        Arc::try_unwrap(consumer)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    } else {
        for desired in [DesiredServerState::Stopped, DesiredServerState::Running] {
            exercise(Exercise {
                kind: NativeConsumerKind::Cli,
                major: 2,
                self_remove: true,
                offline: Some(desired),
                operation_count: 1,
                restart: false,
                processes: true,
                recover_pairing: false,
                missing_runtime: false,
                confirm_obsolete_token: false,
                publish_delta: false,
            })
            .await;
        }
    }
}

#[tokio::test]
async fn offline_cli_confirmation_without_registration_preserves_other_builds() {
    exercise(Exercise {
        kind: NativeConsumerKind::Cli,
        major: 2,
        self_remove: true,
        offline: Some(DesiredServerState::Stopped),
        operation_count: 1,
        restart: false,
        processes: true,
        recover_pairing: true,
        missing_runtime: false,
        confirm_obsolete_token: false,
        publish_delta: false,
    })
    .await;
}

#[tokio::test]
async fn missing_registration_rejects_changed_offline_cli_confirmation_without_partial_removal() {
    exercise(Exercise {
        kind: NativeConsumerKind::Cli,
        major: 2,
        self_remove: true,
        offline: Some(DesiredServerState::Stopped),
        operation_count: 1,
        restart: true,
        processes: true,
        recover_pairing: true,
        missing_runtime: false,
        confirm_obsolete_token: false,
        publish_delta: false,
    })
    .await;
}

#[tokio::test]
async fn offline_cli_confirmation_without_approved_runtime_preserves_other_builds() {
    for desired in [DesiredServerState::Stopped, DesiredServerState::Running] {
        exercise(Exercise {
            kind: NativeConsumerKind::Cli,
            major: 2,
            self_remove: true,
            offline: Some(desired),
            operation_count: 1,
            restart: false,
            processes: true,
            recover_pairing: false,
            missing_runtime: true,
            confirm_obsolete_token: false,
            publish_delta: false,
        })
        .await;
    }
}

#[tokio::test]
async fn offline_cli_confirmation_after_original_pairing_removal_is_stale_and_preserves_revocation()
{
    for desired in [DesiredServerState::Stopped, DesiredServerState::Running] {
        exercise(Exercise {
            kind: NativeConsumerKind::Cli,
            major: 2,
            self_remove: true,
            offline: Some(desired),
            operation_count: 1,
            restart: false,
            processes: true,
            recover_pairing: false,
            missing_runtime: false,
            confirm_obsolete_token: true,
            publish_delta: false,
        })
        .await;
    }
}

#[tokio::test]
async fn offline_cli_self_unpair_removes_stopped_and_unreachable_pairings() {
    for desired in [DesiredServerState::Stopped, DesiredServerState::Running] {
        exercise(Exercise {
            kind: NativeConsumerKind::Cli,
            major: 2,
            self_remove: true,
            offline: Some(desired),
            operation_count: 1,
            restart: false,
            processes: false,
            recover_pairing: false,
            missing_runtime: false,
            confirm_obsolete_token: false,
            publish_delta: false,
        })
        .await;
    }
}
#[tokio::test]
async fn offline_cli_self_unpair_rejects_confirmation_when_original_journal_changed() {
    exercise(Exercise {
        kind: NativeConsumerKind::Cli,
        major: 2,
        self_remove: true,
        offline: Some(DesiredServerState::Stopped),
        operation_count: 1,
        restart: true,
        processes: false,
        recover_pairing: false,
        missing_runtime: false,
        confirm_obsolete_token: false,
        publish_delta: false,
    })
    .await;
}

#[tokio::test]
async fn cli_protocol_results_release_capacity_after_more_than_128_operations() {
    exercise(Exercise {
        kind: NativeConsumerKind::Cli,
        major: 2,
        self_remove: false,
        offline: None,
        operation_count: 130,
        restart: false,
        processes: false,
        recover_pairing: false,
        missing_runtime: false,
        confirm_obsolete_token: false,
        publish_delta: false,
    })
    .await;
}
struct ProvisionCredentials {
    device: Arc<Key>,
    authentication: Arc<Key>,
    items: std::sync::Mutex<Vec<PreparedCredential>>,
}
impl ProvisionCredentials {
    fn new() -> Self {
        Self {
            device: Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[41; 32]).unwrap())),
            authentication: Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap())),
            items: Default::default(),
        }
    }
}
impl NativeDeviceCredentials for ProvisionCredentials {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            let scope = reservation.scope().clone();
            let public_key = if scope.parts().purpose == CredentialPurpose::DeviceIdentity {
                self.device.public_key()
            } else {
                self.authentication.public_key()
            };
            let credential = PreparedCredential { scope, public_key };
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
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            let key = if credential.scope.parts().purpose == CredentialPurpose::DeviceIdentity {
                &self.device
            } else {
                &self.authentication
            };
            assert_eq!(key.public_key(), credential.public_key);
            Ok(key.clone() as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async { panic!("provisioning must not delete any native key") })
    }
}
struct ProvisionInvalidator;
impl spaceterm_client::pending::LocalUseInvalidator for ProvisionInvalidator {
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Ok(vec![])
    }
    fn invalidate_attachment(
        &self,
        _: &LocalPairingKey,
        _: spaceterm_client::view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Err(ClientFailure::new(ClientFailureClass::StaleAuthority))
    }
    fn remove_pairing<'a>(
        &'a self,
        _: &'a LocalPairingKey,
    ) -> spaceterm_client::ports::ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn invalidate_pairing(&self, _: &LocalPairingKey) {}
}

#[tokio::test]
async fn deliberate_provisioning_creates_three_server_issued_clients_before_native_restoration_in_both_majors()
 {
    for major in [1, 2] {
        provision_from_receipt(major, ProvisionInterruption::default()).await;
    }
}

#[tokio::test]
async fn provisioning_recovers_lost_assessment_reply_without_replacing_its_operation_in_both_majors()
 {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                lose_assessment: true,
                ..Default::default()
            },
        )
        .await;
    }
}

#[tokio::test]
async fn provisioning_recovers_saved_client_receipt_after_lost_acknowledgement_in_both_majors() {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                lose_acknowledgement: true,
                ..Default::default()
            },
        )
        .await;
    }
}

struct PendingAssessmentPeer;
impl spaceterm_client::operations::OperationPeer for PendingAssessmentPeer {
    fn validate_submission(
        &self,
        _: &PairedServerBinding,
        command: &Command,
        workspace: Option<WorkspaceId>,
    ) -> Result<(), ClientFailure> {
        assert!(matches!(
            command,
            Command::Pairing(PairingCommand::AssessUnpair { .. })
        ));
        assert!(workspace.is_none());
        Ok(())
    }
    fn submit<'a>(
        &'a self,
        _: &'a spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest,
    ) -> spaceterm_client::ports::ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn lookup(
        &self,
        _: OperationBinding,
    ) -> spaceterm_client::ports::ClientFuture<'_, OperationLookup> {
        Box::pin(async { panic!("the user owns assessment recovery") })
    }
    fn acknowledge(
        &self,
        _: OperationStreamId,
        _: u64,
    ) -> spaceterm_client::ports::ClientFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

struct ProvisionLookup {
    record: PairingRecord,
    issued: Vec<ClientId>,
    authenticated: std::sync::Mutex<ClientId>,
}
impl PairingLookup for ProvisionLookup {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        assert_eq!(server, self.record.principal.server);
        assert_eq!(pairing, self.record.principal.pairing);
        let id = self
            .issued
            .iter()
            .find(|id| local(id.local()) == client)
            .copied()
            .unwrap();
        *self.authenticated.lock().unwrap() = id;
        Ok(PairingRecord {
            principal: Principal {
                client,
                ..self.record.principal
            },
            ..self.record
        })
    }
}

struct PauseAfterPrepare {
    storage: NativeClientJournal,
    armed: std::sync::atomic::AtomicBool,
    binding_only: bool,
    refuse_binding_prepare: bool,
    metadata_fault: Option<ReceiverMetadataFault>,
    directory: std::path::PathBuf,
    committed: tokio::sync::watch::Sender<Option<OperationId>>,
    release: tokio::sync::Notify,
}
impl spaceterm_client::journal::ClientJournal for PauseAfterPrepare {
    fn authority_fence(&self) -> &spaceterm_client::pairing::credentials::PairingAuthorityFence {
        spaceterm_client::journal::ClientJournal::authority_fence(&self.storage)
    }
    fn admission_latch(&self) -> Arc<spaceterm_client::journal::JournalAdmission> {
        spaceterm_client::journal::ClientJournal::admission_latch(&self.storage)
    }
    fn scheduler_claim(&self) -> Arc<std::sync::atomic::AtomicBool> {
        spaceterm_client::journal::ClientJournal::scheduler_claim(&self.storage)
    }
    fn recover(
        &self,
    ) -> spaceterm_client::ports::ClientFuture<'_, spaceterm_client::journal::RecoveredClientState>
    {
        spaceterm_client::journal::ClientJournal::recover(&self.storage)
    }
    fn commit<'a>(
        &'a self,
        transaction: &'a spaceterm_client::journal::ClientJournalTransaction,
    ) -> spaceterm_client::ports::ClientFuture<
        'a,
        spaceterm_client::journal::JournalCommitDisposition,
    > {
        Box::pin(async move {
            use std::os::unix::fs::PermissionsExt;
            let operation = transaction.changes.iter().find_map(|change| match change {
                ClientJournalChange::Prepare(record)
                | ClientJournalChange::PrepareReceiverBinding { operation: record, .. }
                    if self.binding_only == matches!(
                        spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::decode(
                            &record.request, &CanonicalCodec,
                        ).unwrap().command(),
                        Command::Pairing(PairingCommand::BindAdditionalClient { .. })
                    ) => Some(record.binding.id),
                _ => None,
            });
            let interrupt =
                operation.is_some() && self.armed.swap(false, std::sync::atomic::Ordering::SeqCst);
            if interrupt && self.refuse_binding_prepare {
                std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o500))
                    .unwrap();
            }
            let result =
                spaceterm_client::journal::ClientJournal::commit(&self.storage, transaction).await;
            if interrupt && self.refuse_binding_prepare {
                std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            let result = result?;
            if interrupt
                && matches!(
                    result,
                    spaceterm_client::journal::JournalCommitDisposition::Durable(_)
                )
                && let Some(fault) = self.metadata_fault
            {
                self.committed.send_replace(operation);
                match fault {
                    ReceiverMetadataFault::BeforeEffect => spaceterm_host::account_service::test_support::fail_next_publication_before_effect(),
                    ReceiverMetadataFault::Unsynced => spaceterm_host::account_service::test_support::fail_next_publication_durability(),
                }
                return Ok(result);
            }
            if interrupt
                && !self.refuse_binding_prepare
                && matches!(
                    result,
                    spaceterm_client::journal::JournalCommitDisposition::Durable(_)
                )
            {
                self.committed.send_replace(operation);
                self.release.notified().await;
            }
            Ok(result)
        })
    }
}

async fn provisioning_controller(
    directory: &std::path::Path,
    create: bool,
    interruption: ProvisionInterruption,
) -> (Arc<JournalController>, Option<Arc<PauseAfterPrepare>>) {
    if !interruption.orphan_assessment
        && !interruption.orphan_binding
        && !interruption.refuse_binding_prepare
        && interruption.metadata_fault.is_none()
    {
        return (controller(directory, create).await, None);
    }
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(Verifier);
    let storage = NativeClientJournal::create(
        directory.to_owned(),
        codec.clone(),
        verifier.clone(),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let (committed, _) = tokio::sync::watch::channel(None);
    let storage = Arc::new(PauseAfterPrepare {
        storage,
        armed: std::sync::atomic::AtomicBool::new(false),
        binding_only: interruption.orphan_binding
            || interruption.refuse_binding_prepare
            || interruption.metadata_fault.is_some(),
        refuse_binding_prepare: interruption.refuse_binding_prepare,
        metadata_fault: interruption.metadata_fault,
        directory: directory.to_owned(),
        committed,
        release: tokio::sync::Notify::new(),
    });
    let journal = Arc::new(
        JournalController::recover(storage.clone(), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    );
    (journal, Some(storage))
}

#[derive(Clone, Copy)]
enum ReceiverMetadataFault {
    BeforeEffect,
    Unsynced,
}

#[derive(Clone, Copy, Default)]
struct ProvisionInterruption {
    lose_assessment: bool,
    lose_acknowledgement: bool,
    companion_save: bool,
    stale_binding: bool,
    user_assessment: bool,
    orphan_assessment: bool,
    orphan_binding: bool,
    refuse_binding_prepare: bool,
    metadata_fault: Option<ReceiverMetadataFault>,
    identical_user_binding: bool,
}

#[tokio::test]
async fn cli_enroll_cannot_replace_desktop_companion_receipt_before_exact_desktop_repair() {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                companion_save: true,
                ..Default::default()
            },
        )
        .await;
    }
}

#[tokio::test]
async fn terminal_stale_provisioning_failure_is_settled_before_fresh_deliberate_retry() {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                stale_binding: true,
                ..Default::default()
            },
        )
        .await;
    }
}

#[tokio::test]
async fn provisioning_recovery_preserves_unrelated_pending_user_assessment_on_same_stream() {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                lose_assessment: true,
                user_assessment: true,
                ..Default::default()
            },
        )
        .await;
    }
}

#[tokio::test]
async fn failed_binding_prepare_does_not_claim_user_assessment_that_takes_the_reserved_slot() {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                refuse_binding_prepare: true,
                ..Default::default()
            },
        )
        .await;
    }
}

#[tokio::test]
async fn failed_receiver_binding_prepare_cannot_adopt_an_identical_unrelated_user_binding() {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                refuse_binding_prepare: true,
                identical_user_binding: true,
                ..Default::default()
            },
        )
        .await;
    }
}

#[tokio::test]
async fn interrupted_receiver_metadata_publication_recovers_exact_original_on_deliberate_retry() {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                metadata_fault: Some(ReceiverMetadataFault::BeforeEffect),
                ..Default::default()
            },
        )
        .await;
    }
}

#[tokio::test]
async fn unsynced_receiver_metadata_publication_recovers_exact_original_on_deliberate_retry() {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                metadata_fault: Some(ReceiverMetadataFault::Unsynced),
                ..Default::default()
            },
        )
        .await;
    }
}

#[tokio::test]
async fn cancelled_binding_reservation_recovers_exact_original_before_submitting_on_a_contiguous_stream()
 {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                orphan_binding: true,
                ..Default::default()
            },
        )
        .await;
    }
}

#[tokio::test]
async fn cancelled_assessment_reservation_reconciles_original_before_provisioning_on_a_contiguous_stream()
 {
    for major in [1, 2] {
        provision_from_receipt(
            major,
            ProvisionInterruption {
                orphan_assessment: true,
                ..Default::default()
            },
        )
        .await;
    }
}

async fn provision_from_receipt(major: u32, interruption: ProvisionInterruption) {
    let lose_first_assessment = interruption.lose_assessment;
    let lose_first_acknowledgement = interruption.lose_acknowledgement;
    use spaceterm_client::pairing::journal::PreparedEnrollment;
    use spaceterm_client_native::{
        pairing_runtime::NativePairingRuntime, restricted_routes::NativePairingConnector,
    };
    let root = tempfile::Builder::new()
        .prefix("st-provision-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = account.initialize(DesiredServerState::Running).unwrap();
    let provider = NativeClientProvider::from_supervisor(account.reopen());
    let pin = registration.server_pin();
    let server = ServerId::new(pin.identity);
    let pairing = PairingId::new(server, 1).unwrap();
    let original_client = ClientId::new(server, 1).unwrap();
    let target = PairingTarget::new(PairingTargetParts {
        server,
        pairing,
        device: DeviceId::new([43; 16]),
    })
    .unwrap();
    let signer = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(pin)
        .unwrap();
    let credentials = Arc::new(ProvisionCredentials::new());
    let admission = provider
        .management()
        .supervisor()
        .admit_client_provisioning()
        .unwrap();
    let (origin_directory, create) = admission.journal_directory("enrollment").unwrap();
    let (mut journal, mut pause) =
        provisioning_controller(&origin_directory, create, interruption).await;
    drop(admission);
    let route = LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap();
    let description = route.description().clone();
    let make_origin = |journal: Arc<JournalController>| {
        let route = LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap();
        let connector = Arc::new(NativePairingConnector::new(
            journal.clone(),
            NativeRouteConnector::new(vec![route], None).unwrap(),
        ));
        Arc::new(NativePairingRuntime::with_adapters(
            journal,
            credentials.clone(),
            connector.clone(),
            connector,
            Arc::new(ProvisionInvalidator),
            HandshakeConfig::default(),
        ))
    };
    let mut origin = make_origin(journal.clone());
    let namespace = CredentialNamespace::new("dev.spaceterm.provision-test".into()).unwrap();
    let scope = |purpose, reference| {
        CredentialScope::new(CredentialScopeParts {
            namespace: namespace.clone(),
            purpose,
            reference: CredentialReference::new([reference; 16]),
        })
        .unwrap()
    };
    let (_, device) = origin
        .prepare_credential(scope(CredentialPurpose::DeviceIdentity, 45))
        .await
        .unwrap();
    let (generation, authentication) = origin
        .prepare_credential(scope(
            CredentialPurpose::PairingAuthentication { server },
            46,
        ))
        .await
        .unwrap();
    let keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: device.public_key,
        pairing_authentication_public_key: authentication.public_key,
    })
    .unwrap();
    let streams_for = |client: ClientId| {
        PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: client,
                pairing,
            },
            ordinary: OperationStreamId::new(
                OperationStreamKey::new(server, client.local() * 2).unwrap(),
                pairing,
                client,
            )
            .unwrap(),
            self_revocation: OperationStreamId::new(
                OperationStreamKey::new(server, client.local() * 2 + 1).unwrap(),
                pairing,
                client,
            )
            .unwrap(),
        })
        .unwrap()
    };
    let streams = streams_for(original_client);
    let original_operation = OperationBinding {
        id: OperationId::new(
            OperationStreamId::new(
                OperationStreamKey::new(server, 1).unwrap(),
                pairing,
                original_client,
            )
            .unwrap(),
            OperationSequence::new(1).unwrap(),
        ),
        digest: RequestDigest::from_bytes([47; 32]),
    };
    let proof = ReceiptCodec::new(pin)
        .sign(
            PairingReceiptBody::Enrollment {
                target: target.clone(),
                keys: keys.clone(),
                client: streams.clone(),
                enrollment: original_operation,
                revision: StoreRevision::new(1),
            },
            signer.as_ref(),
        )
        .unwrap();
    let binding = PairedServerBinding::new(
        ConnectionTarget::new(
            pin,
            target.parts().device,
            pairing,
            original_client,
            description.clone(),
            authentication.scope.reference(),
            generation,
        )
        .unwrap(),
    );
    origin
        .prepare_route(
            &PreparedEnrollment {
                local_pairing: binding.target().local_pairing().unwrap(),
                device: target.parts().device,
                pin,
                keys: keys.clone(),
                client: original_client,
                credential: authentication.clone(),
                route: RevocationRouteReference::new([48; 16]),
                enrollment: original_operation,
                reservation: None,
            },
            description,
        )
        .await
        .unwrap();
    journal
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(
            SavedBinding::verified(binding.clone(), streams, proof, &Verifier).unwrap(),
        ))])
        .await
        .unwrap();
    let user_binding = Arc::new(std::sync::Mutex::new(None::<OperationBinding>));
    let served_user_binding = user_binding.clone();
    let companion_directory = account
        .reopen()
        .client_connections_directory()
        .join("companion");
    let interrupted_directory = companion_directory.clone();
    let binding_ids = Arc::new(std::sync::Mutex::new(Vec::new()));
    let served_binding_ids = binding_ids.clone();
    let issued_clients = Arc::new(std::sync::Mutex::new(Vec::new()));
    let served_clients = issued_clients.clone();
    let listener = IpcListener::bind(
        registration.endpoint().unwrap(),
        MessageLimit::envelope(),
        AdmissionLimits::new(4, Duration::from_secs(20)).unwrap(),
    )
    .unwrap();
    let serving_credentials = credentials.clone();
    let serving_keys = keys.clone();
    let serving_target = target.clone();
    let (finished, mut finish) = tokio::sync::oneshot::channel();
    let served = tokio::spawn(async move {
        let target = serving_target;
        let mut next_client = 2;
        let mut version = 1;
        let mut observed = vec![];
        let mut assessments = vec![];
        let mut retained_assessment = None;
        let mut admitted = std::collections::BTreeMap::<OperationStreamId, u64>::new();
        let mut connection = 0;
        loop {
            let pending = tokio::select! {
                accepted = listener.accept() => accepted.unwrap(),
                _ = &mut finish => break,
            };
            connection += 1;
            let lookup = ProvisionLookup {
                record: PairingRecord {
                    principal: Principal {
                        server: server.bytes(),
                        device: target.parts().device.bytes(),
                        pairing: local(pairing.local()),
                        client: local(original_client.local()),
                    },
                    device_public_key: serving_credentials.authentication.public_key(),
                    active: true,
                },
                issued: std::iter::once(original_client)
                    .chain(observed.iter().copied())
                    .collect(),
                authenticated: std::sync::Mutex::new(original_client),
            };
            let hello = pending.recv().await.unwrap().unwrap();
            let (handshake, reply) = ServerHandshake::accept(
                server.bytes(),
                1,
                HandshakeConfig {
                    majors: vec![major],
                    capabilities: vec![
                        CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
                        CAPABILITY_CANONICAL_DOMAIN,
                        CAPABILITY_ORGANIZATION_COMPARISONS,
                    ],
                    ..HandshakeConfig::default()
                },
                signer.clone(),
                &lookup,
                &hello,
            )
            .unwrap();
            pending.send(&reply).await.unwrap();
            let proof = pending.recv().await.unwrap().unwrap();
            let (Established::Ordinary(mut codec), ready) =
                handshake.receive_device(&lookup, &proof).unwrap()
            else {
                panic!("ordinary source required")
            };
            pending.send(&ready).await.unwrap();
            let transport = pending.finish_authentication().unwrap();
            let authenticated_client = *lookup.authenticated.lock().unwrap();
            while let Some(bytes) = transport.recv().await.unwrap() {
                let payload = codec.decode(&bytes).unwrap();
                macro_rules! respond { ($m:expr, $p:ident, $v:ident) => { match $m.kind {
                    Some($p::message::Kind::OrganizationQuery(query)) => organization::$p::snapshot_payload(&snapshot(server), 0..0, organization::PublicationPart { request: query.request, generation: query.request_generation, transfer: query.request, index: 0, total: 1, epoch: ServerEpoch::new(1), base: OrganizationRevision::new(0), revision: OrganizationRevision::new(1) }).unwrap(),
                    Some($p::message::Kind::Reconciliation(mut query)) => { query.acknowledged = true; query.connection_generation = 1; Payload::$v($p::Message { kind: Some($p::message::Kind::Reconciliation(query)) }) },
                    Some($p::message::Kind::Operation(value)) => {
                        let Some($p::operation::Action::IdentifiedPairing(request)) = value.action else { panic!("identified pairing required") };
                        let id = spaceterm_protocol::pairing_adapter::operation_from_wire(request.operation.as_ref().unwrap()).unwrap();
                        let high_water = admitted.entry(id.stream()).or_default();
                        assert_eq!(id.sequence().get(), *high_water + 1, "the server admits a contiguous stream and never repeats a durable operation");
                        *high_water = id.sequence().get();
                        let mut outcome = $p::PairingOutcome { operation: Some(Box::new(spaceterm_protocol::pairing_adapter::operation_to_wire(id))), ..Default::default() };
                        match request.action.unwrap() {
                            $p::pairing_request::Action::AssessUnpair(value) => {
                                let target = spaceterm_protocol::pairing_adapter::target_from_wire(value.target.as_ref().unwrap()).unwrap();
                                let binding = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(id, Command::Pairing(PairingCommand::AssessUnpair { target: target.clone() }), &CanonicalCodec).unwrap().binding();
                                assessments.push(id);
                                outcome.disposition = 4;
                                outcome.unpair_assessment = Some(Box::new(spaceterm_protocol::wire::pairing::UnpairAssessment { assessment_operation: Some(spaceterm_protocol::pairing_adapter::operation_to_wire(id)), assessment_digest: binding.digest.bytes().to_vec(), target: Some(spaceterm_protocol::pairing_adapter::target_to_wire(&target)), authorization_version: version, clients: vec![spaceterm_protocol::convert::identity::encode_client(original_client)] }));
                            },
                            $p::pairing_request::Action::BindAdditionalClient(value) => {
                                assert_eq!(value.expected_authorization_version, version);
                                let target = spaceterm_protocol::pairing_adapter::target_from_wire(value.target.as_ref().unwrap()).unwrap();
                                let claim = RequestDigest::from_bytes(value.claim_digest.as_slice().try_into().unwrap());
                                spaceterm_protocol::pairing_adapter::verify_client_binding_claim(&target, FieldVersion::new(version), id, &serving_keys, claim, &value.device_identity_proof, &value.pairing_authentication_proof).unwrap();
                                let binding = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(id, Command::Pairing(PairingCommand::BindAdditionalClient { target: target.clone(), expected_authorization_version: FieldVersion::new(version), claim_digest: claim }), &CanonicalCodec).unwrap().binding();
                                let client = ClientId::new(server, next_client).unwrap();
                                let streams = PairingClientStreams::new(PairingClientStreamsParts { client: ClientRecord { id: client, pairing }, ordinary: OperationStreamId::new(OperationStreamKey::new(server, next_client * 2).unwrap(), pairing, client).unwrap(), self_revocation: OperationStreamId::new(OperationStreamKey::new(server, next_client * 2 + 1).unwrap(), pairing, client).unwrap() }).unwrap();
                                let receipt = ReceiptCodec::new(pin).sign(PairingReceiptBody::ClientBinding { target, keys: serving_keys.clone(), client: streams, operation: binding, revision: StoreRevision::new(next_client) }, signer.as_ref()).unwrap();
                                served_binding_ids.lock().unwrap().push(id);
                                if interruption.stale_binding && served_binding_ids.lock().unwrap().len() == 1 {
                                    outcome.disposition = 7;
                                    outcome.classified_failure = spaceterm_protocol::wire::pairing::PairingFailure::PairingStaleAssessment as i32;
                                    version += 1;
                                } else {
                                    observed.push(client); next_client += 1; version += 1;
                                    served_clients.lock().unwrap().push(client);
                                    outcome.disposition = 3;
                                    outcome.client_binding_receipt = Some(Box::new(spaceterm_protocol::pairing_adapter::receipt_to_wire(&receipt)));
                                    if interruption.companion_save && observed.len() == 2 {
                                        use std::os::unix::fs::PermissionsExt;
                                        std::fs::set_permissions(&interrupted_directory, std::fs::Permissions::from_mode(0o500)).unwrap();
                                    }
                                }
                            },
                            _ => panic!("unexpected pairing request"),
                        }
                        Payload::$v($p::Message { kind: Some($p::message::Kind::Outcome($p::Outcome { operation: Some(spaceterm_protocol::convert::identity::encode_operation(id)), publication_revision: 2, result: Some($p::outcome::Result::PairingResult(Box::new(outcome))) })) })
                    },
                    Some($p::message::Kind::OperationLookupRequest(value)) => {
                        let outcome = if let Some(binding) = (*served_user_binding.lock().unwrap()).filter(|binding| value.operation == Some(spaceterm_protocol::convert::identity::encode_operation(binding.id))) {
                            assert_eq!(value.request_digest, binding.digest.bytes());
                            admitted.entry(binding.id.stream()).and_modify(|high_water| *high_water = (*high_water).max(binding.id.sequence().get())).or_insert(binding.id.sequence().get());
                            Some($p::Outcome { operation: value.operation.clone(), publication_revision: 2, result: Some($p::outcome::Result::PairingResult(Box::new($p::PairingOutcome { operation: Some(Box::new(spaceterm_protocol::pairing_adapter::operation_to_wire(binding.id))), disposition: 4, unpair_assessment: Some(Box::new(spaceterm_protocol::wire::pairing::UnpairAssessment { assessment_operation: Some(spaceterm_protocol::pairing_adapter::operation_to_wire(binding.id)), assessment_digest: binding.digest.bytes().to_vec(), target: Some(spaceterm_protocol::pairing_adapter::target_to_wire(&target)), authorization_version: version, clients: vec![spaceterm_protocol::convert::identity::encode_client(original_client)] })), ..Default::default() }))) })
                        } else if let Some(Payload::$v($p::Message { kind: Some($p::message::Kind::Outcome(outcome)) })) = &retained_assessment {
                            if value.operation == outcome.operation {
                                let Some($p::outcome::Result::PairingResult(result)) = &outcome.result else { panic!("pairing result required"); };
                                assert_eq!(value.request_digest, result.unpair_assessment.as_ref().unwrap().assessment_digest);
                                Some(outcome.clone())
                            } else { None }
                        } else { None };
                        let disposition = outcome.map(|outcome| $p::operation_lookup_result::Disposition::Completed(Box::new(outcome)))
                            .unwrap_or_else(|| {
                                let id = spaceterm_protocol::convert::identity::decode_operation(value.operation.as_ref().unwrap()).unwrap();
                                let expected = admitted.get(&id.stream()).copied().unwrap_or(0) + 1;
                                if id.sequence().get() > expected {
                                    $p::operation_lookup_result::Disposition::SequenceGap(expected)
                                } else {
                                    assert_eq!(id.sequence().get(), expected, "completed operations require their retained original outcome");
                                    $p::operation_lookup_result::Disposition::NeverAdmitted(true)
                                }
                            });
                        Payload::$v($p::Message { kind: Some($p::message::Kind::OperationLookupResult($p::OperationLookupResult { operation: value.operation, request_digest: value.request_digest, disposition: Some(disposition) })) })
                    },
                    Some($p::message::Kind::Acknowledgement(value)) => {
                        let (stream, through) = spaceterm_protocol::convert::operations::$p::decode_acknowledgement(&value, pairing, authenticated_client).unwrap();
                        // Delay server confirmation so provisioning must wait for
                        // the originating journal's durable confirmed prefix.
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        Payload::$v($p::Message { kind: Some($p::message::Kind::AcknowledgementConfirmation($p::AcknowledgementConfirmation { stream: Some(spaceterm_protocol::convert::identity::encode_stream(stream)), acknowledged_through: through.get() })) })
                    },
                    _ => panic!("unexpected provisioning request"),
                } }; }
                let response = match payload {
                    Payload::Major1(m) => respond!(m, v1, Major1),
                    Payload::Major2(m) => respond!(m, v2, Major2),
                };
                let assessment_reply = match &response {
                    Payload::Major1(v1::Message {
                        kind:
                            Some(v1::message::Kind::Outcome(v1::Outcome {
                                result: Some(v1::outcome::Result::PairingResult(result)),
                                ..
                            })),
                    }) => result.disposition == 4,
                    Payload::Major2(v2::Message {
                        kind:
                            Some(v2::message::Kind::Outcome(v2::Outcome {
                                result: Some(v2::outcome::Result::PairingResult(result)),
                                ..
                            })),
                    }) => result.disposition == 4,
                    _ => false,
                };
                if lose_first_assessment && retained_assessment.is_none() && assessment_reply {
                    retained_assessment = Some(response);
                    continue;
                }
                let binding_acknowledgement = match &response {
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::AcknowledgementConfirmation(value)),
                    }) => value.acknowledged_through >= 2,
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::AcknowledgementConfirmation(value)),
                    }) => value.acknowledged_through >= 2,
                    _ => false,
                };
                if lose_first_acknowledgement && connection == 1 && binding_acknowledgement {
                    continue;
                }
                if transport
                    .send(&codec.encode(&response).unwrap())
                    .await
                    .is_err()
                {
                    assert!(
                        matches!(
                            response,
                            Payload::Major1(v1::Message {
                                kind: Some(v1::message::Kind::AcknowledgementConfirmation(_))
                            }) | Payload::Major2(v2::Message {
                                kind: Some(v2::message::Kind::AcknowledgementConfirmation(_))
                            })
                        ),
                        "only final acknowledgement confirmation may race an interrupted receiver's disconnect"
                    );
                    break;
                }
            }
        }
        listener.close().unwrap();
        (observed, assessments)
    });
    let consumer = Arc::new(
        Arc::try_unwrap(provider.cli_consumer())
            .ok()
            .unwrap()
            .with_credentials(credentials.clone()),
    );
    let orphan = if interruption.metadata_fault.is_some() {
        let fault = pause.take().unwrap();
        fault.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(consumer.provision_consumers(origin.clone()).await.is_err());
        assert!(binding_ids.lock().unwrap().is_empty());
        let operation = fault.committed.borrow().unwrap();
        let receiver = journal.snapshot().unwrap().receiver_bindings()[0].clone();
        assert_eq!(receiver.operation.id, operation);
        drop(origin);
        drop(journal);
        drop(fault);
        journal = controller(&origin_directory, false).await;
        assert_eq!(journal.snapshot().unwrap().receiver_bindings(), &[receiver]);
        origin = make_origin(journal.clone());
        // Explicit account recovery must replay the original filesystem phase
        // before checking receiver ownership, then refuse suppression of its origin.
        use spaceterm_client_native::cli::bindings::PairingCommandBindings;
        let invocation = spaceterm_client_native::cli::arguments::parse(
            [
                "pair",
                "enroll",
                "--recover-account",
                "--server-fingerprint",
                &spaceterm_client_native::cli::output::hex(&pin.public_key),
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        )
        .unwrap();
        let spaceterm_client_native::cli::arguments::CliCommand::PairEnroll { fingerprint, .. } =
            invocation.command
        else {
            panic!("pair enroll required");
        };
        assert!(matches!(
            consumer
                .enroll(
                    fingerprint,
                    true,
                    spaceterm_host::native_management::SudoMode::NonInteractive,
                    &mut RepeatPairIo
                )
                .await,
            Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
        ));
        assert!(binding_ids.lock().unwrap().is_empty());
        assert!(
            journal.ordinary_use_allowed(&binding).unwrap(),
            "account recovery cannot suppress an unfinished receiver's exact origin"
        );
        let admission = account.reopen().admit_client_provisioning()
                .expect("deliberate retry must recover the original fixed receiver metadata publication before ordinary directory admission");
        let bytes = admission.provisioning_attempt("cli").unwrap().unwrap();
        let marker: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            marker["phase"]["Binding"]["operation"],
            serde_json::to_value(operation).unwrap()
        );
        Some(operation)
    } else if interruption.refuse_binding_prepare {
        let fault = pause.take().unwrap();
        fault.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(consumer.provision_consumers(origin.clone()).await.is_err());
        assert!(binding_ids.lock().unwrap().is_empty());
        drop(origin);
        drop(journal);
        drop(fault);
        journal = controller(&origin_directory, false).await;
        assert!(
            journal
                .snapshot()
                .unwrap()
                .operations()
                .iter()
                .all(|record| !matches!(
                    spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::decode(
                        &record.request,
                        &CanonicalCodec,
                    )
                    .unwrap()
                    .command(),
                    Command::Pairing(PairingCommand::BindAdditionalClient { .. })
                ))
        );
        origin = make_origin(journal.clone());
        None
    } else if let Some(pause) = &pause {
        let mut committed = pause.committed.subscribe();
        let mut revisions = journal.revisions();
        pause.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let interrupted = {
            let consumer = consumer.clone();
            let origin = origin.clone();
            tokio::spawn(async move { consumer.provision_consumers(origin).await })
        };
        tokio::time::timeout(Duration::from_secs(5), committed.changed())
            .await
            .unwrap()
            .unwrap();
        let operation = committed.borrow_and_update().unwrap();
        interrupted.abort();
        assert!(interrupted.await.unwrap_err().is_cancelled());
        pause.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            while journal.snapshot().unwrap().operation(operation).is_none() {
                revisions.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let admission = provider
            .management()
            .supervisor()
            .admit_client_provisioning()
            .unwrap();
        assert_eq!(
            admission.provisioning_attempt("cli").unwrap().is_some(),
            interruption.orphan_binding,
        );
        drop(admission);
        let receiving = controller(
            &account.reopen().client_connections_directory().join("cli"),
            false,
        )
        .await;
        assert!(receiving.snapshot().unwrap().bindings().is_empty());
        drop(receiving);
        assert!(binding_ids.lock().unwrap().is_empty());
        Some(operation)
    } else {
        None
    };
    if interruption.identical_user_binding {
        let operation = OperationId::new(
            journal.snapshot().unwrap().bindings()[0]
                .streams()
                .parts()
                .ordinary,
            OperationSequence::new(2).unwrap(),
        );
        let (claim_digest, _) = spaceterm_protocol::pairing_adapter::client_binding_claim(
            &target,
            FieldVersion::new(1),
            operation,
        )
        .unwrap();
        let request = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(
            operation,
            Command::Pairing(PairingCommand::BindAdditionalClient {
                target: target.clone(),
                expected_authorization_version: FieldVersion::new(1),
                claim_digest,
            }),
            &CanonicalCodec,
        )
        .unwrap();
        journal
            .transact(vec![ClientJournalChange::Prepare(
                spaceterm_client::journal::JournalOperation {
                    binding: request.binding(),
                    request: request.encode(&CanonicalCodec).unwrap(),
                    workspace: None,
                    reserves_grant: false,
                    cleanup: false,
                    state: spaceterm_client::journal::RetainedOperationState::Pending,
                },
            )])
            .await
            .unwrap();
        assert!(journal.snapshot().unwrap().receiver_bindings().is_empty());
        assert!(matches!(
            consumer.provision_consumers(origin.clone()).await,
            Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
        ));
        assert!(
            binding_ids.lock().unwrap().is_empty(),
            "identical command bytes are not receiver ownership"
        );
        assert!(matches!(
            journal
                .snapshot()
                .unwrap()
                .operation(operation)
                .unwrap()
                .state,
            spaceterm_client::journal::RetainedOperationState::Pending
        ));
        let _ = finished.send(());
        served.await.unwrap();
        return;
    }
    if interruption.user_assessment {
        use std::os::unix::fs::PermissionsExt;
        // Refuse the originating journal write before allocation. After restart,
        // the user's command must be free to take the still-unallocated ID.
        std::fs::set_permissions(&origin_directory, std::fs::Permissions::from_mode(0o500))
            .unwrap();
        let failed_preparation = consumer.provision_consumers(origin.clone()).await;
        std::fs::set_permissions(&origin_directory, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert!(failed_preparation.is_err());
        assert!(binding_ids.lock().unwrap().is_empty());
        drop(origin);
        drop(journal);
        journal = controller(&origin_directory, false).await;
        origin = make_origin(journal.clone());
    }
    let user_tracker = spaceterm_client::operations::OperationTracker::new(journal.clone());
    let user_ticket = if interruption.user_assessment || interruption.refuse_binding_prepare {
        let ticket = user_tracker
            .submit(
                &binding,
                Command::Pairing(PairingCommand::AssessUnpair {
                    target: target.clone(),
                }),
                None,
                &PendingAssessmentPeer,
            )
            .await
            .unwrap();
        assert_eq!(
            ticket.id().sequence().get(),
            if interruption.refuse_binding_prepare {
                2
            } else {
                1
            },
            "failed preparation must not have allocated an operation"
        );
        *user_binding.lock().unwrap() = Some(ticket.binding());
        Some(ticket)
    } else {
        None
    };
    if lose_first_assessment {
        assert!(matches!(
            consumer.provision_consumers(origin.clone()).await,
            Err(spaceterm_client::management_cli::ManagementFailure::Offline)
        ));
        if let Some(ticket) = &user_ticket {
            assert!(
                journal
                    .snapshot()
                    .unwrap()
                    .operation(ticket.id())
                    .is_some_and(|record| matches!(
                        record.state,
                        spaceterm_client::journal::RetainedOperationState::Completed {
                            consumed: false,
                            ..
                        }
                    )),
                "journal reconciliation must preserve the user's original assessment result for independent recovery"
            );
        }
        assert_eq!(
            journal.snapshot().unwrap().operations().len(),
            if interruption.user_assessment { 2 } else { 1 }
        );
    }
    if let Some(ticket) = user_ticket
        .as_ref()
        .filter(|_| !interruption.refuse_binding_prepare)
    {
        let assessment = UnpairAssessment::new(UnpairAssessmentParts {
            reference: AssessmentRef {
                operation: ticket.id(),
                digest: ticket.binding().digest,
            },
            target: target.clone(),
            authorization_version: FieldVersion::new(1),
            clients: vec![ClientRecord {
                id: original_client,
                pairing,
            }],
        })
        .unwrap();
        user_tracker
            .resolve_bound(
                ticket.binding(),
                OperationLookup::Completed {
                    result: Outcome::Pairing(Box::new(PairingOutcome::UnpairAssessed(
                        assessment.clone(),
                    ))),
                    publication_revision: OrganizationRevision::new(2),
                },
                &PendingAssessmentPeer,
            )
            .await
            .unwrap();
        assert!(
            matches!(ticket.status().borrow().clone(), spaceterm_client::operations::OperationStatus::Completed { outcome: Outcome::Pairing(value), .. } if *value == PairingOutcome::UnpairAssessed(assessment))
        );
    }
    if interruption.companion_save {
        use spaceterm_client_native::cli::bindings::PairingCommandBindings;
        use std::os::unix::fs::PermissionsExt;
        let first = consumer.provision_consumers(origin.clone()).await;
        std::fs::set_permissions(&companion_directory, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert!(
            first.is_err(),
            "companion receipt persistence must be interrupted"
        );
        assert_eq!(binding_ids.lock().unwrap().len(), 2);
        let interrupted = journal.snapshot().unwrap();
        let companion_operation = binding_ids.lock().unwrap()[1];
        assert!(matches!(
            interrupted.operation(companion_operation).unwrap().state,
            spaceterm_client::journal::RetainedOperationState::Completed {
                consumed: false,
                ..
            }
        ));
        let clients_before = issued_clients.lock().unwrap().clone();
        let invocation = spaceterm_client_native::cli::arguments::parse(
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
        let spaceterm_client_native::cli::arguments::CliCommand::PairEnroll { fingerprint, .. } =
            invocation.command
        else {
            panic!("pair enroll required");
        };
        assert!(
            matches!(
                consumer
                    .enroll(
                        fingerprint,
                        false,
                        spaceterm_host::native_management::SudoMode::NonInteractive,
                        &mut RepeatPairIo
                    )
                    .await,
                Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
            ),
            "a different origin must not replace the desktop companion operation"
        );
        assert_eq!(*issued_clients.lock().unwrap(), clients_before);
        assert_eq!(binding_ids.lock().unwrap().len(), 2);
        let receiving = controller(&companion_directory, false).await;
        assert!(receiving.snapshot().unwrap().bindings().is_empty());
    }
    if interruption.stale_binding {
        assert!(matches!(
            consumer.provision_consumers(origin.clone()).await,
            Err(spaceterm_client::management_cli::ManagementFailure::StaleAssessment)
        ));
        let rejected = binding_ids.lock().unwrap()[0];
        let state = journal.snapshot().unwrap();
        assert!(
            matches!(
                state.operation(rejected).map(|record| &record.state),
                None | Some(
                    spaceterm_client::journal::RetainedOperationState::Completed {
                        consumed: true,
                        ..
                    }
                )
            ),
            "the terminal server rejection must be consumed before surfacing it"
        );
        assert!(
            state.stream(rejected.stream()).unwrap().confirmed_prefix >= rejected.sequence().get(),
            "the original rejection must be acknowledged before a fresh deliberate retry"
        );
    }
    if lose_first_acknowledgement {
        assert!(matches!(
            consumer.provision_consumers(origin.clone()).await,
            Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
        ));
        let receiving = controller(
            &account.reopen().client_connections_directory().join("cli"),
            false,
        )
        .await;
        let state = receiving.snapshot().unwrap();
        assert_eq!(state.bindings().len(), 1);
        let saved = &state.bindings()[0];
        assert!(saved.additional_client_acknowledgement_pending());
        assert!(
            !receiving
                .ordinary_use_allowed(&saved.binding().unwrap())
                .unwrap()
        );
    }
    let cli = consumer.provision_consumers(origin.clone()).await.unwrap();
    let origin_state = journal.snapshot().unwrap();
    let header = origin_state
        .stream(streams_for(original_client).parts().ordinary)
        .unwrap();
    assert_eq!(
        header.confirmed_prefix, header.durable_prefix,
        "provisioning must durably confirm every originating operation before disconnect"
    );
    if interruption.orphan_binding || interruption.metadata_fault.is_some() {
        assert_eq!(binding_ids.lock().unwrap()[0], orphan.unwrap());
    } else if let Some(operation) = orphan {
        assert!(
            matches!(
                journal
                    .snapshot()
                    .unwrap()
                    .operation(operation)
                    .unwrap()
                    .state,
                spaceterm_client::journal::RetainedOperationState::Completed {
                    consumed: false,
                    ..
                }
            ),
            "unassigned predecessor recovery must retain its original result without claiming it for provisioning"
        );
    }
    if let Some(ticket) = &user_ticket {
        assert!(matches!(
            journal
                .snapshot()
                .unwrap()
                .operation(ticket.id())
                .unwrap()
                .state,
            spaceterm_client::journal::RetainedOperationState::Completed {
                consumed: false,
                ..
            }
        ));
    }
    let _ = finished.send(());
    let (observed, assessments) = served.await.unwrap();
    assert_eq!(
        assessments.len(),
        if interruption.stale_binding || interruption.orphan_assessment {
            4
        } else {
            3
        },
        "assigned assessment recovery keeps its ID; unassigned predecessors and rejected bindings require a fresh provisioning assessment"
    );
    assert_eq!(
        assessments
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        assessments.len()
    );
    {
        let ids = binding_ids.lock().unwrap();
        assert_eq!(ids.len(), if interruption.stale_binding { 4 } else { 3 });
        assert_eq!(
            ids.iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            ids.len(),
            "a terminal rejection must never replay its binding operation"
        );
    }
    assert_eq!(observed.len(), 3);
    assert_eq!(cli, observed[0]);
    let mut identities = std::collections::BTreeSet::from([original_client]);
    for (index, name) in ["cli", "companion", "management"].into_iter().enumerate() {
        let receiving = controller(
            &account.reopen().client_connections_directory().join(name),
            false,
        )
        .await;
        let state = receiving.snapshot().unwrap();
        let saved = &state.bindings()[0];
        assert!(!saved.additional_client_acknowledgement_pending());
        let binding = saved.binding().unwrap();
        assert_eq!(binding.target().client(), observed[index]);
        let PairingReceiptBody::ClientBinding { operation, .. } = &saved.proof().parts().claims
        else {
            panic!("server-issued Client receipt required");
        };
        assert_eq!(
            operation.id,
            binding_ids.lock().unwrap()[index + usize::from(interruption.stale_binding)],
            "repair must retain the receiver's original server-issued operation"
        );
        assert!(identities.insert(binding.target().client()));
        assert!(receiving.ordinary_use_allowed(&binding).unwrap());
        if name == "management" {
            use spaceterm_client_native::native::runtime::{
                NativeRuntimeIdentity, RuntimeClientIdentity,
            };
            let access = NativeRuntimeIdentity::from_journal(receiving, credentials.clone())
                .access(&registration)
                .await
                .unwrap();
            assert_eq!(access.binding.target().client(), observed[index]);
        }
    }
    use spaceterm_client_native::cli::{
        arguments::{CliCommand, parse},
        bindings::PairingCommandBindings,
        output::CliOutput,
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
    assert!(
        matches!(consumer.enroll(fingerprint, false, spaceterm_host::native_management::SudoMode::NonInteractive, &mut RepeatPairIo).await.unwrap(), CliOutput::Enrolled { client, .. } if client == cli)
    );
    assert_eq!(
        credentials.items.lock().unwrap().len(),
        2,
        "additional Clients reuse native keys without replacing them"
    );
}

#[tokio::test]
async fn pair_enroll_requires_fingerprint_confirmation_before_native_credentials_or_journal_creation()
 {
    use spaceterm_client::management_cli::{
        ConfirmationPrompt, ConfirmationReply, ManagementFailure,
    };
    use spaceterm_client_native::cli::{
        arguments::{CliCommand, parse},
        bindings::PairingCommandBindings,
        runner::CliIo,
    };
    struct Decline;
    impl CliIo for Decline {
        fn write_stdout(&mut self, _: &[u8]) -> Result<(), ManagementFailure> {
            Ok(())
        }
        fn write_stderr(&mut self, _: &[u8]) -> Result<(), ManagementFailure> {
            Ok(())
        }
        fn read_confirmation(
            &mut self,
            _: &ConfirmationPrompt,
        ) -> Result<ConfirmationReply, ManagementFailure> {
            panic!("pairing uses fingerprint verification")
        }
    }
    let root = tempfile::Builder::new()
        .prefix("st-provision-consent-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/var/tmp")
        .unwrap();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    account.initialize(DesiredServerState::Running).unwrap();
    let provider = NativeClientProvider::from_supervisor(account.reopen());
    let credentials = Arc::new(ProvisionCredentials::new());
    let consumer = NativeConsumer::new(provider.management(), NativeConsumerKind::Cli)
        .with_credentials(credentials.clone());
    let invocation = parse(
        ["pair", "enroll", "--server-fingerprint", &"ff".repeat(32)]
            .into_iter()
            .map(std::ffi::OsString::from),
    )
    .unwrap();
    let CliCommand::PairEnroll { fingerprint, .. } = invocation.command else {
        panic!("pair enroll required");
    };
    assert!(matches!(
        consumer
            .enroll(
                fingerprint,
                false,
                spaceterm_host::native_management::SudoMode::NonInteractive,
                &mut Decline
            )
            .await,
        Err(ManagementFailure::AuthorizationDenied)
    ));
    assert!(matches!(
        consumer
            .enroll(
                None,
                false,
                spaceterm_host::native_management::SudoMode::Interactive,
                &mut Decline
            )
            .await,
        Err(ManagementFailure::ConfirmationRequired)
    ));
    assert!(credentials.items.lock().unwrap().is_empty());
    assert!(!account.reopen().client_connections_directory().exists());
}

struct RepeatPairIo;
impl spaceterm_client_native::cli::runner::CliIo for RepeatPairIo {
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

#[tokio::test]
async fn running_unclean_custody_without_saved_cli_binding_reports_pairing_required() {
    use spaceterm_client_native::native::{classify_service, runtime::NativeRuntimeIdentity};
    let root = tempfile::tempdir().unwrap();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = account.initialize(DesiredServerState::Running).unwrap();
    use std::os::unix::fs::DirBuilderExt;
    let directory = root.path().canonicalize().unwrap().join("unpaired-client");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let journal = controller(&directory, true).await;
    let signer = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    let _custody = spaceterm_host::account_service::runtime_inventory::RuntimeCustody::begin(
        &registration,
        ServerEpoch::new(1),
        signer,
    )
    .unwrap();
    let identity = NativeRuntimeIdentity::from_journal(
        journal,
        Arc::new(Credentials(Arc::new(Key(
            Ed25519KeyPair::from_seed_unchecked(&[27; 32]).unwrap(),
        )))),
    );
    let processes =
        spaceterm_client_native::native::runtime::RuntimeServiceProcesses::new(Arc::new(identity));
    let failure =
        match spaceterm_host::account_service::ServiceProcesses::assess(&processes, &registration)
            .await
        {
            Ok(_) => panic!("an unpaired Client cannot acquire runtime authority"),
            Err(failure) => failure,
        };
    assert_eq!(
        classify_service(failure),
        spaceterm_client::management_cli::ManagementFailure::PairingRequired
    );
}
