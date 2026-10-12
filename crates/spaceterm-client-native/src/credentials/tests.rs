use super::adapter::{DeviceCredentials, NativeKeyStore};
use spaceterm_client::journal::{ClientJournal, JournalCommitDisposition, JournalReceipt};
use spaceterm_client::pairing::{
    credentials::{
        CredentialNamespace, CredentialPurpose, CredentialScope, NativeDeviceCredentials,
        PairingAuthorityFence, PairingFuture, PreparedCredential,
    },
    enrollment::DirectEnrollmentAttempt,
    journal::{
        JournalPairingStore, PairingJournalChange, PairingJournalSnapshot,
        PairingJournalTransaction, prepare_native_credential, recover_native_credential,
    },
};
use spaceterm_core::{
    identity::ServerId,
    pairing::{PairingFailure, RevocationRouteReference},
};
use spaceterm_protocol::authentication::ServerPin;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

#[derive(Default)]
struct MemoryNative {
    items: Mutex<std::collections::HashMap<String, Zeroizing<Vec<u8>>>>,
    requires_foreground: bool,
    journal: Option<Arc<PreparationJournal>>,
    observed_reservations: Mutex<Vec<Option<spaceterm_core::pairing::LocalPairingGeneration>>>,
}
impl NativeKeyStore for MemoryNative {
    fn read<'a>(
        &'a self,
        scope: &'a spaceterm_client::pairing::credentials::CredentialScope,
        access: spaceterm_client::pairing::credentials::NativeCredentialAccess,
    ) -> spaceterm_client::pairing::credentials::PairingFuture<
        'a,
        Result<Option<Zeroizing<Vec<u8>>>, PairingFailure>,
    > {
        Box::pin(async move {
            if self.requires_foreground
                && access
                    != spaceterm_client::pairing::credentials::NativeCredentialAccess::Foreground
            {
                return Err(PairingFailure::CredentialUnavailable);
            }
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(&DeviceCredentials::locator(scope))
                .cloned())
        })
    }
    fn insert<'a>(
        &'a self,
        scope: &'a spaceterm_client::pairing::credentials::CredentialScope,
        secret: &'a [u8],
    ) -> spaceterm_client::pairing::credentials::PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            self.observed_reservations
                .lock()
                .unwrap()
                .push(self.journal.as_ref().and_then(|journal| {
                    journal
                        .state
                        .lock()
                        .unwrap()
                        .credential_preparations
                        .iter()
                        .find(|v| v.scope == *scope)
                        .map(|v| v.generation)
                }));
            let mut items = self.items.lock().unwrap();
            let locator = DeviceCredentials::locator(scope);
            if items.contains_key(&locator) {
                return Err(PairingFailure::CredentialIntegrity);
            }
            items.insert(locator, Zeroizing::new(secret.to_vec()));
            Ok(())
        })
    }
    fn remove<'a>(
        &'a self,
        scope: &'a spaceterm_client::pairing::credentials::CredentialScope,
        expected: &'a [u8],
    ) -> spaceterm_client::pairing::credentials::PairingFuture<'a, Result<bool, PairingFailure>>
    {
        Box::pin(async move {
            let mut items = self.items.lock().unwrap();
            let locator = DeviceCredentials::locator(scope);
            if let Some(value) = items.get(&locator) {
                if value.as_slice() != expected {
                    return Err(PairingFailure::CredentialIntegrity);
                }
            } else {
                return Ok(false);
            }
            items.remove(&locator);
            Ok(true)
        })
    }
}
#[tokio::test]
async fn credential_deletion_cannot_select_shared_identity_slot() {
    let adapter = DeviceCredentials::with_store(Arc::new(MemoryNative::default()));
    let namespace = CredentialNamespace::new("dev.spaceterm.test".into()).unwrap();
    let identity = create(&adapter, &namespace, CredentialPurpose::DeviceIdentity).await;
    assert_eq!(
        adapter.delete_pairing_key(&identity).await,
        Err(PairingFailure::AuthorizationDenied)
    );
}
#[tokio::test]
async fn missing_native_read_never_regenerates_existing_key() {
    let store = Arc::new(MemoryNative::default());
    let adapter = DeviceCredentials::with_store(store.clone());
    let namespace = CredentialNamespace::new("dev.spaceterm.test".into()).unwrap();
    let pin = ServerPin {
        identity: [1; 16],
        public_key: [2; 32],
    };
    let attempt =
        DirectEnrollmentAttempt::new(pin, RevocationRouteReference::new([3; 16]), 1).unwrap();
    let verified = attempt
        .confirm(
            pin,
            RevocationRouteReference::new([3; 16]),
            1,
            &attempt.fingerprint(),
        )
        .unwrap();
    let credential = create(
        &adapter,
        &namespace,
        CredentialPurpose::PairingAuthentication {
            server: ServerId::new(pin.identity),
        },
    )
    .await;
    let lease = verified.credential_use(&credential).unwrap();
    store.items.lock().unwrap().clear();
    assert!(matches!(
        adapter.signing_key(&credential, &lease).await,
        Err(PairingFailure::CredentialUnavailable)
    ));
    assert!(store.items.lock().unwrap().is_empty());
}
#[tokio::test]
async fn signing_capability_rechecks_lease_after_native_read() {
    let adapter = DeviceCredentials::with_store(Arc::new(MemoryNative::default()));
    let namespace = CredentialNamespace::new("dev.spaceterm.test".into()).unwrap();
    let pin = ServerPin {
        identity: [1; 16],
        public_key: [2; 32],
    };
    let route = RevocationRouteReference::new([3; 16]);
    let attempt = DirectEnrollmentAttempt::new(pin, route, 1).unwrap();
    let verified = attempt
        .confirm(pin, route, 1, &attempt.fingerprint())
        .unwrap();
    let credential = create(
        &adapter,
        &namespace,
        CredentialPurpose::PairingAuthentication {
            server: ServerId::new(pin.identity),
        },
    )
    .await;
    let signer = adapter
        .signing_key(&credential, &verified.credential_use(&credential).unwrap())
        .await
        .unwrap();
    attempt.cancel();
    assert!(signer.sign(b"must not sign").is_err());
}

#[tokio::test]
async fn credential_deletion_compares_expected_public_key_and_preserves_other_material() {
    use spaceterm_client::pairing::credentials::CredentialDeletion;
    let store = Arc::new(MemoryNative::default());
    let native = DeviceCredentials::with_store(store.clone());
    let namespace = CredentialNamespace::new("dev.spaceterm.test".into()).unwrap();
    let identity = create(&native, &namespace, CredentialPurpose::DeviceIdentity).await;
    let old = create(
        &native,
        &namespace,
        CredentialPurpose::PairingAuthentication {
            server: ServerId::new([1; 16]),
        },
    )
    .await;
    let newer = create(
        &native,
        &namespace,
        CredentialPurpose::PairingAuthentication {
            server: ServerId::new([1; 16]),
        },
    )
    .await;
    let other = create(
        &native,
        &namespace,
        CredentialPurpose::PairingAuthentication {
            server: ServerId::new([2; 16]),
        },
    )
    .await;
    let mut changed = old.clone();
    changed.public_key = [8; 32];
    assert_eq!(
        native.delete_pairing_key(&changed).await,
        Err(PairingFailure::CredentialIntegrity)
    );
    assert_eq!(store.items.lock().unwrap().len(), 4);
    assert_eq!(
        native.delete_pairing_key(&old).await.unwrap(),
        CredentialDeletion::Deleted
    );
    assert_eq!(
        native.delete_pairing_key(&old).await.unwrap(),
        CredentialDeletion::AlreadyMissing
    );
    let items = store.items.lock().unwrap();
    assert_eq!(items.len(), 3);
    for retained in [identity, newer, other] {
        assert!(items.contains_key(&DeviceCredentials::locator(&retained.scope)));
    }
}

#[tokio::test]
async fn ordinary_signer_rechecks_sealed_lease() {
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use spaceterm_client::pairing::{
        credentials::PreparedCredential,
        enrollment::verify_binding,
        journal::{
            JournalPairingStore, LocalPairingStore, PairingJournalSnapshot, PreparedEnrollment,
        },
    };
    use spaceterm_core::{
        authority::StoreRevision,
        identity::{ClientId, DeviceId, OperationStreamId, OperationStreamKey, PairingId},
        operations::{OperationBinding, OperationId, OperationSequence, RequestDigest},
        pairing::{
            ClientRecord, LocalPairingGeneration, LocalPairingKey, LocalPairingKeyParts,
            PairingClientStreams, PairingClientStreamsParts, PairingKeys, PairingKeysParts,
            PairingReceiptBody, PairingTarget, PairingTargetParts,
        },
    };
    use spaceterm_protocol::{
        ProtocolError, authentication::SigningKey, pairing_adapter::ReceiptCodec,
    };
    struct Server(Ed25519KeyPair);
    impl SigningKey for Server {
        fn public_key(&self) -> [u8; 32] {
            self.0.public_key().as_ref().try_into().unwrap()
        }
        fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
            Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
        }
    }
    let native = DeviceCredentials::with_store(Arc::new(MemoryNative::default()));
    let namespace = CredentialNamespace::new("dev.spaceterm.test".into()).unwrap();
    let server = ServerId::new([1; 16]);
    let pairing = PairingId::new(server, 1).unwrap();
    let client = ClientId::new(server, 1).unwrap();
    let credential: PreparedCredential = create(
        &native,
        &namespace,
        CredentialPurpose::PairingAuthentication { server },
    )
    .await;
    let signer = Server(Ed25519KeyPair::from_seed_unchecked(&[98; 32]).unwrap());
    let pin = ServerPin {
        identity: server.bytes(),
        public_key: signer.public_key(),
    };
    let stream = |local| {
        OperationStreamId::new(
            OperationStreamKey::new(server, local).unwrap(),
            pairing,
            client,
        )
        .unwrap()
    };
    let local_pairing = LocalPairingKey::new(LocalPairingKeyParts {
        server,
        pairing,
        generation: LocalPairingGeneration::new(1).unwrap(),
    })
    .unwrap();
    let target = PairingTarget::new(PairingTargetParts {
        server,
        pairing,
        device: DeviceId::new([7; 16]),
    })
    .unwrap();
    let keys = PairingKeys::new(PairingKeysParts {
        device_identity_public_key: [5; 32],
        pairing_authentication_public_key: credential.public_key,
    })
    .unwrap();
    let enrollment = OperationBinding {
        id: OperationId::new(stream(1), OperationSequence::new(1).unwrap()),
        digest: RequestDigest::from_bytes([8; 32]),
    };
    let prepared = PreparedEnrollment {
        local_pairing,
        device: target.parts().device,
        pin,
        keys: keys.clone(),
        client,
        credential: credential.clone(),
        route: RevocationRouteReference::new([8; 16]),
        enrollment,
        reservation: None,
    };
    let receipt = ReceiptCodec::new(pin)
        .sign(
            PairingReceiptBody::Enrollment {
                target,
                keys,
                client: PairingClientStreams::new(PairingClientStreamsParts {
                    client: ClientRecord {
                        id: client,
                        pairing,
                    },
                    ordinary: stream(2),
                    self_revocation: stream(3),
                })
                .unwrap(),
                enrollment,
                revision: StoreRevision::new(1),
            },
            &signer,
        )
        .unwrap();
    let saved = verify_binding(&prepared, receipt, false).unwrap();
    let snapshot = PairingJournalSnapshot {
        saved: vec![saved],
        ..PairingJournalSnapshot::empty(0)
    };
    let journal = Arc::new(RecoveredJournal {
        state: snapshot,
        fence: Default::default(),
    });
    let store = JournalPairingStore::new(journal.clone());
    let authority = store
        .ordinary_authority(&prepared.local_pairing)
        .await
        .unwrap();
    let other = JournalPairingStore::new(journal)
        .ordinary_authority(&prepared.local_pairing)
        .await
        .unwrap();
    let signer = native
        .signing_key(&credential, &authority.lease())
        .await
        .unwrap();
    assert!(signer.sign(b"before local removal").is_ok());
    let other_signer = native
        .signing_key(&credential, &other.lease())
        .await
        .unwrap();
    authority.seal();
    assert!(
        store
            .ordinary_authority(&prepared.local_pairing)
            .await
            .is_err()
    );
    assert_eq!(
        other_signer.sign(b"independently cached signer after removal"),
        Err(ProtocolError::SigningUnavailable)
    );
    assert_eq!(
        signer.sign(b"after local removal"),
        Err(ProtocolError::SigningUnavailable)
    );
}

#[tokio::test]
async fn deliberate_enrollment_can_authorize_an_existing_key_from_another_executable() {
    let store = Arc::new(MemoryNative {
        requires_foreground: true,
        ..MemoryNative::default()
    });
    let native = DeviceCredentials::with_store(store.clone());
    let namespace = CredentialNamespace::new("dev.spaceterm.test".into()).unwrap();
    let pin = ServerPin {
        identity: [1; 16],
        public_key: [2; 32],
    };
    let route = RevocationRouteReference::new([3; 16]);
    let attempt = DirectEnrollmentAttempt::new(pin, route, 1).unwrap();
    let verified = attempt
        .confirm(pin, route, 1, &attempt.fingerprint())
        .unwrap();
    let credential = create(
        &native,
        &namespace,
        CredentialPurpose::PairingAuthentication {
            server: ServerId::new(pin.identity),
        },
    )
    .await;
    assert!(
        native
            .signing_key(&credential, &verified.credential_use(&credential).unwrap())
            .await
            .is_ok(),
        "foreground enrollment must expose a native authorization path"
    );
    assert_eq!(
        native.recover_preparation(&credential.scope).await,
        Err(PairingFailure::CredentialUnavailable)
    );
    assert_eq!(
        native.delete_pairing_key(&credential).await,
        Err(PairingFailure::CredentialUnavailable)
    );
    assert_eq!(
        store.items.lock().unwrap().len(),
        1,
        "background retries must not prompt or erase unavailable material"
    );
}

struct RecoveredJournal {
    state: spaceterm_client::pairing::journal::PairingJournalSnapshot,
    fence: spaceterm_client::pairing::credentials::PairingAuthorityFence,
}
impl spaceterm_client::journal::ClientJournal for RecoveredJournal {
    fn pairing_authority_fence(
        &self,
    ) -> &spaceterm_client::pairing::credentials::PairingAuthorityFence {
        &self.fence
    }
    fn recover_pairings(
        &self,
    ) -> spaceterm_client::pairing::credentials::PairingFuture<
        '_,
        Result<spaceterm_client::pairing::journal::PairingJournalSnapshot, PairingFailure>,
    > {
        Box::pin(async { Ok(self.state.clone()) })
    }
    fn commit_pairing<'a>(
        &'a self,
        _: &'a spaceterm_client::pairing::journal::PairingJournalTransaction,
    ) -> spaceterm_client::pairing::credentials::PairingFuture<
        'a,
        spaceterm_client::journal::JournalCommitDisposition,
    > {
        panic!("signing test cannot commit")
    }
}

struct PreparationJournal {
    fail: Mutex<Option<JournalCommitDisposition>>,
    state: Mutex<PairingJournalSnapshot>,
    fence: PairingAuthorityFence,
}
impl ClientJournal for PreparationJournal {
    fn pairing_authority_fence(&self) -> &PairingAuthorityFence {
        &self.fence
    }
    fn recover_pairings(
        &self,
    ) -> PairingFuture<'_, Result<PairingJournalSnapshot, PairingFailure>> {
        Box::pin(async { Ok(self.state.lock().unwrap().clone()) })
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
                PairingJournalChange::ReserveCredential(record) => {
                    state.generation_high_water = record.generation.get();
                    state.credential_preparations.push(record.clone());
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
                _ => panic!("native preparation test cannot activate bindings"),
            }
            state.revision += 1;
            JournalCommitDisposition::Durable(JournalReceipt {
                revision: state.revision,
            })
        })
    }
}
async fn create(
    native: &DeviceCredentials,
    namespace: &CredentialNamespace,
    purpose: CredentialPurpose,
) -> PreparedCredential {
    let journal = Arc::new(PreparationJournal {
        fail: Mutex::new(None),
        state: Mutex::new(PairingJournalSnapshot::empty(0)),
        fence: Default::default(),
    });
    prepare_native_credential(
        &JournalPairingStore::new(journal),
        native,
        DeviceCredentials::new_scope(namespace.clone(), purpose).unwrap(),
    )
    .await
    .unwrap()
    .1
}

#[tokio::test]
async fn native_key_creation_retains_its_slot_before_the_first_native_effect() {
    let journal = Arc::new(PreparationJournal {
        fail: Mutex::new(None),
        state: Mutex::new(PairingJournalSnapshot::empty(0)),
        fence: Default::default(),
    });
    let store = Arc::new(MemoryNative {
        journal: Some(journal.clone()),
        ..Default::default()
    });
    let native = DeviceCredentials::with_store(store.clone());
    let namespace = CredentialNamespace::new("dev.spaceterm.test".into()).unwrap();
    let (generation, credential) = prepare_native_credential(
        &JournalPairingStore::new(journal.clone()),
        &native,
        DeviceCredentials::new_scope(
            namespace,
            CredentialPurpose::PairingAuthentication {
                server: ServerId::new([1; 16]),
            },
        )
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        store.observed_reservations.lock().unwrap().as_slice(),
        &[Some(generation)]
    );
    let state = journal.state.lock().unwrap();
    assert_eq!(
        state.credential_preparations[0].credential,
        Some(credential)
    );
}

#[tokio::test]
async fn full_capacity_or_exhausted_generation_creates_no_native_item() {
    use spaceterm_client::pairing::{
        credentials::CredentialScopeParts, journal::CredentialPreparation,
    };
    use spaceterm_core::pairing::{
        CredentialReference, LocalPairingGeneration, MAX_PENDING_REVOCATIONS,
    };
    for exhausted_generation in [false, true] {
        let mut state = PairingJournalSnapshot::empty(0);
        let namespace = CredentialNamespace::new("dev.spaceterm.test".into()).unwrap();
        let purpose = CredentialPurpose::PairingAuthentication {
            server: ServerId::new([1; 16]),
        };
        if exhausted_generation {
            state.generation_high_water = u64::MAX;
        } else {
            state.generation_high_water = MAX_PENDING_REVOCATIONS as u64;
            for number in 1..=MAX_PENDING_REVOCATIONS as u64 {
                let mut slot = [0; 16];
                slot[8..].copy_from_slice(&number.to_be_bytes());
                state.credential_preparations.push(CredentialPreparation {
                    generation: LocalPairingGeneration::new(number).unwrap(),
                    scope: CredentialScope::new(CredentialScopeParts {
                        namespace: namespace.clone(),
                        purpose,
                        reference: CredentialReference::new(slot),
                    })
                    .unwrap(),
                    credential: None,
                });
            }
        }
        let journal = Arc::new(PreparationJournal {
            fail: Mutex::new(None),
            state: Mutex::new(state),
            fence: Default::default(),
        });
        let store = Arc::new(MemoryNative::default());
        let native = DeviceCredentials::with_store(store.clone());
        assert_eq!(
            prepare_native_credential(
                &JournalPairingStore::new(journal),
                &native,
                DeviceCredentials::new_scope(namespace, purpose).unwrap()
            )
            .await,
            Err(PairingFailure::LimitExceeded)
        );
        assert!(store.items.lock().unwrap().is_empty());
    }
}

struct DeferredInsertion {
    memory: Arc<MemoryNative>,
    started: Arc<tokio::sync::Notify>,
    proceed: Arc<tokio::sync::Notify>,
    finished: Arc<tokio::sync::Notify>,
}
impl NativeKeyStore for DeferredInsertion {
    fn read<'a>(
        &'a self,
        scope: &'a CredentialScope,
        access: spaceterm_client::pairing::credentials::NativeCredentialAccess,
    ) -> PairingFuture<'a, Result<Option<Zeroizing<Vec<u8>>>, PairingFailure>> {
        self.memory.read(scope, access)
    }
    fn insert<'a>(
        &'a self,
        scope: &'a CredentialScope,
        secret: &'a [u8],
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            let memory = self.memory.clone();
            let scope = scope.clone();
            let secret = Zeroizing::new(secret.to_vec());
            let proceed = self.proceed.clone();
            let finished = self.finished.clone();
            let insertion = tokio::spawn(async move {
                proceed.notified().await;
                memory.insert(&scope, &secret).await.unwrap();
                finished.notify_one();
            });
            self.started.notify_one();
            insertion
                .await
                .map_err(|_| PairingFailure::CredentialUnavailable)
        })
    }
    fn remove<'a>(
        &'a self,
        scope: &'a CredentialScope,
        expected: &'a [u8],
    ) -> PairingFuture<'a, Result<bool, PairingFailure>> {
        self.memory.remove(scope, expected)
    }
}
#[tokio::test]
async fn cancelled_creation_recovers_the_original_slot_after_native_insertion_finishes() {
    let journal = Arc::new(PreparationJournal {
        fail: Mutex::new(None),
        state: Mutex::new(PairingJournalSnapshot::empty(0)),
        fence: Default::default(),
    });
    let memory = Arc::new(MemoryNative {
        journal: Some(journal.clone()),
        ..Default::default()
    });
    let started = Arc::new(tokio::sync::Notify::new());
    let proceed = Arc::new(tokio::sync::Notify::new());
    let finished = Arc::new(tokio::sync::Notify::new());
    let native = DeviceCredentials::with_store(Arc::new(DeferredInsertion {
        memory: memory.clone(),
        started: started.clone(),
        proceed: proceed.clone(),
        finished: finished.clone(),
    }));
    let store = JournalPairingStore::new(journal.clone());
    let scope = DeviceCredentials::new_scope(
        CredentialNamespace::new("dev.spaceterm.test".into()).unwrap(),
        CredentialPurpose::PairingAuthentication {
            server: ServerId::new([1; 16]),
        },
    )
    .unwrap();
    let mut creation = Box::pin(prepare_native_credential(&store, &native, scope.clone()));
    tokio::select! { biased; result = &mut creation => panic!("insertion must wait: {result:?}"), _ = started.notified() => () }
    let generation = journal.state.lock().unwrap().credential_preparations[0].generation;
    drop(creation);
    assert_eq!(
        recover_native_credential(&store, &native, generation)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        journal.state.lock().unwrap().credential_preparations.len(),
        1
    );
    proceed.notify_one();
    finished.notified().await;
    let recovered = recover_native_credential(&store, &native, generation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.scope, scope);
    assert_eq!(memory.items.lock().unwrap().len(), 1);
    assert_eq!(
        journal.state.lock().unwrap().credential_preparations[0].credential,
        Some(recovered)
    );
}

#[tokio::test]
async fn failed_or_indeterminate_preparation_admission_creates_no_native_item() {
    for failure in [
        JournalCommitDisposition::NotApplied(PairingFailure::StorageUnavailable),
        JournalCommitDisposition::Indeterminate,
    ] {
        let journal = Arc::new(PreparationJournal {
            fail: Mutex::new(Some(failure)),
            state: Mutex::new(PairingJournalSnapshot::empty(0)),
            fence: Default::default(),
        });
        let memory = Arc::new(MemoryNative::default());
        let native = DeviceCredentials::with_store(memory.clone());
        let scope = DeviceCredentials::new_scope(
            CredentialNamespace::new("dev.spaceterm.test".into()).unwrap(),
            CredentialPurpose::PairingAuthentication {
                server: ServerId::new([1; 16]),
            },
        )
        .unwrap();
        assert!(
            prepare_native_credential(&JournalPairingStore::new(journal.clone()), &native, scope)
                .await
                .is_err()
        );
        assert!(memory.items.lock().unwrap().is_empty());
        assert!(
            journal
                .state
                .lock()
                .unwrap()
                .credential_preparations
                .is_empty()
        );
    }
}
