use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_core::{
    authority::*, identity::*, operation_ledger::canonical::CanonicalCodec,
    operation_ledger_contracts::*, operations::*, organization::*, storage::*,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{self, SigningKey},
};
use spaceterm_server::{
    adapters::core::CorePorts, api::*, contracts::*, service::RecoveredServer,
    shutdown::TrustedShutdownRequest,
};
use std::sync::Arc;

#[path = "support/job_a.rs"]
mod native;
#[allow(dead_code)]
#[path = "store_pairing_composition/support.rs"]
mod physical;

struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Self {
        Self(Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
    fn pin(&self) -> authentication::ServerPin {
        authentication::ServerPin {
            identity: [11; 16],
            public_key: self.public_key(),
        }
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
struct Authority;
impl StartupAuthorityPort for Authority {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure> {
        Ok(physical::identity().server)
    }
    fn validate_credentials(
        &self,
        credentials: &dyn ServerCredentialPort,
        _: ServerId,
    ) -> Result<(), ServerFailure> {
        assert_eq!(
            credentials.public_key(),
            physical::identity().server_public_key
        );
        Ok(())
    }
}
impl ServerCredentialPort for Key {}

struct Lookup(spaceterm_core::pairing::policy::PairingSnapshot);
impl authentication::PairingLookup for Lookup {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<authentication::PairingRecord, ProtocolError> {
        spaceterm_server::pairing::protocol_lookup::PairingProtocolLookup::new(self)
            .lookup(server, pairing, client)
    }
}
impl spaceterm_server::pairing::protocol_lookup::PairingSnapshotPort for &Lookup {
    fn snapshot(&self) -> Result<spaceterm_core::pairing::policy::PairingSnapshot, ProtocolError> {
        Ok(self.0.clone())
    }
}
impl PairingAuthorizationPort for Lookup {}

struct Fixture {
    directory: physical::Directory,
    config: ServerConfig,
    dependencies: ServerDependencies,
    stream: OperationStreamId,
    principal: VerifiedPrincipal,
    reconnected: VerifiedPrincipal,
    stop: Arc<native::Host>,
    runtime: Arc<native::Factory>,
    launch: Arc<native::Launch>,
    pairing: spaceterm_core::pairing::PairingRecovery,
    enrollment_receipt: spaceterm_core::pairing::SignedPairingReceipt,
    faults: spaceterm_host::storage::testing::TestControl,
}
impl Fixture {
    fn new() -> Self {
        let directory = physical::Directory::new();
        let mut storage = physical::Fixture::new(&directory.0);
        storage.enroll(42);
        let recovered = storage.recovered();
        let initial = recovered.pairing.as_ref().unwrap().parts().authorizations[0]
            .parts()
            .clients[0]
            .parts()
            .ordinary;
        let extra = OperationStreamId::new(
            OperationStreamKey::new(initial.server(), recovered.high_water.operation_stream + 1)
                .unwrap(),
            initial.pairing(),
            initial.client(),
        )
        .unwrap();
        let cursor = spaceterm_server::adapters::core::CommitCursor::from_recovered(&recovered);
        let prepared = storage
            .ledger
            .prepare_register_stream(StreamBinding::from_stream(extra), cursor.context().unwrap())
            .unwrap();
        let mut parts = cursor.parts().unwrap();
        parts.resulting_high_water.operation_stream += 1;
        prepared.projection().append_to(&mut parts).unwrap();
        let transaction =
            DurableTransaction::new_with_validation(parts, &physical::services()).unwrap();
        let disposition = storage.store.commit(&transaction);
        assert!(matches!(disposition, CommitDisposition::Durable(_)));
        storage
            .ledger
            .resolve_commit(prepared, disposition)
            .unwrap();
        Self::from_storage(directory, storage)
    }
    fn from_storage(directory: physical::Directory, mut storage: physical::Fixture) -> Self {
        let recovered = storage.recovered();
        let enrollment = recovered.pairing.as_ref().unwrap().parts().enrollments[0].clone();
        let spaceterm_core::pairing::EnrollmentReservationState::Consumed {
            receipt: enrollment_receipt,
            ..
        } = &enrollment.parts().state
        else {
            panic!("consumed enrollment")
        };
        let enrollment_receipt = enrollment_receipt.as_ref().clone();
        let authorization = recovered.pairing.as_ref().unwrap().parts().authorizations[0].parts();
        let stream = authorization.clients[0].parts().ordinary;
        let lookup = Arc::new(Lookup(storage.snapshot()));
        let signer = Arc::new(Key::new(71));
        let p = authentication::Principal {
            server: physical::identity().server.bytes(),
            device: enrollment.parts().target.parts().device.bytes(),
            pairing: native::local(stream.pairing().local()),
            client: native::local(stream.client().local()),
        };
        let (client, hello) = authentication::ClientHandshake::begin(
            signer.pin(),
            p,
            authentication::Purpose::Ordinary,
            authentication::HandshakeConfig::default(),
            Arc::new(Key::new(43)),
        )
        .unwrap();
        let (server, reply) = authentication::ServerHandshake::accept(
            p.server,
            recovered.last_epoch.get() + 1,
            authentication::HandshakeConfig::default(),
            signer.clone(),
            lookup.as_ref(),
            &hello,
        )
        .unwrap();
        let (client, proof) = client.receive_server(&reply).unwrap();
        let (established, ready) = server.receive_device(lookup.as_ref(), &proof).unwrap();
        client.receive_ready(&ready).unwrap();
        let authentication::Established::Ordinary(codec) = established else {
            panic!("ordinary")
        };
        let principal =
            VerifiedPrincipal::from_authenticated(&codec, ConnectionGeneration::new(1)).unwrap();
        let reconnected =
            VerifiedPrincipal::from_authenticated(&codec, ConnectionGeneration::new(2)).unwrap();
        let native = native::Fixture::new();
        let stop = native.host.clone();
        let runtime = native.factory.clone();
        let launch = native.launch.clone();
        let mut config = native.config;
        config.host.authority = Arc::new(Authority);
        config.core = CorePorts::production(
            physical::identity(),
            ServerDirectory::new(
                physical::identity().server,
                directory.0.as_os_str().as_encoded_bytes().to_vec(),
            )
            .unwrap(),
            Arc::new(native::Clock),
        )
        .unwrap();
        let faults = storage.store.authority_test_control();
        let mut dependencies = native.dependencies;
        dependencies.store = Box::new(storage.store);
        dependencies.credentials = signer;
        dependencies.pairing = lookup;
        Self {
            directory,
            config,
            dependencies,
            stream,
            principal,
            reconnected,
            faults,
            stop,
            runtime,
            launch,
            pairing: recovered.pairing.unwrap(),
            enrollment_receipt,
        }
    }
    fn reopen(directory: physical::Directory) -> Self {
        let mut store = physical::reopen(&directory.0).unwrap();
        let recovered = store.recover().unwrap();
        let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
            recovered.organization.server,
            recovered.ledger.unwrap(),
        )
        .unwrap();
        Self::from_storage(directory, physical::Fixture { store, ledger })
    }
    fn request(&self, sequence: u64, command: Command) -> CanonicalOperationRequest {
        CanonicalOperationRequest::new(
            OperationId::new(self.stream, OperationSequence::new(sequence).unwrap()),
            command,
            &CanonicalCodec,
        )
        .unwrap()
    }
}

#[tokio::test]
async fn real_actor_rename_and_replay_agree_with_physical_store() {
    let fixture = Fixture::new();
    let create = fixture.request(
        1,
        Command::CreateWorkspace {
            server: physical::identity().server,
            source: WorkspaceCreationSource::ServerHome {
                server: physical::identity().server,
            },
            name: NameDraft::Custom(NameText::new("Before".into()).unwrap()),
        },
    );
    let request = fixture.request(
        2,
        Command::RenameWorkspace {
            workspace: WorkspaceId::new(physical::identity().server, 1).unwrap(),
            expected_version: FieldVersion::new(1),
            draft: NameDraft::Custom(NameText::new("After".into()).unwrap()),
        },
    );
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    let created = handle.submit(fixture.principal, create).await.unwrap();
    assert!(matches!(
        created,
        OperationResponse::Completed {
            outcome: Outcome::CreatedWithControl(_),
            ..
        }
    ));
    let first = handle
        .submit(fixture.principal, request.clone())
        .await
        .unwrap();
    let snapshot = handle.snapshot();
    assert_eq!(
        snapshot.state().workspaces[0].name.version,
        FieldVersion::new(2)
    );
    assert_eq!(
        handle
            .submit(fixture.principal, request.clone())
            .await
            .unwrap(),
        first
    );
    assert_eq!(handle.snapshot(), snapshot);
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    let final_snapshot = handle.snapshot();
    server.wait().await.unwrap();
    let mut store = physical::reopen(&fixture.directory.0).unwrap();
    let recovered = store.recover().unwrap();
    assert_eq!(&recovered.organization, final_snapshot.state());
    let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
        recovered.organization.server,
        recovered.ledger.unwrap(),
    )
    .unwrap();
    assert!(matches!(
        ledger
            .lookup(
                &StreamBinding::from_stream(fixture.stream),
                request.binding().id,
                Some(request.binding().digest)
            )
            .unwrap(),
        OperationLookup::Completed {
            result: Outcome::Applied,
            ..
        }
    ));
}

fn settings_request(
    stream: OperationStreamId,
    sequence: u64,
    version: u64,
) -> CanonicalOperationRequest {
    CanonicalOperationRequest::new(
        OperationId::new(stream, OperationSequence::new(sequence).unwrap()),
        Command::SetWorktreeLocation {
            server: stream.server(),
            expected_version: FieldVersion::new(version),
            template: WorktreeLocationTemplate::new(format!(
                "~/.worktrees/{{repository}}/{{branch}}-{sequence}"
            ))
            .unwrap(),
        },
        &CanonicalCodec,
    )
    .unwrap()
}

#[tokio::test]
async fn physical_admission_and_final_commit_precede_publication_and_acknowledgement() {
    use spaceterm_host::storage::testing::FaultPoint;
    let fixture = Fixture::new();
    let request = settings_request(fixture.stream, 1, 1);
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    let before = handle.snapshot();
    let admission = native::StoreGate::new();
    let completion = native::StoreGate::new();
    let next = fixture.faults.clone();
    let a = admission.clone();
    let b = completion.clone();
    fixture.faults.barrier(FaultPoint::BeforeCommit, move || {
        next.barrier(FaultPoint::BeforeCommit, move || {
            b.block();
        });
        a.block();
    });
    let actor = handle.clone();
    let principal = fixture.principal;
    let submitted = tokio::spawn(async move { actor.submit(principal, request).await });
    admission.entered().await;
    assert_eq!(handle.snapshot(), before);
    assert!(!submitted.is_finished());
    admission.release();
    completion.entered().await;
    assert_eq!(handle.snapshot(), before);
    assert!(!submitted.is_finished());
    completion.release();
    assert!(matches!(
        submitted.await.unwrap().unwrap(),
        OperationResponse::Completed {
            outcome: Outcome::Applied,
            ..
        }
    ));
    assert_eq!(
        handle.snapshot().state().settings.worktree_location.version,
        FieldVersion::new(2)
    );
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn physical_sync_error_stops_mutation_admission_and_retains_original_identity() {
    use spaceterm_host::storage::testing::FaultPoint;
    let fixture = Fixture::new();
    let request = settings_request(fixture.stream, 1, 1);
    let original = request.binding();
    let later = settings_request(fixture.stream, 2, 1);
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    let before = handle.snapshot();
    fixture.faults.fail_once(FaultPoint::DatabaseSync, 1);
    assert_eq!(
        handle
            .submit(fixture.principal, request)
            .await
            .unwrap_err()
            .class,
        ServerFailureClass::RecoveryRequired
    );
    assert!(!fixture.faults.fault_pending());
    assert_eq!(handle.snapshot(), before);
    assert!(handle.submit(fixture.principal, later).await.is_err());
    let _ = server.wait().await;
    let mut store = physical::reopen(&fixture.directory.0).unwrap();
    let recovered = store.recover().unwrap();
    let ledger = spaceterm_core::operation_ledger::model::OperationLedger::recover(
        recovered.organization.server,
        recovered.ledger.unwrap(),
    )
    .unwrap();
    assert!(matches!(
        ledger
            .lookup(
                &StreamBinding::from_stream(fixture.stream),
                original.id,
                Some(original.digest)
            )
            .unwrap(),
        OperationLookup::Pending | OperationLookup::Completed { .. }
    ));
}

#[tokio::test]
async fn store_only_acknowledgement_reaches_both_domain_owners() {
    let fixture = Fixture::new();
    let first = settings_request(fixture.stream, 1, 1);
    let second = settings_request(fixture.stream, 2, 2);
    let third = settings_request(fixture.stream, 3, 3);
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    handle
        .submit(fixture.principal, first.clone())
        .await
        .unwrap();
    let before = handle.snapshot();
    let receipt = handle
        .acknowledge(
            fixture.principal,
            fixture.stream,
            OperationSequence::new(1).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.organization_revision, before.state().revision);
    assert_eq!(handle.snapshot(), before);
    assert_eq!(
        handle
            .lookup_state(fixture.principal, first.binding())
            .await
            .unwrap(),
        OperationLookup::AcknowledgedRetired
    );
    for request in [second, third] {
        assert!(matches!(
            handle.submit(fixture.principal, request).await.unwrap(),
            OperationResponse::Completed {
                outcome: Outcome::Applied,
                ..
            }
        ));
    }
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
}

struct Revoke(spaceterm_core::pairing::PairingRecovery);
impl spaceterm_server::extensions::PairingCompletionPort for Revoke {
    fn prepare(
        &mut self,
        context: spaceterm_server::extensions::PairingCompletionContext<'_>,
    ) -> Result<spaceterm_server::extensions::PreparedPairingCompletion, ServerFailure> {
        use spaceterm_core::pairing::{policy::PairingPolicy, policy::PairingSnapshot, *};
        use spaceterm_protocol::pairing_adapter::ReceiptCodec;
        let Command::Pairing(PairingCommand::SelfRevoke { target }) = context.request.command()
        else {
            panic!("self revoke")
        };
        let cursor = context.cursor;
        let state = PairingSnapshot::new(
            cursor.server,
            cursor.epoch,
            cursor.store,
            cursor.high_water,
            self.0.clone(),
        )
        .unwrap();
        let authorization = state.authorization(target.parts().pairing).unwrap().parts();
        let receipt = ReceiptCodec::new(Key::new(71).pin())
            .sign(
                PairingReceiptBody::Revocation {
                    receipt: RevocationReceipt {
                        server: cursor.server,
                        pairing: target.parts().pairing,
                        device: target.parts().device,
                        operation: context.request.binding().id,
                        revision: cursor.store.checked_successor().unwrap(),
                        receipt_digest: RequestDigest::from_bytes([0; 32]),
                    },
                    authentication_key_digest:
                        spaceterm_core::pairing::policy::authentication_key_digest_for(
                            &authorization.keys,
                        ),
                    revocation_event: [29; 16],
                },
                &Key::new(71),
            )
            .unwrap();
        let transition = PairingPolicy::default()
            .prepare_self_revocation(&state, target, context.request.binding(), receipt)
            .unwrap();
        Ok(spaceterm_server::extensions::PreparedPairingCompletion {
            writes: transition.writes,
            streams: transition.register_streams,
            outcome: transition.outcome,
        })
    }
}

#[tokio::test]
async fn revocation_retires_ordinary_and_self_revocation_streams_in_one_physical_commit() {
    use spaceterm_core::pairing::*;
    let fixture = Fixture::new();
    let request = physical::Fixture::revoke_request(&fixture.enrollment_receipt);
    let binding = request.binding();
    let completion = spaceterm_server::extensions::PairingCompletionRequest {
        allocation: spaceterm_server::extensions::PairingAllocationRequest::None,
        revoked_clients: vec![fixture.stream.client()],
        preparation: Box::new(Revoke(fixture.pairing)),
    };
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    assert!(matches!(
        handle
            .submit(fixture.principal, request.clone())
            .await
            .unwrap(),
        OperationResponse::Pending(_)
    ));
    assert!(matches!(
        handle.complete_pairing(request, completion).await.unwrap(),
        OperationResponse::Completed {
            outcome: Outcome::Pairing(_),
            ..
        }
    ));
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
    let mut store = physical::reopen(&fixture.directory.0).unwrap();
    let recovered = store.recover().unwrap();
    let ledger = recovered.ledger.unwrap();
    assert_eq!(ledger.streams.len(), 4);
    assert!(ledger.operations.iter().any(|record| matches!(record, RecoveredOperationRecord::Completed(value) if value.binding == binding)));
    assert!(
        ledger
            .streams
            .iter()
            .filter(|header| header.binding.pairing() == fixture.stream.pairing())
            .all(|header| header.status
                == StreamStatus::Retired(StreamRetirementReason::PairingRevoked))
    );
    let pairing = recovered.pairing.unwrap();
    let authorization = &pairing.parts().authorizations[0];
    assert!(matches!(
        authorization.parts().record.state,
        PairingState::Revoked { .. }
    ));
    let PairingReceiptBody::Revocation { receipt, .. } = &authorization
        .parts()
        .revocation_receipt
        .as_ref()
        .unwrap()
        .parts()
        .claims
    else {
        panic!("revocation receipt")
    };
    assert_eq!(receipt.operation, binding.id);
}

#[path = "support/job_b.rs"]
mod transport;

#[tokio::test]
async fn native_listener_enrollment_has_single_decode_verified_requests_in_both_majors() {
    use spaceterm_host::transport::MessageTransport;
    use spaceterm_protocol::{pairing_adapter::*, wire::pairing};
    use spaceterm_server::connection::{Authenticator, PeerConnection};
    struct EnrollmentSource(spaceterm_core::pairing::EnrollmentReservation);
    impl EnrollmentLookup for EnrollmentSource {
        fn lookup(
            &self,
            ticket: &spaceterm_core::pairing::EnrollmentTicketId,
        ) -> Result<spaceterm_core::pairing::EnrollmentReservation, ProtocolError> {
            if self.0.parts().ticket != *ticket {
                return Err(ProtocolError::PairingDenied);
            }
            Ok(self.0.clone())
        }
    }
    for major in [1, 2] {
        let physical_directory = physical::Directory::new();
        let mut store = physical::Fixture::new(&physical_directory.0);
        let (reservation, secret) = store.reserve(42);
        let (public, authorization) =
            EnrollmentAuthorization::from_delivery(secret.delivery_message(&reservation).unwrap())
                .unwrap();
        let transport_fixture = transport::Fixture::new();
        let directory = transport::Directory::new();
        let listener = directory.listener();
        let client = directory.connect().await;
        let mut config = transport_fixture.config(client.peer_facts().local_uid.unwrap());
        config.server = physical::identity().server;
        config.credentials = Arc::new(Key::new(71));
        config.handshake.majors = vec![major];
        let authenticator =
            Authenticator::new_with_enrollment(config, Arc::new(EnrollmentSource(reservation)))
                .unwrap();
        let pending = listener.accept().await.unwrap();
        let peer_task = authenticator.authenticate(pending, authenticator.try_admit().unwrap());
        let client_task = async {
            let (channel, hello) = EnrollmentClient::begin(
                Key::new(71).pin(),
                public,
                vec![major],
                Arc::new(Key::new(42)),
                Arc::new(Key::new(43)),
            )
            .unwrap();
            client.send(&hello).await.unwrap();
            let (channel, proof) = channel
                .receive_server(&client.recv().await.unwrap().unwrap())
                .unwrap();
            client.send(&proof).await.unwrap();
            channel
                .receive_ready(&client.recv().await.unwrap().unwrap())
                .unwrap()
        };
        let (peer, channel) = tokio::join!(peer_task, client_task);
        let mut peer = peer.unwrap();
        let mut channel = channel;
        assert_eq!(
            transport_fixture
                .authority
                .registered
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        let PeerConnection::Enrollment(server) = &mut peer.connection else {
            panic!("enrollment purpose")
        };
        assert_eq!(server.major(), major);
        let submission = channel
            .submission_message(&authorization, &Key::new(42))
            .unwrap();
        assert!(matches!(
            server.receive_request(&submission).unwrap(),
            VerifiedEnrollmentRequest::Submit(_)
        ));
        assert!(matches!(
            server
                .receive_request(&channel.lookup_message().unwrap())
                .unwrap(),
            VerifiedEnrollmentRequest::Lookup(_)
        ));
        let e = server.reservation().parts();
        let acknowledgement = channel
            .encode(&pairing::EnrollmentMessage {
                kind: Some(pairing::enrollment_message::Kind::Acknowledgement(
                    pairing::EnrollmentAcknowledgement {
                        operation: Some(operation_to_wire(OperationId::new(
                            e.enrollment_stream,
                            OperationSequence::new(1).unwrap(),
                        ))),
                        request_digest: e.enrollment_request_digest.bytes().to_vec(),
                        acknowledged_through: 1,
                    },
                )),
            })
            .unwrap();
        assert!(matches!(
            server.receive_request(&acknowledgement).unwrap(),
            VerifiedEnrollmentRequest::Acknowledgement(_)
        ));
        assert_eq!(
            server.receive_request(&acknowledgement).err(),
            Some(ProtocolError::Sequence)
        );
        peer.close().await.unwrap();
        authenticator.close().await.unwrap();
    }
}

#[tokio::test]
async fn crash_after_physical_commit_reopens_with_reducer_ledger_store_agreement() {
    use spaceterm_host::storage::testing::FaultPoint;
    const CRASH_DIRECTORY: &str = "SPACETERM_TEST_CORE_CRASH_DIRECTORY";
    if let Some(path) = std::env::var_os(CRASH_DIRECTORY) {
        let fixture = Fixture::reopen(physical::Directory(path.into()));
        let request = settings_request(fixture.stream, 1, 1);
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        native::connect(&handle, fixture.principal).await;
        let final_commit = fixture.faults.clone();
        fixture.faults.barrier(FaultPoint::AfterCommit, move || {
            final_commit.barrier(FaultPoint::AfterCommit, || {
                // Deliberately exit before receipt delivery, reducer installation
                // and destructors. This is not trusted process-cleanup evidence.
                std::process::exit(73);
            });
        });
        handle.submit(fixture.principal, request).await.unwrap();
        panic!("physical final-commit crash barrier did not fire");
    }
    let fixture = Fixture::new();
    let original = settings_request(fixture.stream, 1, 1);
    let Fixture {
        directory,
        config,
        dependencies,
        ..
    } = fixture;
    drop(config);
    drop(dependencies);
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_after_physical_commit_reopens_with_reducer_ledger_store_agreement",
        ])
        .env(CRASH_DIRECTORY, &directory.0)
        .output()
        .unwrap();
    assert_eq!(child.status.code(), Some(73));
    let fixture = Fixture::reopen(directory);
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    let before = handle.snapshot();
    assert_eq!(
        before.state().settings.worktree_location.version,
        FieldVersion::new(2)
    );
    let resolved = handle
        .lookup_state(fixture.principal, original.binding())
        .await
        .unwrap();
    assert!(
        matches!(resolved, OperationLookup::Completed { result: Outcome::Applied, publication_revision } if publication_revision == before.state().revision)
    );
    assert!(matches!(
        handle.submit(fixture.principal, original).await.unwrap(),
        OperationResponse::Completed {
            outcome: Outcome::Applied,
            ..
        }
    ));
    assert_eq!(handle.snapshot(), before);
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn retained_close_work_waits_for_staged_physical_commit_without_poisoning_actor() {
    use spaceterm_core::{control::*, geometry::*, observations::*};
    use spaceterm_host::storage::testing::FaultPoint;
    let fixture = Fixture::new();
    let create = fixture.request(
        1,
        Command::CreateWorkspace {
            server: fixture.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: fixture.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("Observed".into()).unwrap()),
        },
    );
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    let OperationResponse::Completed {
        outcome: Outcome::CreatedWithControl(created),
        ..
    } = handle.submit(fixture.principal, create).await.unwrap()
    else {
        panic!("created")
    };
    let grant = created.control().grant();
    let fence = EffectFence::new(
        SessionKey {
            epoch: fixture.principal.epoch(),
            pane: grant.pane(),
            generation: SessionGeneration::new(0),
        },
        grant,
        fixture.principal.binding(),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    let intent = StartupIntent::new(
        OperationId::new(fixture.stream, OperationSequence::new(2).unwrap()),
        fence,
        StartupSource::ExplicitSelection,
    )
    .unwrap();
    let begin =
        CanonicalOperationRequest::new(intent.id(), Command::BeginStartup(intent), &CanonicalCodec)
            .unwrap();
    handle.submit(fixture.principal, begin).await.unwrap();
    let start = CanonicalOperationRequest::new(
        OperationId::new(fixture.stream, OperationSequence::new(3).unwrap()),
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
        &CanonicalCodec,
    )
    .unwrap();
    let OperationResponse::Completed {
        outcome: Outcome::SessionStarted(session),
        ..
    } = handle.submit(fixture.principal, start).await.unwrap()
    else {
        panic!("reserved start")
    };
    let mut publications = handle.subscribe_snapshot();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !publications.borrow().runtime().iter().any(|record| matches!(record.phase, SessionPhase::Running { generation } if generation == session.generation)) {
            publications.changed().await.unwrap();
        }
    }).await.unwrap();
    let held = native::StoreGate::new();
    let next = fixture.faults.clone();
    let gate = held.clone();
    fixture.faults.barrier(FaultPoint::AfterCommit, move || {
        next.barrier(FaultPoint::BeforeCommit, move || {
            gate.block();
        });
    });
    let request = settings_request(fixture.stream, 4, 1);
    let actor = handle.clone();
    let principal = fixture.principal;
    let submitted = tokio::spawn(async move { actor.submit(principal, request).await });
    held.entered().await;
    fixture.runtime.states.lock().unwrap()[0]
        .lock()
        .unwrap()
        .close_work = Some(
        CloseWorkObservation::new(
            session,
            RuntimeObservationSequence::new(1).unwrap(),
            CloseWorkGeneration::new(1),
            MonotonicTick::new(900),
            CloseWorkState::Idle,
        )
        .unwrap(),
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match handle.refresh_runtime_observations().await {
                Ok(()) => break,
                Err(error) if error.class == ServerFailureClass::Capacity => {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("observation failed: {:?}", error.class),
            }
        }
    })
    .await
    .unwrap();
    let phase = handle.phase();
    held.release();
    assert_eq!(phase, ServicePhase::Ready);
    assert!(matches!(
        submitted.await.unwrap().unwrap(),
        OperationResponse::Completed {
            outcome: Outcome::Applied,
            ..
        }
    ));
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
}

async fn create_and_start(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    stream: OperationStreamId,
) -> SessionKey {
    let create = CanonicalOperationRequest::new(
        OperationId::new(stream, OperationSequence::new(1).unwrap()),
        Command::CreateWorkspace {
            server: stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: stream.server(),
            },
            name: NameDraft::Custom(NameText::new("Runtime".into()).unwrap()),
        },
        &CanonicalCodec,
    )
    .unwrap();
    let OperationResponse::Completed {
        outcome: Outcome::CreatedWithControl(created),
        ..
    } = handle.submit(principal, create).await.unwrap()
    else {
        panic!("created")
    };
    explicit_start(handle, principal, stream, created.control().grant(), 2).await
}

async fn explicit_start(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    stream: OperationStreamId,
    grant: ControlGrant,
    sequence: u64,
) -> SessionKey {
    let response = explicit_start_response(handle, principal, stream, grant, sequence).await;
    let OperationResponse::Completed {
        outcome: Outcome::SessionStarted(session),
        ..
    } = response
    else {
        panic!("reserved start: {response:?}")
    };
    let mut snapshots = handle.subscribe_snapshot();
    // This bounds the fixture wait, not server admission or protocol deadlines.
    // Physical commits on the shared Linux build host can exceed five seconds.
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !snapshots.borrow().runtime().iter().any(|v| v.pane == session.pane && matches!(v.phase, SessionPhase::Running { generation } if generation == session.generation)) {
            snapshots.changed().await.unwrap();
        }
    }).await.unwrap();
    session
}

async fn explicit_start_response(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    stream: OperationStreamId,
    grant: ControlGrant,
    sequence: u64,
) -> OperationResponse {
    use spaceterm_core::{control::*, geometry::*};
    let snapshot = handle.snapshot();
    let generation = snapshot
        .runtime()
        .iter()
        .find(|v| v.pane == grant.pane())
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
        OperationId::new(stream, OperationSequence::new(sequence).unwrap()),
        fence,
        StartupSource::StartSession,
    )
    .unwrap();
    let begin =
        CanonicalOperationRequest::new(intent.id(), Command::BeginStartup(intent), &CanonicalCodec)
            .unwrap();
    handle.submit(principal, begin).await.unwrap();
    let start = CanonicalOperationRequest::new(
        OperationId::new(stream, OperationSequence::new(sequence + 1).unwrap()),
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
        &CanonicalCodec,
    )
    .unwrap();
    handle.submit(principal, start).await.unwrap()
}

async fn refresh(handle: &ServerHandle) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match handle.refresh_runtime_observations().await {
                Ok(()) => break,
                Err(error) if error.class == ServerFailureClass::Capacity => {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("{error:?}"),
            }
        }
    })
    .await
    .unwrap();
}

struct RejectedReservation(Box<dyn spaceterm_server::adapters::core::ReducerPort>);
impl spaceterm_server::adapters::core::ReducerPort for RejectedReservation {
    fn prepare_directories(
        &self,
        request: &CanonicalOperationRequest,
    ) -> Result<
        Option<
            ServerFuture<
                'static,
                Result<spaceterm_core::reducer::DirectoryValidation, ServerFailure>,
            >,
        >,
        ServerFailure,
    > {
        self.0.prepare_directories(request)
    }
    fn recover(
        &mut self,
        recovered: &RecoveredState,
        epoch: ServerEpoch,
        cleanup: &[spaceterm_core::retirement::SessionCleanupObservation],
    ) -> Result<spaceterm_server::adapters::core::PreparedReduction, ServerFailure> {
        self.0.recover(recovered, epoch, cleanup)
    }
    fn snapshot(&self) -> Arc<OrganizationSnapshot> {
        self.0.snapshot()
    }
    fn observe_close_work(
        &mut self,
        observation: spaceterm_core::observations::CloseWorkObservation,
        now: spaceterm_core::observations::MonotonicTick,
    ) -> Result<(), ServerFailure> {
        self.0.observe_close_work(observation, now)
    }
    fn prepare(
        &mut self,
        request: spaceterm_server::adapters::core::ReductionRequest<'_>,
        cursor: spaceterm_server::adapters::core::CommitCursor,
    ) -> Result<spaceterm_server::adapters::core::PreparedReduction, ServerFailure> {
        use spaceterm_server::adapters::core::ReductionRequest;
        // Force a real, staged rejected outcome at the provider boundary. The
        // reducer and physical ledger/store still own all candidates and receipts.
        let request = match request {
            ReductionRequest::StartupReserved { binding, .. } => ReductionRequest::Rejected {
                binding,
                failure: OperationFailure::BusyRetiring,
            },
            other => other,
        };
        self.0.prepare(request, cursor)
    }
    fn committed(&mut self, receipt: CommitReceipt) -> Result<(), ServerFailure> {
        self.0.committed(receipt)
    }
    fn not_applied(&mut self) {
        self.0.not_applied();
    }
}

// Observe disposal versus invocation at the public native launch boundary.
// The rejected-candidate assertion must not depend on a scheduling delay.
struct ObservedPreparation {
    delegate: Arc<native::Launch>,
    invoked: Arc<std::sync::Mutex<Option<bool>>>,
}
struct ObservedLaunch {
    delegate: Option<Box<dyn spaceterm_server::adapters::terminal::PreparedLaunchPort>>,
    invoked: Arc<std::sync::Mutex<Option<bool>>>,
}
impl spaceterm_server::adapters::terminal::LaunchPreparationPort for ObservedPreparation {
    fn prepare<'a>(
        &'a self,
        request: &'a spaceterm_terminal::runtime_contracts::RuntimeStartRequest,
    ) -> ServerFuture<
        'a,
        Result<Box<dyn spaceterm_server::adapters::terminal::PreparedLaunchPort>, OperationFailure>,
    > {
        Box::pin(async move {
            let delegate = self.delegate.prepare(request).await?;
            Ok(Box::new(ObservedLaunch {
                delegate: Some(delegate),
                invoked: self.invoked.clone(),
            })
                as Box<
                    dyn spaceterm_server::adapters::terminal::PreparedLaunchPort,
                >)
        })
    }
}
impl spaceterm_server::adapters::terminal::PreparedLaunchPort for ObservedLaunch {
    fn start(
        mut self: Box<Self>,
        factory: Arc<dyn RuntimeFactoryPort>,
        request: spaceterm_terminal::runtime_contracts::RuntimeStartRequest,
        gate: Arc<spaceterm_server::adapters::terminal::LaunchGate>,
    ) -> ServerFuture<'static, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>> {
        *self.invoked.lock().unwrap() = Some(true);
        self.delegate.take().unwrap().start(factory, request, gate)
    }
}
impl Drop for ObservedLaunch {
    fn drop(&mut self) {
        if self.delegate.is_some() {
            *self.invoked.lock().unwrap() = Some(false);
        }
    }
}

#[tokio::test]
async fn real_reducer_rejected_start_never_admits_a_process() {
    let mut fixture = Fixture::new();
    fixture.config.core.reducer = Box::new(RejectedReservation(fixture.config.core.reducer));
    let invoked = Arc::new(std::sync::Mutex::new(None));
    fixture.config.launch = Arc::new(ObservedPreparation {
        delegate: fixture.launch.clone(),
        invoked: invoked.clone(),
    });
    let create = fixture.request(
        1,
        Command::CreateWorkspace {
            server: fixture.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: fixture.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("Rejected".into()).unwrap()),
        },
    );
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    let OperationResponse::Completed {
        outcome: Outcome::CreatedWithControl(created),
        ..
    } = handle.submit(fixture.principal, create).await.unwrap()
    else {
        panic!("created")
    };
    let before = handle.snapshot();
    let response = explicit_start_response(
        &handle,
        fixture.principal,
        fixture.stream,
        created.control().grant(),
        2,
    )
    .await;
    assert!(matches!(
        response,
        OperationResponse::Completed {
            outcome: Outcome::Failed(OperationFailure::BusyRetiring),
            ..
        }
    ));
    let observed = *invoked.lock().unwrap();
    assert_eq!(
        observed,
        Some(false),
        "rejected preparation must be discarded before the durable reply"
    );
    assert!(
        fixture.runtime.starts.lock().unwrap().is_empty(),
        "rejected reservation must not call launch.start"
    );
    assert_eq!(handle.snapshot(), before);
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn real_runtime_cleanup_reaches_reducer_before_restart_or_stop() {
    use spaceterm_core::{retirement::*, runtime_authority::*};
    use std::sync::atomic::Ordering;
    for failed in [true, false] {
        let fixture = Fixture::new();
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        native::connect(&handle, fixture.principal).await;
        let session = create_and_start(&handle, fixture.principal, fixture.stream).await;
        let state = fixture.runtime.states.lock().unwrap()[0].clone();
        state.lock().unwrap().lifecycle = RuntimeLifecycle::Retiring {
            terminal: SessionTerminalResult::new(
                session,
                if failed {
                    RuntimeTerminalStatus::RuntimeFailed {
                        class: FailureClass::RuntimeFault,
                    }
                } else {
                    RuntimeTerminalStatus::OrdinaryExit {
                        exit_status: Some(0),
                    }
                },
            )
            .unwrap(),
        };
        let mut snapshots = handle.subscribe_snapshot();
        refresh(&handle).await;
        while if failed {
            !matches!(
                snapshots.borrow().runtime()[0].phase,
                SessionPhase::Failed { .. }
            )
        } else {
            !snapshots.borrow().state().panes.is_empty()
        } {
            snapshots.changed().await.unwrap();
        }
        state.lock().unwrap().cleanup = Some(
            SessionCleanupObservation::new(
                session,
                if failed {
                    RetirementReason::RuntimeFailure
                } else {
                    RetirementReason::OrdinaryExit
                },
                CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
            )
            .unwrap(),
        );
        refresh(&handle).await;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while fixture.runtime.released.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if failed {
            let record = handle.snapshot().runtime()[0];
            let ControlRecord::Held { client, generation } = record.control else {
                panic!("grant")
            };
            explicit_start(
                &handle,
                fixture.principal,
                fixture.stream,
                ControlGrant::new(record.pane, client, generation).unwrap(),
                4,
            )
            .await;
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            handle.shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            }),
        )
        .await
        .unwrap()
        .unwrap();
        server.wait().await.unwrap();
    }
}

#[tokio::test]
async fn recovered_native_retirement_does_not_stop_the_new_epoch() {
    use spaceterm_core::retirement::*;
    use spaceterm_host::storage::testing::FaultPoint;
    const CRASH_DIRECTORY: &str = "SPACETERM_TEST_NATIVE_RETIREMENT_CRASH_DIRECTORY";
    if let Some(path) = std::env::var_os(CRASH_DIRECTORY) {
        let fixture = Fixture::reopen(physical::Directory(path.into()));
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        native::connect(&handle, fixture.principal).await;
        create_and_start(&handle, fixture.principal, fixture.stream).await;
        fixture
            .faults
            .barrier(FaultPoint::AfterCommit, || std::process::exit(74));
        handle
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await
            .unwrap();
        panic!("native acceptance crash barrier did not fire");
    }
    let fixture = Fixture::new();
    let Fixture {
        directory,
        config,
        dependencies,
        ..
    } = fixture;
    drop(config);
    drop(dependencies);
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "recovered_native_retirement_does_not_stop_the_new_epoch",
        ])
        .env(CRASH_DIRECTORY, &directory.0)
        .output()
        .unwrap();
    assert_eq!(child.status.code(), Some(74));
    let mut store = physical::reopen(&directory.0).unwrap();
    let old = store.recover().unwrap();
    let descriptor = &old.native_retirements[0];
    let cleanup: Vec<_> = descriptor
        .sessions()
        .iter()
        .map(|retained| {
            SessionCleanupObservation::new(
                retained.session(),
                retained.first_reason(),
                CleanupResult::Confirmed(
                    ConfirmedCleanupClass::AlreadyAbsentWithConfirmedOwnership,
                ),
            )
            .unwrap()
        })
        .collect();
    drop(store);
    let fixture = Fixture::reopen(directory);
    *fixture.stop.cleanup.lock().unwrap() = cleanup;
    let create = fixture.request(
        4,
        Command::CreateWorkspace {
            server: fixture.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: fixture.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("Recovered".into()).unwrap()),
        },
    );
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    assert_eq!(handle.phase(), ServicePhase::Ready);
    assert!(fixture.runtime.starts.lock().unwrap().is_empty());
    native::connect(&handle, fixture.principal).await;
    let OperationResponse::Completed {
        outcome: Outcome::CreatedWithControl(created),
        ..
    } = handle.submit(fixture.principal, create).await.unwrap()
    else {
        panic!("new epoch must accept creation after native settlement")
    };
    let session = explicit_start(
        &handle,
        fixture.principal,
        fixture.stream,
        created.control().grant(),
        5,
    )
    .await;
    assert!(session.epoch > old.last_epoch);
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
    let mut store = physical::reopen(&fixture.directory.0).unwrap();
    assert!(store.recover().unwrap().native_retirements.is_empty());
}

struct ListenerEnrollment(spaceterm_core::pairing::EnrollmentReservation);
impl spaceterm_protocol::pairing_adapter::EnrollmentLookup for ListenerEnrollment {
    fn lookup(
        &self,
        ticket: &spaceterm_core::pairing::EnrollmentTicketId,
    ) -> Result<spaceterm_core::pairing::EnrollmentReservation, ProtocolError> {
        if &self.0.parts().ticket != ticket {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(self.0.clone())
    }
}
struct EnrollmentAuthentication {
    enrollment: Arc<ListenerEnrollment>,
    major: u32,
    uid: u32,
    pairings: Arc<dyn PairingAuthorizationPort>,
}
impl spaceterm_server::listeners::AuthenticationFactoryPort for EnrollmentAuthentication {
    fn config(
        &self,
        handle: ServerHandle,
        snapshot: &OrganizationSnapshot,
    ) -> Result<spaceterm_server::connection::AuthenticationConfig, ServerFailure> {
        Ok(spaceterm_server::connection::AuthenticationConfig {
            server: snapshot.state().server,
            epoch: snapshot.epoch(),
            owning_uid: self.uid,
            handshake: authentication::HandshakeConfig {
                majors: vec![self.major],
                ..Default::default()
            },
            credentials: Arc::new(Key::new(71)),
            pairing: self.pairings.clone(),
            authority: Arc::new(handle),
            unauthenticated_limit: 16,
            authenticated_limit: 32,
            deadline: std::time::Duration::from_secs(5),
        })
    }
    fn enrollment_lookup(
        &self,
        _: ServerHandle,
        _: &OrganizationSnapshot,
    ) -> Result<
        Option<Arc<dyn spaceterm_protocol::pairing_adapter::EnrollmentLookup + Send + Sync>>,
        ServerFailure,
    > {
        Ok(Some(self.enrollment.clone()))
    }
}
struct PendingEnrollment(std::sync::atomic::AtomicUsize);
impl spaceterm_server::pairing::service::PairingActorPort for PendingEnrollment {
    fn execute<'a>(
        &'a self,
        _: spaceterm_server::pairing::service::PairingActorRequest,
        _: Box<dyn spaceterm_server::pairing::service::PairingMutation>,
    ) -> ServerFuture<
        'a,
        Result<
            spaceterm_server::pairing::service::PairingExecution,
            spaceterm_core::pairing::PairingFailure,
        >,
    > {
        Box::pin(async { Err(spaceterm_core::pairing::PairingFailure::AuthorizationDenied) })
    }
    fn restricted_status<'a>(
        &'a self,
        _: OperationBinding,
        _: spaceterm_core::pairing::PairingTarget,
    ) -> ServerFuture<
        'a,
        Result<
            Option<spaceterm_core::pairing::SignedPairingReceipt>,
            spaceterm_core::pairing::PairingFailure,
        >,
    > {
        Box::pin(async { Err(spaceterm_core::pairing::PairingFailure::AuthorizationDenied) })
    }
    fn enrollment_status<'a>(
        &'a self,
        _: OperationBinding,
        _: spaceterm_core::pairing::EnrollmentTicketId,
        _: RequestDigest,
    ) -> ServerFuture<
        'a,
        Result<
            Option<spaceterm_core::pairing::SignedPairingReceipt>,
            spaceterm_core::pairing::PairingFailure,
        >,
    > {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(None) })
    }
    fn enrollment_acknowledge<'a>(
        &'a self,
        _: OperationBinding,
        _: spaceterm_core::pairing::EnrollmentTicketId,
        _: RequestDigest,
    ) -> ServerFuture<'a, Result<(), spaceterm_core::pairing::PairingFailure>> {
        Box::pin(async { Err(spaceterm_core::pairing::PairingFailure::AuthorizationDenied) })
    }
    fn restricted_acknowledge<'a>(
        &'a self,
        _: OperationBinding,
        _: spaceterm_core::pairing::PairingTarget,
    ) -> ServerFuture<'a, Result<(), spaceterm_core::pairing::PairingFailure>> {
        Box::pin(async { Err(spaceterm_core::pairing::PairingFailure::AuthorizationDenied) })
    }
}
impl spaceterm_server::pairing::management::AccountOwnerEnrollmentPort for PendingEnrollment {
    fn validate_owner_request(
        &self,
        _: &spaceterm_host::native_deployment::contracts::NativeRequestId,
        _: RequestDigest,
        _: ServerId,
        _: &spaceterm_core::pairing::EnrollmentProposal,
    ) -> Result<(), spaceterm_core::pairing::PairingFailure> {
        Err(spaceterm_core::pairing::PairingFailure::AuthorizationDenied)
    }
    fn revalidate_owner_issuance(
        &self,
        _: &spaceterm_host::native_deployment::contracts::NativeRequestId,
        _: RequestDigest,
        _: ServerId,
        _: RequestDigest,
    ) -> Result<(), spaceterm_core::pairing::PairingFailure> {
        Err(spaceterm_core::pairing::PairingFailure::AuthorizationDenied)
    }
}
struct EnrollmentDispatch {
    service: spaceterm_server::pairing::service::PairingService,
    ordinary: Arc<std::sync::atomic::AtomicUsize>,
}
impl spaceterm_server::protocol_dispatch::OrdinaryDispatchPort for EnrollmentDispatch {
    fn dispatch(
        &self,
        _: VerifiedPrincipal,
        _: spaceterm_protocol::codec::Payload,
        _: ServerHandle,
    ) -> ServerFuture<'_, Result<Option<spaceterm_protocol::codec::Payload>, ServerFailure>> {
        self.ordinary
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {
            Err(spaceterm_server::failure::failure(
                ServerFailureClass::Authentication,
            ))
        })
    }
}
impl spaceterm_server::protocol_dispatch::RestrictedDispatchPort for EnrollmentDispatch {
    fn dispatch(
        &self,
        _: authentication::Principal,
        _: spaceterm_protocol::wire::common::RestrictedMessage,
    ) -> ServerFuture<'_, Result<spaceterm_protocol::wire::common::RestrictedMessage, ServerFailure>>
    {
        Box::pin(async {
            Err(spaceterm_server::failure::failure(
                ServerFailureClass::Authentication,
            ))
        })
    }
}
impl spaceterm_server::listeners::ConnectionDispatchFactoryPort for EnrollmentDispatch {
    fn create(
        &self,
        authenticator: spaceterm_server::connection::Authenticator,
    ) -> Result<Arc<dyn spaceterm_server::listeners::ConnectionDispatchPort>, ServerFailure> {
        let (_, status) = tokio::sync::watch::channel(transport::status(1));
        let dispatch = Arc::new(EnrollmentDispatch {
            service: self.service.clone(),
            ordinary: self.ordinary.clone(),
        });
        Ok(Arc::new(
            spaceterm_server::protocol_dispatch::ProtocolDispatcher::new(
                authenticator,
                status,
                dispatch.clone(),
                dispatch,
            )
            .with_pairing(self.service.clone()),
        ))
    }
}

#[tokio::test]
async fn server_listeners_route_enrollment_to_protocol_dispatcher_in_both_majors() {
    use spaceterm_host::{
        ipc::IpcTransport,
        transport::{MessageLimit, MessageTransport},
    };
    use spaceterm_protocol::{pairing_adapter::*, wire::pairing};
    use spaceterm_server::listeners::{ListenerConfig, ServerListeners};
    use std::sync::atomic::{AtomicUsize, Ordering};
    for major in [1, 2] {
        let fixture = Fixture::new();
        let reservation_directory = physical::Directory::new();
        let mut storage = physical::Fixture::new(&reservation_directory.0);
        let (reservation, secret) = storage.reserve(42);
        let (public, _) =
            EnrollmentAuthorization::from_delivery(secret.delivery_message(&reservation).unwrap())
                .unwrap();
        let lookup = fixture.dependencies.pairing.clone();
        let actor = Arc::new(PendingEnrollment(AtomicUsize::new(0)));
        let service = spaceterm_server::pairing::service::PairingService::new(
            actor.clone(),
            Arc::new(Key::new(71)),
            Arc::new(CanonicalCodec),
            actor.clone(),
            Key::new(71).pin(),
        )
        .unwrap();
        let ordinary = Arc::new(AtomicUsize::new(0));
        let (socket, _peer) = tokio::net::UnixStream::pair().unwrap();
        let uid = spaceterm_host::peer_credentials::same_account_peer(&socket)
            .unwrap()
            .local_uid
            .unwrap();
        let directory = transport::Directory::new();
        let endpoint = directory.directory.endpoint("s").unwrap();
        let listeners = Arc::new(ServerListeners::new(ListenerConfig {
            ipc: endpoint.clone(),
            tls: transport::tls(71).0,
            authentication: Arc::new(EnrollmentAuthentication {
                enrollment: Arc::new(ListenerEnrollment(reservation)),
                major,
                uid,
                pairings: lookup,
            }),
            dispatch: Arc::new(EnrollmentDispatch {
                service,
                ordinary: ordinary.clone(),
            }),
        }));
        let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
            .await
            .unwrap();
        let handle = server.handle();
        let running = tokio::spawn(server.run(ListenerFactories {
            lifecycle: listeners.clone(),
        }));
        let client = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(client) = IpcTransport::connect(&endpoint, MessageLimit::envelope()).await
                {
                    break client;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let (channel, hello) = EnrollmentClient::begin(
            Key::new(71).pin(),
            public,
            vec![major],
            Arc::new(Key::new(42)),
            Arc::new(Key::new(43)),
        )
        .unwrap();
        client.send(&hello).await.unwrap();
        let response = client
            .recv()
            .await
            .unwrap()
            .expect("listener must accept enrollment Hello");
        let (channel, proof) = channel.receive_server(&response).unwrap();
        client.send(&proof).await.unwrap();
        let mut channel = channel
            .receive_ready(&client.recv().await.unwrap().unwrap())
            .unwrap();
        client
            .send(&channel.lookup_message().unwrap())
            .await
            .unwrap();
        let response = client.recv().await.unwrap().unwrap();
        assert!(matches!(
            channel.decode(&response).unwrap().kind,
            Some(pairing::enrollment_message::Kind::Result(_))
        ));
        assert_eq!(actor.0.load(Ordering::SeqCst), 1);
        assert_eq!(ordinary.load(Ordering::SeqCst), 0);
        assert_eq!(
            handle
                .reconcile(fixture.principal, handle.snapshot().state().revision)
                .await
                .unwrap_err()
                .class,
            ServerFailureClass::Authentication
        );
        client.close().await.unwrap();
        handle
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await
            .unwrap();
        running.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn real_worktree_directory_preparation_allows_creation_and_split() {
    use spaceterm_core::reducer::*;
    let fixture = Fixture::new();
    let repository = fixture.directory.0.join("repository");
    std::fs::create_dir(&repository).unwrap();
    assert!(
        std::process::Command::new("/usr/bin/git")
            .args(["-c", "init.defaultBranch=main", "init", "--quiet"])
            .arg(&repository)
            .status()
            .unwrap()
            .success()
    );
    let directory = ServerDirectory::new(
        fixture.stream.server(),
        repository.as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let create = fixture.request(
        1,
        Command::CreateWorkspace {
            server: fixture.stream.server(),
            source: WorkspaceCreationSource::Directory(directory.clone()),
            name: NameDraft::Custom(NameText::new("Worktree".into()).unwrap()),
        },
    );
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    assert!(matches!(
        handle.submit(fixture.principal, create).await.unwrap(),
        OperationResponse::Completed {
            outcome: Outcome::CreatedWithControl(_),
            ..
        }
    ));
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
    // Seed an existing committed inventory through its actual allocator/reducer.
    // Inventory observation is store-owned metadata, not a fabricated Git write.
    let mut store = physical::reopen(&fixture.directory.0).unwrap();
    let recovered = store.recover().unwrap();
    let mut reducer = OrganizationReducer::from_recovered(
        RetainedOrganization::new(
            recovered.organization.clone(),
            recovered.high_water,
            ServerDirectory::new(
                fixture.stream.server(),
                fixture.directory.0.as_os_str().as_encoded_bytes().to_vec(),
            )
            .unwrap(),
        ),
        recovered.store_revision,
        recovered.last_epoch,
        OrganizationLimits::default(),
    )
    .unwrap();
    let tab = recovered.organization.tabs[0].id;
    let pane = recovered.organization.panes[0].id;
    let context = ReductionContext {
        principal: MutationPrincipal {
            pairing: fixture.stream.pairing(),
            client: fixture.stream.client(),
        },
        operation: OperationId::new(fixture.stream, OperationSequence::new(2).unwrap()),
        request_digest: RequestDigest::from_bytes([9; 32]),
        now: ReducerTick::new(1),
        facts: ReductionFacts::default(),
    };
    let prepared = reducer
        .prepare_git_inventory_reconciliation(
            &GitInventoryPatch {
                repository: None,
                directory: directory.clone(),
                expected_inventory_version: None,
                worktrees: vec![GitInventoryWorktree {
                    id: None,
                    directory: directory.clone(),
                    main: true,
                    missing: false,
                    listed: true,
                }],
            },
            &[GitInventoryMembership {
                tab,
                expected_version: FieldVersion::new(1),
                destination: Some(0),
            }],
            &context,
        )
        .unwrap();
    let mut parts = spaceterm_server::adapters::core::CommitCursor::from_recovered(&recovered)
        .parts()
        .unwrap();
    parts.resulting_organization_revision = prepared.resulting_organization_revision();
    parts.resulting_high_water = prepared.writes().resulting_high_water();
    parts.organization_writes = prepared.writes().organization_writes().to_vec();
    parts.operation_writes = prepared
        .writes()
        .operation_writes()
        .iter()
        .filter(|write| !matches!(write, OperationWrite::ReductionBasis(_)))
        .cloned()
        .collect();
    let transaction =
        DurableTransaction::new_with_validation(parts, &physical::services()).unwrap();
    let CommitDisposition::Durable(receipt) = store.commit(&transaction) else {
        panic!("inventory commit")
    };
    reducer.apply_committed(prepared, &receipt).unwrap();
    let worktree = reducer.snapshot().state().worktrees[0].id;
    drop(store);
    let mut fixture = Fixture::reopen(fixture.directory);
    let home = ServerDirectory::new(
        fixture.stream.server(),
        fixture.directory.0.as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let provider = Arc::new(
        spaceterm_host::server_directories::native::NativeDirectoryProvider::new(
            fixture.stream.server(),
            fixture.directory.0.clone(),
        )
        .unwrap(),
    );
    let git = Arc::new(spaceterm_server::git::host::NativeServerGitHost::new(
        Arc::new(spaceterm_host::git::UnixRepositoryProgramRunner::new()),
        "/usr/bin/git".into(),
        fixture.directory.0.clone(),
        spaceterm_host::git::MINIMUM_GIT_VERSION,
        provider,
    ));
    fixture.config.core = CorePorts::production_with_git_host(
        physical::identity(),
        home,
        Arc::new(native::Clock),
        git,
    )
    .unwrap();
    let split = fixture.request(
        2,
        Command::CreatePane {
            tab,
            source: pane,
            edge: SplitEdge::After,
            axis: SplitAxis::Horizontal,
        },
    );
    let create_tab = fixture.request(
        3,
        Command::CreateTab {
            workspace: WorkspaceId::new(fixture.stream.server(), 1).unwrap(),
            source: TabCreationSource::Worktree(worktree),
        },
    );
    let create_from_pane = fixture.request(
        4,
        Command::CreateTab {
            workspace: WorkspaceId::new(fixture.stream.server(), 1).unwrap(),
            source: TabCreationSource::Pane(pane),
        },
    );
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    for request in [split, create_tab, create_from_pane] {
        assert!(
            matches!(
                handle.submit(fixture.principal, request).await.unwrap(),
                OperationResponse::Completed {
                    outcome: Outcome::CreatedWithControl(_),
                    ..
                }
            ),
            "valid owning Worktree directory must allow creation"
        );
    }
    let snapshot = handle.snapshot();
    assert!(
        snapshot
            .state()
            .panes
            .iter()
            .all(|record| record.starting_directory == directory)
    );
    assert!(
        snapshot
            .state()
            .tabs
            .iter()
            .all(|record| record.worktree == Some(worktree))
    );
    std::fs::rename(repository.join(".git"), repository.join(".git-hidden")).unwrap();
    let refused = CanonicalOperationRequest::new(
        OperationId::new(fixture.stream, OperationSequence::new(5).unwrap()),
        Command::CreatePane {
            tab,
            source: pane,
            edge: SplitEdge::After,
            axis: SplitAxis::Horizontal,
        },
        &CanonicalCodec,
    )
    .unwrap();
    assert!(matches!(
        handle.submit(fixture.principal, refused).await.unwrap(),
        OperationResponse::Completed {
            outcome: Outcome::Failed(OperationFailure::DirectoryUnavailable),
            ..
        }
    ));
    assert_eq!(
        handle.snapshot(),
        snapshot,
        "retained membership cannot replace owning Git discovery"
    );
    assert!(fixture.runtime.starts.lock().unwrap().is_empty());
    handle
        .shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn unstarted_launch_cleanup_does_not_leave_a_real_reducer_retirement() {
    cancelled_launch_settles(false).await;
}

#[tokio::test]
async fn unstarted_launch_evidence_survives_a_definite_failure_commit() {
    cancelled_launch_settles(true).await;
}

async fn cancelled_launch_settles(fail_commit: bool) {
    let fixture = Fixture::new();
    let gate = native::Gate::new();
    *fixture.launch.before_process.lock().unwrap() = Some(gate.clone());
    let create = fixture.request(
        1,
        Command::CreateWorkspace {
            server: fixture.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: fixture.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("Cancelled".into()).unwrap()),
        },
    );
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    let OperationResponse::Completed {
        outcome: Outcome::CreatedWithControl(created),
        ..
    } = handle.submit(fixture.principal, create).await.unwrap()
    else {
        panic!("created")
    };
    assert!(matches!(
        explicit_start_response(
            &handle,
            fixture.principal,
            fixture.stream,
            created.control().grant(),
            2
        )
        .await,
        OperationResponse::Completed {
            outcome: Outcome::SessionStarted(_),
            ..
        }
    ));
    gate.entered().await;
    handle.disconnect(fixture.principal).await.unwrap();
    if fail_commit {
        fixture.faults.fail_once(
            spaceterm_host::storage::testing::FaultPoint::BeforeCommit,
            0,
        );
    }
    gate.release();
    let mut snapshots = handle.subscribe_snapshot();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !matches!(
            snapshots.borrow().runtime()[0].phase,
            SessionPhase::Failed { .. }
        ) {
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(!fixture.faults.fault_pending());
    assert!(fixture.runtime.starts.lock().unwrap().is_empty());
    if fail_commit {
        let restart_gate = native::Gate::new();
        *fixture.launch.before_process.lock().unwrap() = Some(restart_gate.clone());
        native::connect(&handle, fixture.reconnected).await;
        assert!(matches!(
            explicit_start_response(
                &handle,
                fixture.reconnected,
                fixture.stream,
                created.control().grant(),
                4,
            )
            .await,
            OperationResponse::Completed {
                outcome: Outcome::SessionStarted(_),
                ..
            }
        ));
        restart_gate.entered().await;
        handle.disconnect(fixture.reconnected).await.unwrap();
        restart_gate.release();
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !matches!(
                snapshots.borrow().runtime()[0].phase,
                SessionPhase::Failed { .. }
            ) {
                snapshots.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(fixture.runtime.starts.lock().unwrap().is_empty());
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        handle.shutdown(TrustedShutdownRequest {
            authority: fixture.stop,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    server.wait().await.unwrap();
    let mut store = physical::reopen(&fixture.directory.0).unwrap();
    assert!(store.recover().unwrap().native_retirements.is_empty());
}

#[tokio::test]
async fn native_stop_waits_for_unstarted_gate_and_commits_without_process_admission() {
    let fixture = Fixture::new();
    let gate = native::Gate::new();
    *fixture.launch.before_process.lock().unwrap() = Some(gate.clone());
    let create = fixture.request(
        1,
        Command::CreateWorkspace {
            server: fixture.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: fixture.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("Stopped".into()).unwrap()),
        },
    );
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    let OperationResponse::Completed {
        outcome: Outcome::CreatedWithControl(created),
        ..
    } = handle.submit(fixture.principal, create).await.unwrap()
    else {
        panic!("created")
    };
    assert!(matches!(
        explicit_start_response(
            &handle,
            fixture.principal,
            fixture.stream,
            created.control().grant(),
            2
        )
        .await,
        OperationResponse::Completed {
            outcome: Outcome::SessionStarted(_),
            ..
        }
    ));
    gate.entered().await;
    let actor = handle.clone();
    let stopped = tokio::spawn(async move {
        actor
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !matches!(
            handle.phase(),
            ServicePhase::Quiescing | ServicePhase::Draining
        ) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!stopped.is_finished());
    gate.release();
    tokio::time::timeout(std::time::Duration::from_secs(30), stopped)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.wait().await.unwrap();
    assert!(fixture.runtime.starts.lock().unwrap().is_empty());
    let mut store = physical::reopen(&fixture.directory.0).unwrap();
    assert!(store.recover().unwrap().native_retirements.is_empty());
}

#[tokio::test]
async fn retiring_session_does_not_stall_another_sessions_later_lifecycle_publication() {
    use spaceterm_core::{observations::*, retirement::*, runtime_authority::*};
    use std::time::Duration;

    // Release the async fixture callback even if a setup assertion unwinds.
    struct ReleaseRetirement(Arc<native::Gate>, Arc<native::Factory>);
    impl Drop for ReleaseRetirement {
        fn drop(&mut self) {
            self.1.retirement_gate.lock().unwrap().take();
            self.0.release();
        }
    }

    let fixture = Fixture::new();
    let creates = [1, 4].map(|sequence| {
        fixture.request(
            sequence,
            Command::CreateWorkspace {
                server: fixture.stream.server(),
                source: WorkspaceCreationSource::ServerHome {
                    server: fixture.stream.server(),
                },
                name: NameDraft::Automatic,
            },
        )
    });
    let server = RecoveredServer::recover(fixture.config, fixture.dependencies)
        .await
        .unwrap();
    let handle = server.handle();
    native::connect(&handle, fixture.principal).await;
    let mut sessions = Vec::new();
    for (create, sequence) in creates.into_iter().zip([2, 5]) {
        let OperationResponse::Completed {
            outcome: Outcome::CreatedWithControl(created),
            ..
        } = handle.submit(fixture.principal, create).await.unwrap()
        else {
            panic!("created Workspace with control")
        };
        let OperationResponse::Completed {
            outcome: Outcome::SessionStarted(session),
            ..
        } = explicit_start_response(
            &handle,
            fixture.principal,
            fixture.stream,
            created.control().grant(),
            sequence,
        )
        .await
        else {
            panic!("started Session")
        };
        sessions.push(session);
    }
    let [first, second] = sessions.as_slice() else {
        panic!("two Sessions")
    };
    let (first, second) = (*first, *second);
    let mut snapshots = handle.subscribe_snapshot();
    tokio::time::timeout(Duration::from_secs(5), async {
        while ![first, second].iter().all(|session| {
            snapshots.borrow().runtime().iter().any(|record| {
                record.pane == session.pane
                    && record.phase
                        == SessionPhase::Running {
                            generation: session.generation,
                        }
            })
        }) {
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .expect("both Sessions published Running");

    let target = CloseTarget::Pane(first.pane);
    let assess = CanonicalOperationRequest::new(
        OperationId::new(fixture.stream, OperationSequence::new(7).unwrap()),
        Command::AssessClose { target },
        &CanonicalCodec,
    )
    .unwrap();
    let OperationResponse::Completed {
        outcome: Outcome::Assessment(assessment),
        ..
    } = handle.submit(fixture.principal, assess).await.unwrap()
    else {
        panic!("close assessment")
    };
    let close = CanonicalOperationRequest::new(
        OperationId::new(fixture.stream, OperationSequence::new(8).unwrap()),
        Command::CloseConfirmed {
            target,
            assessment: assessment.reference(),
        },
        &CanonicalCodec,
    )
    .unwrap();
    let held = native::Gate::new();
    let release = ReleaseRetirement(held.clone(), fixture.runtime.clone());
    *fixture.runtime.retirement_gate.lock().unwrap() = Some(held.clone());
    assert!(matches!(
        handle.submit(fixture.principal, close).await.unwrap(),
        OperationResponse::Pending(_)
    ));
    tokio::time::timeout(Duration::from_secs(5), held.entered())
        .await
        .expect("first Session entered retirement");
    // Only the first retirement keeps its cloned gate. Teardown must not gate B.
    fixture.runtime.retirement_gate.lock().unwrap().take();
    let states = fixture.runtime.states.lock().unwrap().clone();
    let first_state = states
        .iter()
        .find(|state| state.lock().unwrap().authority.session() == first)
        .unwrap();
    let second_state = states
        .iter()
        .find(|state| state.lock().unwrap().authority.session() == second)
        .unwrap();

    // This new fact cannot be in B's startup cache. Its committed publication
    // proves B answered an inspection after A entered the held retirement.
    let inspected_directory = ServerDirectory::new(
        fixture.stream.server(),
        fixture.directory.0.as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    second_state.lock().unwrap().directory = Some(
        ConfirmedDirectoryObservation::new(
            second,
            RuntimeObservationSequence::new(1).unwrap(),
            MonotonicTick::new(900),
            inspected_directory.clone(),
        )
        .unwrap(),
    );
    let first_inspection = tokio::time::timeout(Duration::from_secs(5), async {
        while !snapshots.borrow().state().panes.iter().any(|record| {
            record.id == second.pane
                && record.confirmed_restoration_directory.value.as_ref()
                    == Some(&inspected_directory)
        }) {
            snapshots.changed().await.unwrap();
        }
    })
    .await;
    let later_lifecycle = if first_inspection.is_ok() {
        assert!(snapshots.borrow().runtime().iter().any(|record| {
            record.pane == second.pane
                && record.phase
                    == SessionPhase::Running {
                        generation: second.generation,
                    }
        }));
        let terminal = SessionTerminalResult::new(
            second,
            RuntimeTerminalStatus::OrdinaryExit {
                exit_status: Some(0),
            },
        )
        .unwrap();
        let cleanup = SessionCleanupObservation::new(
            second,
            RetirementReason::OrdinaryExit,
            CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
        )
        .unwrap();
        {
            let mut state = second_state.lock().unwrap();
            state.lifecycle =
                RuntimeLifecycle::Finished(RuntimeCompletion::new(terminal, cleanup).unwrap());
            state.cleanup = Some(cleanup);
        }
        Some(
            tokio::time::timeout(Duration::from_secs(2), async {
                while snapshots
                    .borrow()
                    .runtime()
                    .iter()
                    .any(|record| record.pane == second.pane)
                {
                    snapshots.changed().await.unwrap();
                }
            })
            .await,
        )
    } else {
        None
    };
    // Check before releasing A, then always release and join before reporting RED.
    let first_still_held = {
        let state = first_state.lock().unwrap();
        state.retire_calls == 1
            && state.cleanup.is_none()
            && matches!(state.lifecycle, RuntimeLifecycle::Retiring { terminal }
                if terminal.retirement_reason() == RetirementReason::ConfirmedClose)
    };
    drop(release);
    let teardown = tokio::time::timeout(Duration::from_secs(10), async {
        // Let a stale baseline catch up after release before requesting Stop, so
        // teardown cannot retag B's already-exited Session with a Stop reason.
        while snapshots
            .borrow()
            .runtime()
            .iter()
            .any(|record| record.pane == second.pane)
        {
            snapshots.changed().await.unwrap();
        }
        let stopped = handle
            .shutdown(TrustedShutdownRequest {
                authority: fixture.stop,
            })
            .await;
        let joined = server.wait().await;
        (stopped, joined)
    })
    .await;
    first_inspection.expect("B published its first inspection while A retirement stayed held");
    let (stopped, joined) = teardown.expect("bounded retirement release and server teardown");
    stopped.unwrap();
    joined.unwrap();
    assert!(
        first_still_held,
        "A kept its original retirement and first reason without cleanup proof"
    );
    later_lifecycle
        .unwrap()
        .expect("B's exit after its first inspection must publish before A retirement is released");
}
