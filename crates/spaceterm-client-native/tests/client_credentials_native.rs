use spaceterm_client::pairing::{
    credentials::{CredentialNamespace, CredentialPurpose, NativeDeviceCredentials},
    enrollment::DirectEnrollmentAttempt,
};
use spaceterm_client_native::credentials::adapter::DeviceCredentials;
use spaceterm_core::{identity::ServerId, pairing::RevocationRouteReference};
use spaceterm_protocol::authentication::ServerPin;

/// Opt-in only: creates one random native pairing slot and deletes that exact
/// slot. An unavailable or locked Keychain/Secret Service fails the evidence run.
#[tokio::test]
#[ignore = "requires explicit authorization to mutate disposable native credential items"]
async fn disposable_native_pairing_key_signs_and_deletes_exact_slot() {
    assert_eq!(
        std::env::var("SPACETERM_NATIVE_CLIENT_CREDENTIAL_TESTS").as_deref(),
        Ok("1")
    );
    let native = DeviceCredentials::native();
    let namespace = CredentialNamespace::new("dev.spaceterm.native-evidence".into()).unwrap();
    let pin = ServerPin {
        identity: [55; 16],
        public_key: [77; 32],
    };
    let route = RevocationRouteReference::new([11; 16]);
    let attempt = DirectEnrollmentAttempt::new(pin, route, 1).unwrap();
    let verified = attempt
        .confirm(pin, route, 1, &attempt.fingerprint())
        .unwrap();
    let journal = std::sync::Arc::new(EvidenceJournal {
        state: std::sync::Mutex::new(
            spaceterm_client::pairing::journal::PairingJournalSnapshot::empty(0),
        ),
        fence: Default::default(),
    });
    let (_, credential) = spaceterm_client::pairing::journal::prepare_native_credential(
        &spaceterm_client::pairing::journal::JournalPairingStore::new(journal),
        &native,
        DeviceCredentials::new_scope(
            namespace,
            CredentialPurpose::PairingAuthentication {
                server: ServerId::new(pin.identity),
            },
        )
        .unwrap(),
    )
    .await
    .unwrap();
    // Preserve exact cleanup even when signing/verification fails.
    let result = async {
        let lease = verified.credential_use(&credential).unwrap();
        let signer = native.signing_key(&credential, &lease).await?;
        let signature = signer
            .sign(b"SpaceTerm native client credential evidence")
            .map_err(|_| spaceterm_core::pairing::PairingFailure::CredentialIntegrity)?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, credential.public_key)
            .verify(b"SpaceTerm native client credential evidence", &signature)
            .map_err(|_| spaceterm_core::pairing::PairingFailure::CredentialIntegrity)
    }
    .await;
    native.delete_pairing_key(&credential).await.unwrap();
    result.unwrap();
    assert_eq!(
        native.delete_pairing_key(&credential).await.unwrap(),
        spaceterm_client::pairing::credentials::CredentialDeletion::AlreadyMissing
    );
}

// Native evidence remains opt-in. This injected journal isolates credential
// evidence from the client-core track's separately required physical recovery.
struct EvidenceJournal {
    state: std::sync::Mutex<spaceterm_client::pairing::journal::PairingJournalSnapshot>,
    fence: spaceterm_client::pairing::credentials::PairingAuthorityFence,
}
impl spaceterm_client::journal::PairingClientJournal for EvidenceJournal {
    fn pairing_authority_fence(
        &self,
    ) -> &spaceterm_client::pairing::credentials::PairingAuthorityFence {
        &self.fence
    }
    fn recover_pairings(
        &self,
    ) -> spaceterm_client::pairing::credentials::PairingFuture<
        '_,
        Result<
            spaceterm_client::pairing::journal::PairingJournalSnapshot,
            spaceterm_core::pairing::PairingFailure,
        >,
    > {
        Box::pin(async { Ok(self.state.lock().unwrap().clone()) })
    }
    fn commit_pairing<'a>(
        &'a self,
        tx: &'a spaceterm_client::pairing::journal::PairingJournalTransaction,
    ) -> spaceterm_client::pairing::credentials::PairingFuture<
        'a,
        spaceterm_client::journal::PairingCommitDisposition,
    > {
        Box::pin(async move {
            use spaceterm_client::{journal::*, pairing::journal::PairingJournalChange};
            let mut state = self.state.lock().unwrap();
            assert_eq!(state.revision, tx.expected_revision);
            match &tx.change {
                PairingJournalChange::ReserveCredential(v) => {
                    state.generation_high_water = v.generation.get();
                    state.credential_preparations.push(v.clone());
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
                _ => panic!("credential evidence cannot enroll"),
            }
            state.revision += 1;
            PairingCommitDisposition::Durable(PairingJournalReceipt {
                revision: state.revision,
            })
        })
    }
}
