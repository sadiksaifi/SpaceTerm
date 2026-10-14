use ring::signature::{Ed25519KeyPair, KeyPair as _};
use spaceterm_client::{
    api::{RouteDescription, RouteParts},
    failure::ClientFailure,
    journal::{JournalController, JournalLimits, ReceiptVerifier},
    pairing::{
        credentials::*,
        enrollment::{EnrollmentConnector, PairingTransport, fingerprint},
        revocation::RestrictedConnector,
    },
    pending::LocalUseInvalidator,
    ports::ClientFuture,
};
use spaceterm_client_native::{
    journal::NativeClientJournal, pairing_runtime::NativePairingRuntime,
};
use spaceterm_core::{
    authority::GrantedControl,
    identity::WorkspaceId,
    operation_ledger::canonical::CanonicalCodec,
    pairing::{
        LocalPairingKey, PairingFailure, PendingRevocationTask, RevocationRouteReference,
        SignedPairingReceipt,
    },
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{HandshakeConfig, ServerPin, SigningKey},
    pairing_adapter::{ReceiptCodec, VerifiedDeviceClaim},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

struct Key(Ed25519KeyPair);
impl Key {
    fn for_scope(scope: &CredentialScope) -> Self {
        Self(
            Ed25519KeyPair::from_seed_unchecked(
                &[if scope.parts().purpose == CredentialPurpose::DeviceIdentity {
                    7
                } else {
                    8
                }; 32],
            )
            .unwrap(),
        )
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
#[derive(Default)]
struct Keys {
    prepared: Mutex<Vec<PreparedCredential>>,
    locked: AtomicBool,
}
impl NativeDeviceCredentials for Keys {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            if self.locked.load(Ordering::SeqCst) {
                return Err(PairingFailure::CredentialUnavailable);
            }
            let scope = reservation.scope().clone();
            let prepared = PreparedCredential {
                public_key: Key::for_scope(&scope).public_key(),
                scope,
            };
            self.prepared.lock().unwrap().push(prepared.clone());
            Ok(prepared)
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            if self.locked.load(Ordering::SeqCst) {
                return Err(PairingFailure::CredentialUnavailable);
            }
            Ok(self
                .prepared
                .lock()
                .unwrap()
                .iter()
                .find(|key| &key.scope == scope)
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
            if self.locked.load(Ordering::SeqCst) {
                return Err(PairingFailure::CredentialUnavailable);
            }
            Ok(Arc::new(Key::for_scope(&credential.scope)) as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        panic!("public claim preparation cannot delete a native key")
    }
}
struct NoConnections;
impl EnrollmentConnector for NoConnections {
    fn open<'a>(
        &'a self,
        _: ServerPin,
        _: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        panic!("public claim preparation cannot open enrollment")
    }
}
impl RestrictedConnector for NoConnections {
    fn open<'a>(
        &'a self,
        _: &'a PendingRevocationTask,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        panic!("public claim preparation cannot open revocation")
    }
}
struct NoWork;
impl LocalUseInvalidator for NoWork {
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        panic!("public claim preparation cannot detach a workspace")
    }
    fn invalidate_attachment(
        &self,
        _: &LocalPairingKey,
        _: spaceterm_client::view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        panic!("public claim preparation cannot detach an attachment")
    }
    fn invalidate_pairing(&self, _: &LocalPairingKey) {
        panic!("public claim preparation cannot revoke a pairing")
    }
    fn remove_pairing<'a>(&'a self, _: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
        panic!("public claim preparation cannot remove a pairing")
    }
}
struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(Into::into)
    }
}
fn pin() -> ServerPin {
    ServerPin {
        identity: [1; 16],
        public_key: Ed25519KeyPair::from_seed_unchecked(&[1; 32])
            .unwrap()
            .public_key()
            .as_ref()
            .try_into()
            .unwrap(),
    }
}
fn namespace() -> CredentialNamespace {
    CredentialNamespace::new("dev.spaceterm.public-claim-test".into()).unwrap()
}
fn route(port: u16) -> RouteDescription {
    RouteDescription::new(RouteParts::Wss {
        address: ([127, 0, 0, 1], port).into(),
        server_name: "claim.invalid".into(),
    })
    .unwrap()
}
async fn fixture() -> (tempfile::TempDir, NativePairingRuntime, Arc<Keys>) {
    let parent = if cfg!(target_os = "linux") {
        spaceterm_host::account_service::TrustedAccountFacts::current()
            .unwrap()
            .home()
            .to_owned()
    } else {
        std::path::PathBuf::from("/private/tmp")
    };
    let root = tempfile::tempdir_in(parent).unwrap();
    use std::os::unix::fs::DirBuilderExt as _;
    let directory = root.path().join("client");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let limits = JournalLimits::default();
    let codec = Arc::new(CanonicalCodec);
    let verifier = Arc::new(Verifier);
    let storage = NativeClientJournal::create(directory, codec.clone(), verifier.clone(), limits)
        .await
        .unwrap();
    let journal = Arc::new(
        JournalController::recover(Arc::new(storage), codec, verifier, limits)
            .await
            .unwrap(),
    );
    let keys = Arc::new(Keys::default());
    let runtime = NativePairingRuntime::with_adapters(
        journal,
        keys.clone(),
        Arc::new(NoConnections),
        Arc::new(NoConnections),
        Arc::new(NoWork),
        HandshakeConfig::default(),
    );
    (root, runtime, keys)
}

#[tokio::test]
async fn first_use_public_claim_prepares_original_native_keys_without_enrollment() {
    let (_root, runtime, keys) = fixture().await;
    let before = runtime.journal().snapshot().unwrap().revision();
    assert!(matches!(
        runtime
            .prepare_public_claim(namespace(), pin(), route(443), "wrong")
            .await,
        Err(PairingFailure::FingerprintMismatch)
    ));
    assert_eq!(runtime.journal().snapshot().unwrap().revision(), before);
    assert!(keys.prepared.lock().unwrap().is_empty());
    let first = runtime
        .prepare_public_claim(
            namespace(),
            pin(),
            route(443),
            &fingerprint(pin().public_key),
        )
        .await
        .unwrap();
    VerifiedDeviceClaim::verify(
        first.proposal.clone(),
        &first.device_proof,
        &first.authentication_proof,
    )
    .unwrap();
    assert_eq!(first.server_identity, pin().identity);
    assert_eq!(first.server_public_key, pin().public_key);
    let slots = runtime
        .journal()
        .snapshot()
        .unwrap()
        .pairing()
        .slots()
        .to_vec();
    assert_eq!(slots.len(), 2);
    assert!(runtime.journal().snapshot().unwrap().bindings().is_empty());
    let second = runtime
        .prepare_public_claim(
            namespace(),
            pin(),
            route(443),
            &fingerprint(pin().public_key),
        )
        .await
        .unwrap();
    assert_eq!(first.proposal.parts().keys, second.proposal.parts().keys);
    assert_eq!(keys.prepared.lock().unwrap().len(), 2);
    let recovered = runtime.journal().snapshot().unwrap();
    assert_eq!(
        recovered
            .pairing()
            .slots()
            .iter()
            .map(|slot| (slot.reference, slot.route_reference))
            .collect::<Vec<_>>(),
        slots
            .iter()
            .map(|slot| (slot.reference, slot.route_reference))
            .collect::<Vec<_>>()
    );
    let public = serde_json::to_value(first).unwrap();
    assert_eq!(public.as_object().unwrap().len(), 6);
    assert!(public.get("authorization_delivery").is_none());
    assert!(public.get("credential_reference").is_none());
}

#[tokio::test]
async fn public_claim_locked_storage_and_changed_route_never_replace_prepared_keys() {
    let (_root, runtime, keys) = fixture().await;
    runtime
        .prepare_public_claim(
            namespace(),
            pin(),
            route(443),
            &fingerprint(pin().public_key),
        )
        .await
        .unwrap();
    keys.locked.store(true, Ordering::SeqCst);
    assert!(matches!(
        runtime
            .prepare_public_claim(
                namespace(),
                pin(),
                route(443),
                &fingerprint(pin().public_key)
            )
            .await,
        Err(PairingFailure::CredentialUnavailable)
    ));
    assert_eq!(keys.prepared.lock().unwrap().len(), 2);
    keys.locked.store(false, Ordering::SeqCst);
    assert!(matches!(
        runtime
            .prepare_public_claim(
                namespace(),
                pin(),
                route(444),
                &fingerprint(pin().public_key)
            )
            .await,
        Err(PairingFailure::RouteChanged)
    ));
    assert_eq!(keys.prepared.lock().unwrap().len(), 2);
}
