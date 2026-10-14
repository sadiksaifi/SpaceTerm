use ring::signature::{Ed25519KeyPair, KeyPair};
use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding, RouteDescription, RouteParts},
    failure::{ClientFailure, ClientFailureClass},
    journal::{
        ClientJournal, ClientJournalChange, ClientJournalTransaction, JournalCredentialSlot,
        JournalLimits, ReceiptVerifier, SavedBinding,
    },
    pairing::credentials::*,
};
use spaceterm_client_native::journal::NativeClientJournal;
use spaceterm_core::{
    authority::StoreRevision, identity::*, operation_ledger::canonical::CanonicalCodec,
    operations::*, pairing::*,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ServerPin, SigningKey},
    pairing_adapter::ReceiptCodec,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub(super) struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))
    }
}
pub(super) struct Directory(pub(super) PathBuf);
impl Directory {
    pub(super) fn in_parent(parent: PathBuf) -> Self {
        use std::os::unix::fs::DirBuilderExt;
        static NEXT: AtomicU64 = AtomicU64::new(0);
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
    pub(super) async fn create(&self) -> NativeClientJournal {
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

pub(super) struct Key(Ed25519KeyPair);
impl Key {
    pub(super) fn new(seed: u8) -> Self {
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
pub(super) struct Fixture {
    pub(super) server: Key,
    pub(super) binding: PairedServerBinding,
    pub(super) credential: PreparedCredential,
    pub(super) saved: SavedBinding,
    pub(super) route: RevocationRouteReference,
}
impl Fixture {
    pub(super) fn new(lifetime: u64) -> Self {
        Self::on_server(lifetime, ServerId::new([1; 16]))
    }
    pub(super) fn on_server(lifetime: u64, id: ServerId) -> Self {
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
    pub(super) fn seed_changes(&self) -> Vec<ClientJournalChange> {
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
    pub(super) async fn seed(&self, journal: &dyn ClientJournal) {
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
