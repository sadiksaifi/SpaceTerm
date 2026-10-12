use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use spaceterm_client::{
    journal::{ClientJournal, JournalCommitDisposition, JournalReceipt},
    pairing::{credentials::*, enrollment::*, journal::*, revocation::*},
};
use spaceterm_core::{
    authority::StoreRevision, identity::*, operation_ledger_contracts::*, operations::*, pairing::*,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{
        self, Established, HandshakeConfig, PairingLookup, Principal, ServerHandshake, ServerPin,
        SigningKey,
    },
    connection::RestrictedConnection,
    pairing_adapter::{
        PairingRestrictedCodec, ReceiptCodec, operation_to_wire, receipt_to_wire, target_to_wire,
    },
    wire::pairing,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

struct Key(Ed25519KeyPair);
impl Key {
    fn new(seed: u8) -> Arc<Self> {
        Arc::new(Self(
            Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap(),
        ))
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
// The real ledger producer is not in this worktree. The frozen digest port is
// injected; all protocol, receipt crypto and client pairing behavior stay real.
struct Codec;
impl LedgerCodec for Codec {
    fn canonical_request_digest(
        &self,
        binding: &StreamBinding,
        operation: OperationId,
        command: &Command,
    ) -> Result<RequestDigest, LedgerFailure> {
        binding.check_operation(operation)?;
        RequestDigest::for_command(command)
            .map_err(|_| LedgerFailure::new(LedgerFailureClass::InvalidRequest))
    }
    fn encode_canonical_request(
        &self,
        _: &StreamBinding,
        _: OperationId,
        _: &Command,
    ) -> Result<Vec<u8>, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::UnsupportedFormat))
    }
    fn decode_canonical_request(
        &self,
        _: &[u8],
    ) -> Result<CanonicalOperationRequest, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::UnsupportedFormat))
    }
    fn encode_compact_outcome(&self, _: &Outcome) -> Result<Vec<u8>, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::UnsupportedFormat))
    }
    fn decode_compact_outcome(&self, _: &[u8]) -> Result<Outcome, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::UnsupportedFormat))
    }
}
struct Fixture {
    prepared: PreparedEnrollment,
    saved: SavedPairing,
    server: Arc<Key>,
    auth: Arc<Key>,
    device: Arc<Key>,
}
impl Fixture {
    fn new(server_byte: u8, lifetime: u64) -> Self {
        let server = Key::new(server_byte);
        let auth = Arc::new(Key(Ed25519KeyPair::from_seed_unchecked(&Sha256::digest(
            lifetime.to_be_bytes(),
        ))
        .unwrap()));
        let device = Key::new(90);
        let id = ServerId::new([server_byte; 16]);
        let pairing = PairingId::new(id, lifetime).unwrap();
        let client = ClientId::new(id, lifetime).unwrap();
        let stream = |local| {
            OperationStreamId::new(OperationStreamKey::new(id, local).unwrap(), pairing, client)
                .unwrap()
        };
        let enrollment = OperationBinding {
            id: OperationId::new(stream(lifetime * 3), OperationSequence::new(1).unwrap()),
            digest: RequestDigest::from_bytes([5; 32]),
        };
        let key = LocalPairingKey::new(LocalPairingKeyParts {
            server: id,
            pairing,
            generation: LocalPairingGeneration::new(lifetime).unwrap(),
        })
        .unwrap();
        let keys = PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key(),
            pairing_authentication_public_key: auth.public_key(),
        })
        .unwrap();
        let binding = PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: client,
                pairing,
            },
            ordinary: stream(lifetime * 3 + 1),
            self_revocation: stream(lifetime * 3 + 2),
        })
        .unwrap();
        let target = PairingTarget::new(PairingTargetParts {
            server: id,
            pairing,
            device: DeviceId::new([8; 16]),
        })
        .unwrap();
        let pin = ServerPin {
            identity: id.bytes(),
            public_key: server.public_key(),
        };
        let credential = PreparedCredential {
            scope: CredentialScope::new(CredentialScopeParts {
                namespace: CredentialNamespace::new("dev.spaceterm.test".into()).unwrap(),
                purpose: CredentialPurpose::PairingAuthentication { server: id },
                reference: CredentialReference::new(
                    ((u128::from(server_byte) << 64) | u128::from(lifetime)).to_be_bytes(),
                ),
            })
            .unwrap(),
            public_key: auth.public_key(),
        };
        let prepared = PreparedEnrollment {
            local_pairing: key,
            device: target.parts().device,
            pin,
            keys: keys.clone(),
            client,
            credential,
            route: RevocationRouteReference::new([6; 16]),
            enrollment,
            reservation: None,
        };
        let proof = ReceiptCodec::new(pin)
            .sign(
                PairingReceiptBody::Enrollment {
                    target,
                    keys,
                    client: binding,
                    enrollment,
                    revision: StoreRevision::new(7),
                },
                server.as_ref(),
            )
            .unwrap();
        let saved = verify_binding(&prepared, proof, false).unwrap();
        Self {
            prepared,
            saved,
            server,
            auth,
            device,
        }
    }
    fn task(&self) -> PendingRevocationTask {
        let original = PendingSelfRevocation::new(PendingParts {
            original_server: self.prepared.local_pairing.parts().server,
            original_pairing: self.prepared.local_pairing.parts().pairing,
            device: self.prepared.device,
            client: self.prepared.client,
            operation: OperationId::new(
                self.saved.client().parts().self_revocation,
                OperationSequence::new(1).unwrap(),
            ),
            route: self.prepared.route,
            credential: self.prepared.credential.scope.reference(),
            notice_dismissed: false,
        })
        .unwrap();
        let digest = RequestDigest::for_command(&Command::Pairing(PairingCommand::SelfRevoke {
            target: self.target(),
        }))
        .unwrap();
        PendingRevocationTask::new(PendingRevocationTaskParts {
            original,
            local_pairing: self.prepared.local_pairing.clone(),
            pinned_server_public_key: self.prepared.pin.public_key,
            request_digest: digest,
        })
        .unwrap()
    }
    fn target(&self) -> PairingTarget {
        PairingTarget::new(PairingTargetParts {
            server: self.prepared.local_pairing.parts().server,
            pairing: self.prepared.local_pairing.parts().pairing,
            device: self.prepared.device,
        })
        .unwrap()
    }
    fn receipt(&self, operation: OperationId) -> SignedPairingReceipt {
        ReceiptCodec::new(self.prepared.pin)
            .sign(
                PairingReceiptBody::Revocation {
                    receipt: RevocationReceipt {
                        server: self.prepared.local_pairing.parts().server,
                        pairing: self.prepared.local_pairing.parts().pairing,
                        device: self.prepared.device,
                        operation,
                        revision: StoreRevision::new(9),
                        receipt_digest: RequestDigest::from_bytes([0; 32]),
                    },
                    authentication_key_digest: RequestDigest::from_bytes(
                        Sha256::digest(self.auth.public_key()).into(),
                    ),
                    revocation_event: [7; 16],
                },
                self.server.as_ref(),
            )
            .unwrap()
    }
    fn pending(&self) -> LocalRevocation {
        LocalRevocation {
            task: self.task(),
            credential: self.prepared.credential.clone(),
            phase: RevocationPhase::AwaitingConfirmation,
            notice_dismissed: false,
        }
    }
}

struct Journal {
    delayed_recovery: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    fence: PairingAuthorityFence,
    state: Mutex<PairingJournalSnapshot>,
    changes: Mutex<Vec<PairingJournalChange>>,
    fail: Mutex<Option<JournalCommitDisposition>>,
}
impl Journal {
    fn new(saved: Vec<SavedPairing>) -> Arc<Self> {
        Arc::new(Self {
            delayed_recovery: Mutex::new(None),
            fence: PairingAuthorityFence::default(),
            state: Mutex::new(PairingJournalSnapshot {
                saved,
                ..PairingJournalSnapshot::empty(0)
            }),
            changes: Mutex::new(Vec::new()),
            fail: Mutex::new(None),
        })
    }
    fn pending(pending: LocalRevocation) -> Arc<Self> {
        let value = Self::new(Vec::new());
        {
            let mut state = value.state.lock().unwrap();
            state
                .suppressed
                .push(pending.task.parts().local_pairing.clone());
            state.revocations.push(pending);
        }
        value
    }
}
impl ClientJournal for Journal {
    fn pairing_authority_fence(&self) -> &PairingAuthorityFence {
        &self.fence
    }
    fn recover_pairings(
        &self,
    ) -> PairingFuture<'_, Result<PairingJournalSnapshot, PairingFailure>> {
        Box::pin(async {
            let captured = self.state.lock().unwrap().clone();
            let delay = self.delayed_recovery.lock().unwrap().take();
            if let Some((started, proceed)) = delay {
                started.notify_one();
                proceed.notified().await;
            }
            Ok(captured)
        })
    }
    fn commit_pairing<'a>(
        &'a self,
        tx: &'a PairingJournalTransaction,
    ) -> PairingFuture<'a, JournalCommitDisposition> {
        Box::pin(async move {
            if let Some(failure) = self.fail.lock().unwrap().take() {
                return failure;
            }
            let mut state = self.state.lock().unwrap();
            assert_eq!(state.revision, tx.expected_revision);
            match &tx.change {
                PairingJournalChange::ReserveCredential(preparation) => {
                    state.generation_high_water = preparation.generation.get();
                    state.credential_preparations.push(preparation.clone());
                }
                PairingJournalChange::RecordCredentialPrepared {
                    generation,
                    credential,
                } => {
                    state
                        .credential_preparations
                        .iter_mut()
                        .find(|v| v.generation == *generation)
                        .unwrap()
                        .credential = Some(credential.clone());
                }
                PairingJournalChange::SelfUnpair(pending) => {
                    state
                        .saved
                        .retain(|saved| saved.key() != &pending.task.parts().local_pairing);
                    state
                        .suppressed
                        .push(pending.task.parts().local_pairing.clone());
                    state.revocations.push(pending.as_ref().clone());
                }
                PairingJournalChange::RecordReceipt { operation, receipt } => {
                    state
                        .revocations
                        .iter_mut()
                        .find(|v| v.task.parts().original.operation() == *operation)
                        .unwrap()
                        .phase = RevocationPhase::ReceiptRecorded {
                        receipt: receipt.clone(),
                        acknowledged: false,
                    }
                }
                PairingJournalChange::RecordAcknowledgement { operation } => {
                    let RevocationPhase::ReceiptRecorded { acknowledged, .. } = &mut state
                        .revocations
                        .iter_mut()
                        .find(|v| v.task.parts().original.operation() == *operation)
                        .unwrap()
                        .phase
                    else {
                        panic!("receipt must be durable first");
                    };
                    *acknowledged = true;
                }
                PairingJournalChange::FinishCleanup {
                    operation,
                    expected_credential,
                } => {
                    assert_eq!(
                        &state.task(*operation).unwrap().credential,
                        expected_credential
                    );
                    state
                        .revocations
                        .retain(|v| v.task.parts().original.operation() != *operation);
                }
                PairingJournalChange::DismissNotice { operation } => {
                    state
                        .revocations
                        .iter_mut()
                        .find(|v| v.task.parts().original.operation() == *operation)
                        .unwrap()
                        .notice_dismissed = true
                }
                PairingJournalChange::SaveBinding(saved) => {
                    state.saved.push(saved.as_ref().clone())
                }
                PairingJournalChange::SaveEnrollment {
                    saved,
                    acknowledgement,
                } => {
                    state.saved.push(saved.as_ref().clone());
                    state
                        .prepared
                        .retain(|v| v.enrollment.id != acknowledgement.prepared.enrollment.id);
                    state
                        .enrollment_acknowledgements
                        .push(acknowledgement.as_ref().clone());
                }
                PairingJournalChange::FinishEnrollmentAcknowledgement { operation } => {
                    state
                        .enrollment_acknowledgements
                        .retain(|v| v.prepared.enrollment.id != *operation);
                }
                PairingJournalChange::PrepareEnrollment(prepared) => {
                    state.prepared.push(prepared.as_ref().clone())
                }
            }
            self.changes.lock().unwrap().push(tx.change.clone());
            state.revision += 1;
            JournalCommitDisposition::Durable(JournalReceipt {
                revision: state.revision,
            })
        })
    }
}
#[derive(Clone)]
struct Credentials {
    auth: Arc<Key>,
    journal: Arc<Journal>,
    deleted: Arc<Mutex<Vec<PreparedCredential>>>,
    missing: bool,
}
impl NativeDeviceCredentials for Credentials {
    fn recover_preparation<'a>(
        &'a self,
        _: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        panic!("restricted retries cannot recover enrollment preparations")
    }
    fn create(
        &self,
        _: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        panic!("background retries cannot create credentials")
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        lease: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            lease.validate(&credential.scope)?;
            assert!(matches!(
                lease.purpose(),
                CredentialUsePurpose::Restricted { .. }
            ));
            Ok(self.auth.clone() as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async move {
            assert!(
                self.journal
                    .changes
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|v| matches!(v, PairingJournalChange::RecordReceipt { .. }))
                    || matches!(
                        self.journal.state.lock().unwrap().revocations[0].phase,
                        RevocationPhase::ReceiptRecorded {
                            acknowledged: true,
                            ..
                        }
                    )
            );
            self.deleted.lock().unwrap().push(credential.clone());
            Ok(if self.missing {
                CredentialDeletion::AlreadyMissing
            } else {
                CredentialDeletion::Deleted
            })
        })
    }
}
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
}
impl RestrictedConnector for Connector {
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
                    pairing::restricted_extension::Kind::Revoke(_)
                    | pairing::restricted_extension::Kind::Status(_) => {
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
fn driver(
    f: &Fixture,
    journal: Arc<Journal>,
    major: u32,
    disposition: i32,
    failure: Option<PairingFailure>,
    missing: bool,
) -> (
    RevocationDriver<Credentials, Connector>,
    Arc<Mutex<Vec<PreparedCredential>>>,
) {
    let pending = f.pending();
    let deleted = Arc::new(Mutex::new(Vec::new()));
    let credentials = Credentials {
        auth: f.auth.clone(),
        journal,
        deleted: deleted.clone(),
        missing,
    };
    let connector = Connector {
        server: f.server.clone(),
        auth: f.auth.clone(),
        proof: f.receipt(pending.task.parts().original.operation()),
        pending,
        major,
        disposition,
        failure,
        opens: Arc::new(Mutex::new(0)),
    };
    (
        RevocationDriver::new(
            credentials,
            connector,
            Arc::new(Codec),
            HandshakeConfig::default(),
        ),
        deleted,
    )
}

#[test]
fn direct_enrollment_requires_exact_fingerprint_verification() {
    let f = Fixture::new(1, 1);
    let attempt = DirectEnrollmentAttempt::new(f.prepared.pin, f.prepared.route, 7).unwrap();
    assert!(
        attempt
            .confirm(f.prepared.pin, f.prepared.route, 6, &attempt.fingerprint())
            .is_err()
    );
    assert!(
        attempt
            .confirm(
                f.prepared.pin,
                RevocationRouteReference::new([9; 16]),
                7,
                &attempt.fingerprint()
            )
            .is_err()
    );
    assert!(
        attempt
            .confirm(
                ServerPin {
                    public_key: [7; 32],
                    ..f.prepared.pin
                },
                f.prepared.route,
                7,
                &attempt.fingerprint()
            )
            .is_err()
    );
    assert!(
        attempt
            .confirm(f.prepared.pin, f.prepared.route, 7, "SHA256:wrong")
            .is_err()
    );
    assert!(
        attempt
            .confirm(f.prepared.pin, f.prepared.route, 7, &attempt.fingerprint())
            .is_ok()
    );
    attempt.cancel();
    assert!(
        attempt
            .confirm(f.prepared.pin, f.prepared.route, 7, &attempt.fingerprint())
            .is_err()
    );
    assert_eq!(
        fingerprint([0; 32]),
        "SHA256:Zmh6rfhivXdsj8GLjp+OIAiXFIVu4jOzkCpZHQ1fKSU"
    );
}
#[tokio::test]
async fn offline_self_unpair_removes_ordinary_access_after_durable_recording() {
    let f = Fixture::new(1, 1);
    let journal = Journal::new(vec![f.saved.clone()]);
    let store = JournalPairingStore::new(journal.clone());
    let authority = store.ordinary_authority(f.saved.key()).await.unwrap();
    let lease = authority.lease();
    let operation = f.task().parts().original.operation();
    let removal = PairingController::new(store, Arc::new(Codec))
        .self_unpair(
            &authority,
            ConfirmedSelfUnpair::confirm(f.saved.key().clone(), operation).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(removal.key, *f.saved.key());
    assert!(lease.validate(&f.prepared.credential.scope).is_err());
    let recovered = JournalPairingStore::new(journal.clone())
        .recover()
        .await
        .unwrap();
    assert!(recovered.active(f.saved.key()).is_none());
    assert!(recovered.saved.is_empty());
    assert_eq!(recovered.task(operation).unwrap().task, f.task());
    let (driver, deleted) = driver(&f, journal, 2, 1, Some(PairingFailure::Offline), false);
    assert_eq!(
        driver
            .run_once(
                &JournalPairingStore::new(Journal::pending(f.pending())),
                operation
            )
            .await,
        Err(PairingFailure::Offline)
    );
    assert!(deleted.lock().unwrap().is_empty());
}
#[tokio::test]
async fn local_indeterminate_removal_is_not_reported_as_durable_success() {
    for failure in [
        JournalCommitDisposition::Indeterminate,
        JournalCommitDisposition::NotApplied(PairingFailure::StorageUnavailable),
    ] {
        let f = Fixture::new(1, 1);
        let journal = Journal::new(vec![f.saved.clone()]);
        let store = JournalPairingStore::new(journal.clone());
        let authority = store.ordinary_authority(f.saved.key()).await.unwrap();
        let lease = authority.lease();
        *journal.fail.lock().unwrap() = Some(failure);
        assert!(
            PairingController::new(store, Arc::new(Codec))
                .self_unpair(
                    &authority,
                    ConfirmedSelfUnpair::confirm(
                        f.saved.key().clone(),
                        f.task().parts().original.operation()
                    )
                    .unwrap()
                )
                .await
                .is_err()
        );
        assert!(lease.validate(&f.prepared.credential.scope).is_err());
        assert!(
            journal
                .state
                .lock()
                .unwrap()
                .active(f.saved.key())
                .is_some()
        );
    }
}
#[tokio::test]
async fn receipt_recording_precedes_native_credential_deletion_in_both_majors() {
    for major in [1, 2] {
        let f = Fixture::new(1, 1);
        let journal = Journal::pending(f.pending());
        let (driver, deleted) = driver(&f, journal.clone(), major, 1, None, false);
        driver
            .run_once(
                &JournalPairingStore::new(journal.clone()),
                f.task().parts().original.operation(),
            )
            .await
            .unwrap();
        assert_eq!(
            deleted.lock().unwrap().as_slice(),
            std::slice::from_ref(&f.prepared.credential)
        );
        let changes = journal.changes.lock().unwrap();
        assert!(matches!(
            changes.as_slice(),
            [
                PairingJournalChange::RecordReceipt { .. },
                PairingJournalChange::RecordAcknowledgement { .. },
                PairingJournalChange::FinishCleanup { .. }
            ]
        ));
        assert!(journal.state.lock().unwrap().revocations.is_empty());
    }
}
#[tokio::test]
async fn receipt_recording_failure_never_deletes_credential() {
    let f = Fixture::new(1, 1);
    let journal = Journal::pending(f.pending());
    let (driver, deleted) = driver(&f, journal.clone(), 2, 1, None, false);
    *journal.fail.lock().unwrap() = Some(JournalCommitDisposition::Indeterminate);
    assert_eq!(
        driver
            .run_once(
                &JournalPairingStore::new(journal.clone()),
                f.task().parts().original.operation()
            )
            .await,
        Err(PairingFailure::RecoveryRequired)
    );
    assert!(deleted.lock().unwrap().is_empty());
    assert!(matches!(
        journal.state.lock().unwrap().revocations[0].phase,
        RevocationPhase::AwaitingConfirmation
    ));
}
#[tokio::test]
async fn crash_after_native_deletion_finishes_cleanup_idempotently() {
    let f = Fixture::new(1, 1);
    let mut pending = f.pending();
    let receipt = VerifiedRevocationReceipt::verify(
        &pending.task,
        &pending.credential,
        f.receipt(pending.task.parts().original.operation()),
        false,
    )
    .unwrap();
    pending.phase = RevocationPhase::ReceiptRecorded {
        receipt: Box::new(receipt),
        acknowledged: true,
    };
    let journal = Journal::pending(pending);
    let (driver, deleted) = driver(
        &f,
        journal.clone(),
        2,
        1,
        Some(PairingFailure::Offline),
        true,
    );
    driver
        .run_once(
            &JournalPairingStore::new(journal.clone()),
            f.task().parts().original.operation(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.lock().unwrap().len(), 1);
    assert!(journal.state.lock().unwrap().revocations.is_empty());
}
#[tokio::test]
async fn old_task_completion_preserves_new_pairing_and_other_server() {
    let f = Fixture::new(1, 1);
    let newer = Fixture::new(1, 2);
    let other = Fixture::new(2, 1);
    let journal = Journal::pending(f.pending());
    journal
        .state
        .lock()
        .unwrap()
        .saved
        .extend([newer.saved.clone(), other.saved.clone()]);
    let (driver, deleted) = driver(&f, journal.clone(), 2, 1, None, false);
    driver
        .run_once(
            &JournalPairingStore::new(journal.clone()),
            f.task().parts().original.operation(),
        )
        .await
        .unwrap();
    assert_eq!(
        journal.state.lock().unwrap().saved,
        vec![newer.saved, other.saved]
    );
    assert_eq!(deleted.lock().unwrap().as_slice(), &[f.prepared.credential]);
}
#[tokio::test]
async fn dismissal_hides_notice_without_cancelling_retry() {
    let f = Fixture::new(1, 1);
    let journal = Journal::pending(f.pending());
    let store = JournalPairingStore::new(journal.clone());
    store
        .dismiss_notice(0, f.task().parts().original.operation())
        .await;
    let state = store.recover().await.unwrap();
    assert!(state.revocations[0].notice_dismissed);
    assert!(matches!(
        state.revocations[0].phase,
        RevocationPhase::AwaitingConfirmation
    ));
    let (driver, _) = driver(&f, journal.clone(), 1, 1, None, false);
    driver
        .run_once(&store, f.task().parts().original.operation())
        .await
        .unwrap();
    assert!(journal.state.lock().unwrap().revocations.is_empty());
}
#[tokio::test]
async fn authorization_compatibility_and_changed_route_remain_unconfirmed() {
    for failure in [
        PairingFailure::AuthorizationDenied,
        PairingFailure::Incompatible,
        PairingFailure::RouteChanged,
    ] {
        let f = Fixture::new(1, 1);
        let journal = Journal::pending(f.pending());
        let (driver, deleted) = driver(&f, journal.clone(), 1, 1, Some(failure), false);
        assert_eq!(
            driver
                .run_once(
                    &JournalPairingStore::new(journal.clone()),
                    f.task().parts().original.operation()
                )
                .await,
            Err(failure)
        );
        assert!(matches!(
            journal.state.lock().unwrap().revocations[0].phase,
            RevocationPhase::AwaitingConfirmation
        ));
        assert!(deleted.lock().unwrap().is_empty());
    }
}
#[test]
fn receipt_rejects_wrong_authentication_key_and_target_but_accepts_other_device_revocation() {
    let f = Fixture::new(1, 1);
    let pending = f.pending();
    let other = Fixture::new(2, 1);
    assert!(
        VerifiedRevocationReceipt::verify(
            &pending.task,
            &pending.credential,
            other.receipt(other.task().parts().original.operation()),
            false
        )
        .is_err()
    );
    let mut wrong = pending.credential.clone();
    wrong.public_key = [9; 32];
    assert!(
        VerifiedRevocationReceipt::verify(
            &pending.task,
            &wrong,
            f.receipt(pending.task.parts().original.operation()),
            false
        )
        .is_err()
    );
    let authorizer = Fixture::new(1, 2);
    let different = OperationId::new(
        authorizer.saved.client().parts().ordinary,
        OperationSequence::new(2).unwrap(),
    );
    let proof = f.receipt(different);
    assert!(
        VerifiedRevocationReceipt::verify(&pending.task, &pending.credential, proof.clone(), false)
            .is_err()
    );
    assert!(
        VerifiedRevocationReceipt::verify(&pending.task, &pending.credential, proof, true).is_ok()
    );
}
#[tokio::test]
async fn enrollment_receipt_cannot_reactivate_suppressed_pairing() {
    let f = Fixture::new(1, 1);
    let store = JournalPairingStore::new(Journal::pending(f.pending()));
    assert_eq!(
        save_binding(&store, &f.prepared, f.saved.receipt().clone(), false).await,
        Err(PairingFailure::PairingRevoked)
    );
    let mut wrong = f.prepared.clone();
    wrong.enrollment.digest = RequestDigest::from_bytes([0; 32]);
    assert!(verify_binding(&wrong, f.saved.receipt().clone(), false).is_err());
}
#[test]
fn recovery_rejects_task_without_suppression_and_duplicate_operation() {
    let f = Fixture::new(1, 1);
    let state = PairingJournalSnapshot {
        revocations: vec![f.pending()],
        ..PairingJournalSnapshot::empty(0)
    };
    assert_eq!(state.validate(), Err(PairingFailure::InvalidDurableState));
    let state = PairingJournalSnapshot {
        suppressed: vec![f.saved.key().clone()],
        revocations: vec![f.pending(), f.pending()],
        ..PairingJournalSnapshot::empty(0)
    };
    assert_eq!(state.validate(), Err(PairingFailure::InvalidDurableState));
}

#[tokio::test]
async fn historical_receipt_cannot_bypass_suppression_with_new_local_generation() {
    let f = Fixture::new(1, 1);
    let store = JournalPairingStore::new(Journal::pending(f.pending()));
    let mut prepared = f.prepared.clone();
    prepared.local_pairing = LocalPairingKey::new(LocalPairingKeyParts {
        generation: LocalPairingGeneration::new(99).unwrap(),
        ..prepared.local_pairing.parts().clone()
    })
    .unwrap();
    assert_eq!(
        save_binding(&store, &prepared, f.saved.receipt().clone(), false).await,
        Err(PairingFailure::PairingRevoked)
    );
}
#[tokio::test]
async fn verified_additional_client_binding_shares_only_its_original_pairing_key() {
    let f = Fixture::new(1, 1);
    let journal = Journal::new(vec![f.saved.clone()]);
    let store = JournalPairingStore::new(journal);
    let mut prepared = f.prepared.clone();
    let server = prepared.local_pairing.parts().server;
    let pairing = prepared.local_pairing.parts().pairing;
    prepared.client = ClientId::new(server, 20).unwrap();
    prepared.enrollment = OperationBinding {
        id: OperationId::new(
            f.saved.client().parts().ordinary,
            OperationSequence::new(2).unwrap(),
        ),
        digest: RequestDigest::from_bytes([12; 32]),
    };
    let client = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: prepared.client,
            pairing,
        },
        ordinary: OperationStreamId::new(
            OperationStreamKey::new(server, 60).unwrap(),
            pairing,
            prepared.client,
        )
        .unwrap(),
        self_revocation: OperationStreamId::new(
            OperationStreamKey::new(server, 61).unwrap(),
            pairing,
            prepared.client,
        )
        .unwrap(),
    })
    .unwrap();
    let proof = ReceiptCodec::new(prepared.pin)
        .sign(
            PairingReceiptBody::ClientBinding {
                target: f.target(),
                keys: prepared.keys.clone(),
                client,
                operation: prepared.enrollment,
                revision: StoreRevision::new(10),
            },
            f.server.as_ref(),
        )
        .unwrap();
    save_binding(&store, &prepared, proof, true).await.unwrap();
    let recovered = store.recover().await.unwrap();
    assert_eq!(recovered.saved.len(), 2);
    assert_ne!(
        recovered.saved[0].client().parts().client.id,
        recovered.saved[1].client().parts().client.id
    );
    assert_eq!(
        recovered.saved[0].credential(),
        recovered.saved[1].credential()
    );
}

#[tokio::test]
async fn pending_task_capacity_is_reserved_before_local_pairing_activation() {
    let f = Fixture::new(1, 1);
    let journal = Journal::new(Vec::new());
    for local in 2..MAX_PENDING_REVOCATIONS as u64 + 2 {
        let mut prepared = f.prepared.clone();
        let server = prepared.local_pairing.parts().server;
        let pairing = PairingId::new(server, local).unwrap();
        let client = ClientId::new(server, local).unwrap();
        prepared.local_pairing = LocalPairingKey::new(LocalPairingKeyParts {
            server,
            pairing,
            generation: LocalPairingGeneration::new(local).unwrap(),
        })
        .unwrap();
        prepared.client = client;
        prepared.enrollment.id = OperationId::new(
            OperationStreamId::new(
                OperationStreamKey::new(server, local * 3).unwrap(),
                pairing,
                client,
            )
            .unwrap(),
            OperationSequence::new(1).unwrap(),
        );
        let mut slot = [0; 16];
        slot[8..].copy_from_slice(&local.to_be_bytes());
        prepared.credential.scope = CredentialScope::new(CredentialScopeParts {
            reference: CredentialReference::new(slot),
            ..prepared.credential.scope.parts().clone()
        })
        .unwrap();
        journal.state.lock().unwrap().prepared.push(prepared);
    }
    let store = JournalPairingStore::new(journal.clone());
    assert_eq!(
        save_binding(&store, &f.prepared, f.saved.receipt().clone(), false).await,
        Err(PairingFailure::LimitExceeded)
    );
    assert!(journal.changes.lock().unwrap().is_empty());
}

#[derive(Clone)]
struct EnrollmentCredentials {
    device: Arc<Key>,
    auth: Arc<Key>,
}
impl NativeDeviceCredentials for EnrollmentCredentials {
    fn recover_preparation<'a>(
        &'a self,
        _: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        panic!("test keys are prepared")
    }
    fn create(
        &self,
        _: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        panic!("test uses already prepared native keys")
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        lease: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            lease.validate(&credential.scope)?;
            assert!(matches!(
                lease.purpose(),
                CredentialUsePurpose::Enrollment
                    | CredentialUsePurpose::EnrollmentAcknowledgement { .. }
            ));
            assert_eq!(
                lease.native_access(),
                if matches!(
                    lease.purpose(),
                    CredentialUsePurpose::EnrollmentAcknowledgement { .. }
                ) {
                    NativeCredentialAccess::Noninteractive
                } else {
                    NativeCredentialAccess::Foreground
                }
            );
            match credential.scope.parts().purpose {
                CredentialPurpose::DeviceIdentity => Ok(self.device.clone() as Arc<dyn SigningKey>),
                CredentialPurpose::PairingAuthentication { .. } => {
                    Ok(self.auth.clone() as Arc<dyn SigningKey>)
                }
            }
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        panic!("enrollment cannot delete old pairing keys")
    }
}
#[derive(Clone)]
struct EnrollmentEndpoint {
    server: Arc<Key>,
    reservation: Arc<Mutex<EnrollmentReservation>>,
    receipt: SignedPairingReceipt,
    major: u32,
    submissions: Arc<Mutex<usize>>,
    lookups: Arc<Mutex<usize>>,
    acknowledgements: Arc<Mutex<usize>>,
    journal: Arc<Journal>,
    lose_ack_reply: Arc<Mutex<bool>>,
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
                    // Commit happened, but the original reply is lost.
                    return Ok(());
                }
                let message = connection.decode(bytes).unwrap();
                if let Some(pairing::enrollment_message::Kind::Acknowledgement(ack)) = message.kind
                {
                    assert!(
                        !self.endpoint.journal.state.lock().unwrap().saved.is_empty(),
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
#[tokio::test]
async fn lost_enrollment_reply_recovers_original_identity_and_native_reference_in_both_majors() {
    use spaceterm_protocol::pairing_adapter::{
        EnrollmentAuthorization, PublicEnrollmentReservation, reservation_to_wire,
    };
    for (major, lose_ack_reply) in [(1, false), (2, false), (1, true), (2, true)] {
        let f = Fixture::new(1, 1);
        let journal = Journal::new(Vec::new());
        let authorization = EnrollmentAuthorization::generate().unwrap();
        let reservation = EnrollmentReservation::new(EnrollmentReservationParts {
            ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
                server: f.prepared.local_pairing.parts().server,
                nonce: [3; 16],
            })
            .unwrap(),
            target: f.target(),
            keys: f.prepared.keys.clone(),
            initial_client: f.prepared.client,
            enrollment_stream: f.prepared.enrollment.id.stream(),
            claim_digest: RequestDigest::from_bytes([4; 32]),
            authorization_digest: authorization.digest(),
            enrollment_request_digest: f.prepared.enrollment.digest,
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
        let endpoint = EnrollmentEndpoint {
            server: f.server.clone(),
            reservation: Arc::new(Mutex::new(reservation)),
            receipt: f.saved.receipt().clone(),
            major,
            submissions: Arc::new(Mutex::new(0)),
            lookups: Arc::new(Mutex::new(0)),
            acknowledgements: Arc::new(Mutex::new(0)),
            journal: journal.clone(),
            lose_ack_reply: Arc::new(Mutex::new(lose_ack_reply)),
        };
        let attempt = DirectEnrollmentAttempt::new(f.prepared.pin, f.prepared.route, 1).unwrap();
        let verified = attempt
            .confirm(f.prepared.pin, f.prepared.route, 1, &attempt.fingerprint())
            .unwrap();
        let device = PreparedCredential {
            scope: CredentialScope::new(CredentialScopeParts {
                namespace: f.prepared.credential.scope.parts().namespace.clone(),
                purpose: CredentialPurpose::DeviceIdentity,
                reference: CredentialReference::new([89; 16]),
            })
            .unwrap(),
            public_key: f.device.public_key(),
        };
        let driver = EnrollmentDriver::new(
            JournalPairingStore::new(journal.clone()),
            EnrollmentCredentials {
                device: f.device.clone(),
                auth: f.auth.clone(),
            },
            endpoint.clone(),
        );
        assert_eq!(
            driver
                .enroll(EnrollmentAttemptRequest {
                    verified: &verified,
                    prepared: &f.prepared,
                    reservation: public.clone(),
                    device: &device,
                    authorization: Some(&authorization),
                    majors: vec![2, 1]
                })
                .await,
            Err(PairingFailure::Offline)
        );
        let recovered = JournalPairingStore::new(journal.clone())
            .recover()
            .await
            .unwrap();
        assert!(recovered.saved.is_empty());
        assert_eq!(recovered.prepared.len(), 1);
        assert_eq!(recovered.prepared[0].reservation.as_ref(), Some(&public));
        let result = driver
            .enroll(EnrollmentAttemptRequest {
                verified: &verified,
                prepared: &recovered.prepared[0],
                reservation: public,
                device: &device,
                authorization: None,
                majors: vec![2, 1],
            })
            .await
            .unwrap();
        assert_eq!(result, f.saved);
        assert_eq!(*endpoint.submissions.lock().unwrap(), 1);
        assert_eq!(*endpoint.lookups.lock().unwrap(), 2);
        assert_eq!(
            *endpoint.acknowledgements.lock().unwrap(),
            1,
            "completed enrollment must acknowledge its retained result"
        );
        assert_eq!(
            journal
                .state
                .lock()
                .unwrap()
                .enrollment_acknowledgements
                .len(),
            usize::from(lose_ack_reply)
        );
        if lose_ack_reply {
            attempt.cancel();
            let restarted = EnrollmentDriver::new(
                JournalPairingStore::new(journal.clone()),
                EnrollmentCredentials {
                    device: f.device.clone(),
                    auth: f.auth.clone(),
                },
                endpoint.clone(),
            );
            restarted
                .acknowledge_once(f.prepared.enrollment.id, vec![2, 1])
                .await
                .unwrap();
            assert_eq!(*endpoint.acknowledgements.lock().unwrap(), 2);
            assert_eq!(*endpoint.submissions.lock().unwrap(), 1);
            assert!(
                journal
                    .state
                    .lock()
                    .unwrap()
                    .enrollment_acknowledgements
                    .is_empty()
            );
        }
        assert!(
            journal
                .changes
                .lock()
                .unwrap()
                .iter()
                .all(|change| !matches!(change, PairingJournalChange::SelfUnpair(_)))
        );
    }
}

struct BlockingConnector {
    opens: Arc<Mutex<usize>>,
}
impl RestrictedConnector for BlockingConnector {
    fn open<'a>(
        &'a self,
        _: &'a PendingRevocationTask,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async move {
            *self.opens.lock().unwrap() += 1;
            std::future::pending().await
        })
    }
}
#[tokio::test]
async fn restricted_driver_bounds_concurrency_and_releases_cancelled_attempts() {
    let a = Fixture::new(1, 1);
    let b = Fixture::new(1, 2);
    let c = Fixture::new(1, 3);
    let opens = Arc::new(Mutex::new(0));
    let driver = RevocationDriver::new(
        Credentials {
            auth: a.auth.clone(),
            journal: Journal::pending(a.pending()),
            deleted: Arc::new(Mutex::new(Vec::new())),
            missing: false,
        },
        BlockingConnector {
            opens: opens.clone(),
        },
        Arc::new(Codec),
        HandshakeConfig::default(),
    );
    let a_task = a.pending();
    let b_task = b.pending();
    let c_task = c.pending();
    let mut first = Box::pin(driver.resolve(&a_task));
    tokio::select! { biased; result = &mut first => panic!("unexpected resolution: {result:?}"), _ = tokio::task::yield_now() => () }
    assert_eq!(
        driver.resolve(&a_task).await,
        Err(PairingFailure::LimitExceeded)
    );
    let mut second = Box::pin(driver.resolve(&b_task));
    tokio::select! { biased; result = &mut second => panic!("unexpected resolution: {result:?}"), _ = tokio::task::yield_now() => () }
    assert_eq!(
        driver.resolve(&c_task).await,
        Err(PairingFailure::LimitExceeded)
    );
    assert_eq!(*opens.lock().unwrap(), 2);
    drop(first);
    let mut replacement = Box::pin(driver.resolve(&a_task));
    tokio::select! { biased; result = &mut replacement => panic!("unexpected resolution: {result:?}"), _ = tokio::task::yield_now() => () }
    assert_eq!(*opens.lock().unwrap(), 3);
}
struct BlockingStore;
impl LocalPairingStore for BlockingStore {
    fn ordinary_authority<'a>(
        &'a self,
        _: &'a LocalPairingKey,
    ) -> PairingFuture<'a, Result<OrdinaryCredentialAuthority, PairingFailure>> {
        panic!("enrollment cannot issue ordinary authority")
    }
    fn seal_ordinary(&self, _: &LocalPairingKey) {
        panic!("enrollment cannot unpair")
    }
    fn recover(&self) -> PairingFuture<'_, Result<PairingJournalSnapshot, PairingFailure>> {
        Box::pin(std::future::pending())
    }
    fn commit<'a>(
        &'a self,
        _: &'a PairingJournalTransaction,
    ) -> PairingFuture<'a, JournalCommitDisposition> {
        panic!("capacity test must not commit")
    }
}
#[tokio::test]
async fn enrollment_driver_bounds_concurrency_before_journal_and_native_access() {
    let f = Fixture::new(1, 1);
    let authorization =
        spaceterm_protocol::pairing_adapter::EnrollmentAuthorization::generate().unwrap();
    let r = EnrollmentReservation::new(EnrollmentReservationParts {
        ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
            server: f.prepared.local_pairing.parts().server,
            nonce: [3; 16],
        })
        .unwrap(),
        target: f.target(),
        keys: f.prepared.keys.clone(),
        initial_client: f.prepared.client,
        enrollment_stream: f.prepared.enrollment.id.stream(),
        claim_digest: RequestDigest::from_bytes([4; 32]),
        authorization_digest: authorization.digest(),
        enrollment_request_digest: f.prepared.enrollment.digest,
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
    let public = spaceterm_protocol::pairing_adapter::PublicEnrollmentReservation::from_wire(
        &spaceterm_protocol::pairing_adapter::reservation_to_wire(&r),
    )
    .unwrap();
    let endpoint = EnrollmentEndpoint {
        server: f.server.clone(),
        reservation: Arc::new(Mutex::new(r)),
        receipt: f.saved.receipt().clone(),
        major: 2,
        submissions: Arc::new(Mutex::new(0)),
        lookups: Arc::new(Mutex::new(0)),
        acknowledgements: Arc::new(Mutex::new(0)),
        journal: Journal::new(Vec::new()),
        lose_ack_reply: Arc::new(Mutex::new(false)),
    };
    let driver = EnrollmentDriver::new(
        BlockingStore,
        EnrollmentCredentials {
            device: f.device.clone(),
            auth: f.auth.clone(),
        },
        endpoint.clone(),
    );
    let attempt = DirectEnrollmentAttempt::new(f.prepared.pin, f.prepared.route, 1).unwrap();
    let verified = attempt
        .confirm(f.prepared.pin, f.prepared.route, 1, &attempt.fingerprint())
        .unwrap();
    let device = PreparedCredential {
        scope: CredentialScope::new(CredentialScopeParts {
            namespace: f.prepared.credential.scope.parts().namespace.clone(),
            purpose: CredentialPurpose::DeviceIdentity,
            reference: CredentialReference::new([89; 16]),
        })
        .unwrap(),
        public_key: f.device.public_key(),
    };
    let request = || EnrollmentAttemptRequest {
        verified: &verified,
        prepared: &f.prepared,
        reservation: public.clone(),
        device: &device,
        authorization: None,
        majors: vec![2, 1],
    };
    let mut attempts = Vec::new();
    for _ in 0..4 {
        let mut future = Box::pin(driver.enroll(request()));
        tokio::select! { biased; result = &mut future => panic!("unexpected enrollment: {result:?}"), _ = tokio::task::yield_now() => () }
        attempts.push(future);
    }
    assert_eq!(
        driver.enroll(request()).await,
        Err(PairingFailure::LimitExceeded)
    );
    assert_eq!(*endpoint.submissions.lock().unwrap(), 0);
    drop(attempts);
    let mut replacement = Box::pin(driver.enroll(request()));
    tokio::select! { biased; result = &mut replacement => panic!("unexpected enrollment: {result:?}"), _ = tokio::task::yield_now() => () }
}

struct ClaimCredentials {
    device: Arc<Key>,
    auth: Arc<Key>,
}
impl NativeDeviceCredentials for ClaimCredentials {
    fn recover_preparation<'a>(
        &'a self,
        _: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        panic!("claim cannot create keys")
    }
    fn create(
        &self,
        _: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        panic!("claims use existing native references")
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        lease: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            lease.validate(&credential.scope)?;
            Ok(match credential.scope.parts().purpose {
                CredentialPurpose::DeviceIdentity => self.device.clone(),
                CredentialPurpose::PairingAuthentication { .. } => self.auth.clone(),
            } as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        panic!("claim signing cannot delete credentials")
    }
}
#[tokio::test]
async fn additional_client_claim_requires_both_original_keys_and_live_ordinary_lease() {
    let f = Fixture::new(1, 1);
    let snapshot = PairingJournalSnapshot {
        saved: vec![f.saved.clone()],
        ..PairingJournalSnapshot::empty(0)
    };
    let authority = JournalPairingStore::new(Journal::new(snapshot.saved))
        .ordinary_authority(f.saved.key())
        .await
        .unwrap();
    let device = PreparedCredential {
        scope: CredentialScope::new(CredentialScopeParts {
            namespace: f.prepared.credential.scope.parts().namespace.clone(),
            purpose: CredentialPurpose::DeviceIdentity,
            reference: CredentialReference::new([89; 16]),
        })
        .unwrap(),
        public_key: f.device.public_key(),
    };
    let operation = OperationId::new(
        f.saved.client().parts().ordinary,
        OperationSequence::new(2).unwrap(),
    );
    let credentials = ClaimCredentials {
        device: f.device.clone(),
        auth: f.auth.clone(),
    };
    let version = spaceterm_core::authority::FieldVersion::new(4);
    let claim = prepare_additional_client_binding(
        &f.saved,
        &authority,
        &device,
        &credentials,
        version,
        operation,
    )
    .await
    .unwrap();
    spaceterm_protocol::pairing_adapter::verify_client_binding_claim(
        &f.target(),
        version,
        operation,
        &f.prepared.keys,
        claim.claim_digest(),
        &claim.wire().device_identity_proof,
        &claim.wire().pairing_authentication_proof,
    )
    .unwrap();
    assert!(
        spaceterm_protocol::pairing_adapter::verify_client_binding_claim(
            &f.target(),
            version,
            OperationId::new(operation.stream(), OperationSequence::new(3).unwrap()),
            &f.prepared.keys,
            claim.claim_digest(),
            &claim.wire().device_identity_proof,
            &claim.wire().pairing_authentication_proof
        )
        .is_err()
    );
    authority.seal();
    assert!(matches!(
        prepare_additional_client_binding(
            &f.saved,
            &authority,
            &device,
            &credentials,
            version,
            operation
        )
        .await,
        Err(PairingFailure::AuthorizationDenied)
    ));
}

#[tokio::test]
async fn self_unpair_fences_independently_issued_authorities_and_stale_snapshots() {
    let f = Fixture::new(1, 1);
    let journal = Journal::new(vec![f.saved.clone()]);
    let store = JournalPairingStore::new(journal.clone());
    let stale = store.recover().await.unwrap();
    let first = store.ordinary_authority(f.saved.key()).await.unwrap();
    let second = store.ordinary_authority(f.saved.key()).await.unwrap();
    let second_lease = second.lease();
    PairingController::new(JournalPairingStore::new(journal), Arc::new(Codec))
        .self_unpair(
            &first,
            ConfirmedSelfUnpair::confirm(
                f.saved.key().clone(),
                f.task().parts().original.operation(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        second_lease.validate(&f.prepared.credential.scope),
        Err(PairingFailure::AuthorizationDenied)
    );
    assert!(stale.active(f.saved.key()).is_some());
    assert!(store.ordinary_authority(f.saved.key()).await.is_err());
}

#[tokio::test]
async fn failed_self_unpair_cannot_reissue_ordinary_authority_from_unchanged_recovery() {
    for failure in [
        JournalCommitDisposition::Indeterminate,
        JournalCommitDisposition::NotApplied(PairingFailure::StorageUnavailable),
    ] {
        let f = Fixture::new(1, 1);
        let journal = Journal::new(vec![f.saved.clone()]);
        let store = JournalPairingStore::new(journal.clone());
        let authority = store.ordinary_authority(f.saved.key()).await.unwrap();
        *journal.fail.lock().unwrap() = Some(failure);
        assert!(
            PairingController::new(JournalPairingStore::new(journal), Arc::new(Codec))
                .self_unpair(
                    &authority,
                    ConfirmedSelfUnpair::confirm(
                        f.saved.key().clone(),
                        f.task().parts().original.operation()
                    )
                    .unwrap()
                )
                .await
                .is_err()
        );
        assert!(store.ordinary_authority(f.saved.key()).await.is_err());
    }
}

fn newer_pairing_reusing_original_slot(
    old: &Fixture,
) -> (PreparedEnrollment, SignedPairingReceipt) {
    let newer = Fixture::new(1, 2);
    let mut prepared = newer.prepared;
    prepared.credential = old.prepared.credential.clone();
    prepared.keys = old.prepared.keys.clone();
    let proof = ReceiptCodec::new(prepared.pin)
        .sign(
            PairingReceiptBody::Enrollment {
                target: PairingTarget::new(PairingTargetParts {
                    server: prepared.local_pairing.parts().server,
                    pairing: prepared.local_pairing.parts().pairing,
                    device: prepared.device,
                })
                .unwrap(),
                keys: prepared.keys.clone(),
                client: newer.saved.client().clone(),
                enrollment: prepared.enrollment,
                revision: StoreRevision::new(12),
            },
            old.server.as_ref(),
        )
        .unwrap();
    (prepared, proof)
}

#[tokio::test]
async fn pending_old_cleanup_rejects_new_valid_receipt_using_the_same_native_slot() {
    let old = Fixture::new(1, 1);
    let (prepared, proof) = newer_pairing_reusing_original_slot(&old);
    let saved = verify_binding(&prepared, proof.clone(), false).unwrap();
    let journal = Journal::pending(old.pending());
    let store = JournalPairingStore::new(journal.clone());
    assert_eq!(
        save_binding(&store, &prepared, proof, false).await,
        Err(PairingFailure::CredentialIntegrity)
    );
    assert_eq!(
        store.recover().await.unwrap().reserve_binding(&prepared),
        Err(PairingFailure::CredentialIntegrity)
    );
    journal.state.lock().unwrap().saved.push(saved);
    assert_eq!(
        store.recover().await,
        Err(PairingFailure::InvalidDurableState)
    );
}

#[test]
fn recovery_rejects_native_slot_reuse_between_prepared_and_saved_lifetimes() {
    let old = Fixture::new(1, 1);
    let (newer, _) = newer_pairing_reusing_original_slot(&old);
    let state = PairingJournalSnapshot {
        saved: vec![old.saved],
        prepared: vec![newer],
        ..PairingJournalSnapshot::empty(0)
    };
    assert_eq!(state.validate(), Err(PairingFailure::InvalidDurableState));
}

#[tokio::test]
async fn initial_enrollment_cannot_bypass_journalled_outcome_acknowledgement() {
    let f = Fixture::new(1, 1);
    let journal = Journal::new(Vec::new());
    assert_eq!(
        save_binding(
            &JournalPairingStore::new(journal.clone()),
            &f.prepared,
            f.saved.receipt().clone(),
            false
        )
        .await,
        Err(PairingFailure::AuthorizationRequired)
    );
    assert!(journal.changes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delayed_authority_issuance_from_captured_pre_removal_state_is_denied() {
    let f = Fixture::new(1, 1);
    let journal = Journal::new(vec![f.saved.clone()]);
    let store = JournalPairingStore::new(journal.clone());
    let first = store.ordinary_authority(f.saved.key()).await.unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let proceed = Arc::new(tokio::sync::Notify::new());
    *journal.delayed_recovery.lock().unwrap() = Some((started.clone(), proceed.clone()));
    let mut delayed = Box::pin(store.ordinary_authority(f.saved.key()));
    tokio::select! { biased; result = &mut delayed => panic!("recovery must wait: {}", result.is_ok()), _ = started.notified() => () }
    PairingController::new(JournalPairingStore::new(journal), Arc::new(Codec))
        .self_unpair(
            &first,
            ConfirmedSelfUnpair::confirm(
                f.saved.key().clone(),
                f.task().parts().original.operation(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    proceed.notify_one();
    assert!(matches!(
        delayed.await,
        Err(PairingFailure::AuthorizationDenied)
    ));
}

#[tokio::test]
async fn recovered_suppression_denies_delayed_first_authority_issuance() {
    let f = Fixture::new(1, 1);
    let other = Fixture::new(2, 1);
    let journal = Journal::new(vec![f.saved.clone(), other.saved.clone()]);
    let store = JournalPairingStore::new(journal.clone());
    let started = Arc::new(tokio::sync::Notify::new());
    let proceed = Arc::new(tokio::sync::Notify::new());
    *journal.delayed_recovery.lock().unwrap() = Some((started.clone(), proceed.clone()));
    let mut delayed = Box::pin(store.ordinary_authority(f.saved.key()));
    tokio::select! { biased; result = &mut delayed => panic!("first recovery must wait: {}", result.is_ok()), _ = started.notified() => () }

    // Another journal owner commits removal without using this runtime's facade.
    let tx = PairingJournalTransaction {
        expected_revision: 0,
        change: PairingJournalChange::SelfUnpair(Box::new(f.pending())),
    };
    require_durable(journal.commit_pairing(&tx).await, tx.expected_revision).unwrap();
    let recovered = JournalPairingStore::new(journal.clone())
        .recover()
        .await
        .unwrap();
    assert!(recovered.active(f.saved.key()).is_none());
    assert!(
        recovered
            .task(f.task().parts().original.operation())
            .is_some()
    );

    store
        .ordinary_authority(other.saved.key())
        .await
        .unwrap()
        .lease()
        .validate(&other.prepared.credential.scope)
        .unwrap();
    proceed.notify_one();
    assert!(matches!(
        delayed.await,
        Err(PairingFailure::AuthorizationDenied)
    ));
}

#[tokio::test]
async fn authority_capacity_is_reclaimed_after_completed_pairing_lifetimes() {
    let journal = Journal::new(Vec::new());
    let store = JournalPairingStore::new(journal.clone());
    let controller =
        PairingController::new(JournalPairingStore::new(journal.clone()), Arc::new(Codec));
    let mut retained = None;
    for lifetime in 1..=MAX_PENDING_REVOCATIONS as u64 {
        let f = Fixture::new(1, lifetime);
        {
            let mut state = journal.state.lock().unwrap();
            // Inject a verified durable binding at the frozen journal boundary.
            state.saved.push(f.saved.clone());
            state.generation_high_water = lifetime;
            state.revision += 1;
        }
        let authority = store.ordinary_authority(f.saved.key()).await.unwrap();
        if retained.is_none() {
            retained = Some((authority.lease(), f.prepared.credential.scope.clone()));
        }
        let operation = f.task().parts().original.operation();
        controller
            .self_unpair(
                &authority,
                ConfirmedSelfUnpair::confirm(f.saved.key().clone(), operation).unwrap(),
            )
            .await
            .unwrap();
        let (driver, deleted) = driver(&f, journal.clone(), 2, 1, None, false);
        driver.run_once(&store, operation).await.unwrap();
        assert_eq!(deleted.lock().unwrap().as_slice(), &[f.prepared.credential]);
        assert!(store.recover().await.unwrap().revocations.is_empty());
    }

    let newer = Fixture::new(1, MAX_PENDING_REVOCATIONS as u64 + 1);
    {
        let mut state = journal.state.lock().unwrap();
        assert!(state.saved.is_empty());
        assert_eq!(state.suppressed.len(), MAX_PENDING_REVOCATIONS);
        state.saved.push(newer.saved.clone());
        state.generation_high_water = newer.saved.key().parts().generation.get();
        state.revision += 1;
    }
    let recovered = store.recover().await.unwrap();
    assert_eq!(
        recovered.saved.as_slice(),
        std::slice::from_ref(&newer.saved)
    );
    assert!(recovered.revocations.is_empty());
    let authority = store.ordinary_authority(newer.saved.key()).await.unwrap();
    authority
        .lease()
        .validate(&newer.prepared.credential.scope)
        .unwrap();
    let (old_lease, old_scope) = retained.unwrap();
    assert_eq!(
        old_lease.validate(&old_scope),
        Err(PairingFailure::AuthorizationDenied)
    );
}

#[tokio::test]
async fn recovered_suppression_closes_existing_cached_authority() {
    let f = Fixture::new(1, 1);
    let journal = Journal::new(vec![f.saved.clone()]);
    let store = JournalPairingStore::new(journal.clone());
    let authority = store.ordinary_authority(f.saved.key()).await.unwrap();
    journal
        .state
        .lock()
        .unwrap()
        .suppressed
        .push(f.saved.key().clone());
    assert!(store.recover().await.unwrap().saved.is_empty());
    assert_eq!(
        authority.lease().validate(&f.prepared.credential.scope),
        Err(PairingFailure::AuthorizationDenied)
    );
}

#[tokio::test]
async fn confirmed_revocation_retains_credential_until_original_enrollment_is_acknowledged() {
    let f = Fixture::new(1, 1);
    let mut pending = f.pending();
    let operation = pending.task.parts().original.operation();
    pending.phase = RevocationPhase::ReceiptRecorded {
        receipt: Box::new(
            VerifiedRevocationReceipt::verify(
                &pending.task,
                &pending.credential,
                f.receipt(operation),
                false,
            )
            .unwrap(),
        ),
        acknowledged: true,
    };
    let reservation = EnrollmentReservation::new(EnrollmentReservationParts {
        ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
            server: f.prepared.local_pairing.parts().server,
            nonce: [3; 16],
        })
        .unwrap(),
        target: f.target(),
        keys: f.prepared.keys.clone(),
        initial_client: f.prepared.client,
        enrollment_stream: f.prepared.enrollment.id.stream(),
        claim_digest: RequestDigest::from_bytes([4; 32]),
        authorization_digest: RequestDigest::from_bytes([7; 32]),
        enrollment_request_digest: f.prepared.enrollment.digest,
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
    let mut prepared = f.prepared.clone();
    prepared.reservation = Some(
        spaceterm_protocol::pairing_adapter::PublicEnrollmentReservation::from_wire(
            &spaceterm_protocol::pairing_adapter::reservation_to_wire(&reservation),
        )
        .unwrap(),
    );
    let journal = Journal::pending(pending);
    journal
        .state
        .lock()
        .unwrap()
        .enrollment_acknowledgements
        .push(EnrollmentAcknowledgementWork {
            prepared,
            device: PreparedCredential {
                scope: CredentialScope::new(CredentialScopeParts {
                    namespace: f.prepared.credential.scope.parts().namespace.clone(),
                    purpose: CredentialPurpose::DeviceIdentity,
                    reference: CredentialReference::new([89; 16]),
                })
                .unwrap(),
                public_key: f.device.public_key(),
            },
            receipt: f.saved.receipt().clone(),
        });
    let (driver, deleted) = driver(&f, journal.clone(), 2, 1, None, false);
    assert_eq!(
        driver
            .run_once(&JournalPairingStore::new(journal.clone()), operation)
            .await,
        Err(PairingFailure::RecoveryRequired)
    );
    assert!(deleted.lock().unwrap().is_empty());
    assert_eq!(journal.state.lock().unwrap().revocations.len(), 1);
}
