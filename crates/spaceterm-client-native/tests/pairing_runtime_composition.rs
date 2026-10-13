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
        journal::*,
        
    },
    pending::{
        LocalUseInvalidator,
    },
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

use spaceterm_client_native::pairing_runtime::NativePairingRuntime;
use spaceterm_protocol::authentication::HandshakeConfig;

fn runtime(journal: Arc<JournalController>) -> NativePairingRuntime {
    NativePairingRuntime::native(
        journal,
        spaceterm_client_native::route::NativeRouteConnector::new(vec![], None).unwrap(),
        Arc::new(NoEffects),
        HandshakeConfig::default(),
    )
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
    let restored = composition.restore(&key, fixture.binding.target().client()).await.unwrap();
    assert_eq!(restored.binding(), &fixture.saved);
    assert!(restored.authority().lease().validate(&fixture.credential.scope).is_ok());
    assert!(matches!(
        composition.restore(&key, ClientId::new(key.parts().server, 99).unwrap()).await,
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
    let ordinary = composition.restore(&key, fixture.binding.target().client()).await.unwrap();
    let lease = ordinary.authority().lease();
    let assessment = composition.assess_unpair(&key, fixture.binding.target().client()).await.unwrap();
    let receipt = composition.self_unpair(assessment).await.unwrap();
    assert!(lease.validate(&fixture.credential.scope).is_err());
    composition.dismiss_notice(receipt.operation).await.unwrap();
    drop(ordinary);
    drop(composition);
    let native = NativeClientJournal::open_existing(
        directory.0.clone(), Arc::new(CanonicalCodec), Arc::new(Verifier), JournalLimits::default()
    ).await.unwrap();
    let reopened = runtime(controller(Arc::new(native)).await);
    let inventory = reopened.inventory().await.unwrap();
    assert!(inventory.is_suppressed(&key));
    assert!(inventory.task(receipt.operation).unwrap().notice_dismissed);
    assert_eq!(inventory.revocations.len(), 1);
    assert!(matches!(reopened.restore(&key, fixture.binding.target().client()).await, Err(PairingFailure::PairingRevoked)));
    let scheduler = reopened.start_scheduler().unwrap();
    assert!(reopened.start_scheduler().is_err());
    scheduler.shutdown().await.unwrap();
}
