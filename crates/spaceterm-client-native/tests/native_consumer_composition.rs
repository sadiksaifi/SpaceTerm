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
use std::{num::NonZeroU64, sync::Arc, time::Duration};

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
        NativeClientJournal::open_existing(
            directory.to_owned(),
            codec.clone(),
            verifier.clone(),
            JournalLimits::default(),
        )
        .await
    }
    .unwrap();
    Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, JournalLimits::default())
            .await
            .unwrap(),
    )
}

async fn exercise(
    kind: NativeConsumerKind,
    major: u32,
    self_remove: bool,
    restart: bool,
    processes: bool,
    recover_pairing: bool,
) {
    use std::os::unix::fs::DirBuilderExt;
    let root = tempfile::Builder::new()
        .prefix("st-consumer-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = account.initialize(DesiredServerState::Running).unwrap();
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
    let directory = root
        .path()
        .canonicalize()
        .unwrap()
        .join(if kind == NativeConsumerKind::Cli {
            "cli"
        } else {
            "companion"
        });
    std::fs::DirBuilder::new()
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
        for _ in 0..if restart || processes { 2 } else { 1 } {
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
    let mut consumer = NativeConsumer::restore(
        provider.management(),
        kind,
        journal.clone(),
        Arc::new(Credentials(device.clone())),
        NativeRouteConnector::new(
            vec![LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap()],
            None,
        )
        .unwrap(),
    )
    .await
    .unwrap();
    if kind == NativeConsumerKind::Cli {
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
        if recover_pairing {
            use spaceterm_client_native::cli::bindings::PairingCommandBindings;
            assert_eq!(
                consumer.assess_unpair(DeviceId::new([6; 16]), None).await,
                Err(spaceterm_client::management_cli::ManagementFailure::OutcomePending)
            );
            let operation = lost_pairing_operation.lock().unwrap().unwrap();
            let recovered = consumer.resolve(operation).await.unwrap();
            assert_eq!(recovered.request.operation_id(), Some(operation));
            assert_eq!(recovered.state, ManagementProgressState::Completed);
        }
        if self_remove {
            use spaceterm_client_native::cli::bindings::{PairingCommandBindings, RevocationState};
            let result = consumer.assess_unpair(DeviceId::new([6; 16]), None).await;
            let assessment = result.unwrap();
            let runtime = consumer.pairing_runtime().await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let state = runtime.journal().snapshot().unwrap();
                    let header = state
                        .stream(match assessment.evidence() {
                            spaceterm_client::management_cli::AssessmentEvidence::Unpair(value) => {
                                value.parts().reference.operation.stream()
                            }
                            _ => panic!("unpair evidence required"),
                        })
                        .unwrap();
                    if header.confirmed_prefix == header.durable_prefix {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
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
            (2, 1)
        } else {
            (2, 0)
        }
    );
}
#[tokio::test]
async fn production_cli_reopens_saved_additional_client_and_executes_authenticated_management_in_both_majors()
 {
    for major in [1, 2] {
        exercise(NativeConsumerKind::Cli, major, false, false, false, false).await;
    }
}
#[tokio::test]
async fn production_companion_reopens_saved_additional_client_and_subscribes_over_authenticated_ipc()
 {
    for major in [1, 2] {
        exercise(
            NativeConsumerKind::StatusCompanion,
            major,
            false,
            false,
            false,
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn production_cli_self_unpair_closes_ordinary_worker_before_restricted_retry_in_both_majors()
{
    for major in [1, 2] {
        exercise(NativeConsumerKind::Cli, major, true, false, false, false).await;
    }
}

#[tokio::test]
async fn cli_assessment_survives_consumer_and_journal_restart_without_refreshing_scope() {
    for major in [1, 2] {
        exercise(NativeConsumerKind::Cli, major, false, true, false, false).await;
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
            exercise(NativeConsumerKind::Cli, major, false, false, true, false).await;
        }
    }
}

#[tokio::test]
async fn lost_assess_unpair_reply_recovers_original_operation_over_authenticated_ipc_in_both_majors()
 {
    for major in [1, 2] {
        exercise(NativeConsumerKind::Cli, major, false, false, false, true).await;
    }
}
