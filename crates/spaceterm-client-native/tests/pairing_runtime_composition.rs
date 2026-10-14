use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::{ClientJournal, JournalLimits, ReceiptVerifier},
};
use spaceterm_client_native::journal::NativeClientJournal;
use spaceterm_core::{operation_ledger::canonical::CanonicalCodec, pairing::SignedPairingReceipt};
use spaceterm_protocol::{authentication::ServerPin, pairing_adapter::ReceiptCodec};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))
    }
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Linux /tmp can be tmpfs, which native storage rejects. Use persistent
        // temporary storage outside the build tree to isolate journal syncs from
        // compiler I/O. Native filesystem and durability checks remain unchanged.
        let parent = if cfg!(target_os = "linux") {
            PathBuf::from("/var/tmp")
        } else {
            std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"))
        };
        std::fs::create_dir_all(&parent).unwrap();
        let path = parent.canonicalize().unwrap().join(format!(
            "spaceterm-client-compose-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
    async fn create(&self) -> NativeClientJournal {
        NativeClientJournal::create(
            self.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding, RouteDescription, RouteParts},
    journal::{
        ClientJournalChange, ClientJournalTransaction, JournalController, JournalCredentialSlot,
        SavedBinding,
    },
    pairing::{
        credentials::*,
        journal::{LocalPairingStore, PreparedEnrollment, SavedPairing},
    },
    pending::LocalUseInvalidator,
    ports::ClientFuture,
};
use spaceterm_core::{
    authority::{GrantedControl, StoreRevision},
    identity::*,
    operations::*,
    pairing::*,
};
use spaceterm_protocol::{ProtocolError, authentication::SigningKey};

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
struct Fixture {
    server: Key,
    binding: PairedServerBinding,
    credential: PreparedCredential,
    saved: SavedBinding,
    route: RevocationRouteReference,
}
impl Fixture {
    fn new(lifetime: u64) -> Self {
        Self::on_server(lifetime, ServerId::new([1; 16]))
    }
    fn on_server(lifetime: u64, id: ServerId) -> Self {
        let server = Key::new(1);
        let pairing = PairingId::new(id, lifetime).unwrap();
        let client = ClientId::new(id, lifetime).unwrap();
        let stream = |local| {
            OperationStreamId::new(OperationStreamKey::new(id, local).unwrap(), pairing, client)
                .unwrap()
        };
        let credential = PreparedCredential {
            scope: CredentialScope::new(CredentialScopeParts {
                namespace: CredentialNamespace::new("dev.spaceterm.test".into()).unwrap(),
                purpose: CredentialPurpose::PairingAuthentication { server: id },
                reference: CredentialReference::new(u128::from(lifetime).to_be_bytes()),
            })
            .unwrap(),
            public_key: Key::new(2).public_key(),
        };
        let target = ConnectionTarget::new(
            ServerPin {
                identity: id.bytes(),
                public_key: server.public_key(),
            },
            DeviceId::new([3; 16]),
            pairing,
            client,
            RouteDescription::new(RouteParts::Ipc {
                directory: "/local/client-route".into(),
                name: "server".into(),
            })
            .unwrap(),
            credential.scope.reference(),
            LocalPairingGeneration::new(lifetime).unwrap(),
        )
        .unwrap();
        let binding = PairedServerBinding::new(target);
        let streams = PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: client,
                pairing,
            },
            ordinary: stream(lifetime * 3),
            self_revocation: stream(lifetime * 3 + 1),
        })
        .unwrap();
        let proof = ReceiptCodec::new(binding.target().pin())
            .sign(
                PairingReceiptBody::ClientBinding {
                    target: PairingTarget::new(PairingTargetParts {
                        server: id,
                        device: binding.target().device(),
                        pairing,
                    })
                    .unwrap(),
                    keys: PairingKeys::new(PairingKeysParts {
                        device_identity_public_key: Key::new(3).public_key(),
                        pairing_authentication_public_key: credential.public_key,
                    })
                    .unwrap(),
                    client: streams,
                    operation: OperationBinding {
                        id: OperationId::new(
                            stream(lifetime * 3 + 2),
                            OperationSequence::new(1).unwrap(),
                        ),
                        digest: RequestDigest::from_bytes([1; 32]),
                    },
                    revision: StoreRevision::new(1),
                },
                &server,
            )
            .unwrap();
        let saved = SavedBinding::verified(
            binding.clone(),
            match &proof.parts().claims {
                PairingReceiptBody::ClientBinding { client, .. } => client.clone(),
                _ => unreachable!(),
            },
            proof,
            &Verifier,
        )
        .unwrap();
        Self {
            server,
            binding,
            credential,
            saved,
            route: RevocationRouteReference::new([9; 16]),
        }
    }
    fn seed_changes(&self) -> Vec<ClientJournalChange> {
        vec![
            ClientJournalChange::ReserveCredential(JournalCredentialSlot {
                namespace: self.credential.scope.parts().namespace.as_str().to_owned(),
                server: Some(self.binding.target().server()),
                reference: self.credential.scope.reference(),
                public_key: None,
                route_reference: None,
                route: None,
                ssh_capture: None,
                generation: self.binding.target().incarnation(),
            }),
            ClientJournalChange::RecordCredentialPublicKey {
                reference: self.credential.scope.reference(),
                public_key: self.credential.public_key,
            },
            ClientJournalChange::BindCredentialRoute {
                reference: self.credential.scope.reference(),
                route_reference: self.route,
                route: self.binding.target().route().clone(),
            },
            ClientJournalChange::SaveBinding(Box::new(self.saved.clone())),
        ]
    }
    async fn seed(&self, journal: &dyn ClientJournal) {
        let tx = ClientJournalTransaction {
            expected_revision: journal.recover().await.unwrap().revision(),
            changes: self.seed_changes(),
        };
        assert!(matches!(
            journal.commit(&tx).await.unwrap(),
            spaceterm_client::journal::JournalCommitDisposition::Durable(_)
        ));
    }
}
async fn controller(journal: Arc<NativeClientJournal>) -> Arc<JournalController> {
    Arc::new(
        JournalController::recover(
            journal,
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap(),
    )
}
struct NoEffects;
impl LocalUseInvalidator for NoEffects {
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
    fn invalidate_pairing(&self, _: &LocalPairingKey) {}
    fn remove_pairing<'a>(&'a self, _: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

use spaceterm_client::pairing::enrollment::*;
use spaceterm_client_native::pairing_runtime::NativePairingRuntime;
use spaceterm_protocol::authentication::HandshakeConfig;

fn runtime(journal: Arc<JournalController>) -> NativePairingRuntime {
    runtime_with(journal, Arc::new(Credentials::new()), Arc::new(Offline))
}
fn runtime_with(
    journal: Arc<JournalController>,
    credentials: Arc<Credentials>,
    connector: Arc<dyn EnrollmentConnector>,
) -> NativePairingRuntime {
    NativePairingRuntime::with_adapters(
        journal,
        credentials,
        connector,
        Arc::new(Offline),
        Arc::new(NoEffects),
        HandshakeConfig::default(),
    )
}
struct Offline;
impl EnrollmentConnector for Offline {
    fn open<'a>(
        &'a self,
        _: ServerPin,
        _: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async { Err(PairingFailure::Offline) })
    }
}
impl spaceterm_client::pairing::revocation::RestrictedConnector for Offline {
    fn open<'a>(
        &'a self,
        _: &'a PendingRevocationTask,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async { Err(PairingFailure::Offline) })
    }
}
struct Credentials {
    device: Arc<Key>,
    auth: Arc<Key>,
    items: std::sync::Mutex<Vec<PreparedCredential>>,
    lose_creation_reply: std::sync::atomic::AtomicBool,
    deletions: std::sync::Mutex<Vec<CredentialScope>>,
    lose_deletion_reply: std::sync::atomic::AtomicBool,
    cleanup_journal: Mutex<std::sync::Weak<JournalController>>,
}
impl Credentials {
    fn new() -> Self {
        Self {
            device: Arc::new(Key::new(3)),
            auth: Arc::new(Key::new(2)),
            items: Default::default(),
            lose_creation_reply: Default::default(),
            deletions: Default::default(),
            lose_deletion_reply: Default::default(),
            cleanup_journal: Default::default(),
        }
    }
}
impl NativeDeviceCredentials for Credentials {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            let scope = reservation.scope().clone();
            let public_key = if scope.parts().purpose == CredentialPurpose::DeviceIdentity {
                self.device.public_key()
            } else {
                self.auth.public_key()
            };
            let credential = PreparedCredential { scope, public_key };
            self.items.lock().unwrap().push(credential.clone());
            if self.lose_creation_reply.swap(false, Ordering::SeqCst) {
                return Err(PairingFailure::CredentialUnavailable);
            }
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
                self.device.clone()
            } else {
                self.auth.clone()
            };
            assert_eq!(credential.public_key, key.public_key());
            Ok(key as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async move {
            assert!(matches!(
                credential.scope.parts().purpose,
                CredentialPurpose::PairingAuthentication { .. }
            ));
            if let Some(journal) = self.cleanup_journal.lock().unwrap().upgrade() {
                let inventory = journal.snapshot().unwrap().pairing_snapshot().unwrap();
                assert!(inventory.revocations.iter().any(|pending| pending.credential == *credential
                    && matches!(pending.phase, spaceterm_client::pairing::journal::RevocationPhase::ReceiptRecorded { acknowledged: true, .. })));
            }
            self.deletions
                .lock()
                .unwrap()
                .push(credential.scope.clone());
            let mut items = self.items.lock().unwrap();
            if let Some(index) = items.iter().position(|item| item == credential) {
                items.remove(index);
                if self.lose_deletion_reply.swap(false, Ordering::SeqCst) {
                    return Err(PairingFailure::CredentialUnavailable);
                }
                Ok(CredentialDeletion::Deleted)
            } else {
                Ok(CredentialDeletion::AlreadyMissing)
            }
        })
    }
}

#[tokio::test]
async fn restored_consumer_uses_exact_saved_identity_and_never_borrows_another_client() {
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let fixture = Fixture::new(1);
    fixture.seed(native.as_ref()).await;
    let journal = controller(native).await;
    let composition = runtime(journal);
    let key = fixture.binding.target().local_pairing().unwrap();
    let restored = composition
        .restore(&key, fixture.binding.target().client())
        .await
        .unwrap();
    assert_eq!(restored.binding(), &fixture.saved);
    assert!(
        restored
            .authority()
            .lease()
            .validate(&fixture.credential.scope)
            .is_ok()
    );
    assert!(matches!(
        composition
            .restore(&key, ClientId::new(key.parts().server, 99).unwrap())
            .await,
        Err(PairingFailure::ClientNotBound)
    ));
}

#[tokio::test]
async fn removal_seals_existing_lease_and_reopen_keeps_dismissed_retry_without_restoration() {
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let fixture = Fixture::new(1);
    fixture.seed(native.as_ref()).await;
    let key = fixture.binding.target().local_pairing().unwrap();
    let journal = controller(native).await;
    let composition = runtime(journal);
    let ordinary = composition
        .restore(&key, fixture.binding.target().client())
        .await
        .unwrap();
    let lease = ordinary.authority().lease();
    let assessment = composition
        .assess_unpair(&key, fixture.binding.target().client())
        .await
        .unwrap();
    let receipt = composition.self_unpair(assessment).await.unwrap();
    assert!(lease.validate(&fixture.credential.scope).is_err());
    composition.dismiss_notice(receipt.operation).await.unwrap();
    drop(ordinary);
    drop(composition);
    let native = NativeClientJournal::open_existing(
        directory.0.clone(),
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let reopened = runtime(controller(Arc::new(native)).await);
    let inventory = reopened.inventory().await.unwrap();
    assert!(inventory.is_suppressed(&key));
    assert!(inventory.task(receipt.operation).unwrap().notice_dismissed);
    assert_eq!(inventory.revocations.len(), 1);
    let pending = reopened.pending_work().await.unwrap();
    assert_eq!(
        pending[0].phase,
        spaceterm_client_native::pairing_runtime::PendingPairingPhase::RevocationConfirmation
    );
    assert_eq!(
        pending[0].reason,
        spaceterm_client_native::pairing_runtime::PendingPairingReason::ServerConfirmationPending
    );
    assert!(pending[0].notice_dismissed);
    assert!(matches!(
        reopened
            .restore(&key, fixture.binding.target().client())
            .await,
        Err(PairingFailure::PairingRevoked)
    ));
    let scheduler = reopened.start_scheduler().unwrap();
    assert!(reopened.start_scheduler().is_err());
    scheduler.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupted_native_preparation_reopens_original_reference_without_creating_a_replacement()
{
    let directory = Directory::new();
    let credentials = Arc::new(Credentials::new());
    let scope = CredentialScope::new(CredentialScopeParts {
        namespace: CredentialNamespace::new("dev.spaceterm.runtime".into()).unwrap(),
        purpose: CredentialPurpose::PairingAuthentication {
            server: ServerId::new([1; 16]),
        },
        reference: CredentialReference::new([70; 16]),
    })
    .unwrap();
    let composition = runtime_with(
        controller(Arc::new(directory.create().await)).await,
        credentials.clone(),
        Arc::new(Offline),
    );
    credentials
        .lose_creation_reply
        .store(true, Ordering::SeqCst);
    assert_eq!(
        composition.prepare_credential(scope.clone()).await,
        Err(PairingFailure::CredentialUnavailable)
    );
    let preparation = composition
        .inventory()
        .await
        .unwrap()
        .credential_preparations
        .remove(0);
    assert_eq!(preparation.scope, scope);
    assert!(preparation.credential.is_none());
    drop(composition);
    let native = NativeClientJournal::open_existing(
        directory.0.clone(),
        Arc::new(CanonicalCodec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let composition = runtime_with(
        controller(Arc::new(native)).await,
        credentials.clone(),
        Arc::new(Offline),
    );
    let restored = composition
        .recover_credential(preparation.generation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.scope, scope);
    assert_eq!(credentials.items.lock().unwrap().len(), 1);
    assert_eq!(
        composition
            .inventory()
            .await
            .unwrap()
            .credential_preparations[0]
            .credential
            .as_ref(),
        Some(&restored)
    );
}

use spaceterm_protocol::pairing_adapter::{
    EnrollmentAuthorization, PublicEnrollmentReservation, operation_to_wire, receipt_to_wire,
    reservation_to_wire,
};
use spaceterm_protocol::wire::pairing;
use std::{collections::VecDeque, sync::Mutex};
use zeroize::Zeroizing;
#[derive(Clone)]
struct EnrollmentEndpoint {
    server: Arc<Key>,
    reservation: Arc<Mutex<EnrollmentReservation>>,
    receipt: SignedPairingReceipt,
    major: u32,
    submissions: Arc<Mutex<usize>>,
    lookups: Arc<Mutex<usize>>,
    acknowledgements: Arc<Mutex<usize>>,
    journal: Arc<std::sync::Mutex<std::sync::Weak<JournalController>>>,
    lose_ack_reply: Arc<Mutex<bool>>,
    lose_initial_reply: Arc<std::sync::atomic::AtomicBool>,
}
impl spaceterm_protocol::pairing_adapter::EnrollmentLookup for EnrollmentEndpoint {
    fn lookup(&self, ticket: &EnrollmentTicketId) -> Result<EnrollmentReservation, ProtocolError> {
        let r = self.reservation.lock().unwrap();
        assert_eq!(ticket, &r.parts().ticket);
        Ok(r.clone())
    }
}
impl EnrollmentConnector for EnrollmentEndpoint {
    fn open<'a>(
        &'a self,
        pin: ServerPin,
        _: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async move {
            assert_eq!(pin.public_key, self.server.public_key());
            Ok(Box::new(EnrollmentTransport {
                endpoint: self.clone(),
                handshake: None,
                connection: None,
                replies: VecDeque::new(),
            }) as Box<dyn PairingTransport>)
        })
    }
}
struct EnrollmentTransport {
    endpoint: EnrollmentEndpoint,
    handshake: Option<spaceterm_protocol::pairing_adapter::EnrollmentServer>,
    connection: Option<spaceterm_protocol::pairing_adapter::EnrollmentConnection>,
    replies: VecDeque<Vec<u8>>,
}
impl PairingTransport for EnrollmentTransport {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            use spaceterm_protocol::pairing_adapter::EnrollmentServer;
            if let Some(connection) = &mut self.connection {
                let historical = matches!(
                    self.endpoint.reservation.lock().unwrap().parts().state,
                    EnrollmentReservationState::Consumed { .. }
                );
                if !historical {
                    let request = connection.receive_submission(bytes).unwrap();
                    assert_eq!(
                        request.authorization_digest(),
                        self.endpoint
                            .reservation
                            .lock()
                            .unwrap()
                            .parts()
                            .authorization_digest
                    );
                    *self.endpoint.submissions.lock().unwrap() += 1;
                    let mut p = self.endpoint.reservation.lock().unwrap().parts().clone();
                    p.state = EnrollmentReservationState::Consumed {
                        enrollment: OperationBinding {
                            id: OperationId::new(
                                p.enrollment_stream,
                                OperationSequence::new(1).unwrap(),
                            ),
                            digest: p.enrollment_request_digest,
                        },
                        receipt: Box::new(self.endpoint.receipt.clone()),
                    };
                    *self.endpoint.reservation.lock().unwrap() =
                        EnrollmentReservation::new(p).unwrap();
                    if self
                        .endpoint
                        .lose_initial_reply
                        .swap(false, Ordering::SeqCst)
                    {
                        // Commit happened, but the original reply is lost.
                        return Ok(());
                    }
                    let p = self.endpoint.reservation.lock().unwrap().parts().clone();
                    let result = pairing::EnrollmentMessage {
                        kind: Some(pairing::enrollment_message::Kind::Result(
                            pairing::EnrollmentResult {
                                operation: Some(operation_to_wire(OperationId::new(
                                    p.enrollment_stream,
                                    OperationSequence::new(1).unwrap(),
                                ))),
                                request_digest: p.enrollment_request_digest.bytes().to_vec(),
                                receipt: Some(receipt_to_wire(&self.endpoint.receipt)),
                                failure: None,
                            },
                        )),
                    };
                    self.replies.push_back(connection.encode(&result).unwrap());
                    return Ok(());
                }
                let message = connection.decode(bytes).unwrap();
                if let Some(pairing::enrollment_message::Kind::Acknowledgement(ack)) = message.kind
                {
                    assert!(
                        !self
                            .endpoint
                            .journal
                            .lock()
                            .unwrap()
                            .upgrade()
                            .unwrap()
                            .snapshot()
                            .unwrap()
                            .bindings()
                            .is_empty(),
                        "enrollment receipt must be durable before acknowledgement"
                    );
                    assert_eq!(ack.acknowledged_through, 1);
                    let p = self.endpoint.reservation.lock().unwrap().parts().clone();
                    assert_eq!(
                        ack.operation,
                        Some(operation_to_wire(OperationId::new(
                            p.enrollment_stream,
                            OperationSequence::new(1).unwrap()
                        )))
                    );
                    assert_eq!(ack.request_digest, p.enrollment_request_digest.bytes());
                    *self.endpoint.acknowledgements.lock().unwrap() += 1;
                    return Ok(());
                }
                assert!(matches!(
                    message.kind,
                    Some(pairing::enrollment_message::Kind::Lookup(_))
                ));
                *self.endpoint.lookups.lock().unwrap() += 1;
                if *self.endpoint.acknowledgements.lock().unwrap() > 0
                    && std::mem::take(&mut *self.endpoint.lose_ack_reply.lock().unwrap())
                {
                    return Ok(());
                }
                let p = self.endpoint.reservation.lock().unwrap().parts().clone();
                let operation = operation_to_wire(OperationId::new(
                    p.enrollment_stream,
                    OperationSequence::new(1).unwrap(),
                ));
                let result = pairing::EnrollmentMessage {
                    kind: Some(pairing::enrollment_message::Kind::Result(
                        pairing::EnrollmentResult {
                            operation: Some(operation),
                            request_digest: p.enrollment_request_digest.bytes().to_vec(),
                            receipt: Some(receipt_to_wire(&self.endpoint.receipt)),
                            failure: None,
                        },
                    )),
                };
                self.replies.push_back(connection.encode(&result).unwrap());
            } else if let Some(handshake) = self.handshake.take() {
                let (connection, ready) = handshake.receive_proof(&self.endpoint, bytes).unwrap();
                assert_eq!(connection.major(), self.endpoint.major);
                self.connection = Some(connection);
                self.replies.push_back(ready);
            } else {
                let server = self
                    .endpoint
                    .reservation
                    .lock()
                    .unwrap()
                    .parts()
                    .target
                    .parts()
                    .server
                    .bytes();
                let (handshake, reply) = EnrollmentServer::accept(
                    server,
                    vec![self.endpoint.major],
                    self.endpoint.server.clone(),
                    &self.endpoint,
                    bytes,
                )
                .unwrap();
                self.handshake = Some(handshake);
                self.replies.push_back(reply);
            }
            Ok(())
        })
    }
    fn receive(&mut self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, PairingFailure>> {
        Box::pin(async {
            self.replies
                .pop_front()
                .map(Zeroizing::new)
                .ok_or(PairingFailure::Offline)
        })
    }
}
struct EnrollmentFixture {
    fixture: Fixture,
    prepared: PreparedEnrollment,
    saved: SavedPairing,
    device: PreparedCredential,
    authorization: EnrollmentAuthorization,
    endpoint: Arc<EnrollmentEndpoint>,
}
impl EnrollmentFixture {
    async fn prepare(composition: &NativePairingRuntime, major: u32) -> Self {
        let fixture = Fixture::new(2);
        let device_scope = CredentialScope::new(CredentialScopeParts {
            namespace: fixture.credential.scope.parts().namespace.clone(),
            purpose: CredentialPurpose::DeviceIdentity,
            reference: CredentialReference::new([33; 16]),
        })
        .unwrap();
        let (_, device) = composition.prepare_credential(device_scope).await.unwrap();
        let (generation, credential) = composition
            .prepare_credential(fixture.credential.scope.clone())
            .await
            .unwrap();
        assert_eq!(generation.get(), 2);
        let keys = match &fixture.saved.proof().parts().claims {
            PairingReceiptBody::ClientBinding { keys, .. } => keys.clone(),
            _ => unreachable!(),
        };
        let enrollment = match &fixture.saved.proof().parts().claims {
            PairingReceiptBody::ClientBinding { operation, .. } => *operation,
            _ => unreachable!(),
        };
        let target = PairingTarget::new(PairingTargetParts {
            server: fixture.binding.target().server(),
            pairing: fixture.binding.target().pairing(),
            device: fixture.binding.target().device(),
        })
        .unwrap();
        let authorization = EnrollmentAuthorization::generate().unwrap();
        let reservation = EnrollmentReservation::new(EnrollmentReservationParts {
            ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
                server: target.parts().server,
                nonce: [3; 16],
            })
            .unwrap(),
            target: target.clone(),
            keys: keys.clone(),
            initial_client: fixture.binding.target().client(),
            enrollment_stream: enrollment.id.stream(),
            claim_digest: RequestDigest::from_bytes([4; 32]),
            authorization_digest: authorization.digest(),
            enrollment_request_digest: enrollment.digest,
            issuer: EnrollmentIssuer::AccountOwner {
                native_request: [8; 16],
                native_request_digest: RequestDigest::from_bytes([9; 32]),
            },
            issued_epoch: spaceterm_core::authority::ServerEpoch::new(1),
            issued_revision: StoreRevision::new(1),
            issued_at_millis: 0,
            expires_at_millis: 1,
            state: EnrollmentReservationState::Reserved,
        })
        .unwrap();
        let public =
            PublicEnrollmentReservation::from_wire(&reservation_to_wire(&reservation)).unwrap();
        let prepared = PreparedEnrollment {
            local_pairing: fixture.binding.target().local_pairing().unwrap(),
            device: fixture.binding.target().device(),
            pin: fixture.binding.target().pin(),
            keys: keys.clone(),
            client: fixture.binding.target().client(),
            credential,
            route: fixture.route,
            enrollment,
            reservation: Some(public),
        };
        let proof = ReceiptCodec::new(prepared.pin)
            .sign(
                PairingReceiptBody::Enrollment {
                    target,
                    keys,
                    client: fixture.saved.streams().clone(),
                    enrollment,
                    revision: StoreRevision::new(1),
                },
                &fixture.server,
            )
            .unwrap();
        let saved = verify_binding(&prepared, proof.clone(), false).unwrap();
        let endpoint = Arc::new(EnrollmentEndpoint {
            server: Arc::new(Key::new(1)),
            reservation: Arc::new(Mutex::new(reservation)),
            receipt: proof,
            major,
            submissions: Default::default(),
            lookups: Default::default(),
            acknowledgements: Default::default(),
            journal: Arc::new(Mutex::new(Arc::downgrade(composition.journal()))),
            lose_ack_reply: Arc::new(Mutex::new(true)),
            lose_initial_reply: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        });
        Self {
            fixture,
            prepared,
            saved,
            device,
            authorization,
            endpoint,
        }
    }
}

#[tokio::test]
async fn physical_reopen_recovers_original_enrollment_and_blocks_registration_until_ack_in_both_majors()
 {
    for major in [1, 2] {
        let directory = Directory::new();
        let credentials = Arc::new(Credentials::new());
        let composition = runtime_with(
            controller(Arc::new(directory.create().await)).await,
            credentials.clone(),
            Arc::new(Offline),
        );
        let fixture = EnrollmentFixture::prepare(&composition, major).await;
        let journal = composition.journal().clone();
        drop(composition);
        let composition = runtime_with(journal, credentials.clone(), fixture.endpoint.clone());
        let attempt =
            DirectEnrollmentAttempt::new(fixture.prepared.pin, fixture.prepared.route, 1).unwrap();
        let verified = attempt
            .confirm(
                fixture.prepared.pin,
                fixture.prepared.route,
                1,
                &attempt.fingerprint(),
            )
            .unwrap();
        let request = |authorization| EnrollmentAttemptRequest {
            verified: &verified,
            prepared: &fixture.prepared,
            reservation: fixture.prepared.reservation.clone().unwrap(),
            device: &fixture.device,
            authorization,
            majors: vec![2, 1],
        };
        assert_eq!(
            composition
                .enroll(
                    request(Some(&fixture.authorization)),
                    fixture.fixture.binding.target().route().clone()
                )
                .await,
            Err(PairingFailure::Offline)
        );
        let inventory = composition.inventory().await.unwrap();
        assert!(inventory.saved.is_empty());
        assert_eq!(inventory.prepared, vec![fixture.prepared.clone()]);
        drop(composition);
        let native = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let composition = runtime_with(
            controller(Arc::new(native)).await,
            credentials.clone(),
            fixture.endpoint.clone(),
        );
        *fixture.endpoint.journal.lock().unwrap() = Arc::downgrade(composition.journal());
        assert_eq!(
            composition
                .enroll(
                    request(None),
                    fixture.fixture.binding.target().route().clone()
                )
                .await,
            Err(PairingFailure::RecoveryRequired)
        );
        assert_eq!(
            composition.inventory().await.unwrap().saved,
            vec![fixture.saved.clone()]
        );
        assert!(matches!(
            composition
                .restore(fixture.saved.key(), fixture.prepared.client)
                .await,
            Err(PairingFailure::RecoveryRequired)
        ));
        drop(composition);
        attempt.cancel();
        let native = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let composition = runtime_with(
            controller(Arc::new(native)).await,
            credentials,
            fixture.endpoint.clone(),
        );
        *fixture.endpoint.journal.lock().unwrap() = Arc::downgrade(composition.journal());
        let scheduler = composition.start_scheduler().unwrap();
        let mut changes = composition.changes();
        let completed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if composition
                    .inventory()
                    .await
                    .unwrap()
                    .enrollment_acknowledgements
                    .is_empty()
                {
                    break;
                }
                changes.changed().await.unwrap();
            }
        })
        .await;
        scheduler.shutdown().await.unwrap();
        assert!(
            completed.is_ok(),
            "the journal scheduler must settle original enrollment acknowledgements"
        );
        let ordinary = composition
            .restore(fixture.saved.key(), fixture.prepared.client)
            .await
            .unwrap();
        assert_eq!(ordinary.binding().proof(), fixture.saved.receipt());
        assert_eq!(*fixture.endpoint.submissions.lock().unwrap(), 1);
        assert_eq!(*fixture.endpoint.acknowledgements.lock().unwrap(), 2);
        assert_eq!(ordinary.binding().streams(), fixture.saved.client());
        let retry_attempt =
            DirectEnrollmentAttempt::new(fixture.prepared.pin, fixture.prepared.route, 2).unwrap();
        let retry_verified = retry_attempt
            .confirm(
                fixture.prepared.pin,
                fixture.prepared.route,
                2,
                &retry_attempt.fingerprint(),
            )
            .unwrap();
        assert_eq!(
            composition
                .enroll(
                    EnrollmentAttemptRequest {
                        verified: &retry_verified,
                        prepared: &fixture.prepared,
                        reservation: fixture.prepared.reservation.clone().unwrap(),
                        device: &fixture.device,
                        authorization: None,
                        majors: vec![2, 1],
                    },
                    fixture.fixture.binding.target().route().clone()
                )
                .await
                .unwrap(),
            fixture.saved
        );
        assert_eq!(*fixture.endpoint.submissions.lock().unwrap(), 1);
        assert_eq!(*fixture.endpoint.acknowledgements.lock().unwrap(), 2);
    }
}

use spaceterm_client::pairing::journal::LocalRevocation;
use spaceterm_protocol::authentication::{
    self, Established, PairingLookup, Principal, ServerHandshake,
};
use spaceterm_protocol::connection::RestrictedConnection;
use spaceterm_protocol::pairing_adapter::{PairingRestrictedCodec, target_to_wire};
struct Lookup {
    principal: Principal,
    key: [u8; 32],
}
impl PairingLookup for Lookup {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<authentication::PairingRecord, ProtocolError> {
        assert_eq!(
            (server, pairing, client),
            (
                self.principal.server,
                self.principal.pairing,
                self.principal.client
            )
        );
        Ok(authentication::PairingRecord {
            principal: self.principal,
            device_public_key: self.key,
            active: false,
        })
    }
}
#[derive(Clone)]
struct Connector {
    server: Arc<Key>,
    auth: Arc<Key>,
    pending: LocalRevocation,
    proof: SignedPairingReceipt,
    major: u32,
    disposition: i32,
    failure: Option<PairingFailure>,
    opens: Arc<Mutex<usize>>,
    lose_revocation_reply: Arc<std::sync::atomic::AtomicBool>,
    lose_ack_reply: Arc<std::sync::atomic::AtomicBool>,
}
impl spaceterm_client::pairing::revocation::RestrictedConnector for Connector {
    fn open<'a>(
        &'a self,
        task: &'a PendingRevocationTask,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async move {
            assert_eq!(task, &self.pending.task);
            *self.opens.lock().unwrap() += 1;
            if let Some(failure) = self.failure {
                return Err(failure);
            }
            Ok(Box::new(Transport {
                connector: self.clone(),
                handshake: None,
                connection: None,
                replies: VecDeque::new(),
            }) as Box<dyn PairingTransport>)
        })
    }
}
struct Transport {
    connector: Connector,
    handshake: Option<(ServerHandshake, Lookup)>,
    connection: Option<RestrictedConnection>,
    replies: VecDeque<Vec<u8>>,
}
impl PairingTransport for Transport {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            let p = self.connector.pending.task.parts();
            let pin = ServerPin {
                identity: p.original.server().bytes(),
                public_key: self.connector.server.public_key(),
            };
            if let Some(connection) = &mut self.connection {
                let codec = PairingRestrictedCodec::new(pin);
                let decoded = connection.decode_pairing_extension(bytes, &codec).unwrap();
                match decoded.kind.unwrap() {
                    pairing::restricted_extension::Kind::Acknowledgement(_) => (),
                    kind @ (pairing::restricted_extension::Kind::Revoke(_)
                    | pairing::restricted_extension::Kind::Status(_)) => {
                        if matches!(kind, pairing::restricted_extension::Kind::Revoke(_))
                            && self
                                .connector
                                .lose_revocation_reply
                                .swap(false, Ordering::SeqCst)
                        {
                            return Ok(());
                        }
                        if matches!(kind, pairing::restricted_extension::Kind::Status(_))
                            && self.connector.lose_ack_reply.swap(false, Ordering::SeqCst)
                        {
                            return Ok(());
                        }
                        let target = PairingTarget::new(PairingTargetParts {
                            server: p.original.server(),
                            pairing: p.original.pairing(),
                            device: p.original.device(),
                        })
                        .unwrap();
                        let result = pairing::RestrictedExtension {
                            kind: Some(pairing::restricted_extension::Kind::Result(
                                pairing::RestrictedResult {
                                    request_operation: Some(operation_to_wire(
                                        p.original.operation(),
                                    )),
                                    request_digest: p.request_digest.bytes().to_vec(),
                                    target: Some(target_to_wire(&target)),
                                    disposition: self.connector.disposition,
                                    receipt: Some(receipt_to_wire(&self.connector.proof)),
                                },
                            )),
                        };
                        self.replies.push_back(
                            connection
                                .encode_pairing_extension(&result, &codec)
                                .unwrap(),
                        );
                    }
                    _ => panic!("restricted retry escalated purpose"),
                }
            } else if let Some((handshake, lookup)) = self.handshake.take() {
                let (established, reply) = handshake.receive_device(&lookup, bytes).unwrap();
                assert!(matches!(established, Established::Restricted(_)));
                if let Established::Restricted(connection) = established {
                    assert_eq!(connection.major(), self.connector.major);
                    self.connection = Some(connection);
                }
                self.replies.push_back(reply);
            } else {
                let local = |value: u64| {
                    let mut bytes = [0; 16];
                    bytes[8..].copy_from_slice(&value.to_be_bytes());
                    bytes
                };
                let lookup = Lookup {
                    principal: Principal {
                        server: pin.identity,
                        device: p.original.device().bytes(),
                        pairing: local(p.original.pairing().local()),
                        client: local(p.original.client().local()),
                    },
                    key: self.connector.auth.public_key(),
                };
                let (handshake, reply) = ServerHandshake::accept(
                    pin.identity,
                    8,
                    HandshakeConfig {
                        majors: vec![self.connector.major],
                        ..HandshakeConfig::default()
                    },
                    self.connector.server.clone(),
                    &lookup,
                    bytes,
                )
                .unwrap();
                self.handshake = Some((handshake, lookup));
                self.replies.push_back(reply);
            }
            Ok(())
        })
    }
    fn receive(&mut self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, PairingFailure>> {
        Box::pin(async {
            self.replies
                .pop_front()
                .map(Zeroizing::new)
                .ok_or(PairingFailure::Offline)
        })
    }
}
#[tokio::test]
async fn acknowledged_cleanup_reopens_after_exact_key_deletion_and_preserves_newer_pairing_in_both_majors()
 {
    for major in [1, 2] {
        let directory = Directory::new();
        let credentials = Arc::new(Credentials::new());
        let native = Arc::new(directory.create().await);
        let old = Fixture::new(1);
        let other = Fixture::on_server(2, ServerId::new([2; 16]));
        old.seed(native.as_ref()).await;
        other.seed(native.as_ref()).await;
        let journal = controller(native).await;
        let composition = runtime_with(journal.clone(), credentials.clone(), Arc::new(Offline));
        let removal = composition
            .self_unpair(
                composition
                    .assess_unpair(
                        &old.binding.target().local_pairing().unwrap(),
                        old.binding.target().client(),
                    )
                    .await
                    .unwrap(),
            )
            .await
            .unwrap();
        let newer = Fixture::new(3);
        let native_store = composition.journal();
        let tx = ClientJournalTransaction {
            expected_revision: native_store.snapshot().unwrap().revision(),
            changes: newer.seed_changes(),
        };
        assert!(matches!(
            native_store.commit(&tx).await.unwrap(),
            spaceterm_client::journal::JournalCommitDisposition::Durable(_)
        ));
        let pending = composition
            .inventory()
            .await
            .unwrap()
            .task(removal.operation)
            .unwrap()
            .clone();
        let receipt = ReceiptCodec::new(old.binding.target().pin())
            .sign(
                PairingReceiptBody::Revocation {
                    receipt: RevocationReceipt {
                        server: old.binding.target().server(),
                        pairing: old.binding.target().pairing(),
                        device: old.binding.target().device(),
                        operation: removal.operation,
                        revision: StoreRevision::new(2),
                        receipt_digest: RequestDigest::from_bytes([0; 32]),
                    },
                    authentication_key_digest: RequestDigest::from_bytes(
                        ring::digest::digest(&ring::digest::SHA256, &old.credential.public_key)
                            .as_ref()
                            .try_into()
                            .unwrap(),
                    ),
                    revocation_event: [10; 16],
                },
                &old.server,
            )
            .unwrap();
        let connector = Arc::new(Connector {
            server: Arc::new(Key::new(1)),
            auth: credentials.auth.clone(),
            pending,
            proof: receipt,
            major,
            disposition: 1,
            failure: None,
            opens: Default::default(),
            lose_revocation_reply: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            lose_ack_reply: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        });
        drop(composition);
        let composition = NativePairingRuntime::with_adapters(
            journal,
            credentials.clone(),
            Arc::new(Offline),
            connector.clone(),
            Arc::new(NoEffects),
            HandshakeConfig::default(),
        );
        let shared_device = PreparedCredential {
            scope: CredentialScope::new(CredentialScopeParts {
                namespace: old.credential.scope.parts().namespace.clone(),
                purpose: CredentialPurpose::DeviceIdentity,
                reference: CredentialReference::new([33; 16]),
            })
            .unwrap(),
            public_key: credentials.device.public_key(),
        };
        credentials.items.lock().unwrap().extend([
            old.credential.clone(),
            other.credential.clone(),
            newer.credential.clone(),
            shared_device.clone(),
        ]);
        *credentials.cleanup_journal.lock().unwrap() = Arc::downgrade(composition.journal());
        credentials
            .lose_deletion_reply
            .store(true, Ordering::SeqCst);
        assert_eq!(
            composition.retry_revocation_once(removal.operation).await,
            Err(PairingFailure::Offline)
        );
        assert!(matches!(
            composition
                .inventory()
                .await
                .unwrap()
                .task(removal.operation)
                .unwrap()
                .phase,
            spaceterm_client::pairing::journal::RevocationPhase::AwaitingConfirmation
        ));
        assert!(credentials.deletions.lock().unwrap().is_empty());
        drop(composition);
        let native = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let composition = NativePairingRuntime::with_adapters(
            controller(Arc::new(native)).await,
            credentials.clone(),
            Arc::new(Offline),
            connector.clone(),
            Arc::new(NoEffects),
            HandshakeConfig::default(),
        );
        *credentials.cleanup_journal.lock().unwrap() = Arc::downgrade(composition.journal());
        assert_eq!(
            composition.retry_revocation_once(removal.operation).await,
            Err(PairingFailure::Offline)
        );
        assert!(matches!(
            composition
                .inventory()
                .await
                .unwrap()
                .task(removal.operation)
                .unwrap()
                .phase,
            spaceterm_client::pairing::journal::RevocationPhase::ReceiptRecorded {
                acknowledged: false,
                ..
            }
        ));
        assert!(credentials.deletions.lock().unwrap().is_empty());
        drop(composition);
        let native = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let composition = NativePairingRuntime::with_adapters(
            controller(Arc::new(native)).await,
            credentials.clone(),
            Arc::new(Offline),
            connector.clone(),
            Arc::new(NoEffects),
            HandshakeConfig::default(),
        );
        *credentials.cleanup_journal.lock().unwrap() = Arc::downgrade(composition.journal());
        assert_eq!(
            composition.retry_revocation_once(removal.operation).await,
            Err(PairingFailure::CredentialUnavailable)
        );
        assert!(matches!(
            composition
                .inventory()
                .await
                .unwrap()
                .task(removal.operation)
                .unwrap()
                .phase,
            spaceterm_client::pairing::journal::RevocationPhase::ReceiptRecorded {
                acknowledged: true,
                ..
            }
        ));
        assert_eq!(*connector.opens.lock().unwrap(), 4);
        assert_eq!(
            composition.pending_work().await.unwrap()[0].phase,
            spaceterm_client_native::pairing_runtime::PendingPairingPhase::CredentialCleanup
        );
        drop(composition);
        let native = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let composition = NativePairingRuntime::with_adapters(
            controller(Arc::new(native)).await,
            credentials.clone(),
            Arc::new(Offline),
            connector.clone(),
            Arc::new(NoEffects),
            HandshakeConfig::default(),
        );
        *credentials.cleanup_journal.lock().unwrap() = Arc::downgrade(composition.journal());
        composition
            .retry_revocation_once(removal.operation)
            .await
            .unwrap();
        assert!(
            composition
                .inventory()
                .await
                .unwrap()
                .revocations
                .is_empty()
        );
        assert_eq!(
            *connector.opens.lock().unwrap(),
            4,
            "cleanup must not reconnect or resubmit after its acknowledgement is durable"
        );
        assert_eq!(
            *credentials.deletions.lock().unwrap(),
            vec![old.credential.scope.clone(), old.credential.scope.clone()]
        );
        assert_eq!(
            *credentials.items.lock().unwrap(),
            vec![
                other.credential.clone(),
                newer.credential.clone(),
                shared_device
            ]
        );
        assert_eq!(
            composition
                .restore(
                    &newer.binding.target().local_pairing().unwrap(),
                    newer.binding.target().client()
                )
                .await
                .unwrap()
                .binding(),
            &newer.saved
        );
        assert_eq!(
            composition
                .restore(
                    &other.binding.target().local_pairing().unwrap(),
                    other.binding.target().client()
                )
                .await
                .unwrap()
                .binding(),
            &other.saved
        );
    }
}

#[tokio::test]
async fn additional_client_receipt_is_acknowledged_before_consumer_registration() {
    let credentials = Arc::new(Credentials::new());
    let original = Fixture::new(1);
    credentials
        .items
        .lock()
        .unwrap()
        .push(original.credential.clone());
    let server = original.binding.target().server();
    let pairing = original.binding.target().pairing();
    let origin_directory = Directory::new();
    let origin_native = Arc::new(origin_directory.create().await);
    original.seed(origin_native.as_ref()).await;
    let mut origin = controller(origin_native).await;
    let unrelated_directory = Directory::new();
    let unrelated = controller(Arc::new(unrelated_directory.create().await)).await;
    let mut consumers = vec![];
    for consumer in [7, 8] {
        let directory = Directory::new();
        let composition = runtime_with(
            controller(Arc::new(directory.create().await)).await,
            credentials.clone(),
            Arc::new(Offline),
        );
        let (generation, credential) = composition
            .retain_existing_credential(
                original.credential.scope.clone(),
                original.credential.public_key,
            )
            .await
            .unwrap();
        let client = ClientId::new(server, consumer).unwrap();
        let stream = |local| {
            OperationStreamId::new(
                OperationStreamKey::new(server, local).unwrap(),
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
            ordinary: stream(100 + consumer * 2),
            self_revocation: stream(101 + consumer * 2),
        })
        .unwrap();
        let (target, keys) = match &original.saved.proof().parts().claims {
            PairingReceiptBody::ClientBinding { target, keys, .. } => {
                (target.clone(), keys.clone())
            }
            _ => unreachable!(),
        };
        use spaceterm_core::operation_ledger_contracts::{
            CanonicalOperationRequest, CompactOutcome,
        };
        let request = CanonicalOperationRequest::new(
            OperationId::new(
                original.saved.streams().parts().ordinary,
                OperationSequence::new(consumer - 6).unwrap(),
            ),
            Command::Pairing(PairingCommand::BindAdditionalClient {
                target: target.clone(),
                expected_authorization_version: spaceterm_core::authority::FieldVersion::new(1),
                claim_digest: RequestDigest::from_bytes([1; 32]),
            }),
            &CanonicalCodec,
        )
        .unwrap();
        let operation = request.binding();
        let proof = ReceiptCodec::new(original.binding.target().pin())
            .sign(
                PairingReceiptBody::ClientBinding {
                    target,
                    keys: keys.clone(),
                    client: streams.clone(),
                    operation,
                    revision: StoreRevision::new(2),
                },
                &original.server,
            )
            .unwrap();
        origin
            .transact(vec![
                ClientJournalChange::Prepare(spaceterm_client::journal::JournalOperation {
                    binding: operation,
                    request: request.encode(&CanonicalCodec).unwrap(),
                    workspace: None,
                    reserves_grant: false,
                    cleanup: false,
                    state: spaceterm_client::journal::RetainedOperationState::Pending,
                }),
                ClientJournalChange::Complete {
                    binding: operation,
                    outcome: CompactOutcome::new(
                        &Outcome::Pairing(Box::new(PairingOutcome::ClientBound(proof.clone()))),
                        &CanonicalCodec,
                    )
                    .unwrap(),
                    publication_revision: spaceterm_core::authority::OrganizationRevision::new(2),
                },
            ])
            .await
            .unwrap();
        let prepared = PreparedEnrollment {
            local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
                server,
                pairing,
                generation,
            })
            .unwrap(),
            device: original.binding.target().device(),
            pin: original.binding.target().pin(),
            keys,
            client,
            credential,
            route: original.route,
            enrollment: operation,
            reservation: None,
        };
        composition
            .prepare_route(&prepared, original.binding.target().route().clone())
            .await
            .unwrap();
        let saved = composition
            .save_additional_client(&prepared, proof)
            .await
            .unwrap();
        assert!(matches!(
            composition.restore(saved.key(), client).await,
            Err(PairingFailure::RecoveryRequired)
        ));
        assert!(
            !composition
                .journal()
                .ordinary_use_allowed(
                    &composition.journal().snapshot().unwrap().bindings()[0]
                        .binding()
                        .unwrap()
                )
                .unwrap()
        );
        assert!(matches!(
            spaceterm_client::pairing::journal::JournalPairingStore::new(
                composition.journal().clone()
            )
            .ordinary_authority(saved.key())
            .await,
            Err(PairingFailure::RecoveryRequired)
        ));
        assert_eq!(
            composition
                .confirm_additional_client_acknowledgement(saved.key(), client, &origin)
                .await,
            Err(PairingFailure::RecoveryRequired)
        );
        drop(composition);
        let reopened = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let composition = runtime_with(
            controller(Arc::new(reopened)).await,
            credentials.clone(),
            Arc::new(Offline),
        );
        assert!(matches!(
            composition.restore(saved.key(), client).await,
            Err(PairingFailure::RecoveryRequired)
        ));
        origin
            .transact(vec![
                ClientJournalChange::ConfirmAcknowledgement {
                    stream: operation.id.stream(),
                    through: operation.id.sequence().get(),
                },
                ClientJournalChange::ConsumeResult(operation.id),
            ])
            .await
            .unwrap();
        assert!(origin.snapshot().unwrap().operation(operation.id).is_none());
        drop(origin);
        let origin_reopened = NativeClientJournal::open_existing(
            origin_directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        origin = controller(Arc::new(origin_reopened)).await;
        assert_eq!(
            composition
                .confirm_additional_client_acknowledgement(saved.key(), client, &unrelated)
                .await,
            Err(PairingFailure::RecoveryRequired)
        );
        composition
            .confirm_additional_client_acknowledgement(saved.key(), client, &origin)
            .await
            .unwrap();
        composition
            .confirm_additional_client_acknowledgement(saved.key(), client, &origin)
            .await
            .unwrap();
        assert!(
            composition
                .inventory()
                .await
                .unwrap()
                .additional_client_acknowledgements
                .is_empty()
        );
        drop(composition);
        let reopened = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let composition = runtime_with(
            controller(Arc::new(reopened)).await,
            credentials.clone(),
            Arc::new(Offline),
        );
        let restored = composition.restore(saved.key(), client).await.unwrap();
        assert_eq!(restored.binding().streams(), &streams);
        assert_eq!(
            restored.binding().binding().unwrap().target().client(),
            client
        );
        consumers.push((directory, composition, restored));
    }
    assert_ne!(
        consumers[0].2.binding().streams().parts().client.id,
        consumers[1].2.binding().streams().parts().client.id
    );
    assert_eq!(
        *credentials.items.lock().unwrap(),
        vec![original.credential]
    );
}

#[tokio::test]
async fn cli_pair_enroll_records_exact_owner_request() {
    use spaceterm_client_native::pairing_runtime::OwnerEnrollmentRequest;
    use spaceterm_host::{
        account_service::test_support::AccountServiceFixture,
        startup::{BuildIdentity, DesiredServerState},
    };
    let account_root = Directory::new();
    let account = AccountServiceFixture::new(&account_root.0, BuildIdentity::Development).unwrap();
    let registration = account.initialize(DesiredServerState::Stopped).unwrap();
    let supervisor = account.reopen();
    let directory = Directory::new();
    let credentials = Arc::new(Credentials::new());
    let composition = runtime_with(
        controller(Arc::new(directory.create().await)).await,
        credentials.clone(),
        Arc::new(Offline),
    );
    let namespace = CredentialNamespace::new("dev.spaceterm.owner-test".into()).unwrap();
    let scope = |purpose, reference| {
        CredentialScope::new(CredentialScopeParts {
            namespace: namespace.clone(),
            purpose,
            reference: CredentialReference::new([reference; 16]),
        })
        .unwrap()
    };
    let (_, device) = composition
        .prepare_credential(scope(CredentialPurpose::DeviceIdentity, 30))
        .await
        .unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let (generation, credential) = composition
        .prepare_credential(scope(
            CredentialPurpose::PairingAuthentication { server },
            31,
        ))
        .await
        .unwrap();
    let pairing = PairingId::new(server, 1).unwrap();
    let client = ClientId::new(server, 1).unwrap();
    let stream =
        OperationStreamId::new(OperationStreamKey::new(server, 1).unwrap(), pairing, client)
            .unwrap();
    let enrollment = OperationBinding {
        id: OperationId::new(stream, OperationSequence::new(1).unwrap()),
        digest: RequestDigest::from_bytes([5; 32]),
    };
    let authorization = EnrollmentAuthorization::generate().unwrap();
    let claim = RequestDigest::from_bytes([6; 32]);
    let request = supervisor.allocate_request().unwrap();
    let digest = supervisor
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    let reservation = EnrollmentReservation::new(EnrollmentReservationParts {
        ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
            server,
            nonce: [3; 16],
        })
        .unwrap(),
        target: PairingTarget::new(PairingTargetParts {
            server,
            pairing,
            device: DeviceId::new([9; 16]),
        })
        .unwrap(),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key,
            pairing_authentication_public_key: credential.public_key,
        })
        .unwrap(),
        initial_client: client,
        enrollment_stream: stream,
        claim_digest: claim,
        authorization_digest: authorization.digest(),
        enrollment_request_digest: enrollment.digest,
        issuer: EnrollmentIssuer::AccountOwner {
            native_request: *request.as_bytes(),
            native_request_digest: digest,
        },
        issued_epoch: spaceterm_core::authority::ServerEpoch::new(1),
        issued_revision: StoreRevision::new(1),
        issued_at_millis: 0,
        expires_at_millis: 1,
        state: EnrollmentReservationState::Reserved,
    })
    .unwrap();
    let public =
        PublicEnrollmentReservation::from_wire(&reservation_to_wire(&reservation)).unwrap();
    let prepared = PreparedEnrollment {
        local_pairing: LocalPairingKey::new(LocalPairingKeyParts {
            server,
            pairing,
            generation,
        })
        .unwrap(),
        device: DeviceId::new([9; 16]),
        pin: registration.server_pin(),
        keys: public.parts().keys.clone(),
        client,
        credential,
        route: RevocationRouteReference::new([5; 16]),
        enrollment,
        reservation: Some(public),
    };
    let route = RouteDescription::new(RouteParts::Ipc {
        directory: "/explicit/local/authority".into(),
        name: "server".into(),
    })
    .unwrap();
    let owner = |native_digest| OwnerEnrollmentRequest {
        prepared: &prepared,
        device: &device,
        authorization: Some(&authorization),
        native_request: request,
        native_digest,
    };
    let revision = composition.inventory().await.unwrap().revision;
    assert_eq!(
        composition
            .enroll_account_owner(&supervisor, owner(digest), route.clone())
            .await,
        Err(PairingFailure::AuthorizationRequired)
    );
    assert_eq!(composition.inventory().await.unwrap().revision, revision);
    supervisor
        .record_owner_enrollment(request, digest, server, claim)
        .unwrap();
    assert_eq!(
        composition
            .enroll_account_owner(
                &supervisor,
                owner(RequestDigest::from_bytes([99; 32])),
                route.clone()
            )
            .await,
        Err(PairingFailure::AuthorizationDenied)
    );
    assert_eq!(composition.inventory().await.unwrap().revision, revision);
    assert_eq!(
        composition
            .enroll_account_owner(&supervisor, owner(digest), route)
            .await,
        Err(PairingFailure::Offline)
    );
    assert_eq!(
        composition.inventory().await.unwrap().prepared,
        vec![prepared]
    );
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
}

#[tokio::test]
async fn cancelled_foreground_attempt_cannot_bind_or_capture_a_native_route() {
    let directory = Directory::new();
    let composition = runtime(controller(Arc::new(directory.create().await)).await);
    let fixture = EnrollmentFixture::prepare(&composition, 2).await;
    let attempt =
        DirectEnrollmentAttempt::new(fixture.prepared.pin, fixture.prepared.route, 1).unwrap();
    let verified = attempt
        .confirm(
            fixture.prepared.pin,
            fixture.prepared.route,
            1,
            &attempt.fingerprint(),
        )
        .unwrap();
    let revision = composition.inventory().await.unwrap().revision;
    attempt.cancel();
    assert_eq!(
        composition
            .enroll(
                EnrollmentAttemptRequest {
                    verified: &verified,
                    prepared: &fixture.prepared,
                    reservation: fixture.prepared.reservation.clone().unwrap(),
                    device: &fixture.device,
                    authorization: Some(&fixture.authorization),
                    majors: vec![2, 1],
                },
                fixture.fixture.binding.target().route().clone()
            )
            .await,
        Err(PairingFailure::Cancelled)
    );
    assert_eq!(composition.inventory().await.unwrap().revision, revision);
}

#[tokio::test]
async fn stale_removal_assessment_preserves_the_live_pairing() {
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let fixture = Fixture::new(1);
    fixture.seed(native.as_ref()).await;
    let composition = runtime(controller(native).await);
    let key = fixture.binding.target().local_pairing().unwrap();
    let ordinary = composition
        .restore(&key, fixture.binding.target().client())
        .await
        .unwrap();
    let assessment = composition
        .assess_unpair(&key, fixture.binding.target().client())
        .await
        .unwrap();
    composition
        .prepare_credential(
            CredentialScope::new(CredentialScopeParts {
                namespace: fixture.credential.scope.parts().namespace.clone(),
                purpose: CredentialPurpose::DeviceIdentity,
                reference: CredentialReference::new([95; 16]),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        composition.self_unpair(assessment).await,
        Err(PairingFailure::StaleAssessment)
    );
    assert!(
        ordinary
            .authority()
            .lease()
            .validate(&fixture.credential.scope)
            .is_ok()
    );
    assert!(
        composition
            .inventory()
            .await
            .unwrap()
            .revocations
            .is_empty()
    );
    assert!(
        composition
            .restore(&key, fixture.binding.target().client())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn native_ipc_pairing_runtime_saves_and_acknowledges_before_registration_in_both_majors() {
    use spaceterm_client_native::{
        restricted_routes::NativePairingConnector,
        route::{LocalRouteAuthority, NativeRouteConnector},
    };
    use spaceterm_host::{
        ipc::{IpcListener, PrivateIpcDirectory},
        transport::{AdmissionLimits, MessageLimit, MessageTransport},
    };
    for major in [1, 2] {
        let directory = Directory::new();
        let credentials = Arc::new(Credentials::new());
        let composition = runtime_with(
            controller(Arc::new(directory.create().await)).await,
            credentials.clone(),
            Arc::new(Offline),
        );
        let fixture = EnrollmentFixture::prepare(&composition, major).await;
        fixture
            .endpoint
            .lose_initial_reply
            .store(false, Ordering::SeqCst);
        *fixture.endpoint.lose_ack_reply.lock().unwrap() = false;
        let sockets = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(sockets.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = PrivateIpcDirectory::open(&sockets.path().canonicalize().unwrap())
            .unwrap()
            .endpoint("server")
            .unwrap();
        let authority = LocalRouteAuthority::new(endpoint.clone()).unwrap();
        let route = authority.description().clone();
        let listener = IpcListener::bind(
            endpoint,
            MessageLimit::envelope(),
            AdmissionLimits::new(2, std::time::Duration::from_secs(20)).unwrap(),
        )
        .unwrap();
        let server = fixture.endpoint.clone();
        let exchange = tokio::spawn(async move {
            let transport = listener.accept().await.unwrap();
            let mut protocol = EnrollmentTransport {
                endpoint: server.as_ref().clone(),
                handshake: None,
                connection: None,
                replies: VecDeque::new(),
            };
            while let Some(bytes) = transport.recv().await.unwrap() {
                protocol.send(&bytes).await.unwrap();
                while let Some(reply) = protocol.replies.pop_front() {
                    transport.send(&reply).await.unwrap();
                }
            }
        });
        let journal = composition.journal().clone();
        drop(composition);
        let connector = Arc::new(NativePairingConnector::new(
            journal.clone(),
            NativeRouteConnector::new(vec![authority], None).unwrap(),
        ));
        let composition = NativePairingRuntime::with_adapters(
            journal,
            credentials,
            connector.clone(),
            connector,
            Arc::new(NoEffects),
            HandshakeConfig::default(),
        );
        *fixture.endpoint.journal.lock().unwrap() = Arc::downgrade(composition.journal());
        let attempt =
            DirectEnrollmentAttempt::new(fixture.prepared.pin, fixture.prepared.route, 1).unwrap();
        let verified = attempt
            .confirm(
                fixture.prepared.pin,
                fixture.prepared.route,
                1,
                &attempt.fingerprint(),
            )
            .unwrap();
        let saved = composition
            .enroll(
                EnrollmentAttemptRequest {
                    verified: &verified,
                    prepared: &fixture.prepared,
                    reservation: fixture.prepared.reservation.clone().unwrap(),
                    device: &fixture.device,
                    authorization: Some(&fixture.authorization),
                    majors: vec![2, 1],
                },
                route,
            )
            .await
            .unwrap();
        assert_eq!(saved, fixture.saved);
        assert!(
            composition
                .inventory()
                .await
                .unwrap()
                .enrollment_acknowledgements
                .is_empty()
        );
        assert_eq!(
            composition
                .restore(saved.key(), fixture.prepared.client)
                .await
                .unwrap()
                .binding()
                .proof(),
            saved.receipt()
        );
        assert_eq!(*fixture.endpoint.submissions.lock().unwrap(), 1);
        assert_eq!(*fixture.endpoint.acknowledgements.lock().unwrap(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(5), exchange)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn enrollment_ssh_uses_foreground_askpass_without_granting_restricted_retry_prompt_authority()
{
    use spaceterm_client::pairing::revocation::RestrictedConnector;
    use spaceterm_client_native::{
        restricted_routes::NativePairingConnector,
        route::{NativeRouteConnector, SshRouteConfiguration, TunnelEndpoints},
    };
    use spaceterm_host::{
        ipc::PrivateIpcDirectory,
        ssh_tunnel::{AskPassHandoff, TunnelAskPass},
        transport::TransportError,
    };
    use std::os::unix::fs::PermissionsExt;
    struct Prompt(AtomicU64);
    impl TunnelAskPass for Prompt {
        fn handoff(&self) -> Result<AskPassHandoff, TransportError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            AskPassHandoff::new(
                PathBuf::from("/usr/bin/true"),
                "00000000:/test-broker".into(),
                zeroize::Zeroizing::new(vec![b'a'; 64]),
            )
        }
        fn handoff_authorized(
            &self,
            authority: spaceterm_host::ssh_tunnel::AskPassAuthorityCheck,
        ) -> Result<AskPassHandoff, TransportError> {
            if !authority() {
                return Err(TransportError::PeerAccountRejected);
            }
            self.handoff()
        }
    }
    let directory = Directory::new();
    let journal = controller(Arc::new(directory.create().await)).await;
    let sockets = tempfile::tempdir().unwrap();
    std::fs::set_permissions(sockets.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = sockets.path().join("ssh-fixture");
    let config = sockets.path().join("config");
    let observed = sockets.path().join("modes");
    std::fs::write(
        &config,
        "Host target\n HostName owner.example\n User owner\n Port 22\n",
    )
    .unwrap();
    std::fs::write(
        &executable,
        format!(
            r#"#!/usr/bin/python3 -I
import os, socket, sys
if '-G' in sys.argv:
    if sys.argv[-1] == 'target':
        print('host target\nhostname owner.example\nuser owner\nport 22\nbatchmode yes\nproxyjump gateway')
    else:
        print('host gateway\nhostname gateway.example\nuser owner\nport 22\nbatchmode yes')
    sys.exit(0)
foreground = 'BatchMode=no' in sys.argv
has_prompt = 'SPACETERM_SSH_ASKPASS_CAPABILITY' in os.environ
assert foreground == has_prompt
if foreground:
    assert os.environ['SSH_ASKPASS_REQUIRE'] == 'force'
    assert os.environ['SPACETERM_SSH_ASKPASS_MODE'] == 'broker-v1'
else:
    assert 'BatchMode=yes' in sys.argv
    assert 'SSH_ASKPASS' not in os.environ
configuration = open(sys.argv[sys.argv.index('-F') + 1]).read()
captured = 'Host st-route-0' in configuration
if captured:
    assert 'Host st-route-1' in configuration
    expected_batch = 'no' if foreground else 'yes'
    assert configuration.startswith('Host *\n BatchMode ' + expected_batch + '\n')
with open({observed:?}, 'a') as output:
    output.write(('foreground-ordinary\n' if foreground else 'ordinary\n') if not captured else ('foreground\n' if foreground else 'restricted\n'))
forward = sys.argv[sys.argv.index('-L') + 1]
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
listener.bind(forward.split(':', 1)[0])
listener.listen(8)
while True:
    peer, _ = listener.accept()
    while peer.recv(4096):
        pass
    peer.close()
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let prompt = Arc::new(Prompt(AtomicU64::new(0)));
    let ssh = SshRouteConfiguration::new(
        executable,
        config,
        sockets.path().canonicalize().unwrap(),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
        TunnelEndpoints::new(
            PrivateIpcDirectory::open(&sockets.path().canonicalize().unwrap()).unwrap(),
        )
        .unwrap(),
        Some(prompt.clone()),
    )
    .unwrap();
    let routes = NativeRouteConnector::new(vec![], Some(ssh)).unwrap();
    let mut fixture = Fixture::new(1);
    let target = fixture.binding.target();
    let route = RouteDescription::new(RouteParts::Ssh {
        destination: "target".into(),
        remote_socket: "/server".into(),
    })
    .unwrap();
    fixture.binding = PairedServerBinding::new(
        ConnectionTarget::new(
            target.pin(),
            target.device(),
            target.pairing(),
            target.client(),
            route,
            target.credential(),
            target.incarnation(),
        )
        .unwrap(),
    );
    fixture.saved = SavedBinding::verified(
        fixture.binding.clone(),
        fixture.saved.streams().clone(),
        fixture.saved.proof().clone(),
        &Verifier,
    )
    .unwrap();
    let ordinary = spaceterm_client::ports::RouteConnector::open(
        &routes,
        fixture.binding.target(),
        spaceterm_client::ports::ConnectionPurpose::Ordinary,
    )
    .await
    .unwrap();
    assert_eq!(prompt.0.load(Ordering::SeqCst), 0);
    ordinary.sender.close().await.unwrap();
    let routes = Arc::new(routes);
    let connector = Arc::new(NativePairingConnector::with_shared_routes(
        journal.clone(),
        routes.clone(),
    ));
    journal
        .transact(fixture.seed_changes()[..3].to_vec())
        .await
        .unwrap();
    connector
        .capture_route(fixture.credential.scope.reference())
        .await
        .unwrap();
    let attempt =
        DirectEnrollmentAttempt::new(fixture.binding.target().pin(), fixture.route, 1).unwrap();
    let verified = attempt
        .confirm(
            fixture.binding.target().pin(),
            fixture.route,
            1,
            &attempt.fingerprint(),
        )
        .unwrap();
    let lease = verified.credential_use(&fixture.credential).unwrap();
    let foreground = connector
        .open_foreground(fixture.binding.target().pin(), fixture.route, &lease)
        .await
        .unwrap();
    assert_eq!(prompt.0.load(Ordering::SeqCst), 1);
    drop(foreground);
    let acknowledgement = EnrollmentConnector::open(
        connector.as_ref(),
        fixture.binding.target().pin(),
        fixture.route,
    )
    .await
    .unwrap();
    assert_eq!(prompt.0.load(Ordering::SeqCst), 1);
    drop(acknowledgement);
    journal
        .transact(vec![ClientJournalChange::SaveBinding(Box::new(
            fixture.saved.clone(),
        ))])
        .await
        .unwrap();
    use spaceterm_client::{
        pairing::journal::{JournalPairingStore, LocalPairingStore},
        ports::ForegroundCancellation,
    };
    let key = fixture.binding.target().local_pairing().unwrap();
    let authority = JournalPairingStore::new(journal.clone())
        .ordinary_authority(&key)
        .await
        .unwrap();
    // Ordinary leases stay noninteractive. The person must deliberately Retry.
    assert!(matches!(
        routes.open_foreground_ordinary(fixture.binding.target(), &authority.lease(), ForegroundCancellation::new()).await,
        Err(error) if error.class() == spaceterm_client::failure::ClientFailureClass::AuthorizationDenied
    ));
    let reconnect_lease = authority.foreground_lease();
    let target = fixture.binding.target();
    let different_incarnation = ConnectionTarget::new(
        target.pin(),
        target.device(),
        target.pairing(),
        target.client(),
        target.route().clone(),
        target.credential(),
        LocalPairingGeneration::new(target.incarnation().get() + 1).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        routes.open_foreground_ordinary(&different_incarnation, &reconnect_lease, ForegroundCancellation::new()).await,
        Err(error) if error.class() == spaceterm_client::failure::ClientFailureClass::AuthorizationDenied
    ));
    assert_eq!(prompt.0.load(Ordering::SeqCst), 1);
    let reconnect = routes
        .open_foreground_ordinary(
            fixture.binding.target(),
            &reconnect_lease,
            ForegroundCancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(prompt.0.load(Ordering::SeqCst), 2);
    reconnect.sender.close().await.unwrap();
    let background = spaceterm_client::ports::RouteConnector::open(
        routes.as_ref(),
        fixture.binding.target(),
        spaceterm_client::ports::ConnectionPurpose::Ordinary,
    )
    .await
    .unwrap();
    assert_eq!(prompt.0.load(Ordering::SeqCst), 2);
    background.sender.close().await.unwrap();
    let composition = runtime(journal);
    let assessment = composition
        .assess_unpair(&key, fixture.binding.target().client())
        .await
        .unwrap();
    let removed = composition.self_unpair(assessment).await.unwrap();
    assert!(matches!(
        routes.open_foreground_ordinary(fixture.binding.target(), &reconnect_lease, ForegroundCancellation::new()).await,
        Err(error) if error.class() == spaceterm_client::failure::ClientFailureClass::AuthorizationDenied
    ));
    let task = composition
        .inventory()
        .await
        .unwrap()
        .task(removed.operation)
        .unwrap()
        .task
        .clone();
    let retry = RestrictedConnector::open(connector.as_ref(), &task)
        .await
        .unwrap();
    assert_eq!(prompt.0.load(Ordering::SeqCst), 2);
    drop(retry);
    attempt.cancel();
    assert!(matches!(
        connector
            .open_foreground(fixture.binding.target().pin(), fixture.route, &lease)
            .await,
        Err(PairingFailure::AuthorizationDenied)
    ));
    assert_eq!(prompt.0.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read_to_string(observed).unwrap(),
        "ordinary\nforeground\nrestricted\nforeground-ordinary\nordinary\nrestricted\n"
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        use std::os::unix::fs::FileTypeExt;
        if !std::fs::read_dir(sockets.path())
            .unwrap()
            .any(|entry| entry.unwrap().file_type().unwrap().is_socket())
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "SSH fixture cleanup did not finish"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn cli_unpair_resolves_exact_saved_pairing() {
    let directory = Directory::new();
    let native = Arc::new(directory.create().await);
    let first = Fixture::new(1);
    let selected = Fixture::on_server(2, ServerId::new([9; 16]));
    first.seed(native.as_ref()).await;
    selected.seed(native.as_ref()).await;
    let composition = runtime(controller(native).await);
    let first_key = first.binding.target().local_pairing().unwrap();
    let selected_key = selected.binding.target().local_pairing().unwrap();
    let selected_client = selected.binding.target().client();
    let first_ordinary = composition
        .restore(&first_key, first.binding.target().client())
        .await
        .unwrap();
    assert!(matches!(
        composition
            .assess_unpair(
                &selected_key,
                ClientId::new(selected_key.parts().server, 999).unwrap()
            )
            .await,
        Err(PairingFailure::ClientNotBound)
    ));
    let assessment = composition
        .assess_unpair(&selected_key, selected_client)
        .await
        .unwrap();
    assert_eq!(assessment.pairing(), &selected_key);
    assert_eq!(assessment.client(), selected_client);
    let removed = composition.self_unpair(assessment).await.unwrap();
    assert_eq!(
        removed.operation.stream(),
        selected.saved.streams().parts().self_revocation
    );
    let inventory = composition.inventory().await.unwrap();
    assert!(inventory.is_suppressed(&selected_key));
    assert!(!inventory.is_suppressed(&first_key));
    let pending = inventory.task(removed.operation).unwrap();
    assert_eq!(pending.task.parts().local_pairing, selected_key);
    assert_eq!(pending.task.parts().original.client(), selected_client);
    assert_eq!(
        pending.task.parts().original.credential(),
        selected.credential.scope.reference()
    );
    assert!(
        first_ordinary
            .authority()
            .lease()
            .validate(&first.credential.scope)
            .is_ok()
    );
    assert!(
        composition
            .restore(&first_key, first.binding.target().client())
            .await
            .is_ok()
    );
    assert!(matches!(
        composition.restore(&selected_key, selected_client).await,
        Err(PairingFailure::PairingRevoked)
    ));
    assert_eq!(inventory.revocations.len(), 1);
}

#[tokio::test]
async fn cli_reopens_suppressed_pairing_for_restricted_retry_without_native_registration() {
    use spaceterm_client_native::{
        NativeClientProvider,
        cli::bindings::{PairingCommandBindings, RevocationState},
    };
    use spaceterm_host::{
        account_service::test_support::AccountServiceFixture, startup::BuildIdentity,
    };
    let account_root = Directory::new();
    let account = AccountServiceFixture::new(&account_root.0, BuildIdentity::Development).unwrap();
    let path = account.reopen().client_connections_directory().join("cli");
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)
        .unwrap();
    let directory = Directory(path);
    let native = Arc::new(directory.create().await);
    let fixture = Fixture::new(1);
    fixture.seed(native.as_ref()).await;
    let key = fixture.binding.target().local_pairing().unwrap();
    let composition = runtime(controller(native).await);
    let assessment = composition
        .assess_unpair(&key, fixture.binding.target().client())
        .await
        .unwrap();
    let removal = composition.self_unpair(assessment).await.unwrap();
    composition.dismiss_notice(removal.operation).await.unwrap();
    drop(composition);
    assert!(account.reopen().discover().unwrap().is_none());
    let provider = NativeClientProvider::from_supervisor(account.reopen());
    let consumer = provider.cli_consumer();
    let reports = consumer.retry_pending().await.unwrap();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].local_connection_removed);
    assert_eq!(reports[0].revocation, RevocationState::Pending);
    let inventory = consumer
        .pairing_runtime()
        .await
        .unwrap()
        .inventory()
        .await
        .unwrap();
    assert!(inventory.is_suppressed(&key));
    assert!(inventory.task(removal.operation).unwrap().notice_dismissed);
    assert!(
        consumer
            .pairing_runtime()
            .await
            .unwrap()
            .journal()
            .snapshot()
            .unwrap()
            .bindings()
            .is_empty()
    );
    assert!(account.reopen().discover().unwrap().is_none());
    // A malformed live registration must not hide an independently retained,
    // restricted revocation journal in a newly launched consumer.
    let registration = account
        .reopen()
        .client_connections_directory()
        .parent()
        .unwrap()
        .join("registration");
    std::fs::write(&registration, b"invalid registration").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&registration, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(account.reopen().discover().is_err());
    let retry = NativeClientProvider::from_supervisor(account.reopen()).cli_consumer();
    let reports = retry.retry_pending().await.unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].revocation, RevocationState::Pending);
    assert!(
        retry
            .pairing_runtime()
            .await
            .unwrap()
            .inventory()
            .await
            .unwrap()
            .is_suppressed(&key)
    );
}
