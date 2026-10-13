use super::*;
use crate::credentials::adapter::NativeKeyStore;
use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::{JournalLimits, ReceiptVerifier},
    pairing::{
        bootstrap::{BuildChannel, ForegroundBootstrap},
        credentials::{CredentialScope, NativeCredentialAccess},
        enrollment::PairingTransport,
    },
};
use spaceterm_core::{operation_ledger::canonical::CanonicalCodec, pairing::SignedPairingReceipt};
use spaceterm_protocol::{authentication::ServerPin, pairing_adapter::ReceiptCodec};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use zeroize::Zeroizing;

struct Verify;
impl ReceiptVerifier for Verify {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))
    }
}
struct Native {
    journal: Arc<NativeClientJournal>,
    entries: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
    writes: AtomicUsize,
    locked: AtomicBool,
}
impl NativeKeyStore for Native {
    fn read<'a>(
        &'a self,
        scope: &'a CredentialScope,
        _: NativeCredentialAccess,
    ) -> PairingFuture<'a, Result<Option<Zeroizing<Vec<u8>>>, PairingFailure>> {
        Box::pin(async move {
            if self.locked.load(Ordering::Acquire) {
                return Err(PairingFailure::CredentialUnavailable);
            }
            Ok(self
                .entries
                .lock()
                .unwrap()
                .get(&DeviceCredentials::locator(scope))
                .cloned())
        })
    }
    fn insert<'a>(
        &'a self,
        scope: &'a CredentialScope,
        secret: &'a [u8],
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            if self.locked.load(Ordering::Acquire) {
                return Err(PairingFailure::CredentialUnavailable);
            }
            let recovered = JournalPairingStore::new(Arc::clone(&self.journal))
                .recover()
                .await?;
            assert!(
                recovered
                    .credential_preparations
                    .iter()
                    .any(|p| p.scope == *scope)
            );
            let mut entries = self.entries.lock().unwrap();
            let name = DeviceCredentials::locator(scope);
            if entries.contains_key(&name) {
                return Err(PairingFailure::CredentialIntegrity);
            }
            entries.insert(name, Zeroizing::new(secret.to_vec()));
            self.writes.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    }
    fn remove<'a>(
        &'a self,
        _: &'a CredentialScope,
        _: &'a [u8],
    ) -> PairingFuture<'a, Result<bool, PairingFailure>> {
        Box::pin(async { Err(PairingFailure::AuthorizationDenied) })
    }
}
#[derive(Clone)]
struct Connector;
impl EnrollmentConnector for Connector {
    fn open<'a>(
        &'a self,
        _: ServerPin,
        _: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async { Err(PairingFailure::Offline) })
    }
}
struct Owner(Arc<AtomicUsize>);
impl BootstrapOwnerExchange for Owner {
    fn lookup<'a>(
        &'a self,
        _: &'a BootstrapServer,
        _: [u8; 16],
        _: &'a EnrollmentProposal,
    ) -> PairingFuture<'a, Result<BootstrapOwnerDelivery, PairingFailure>> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Err(PairingFailure::Offline) })
    }
    fn connect<'a>(
        &'a self,
        _: &'a BootstrapServer,
        _: &'a SavedPairing,
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Err(PairingFailure::Offline) })
    }
}
fn route() -> BootstrapRoute {
    BootstrapRoute::capture([1; 32], [2; 32], [3; 32], 501, BuildChannel::Development).unwrap()
}
fn server() -> BootstrapServer {
    BootstrapServer {
        route: route(),
        pin: ServerPin {
            identity: [4; 16],
            public_key: [5; 32],
        },
        registration_generation: 1,
        approved_artifact_identity: [6; 32],
        protocol_majors: [1, 2],
        running: true,
    }
}
fn provider(
    journal: Arc<NativeClientJournal>,
    native: Arc<Native>,
    owner: Arc<AtomicUsize>,
) -> NativeBootstrapEnrollment<Connector, Owner> {
    NativeBootstrapEnrollment::new(
        journal,
        DeviceCredentials::with_store(native),
        Connector,
        Owner(owner),
        CredentialNamespace::new("dev.spaceterm.bootstrap-test".into()).unwrap(),
        route(),
        RevocationRouteReference::new([7; 16]),
    )
    .unwrap()
}
fn private_root() -> tempfile::TempDir {
    let parent = std::env::current_dir().unwrap().join("target");
    std::fs::create_dir_all(&parent).unwrap();
    tempfile::Builder::new()
        .prefix("bootstrap-enrollment-")
        .tempdir_in(parent)
        .unwrap()
}
async fn journal(path: &std::path::Path, create: bool) -> Arc<NativeClientJournal> {
    if create {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    }
    let result = if create {
        NativeClientJournal::create(
            path.to_owned(),
            Arc::new(CanonicalCodec),
            Arc::new(Verify),
            JournalLimits::default(),
        )
        .await
    } else {
        NativeClientJournal::open_existing(
            path.to_owned(),
            Arc::new(CanonicalCodec),
            Arc::new(Verify),
            JournalLimits::default(),
        )
        .await
    };
    Arc::new(result.unwrap())
}

#[tokio::test]
async fn bootstrap_native_owner_proposal_survives_journal_and_provider_restart() {
    let root = private_root();
    let path = root.path().join("client");
    let first_journal = journal(&path, true).await;
    let native = Arc::new(Native {
        journal: Arc::clone(&first_journal),
        entries: Mutex::new(HashMap::new()),
        writes: AtomicUsize::new(0),
        locked: AtomicBool::new(false),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let first = provider(
        Arc::clone(&first_journal),
        Arc::clone(&native),
        Arc::clone(&calls),
    );
    let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
    let attempt = intent.cancellation();
    first.preflight(route(), &attempt).await.unwrap();
    let original = first.prepare_owner(&server(), &attempt).await.unwrap();
    drop(first);
    let reopened = provider(
        journal(&path, false).await,
        Arc::clone(&native),
        Arc::clone(&calls),
    );
    reopened.preflight(route(), &attempt).await.unwrap();
    assert_eq!(
        reopened
            .prepare_owner(&server(), &attempt)
            .await
            .unwrap()
            .parts()
            .claim_nonce,
        original.parts().claim_nonce
    );
    assert_eq!(
        reopened
            .prepare_owner(&server(), &attempt)
            .await
            .unwrap()
            .parts()
            .keys,
        original.parts().keys
    );
    assert_eq!(native.writes.load(Ordering::Acquire), 2);
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn bootstrap_locked_native_custody_fails_before_owner_or_remote_install() {
    let root = private_root();
    let journal = journal(&root.path().join("client"), true).await;
    let native = Arc::new(Native {
        journal: Arc::clone(&journal),
        entries: Mutex::new(HashMap::new()),
        writes: AtomicUsize::new(0),
        locked: AtomicBool::new(true),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = provider(journal, Arc::clone(&native), Arc::clone(&calls));
    let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
    let failure = provider
        .preflight(route(), &intent.cancellation())
        .await
        .unwrap_err();
    assert_eq!(
        failure.class,
        spaceterm_client::pairing::bootstrap::BootstrapFailureClass::Pairing(
            PairingFailure::CredentialUnavailable
        )
    );
    assert_eq!(native.writes.load(Ordering::Acquire), 0);
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn bootstrap_cancelled_attempt_and_unverified_binding_cannot_connect() {
    let root = private_root();
    let journal = journal(&root.path().join("client"), true).await;
    let native = Arc::new(Native {
        journal: Arc::clone(&journal),
        entries: Mutex::new(HashMap::new()),
        writes: AtomicUsize::new(0),
        locked: AtomicBool::new(false),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = provider(journal, Arc::clone(&native), Arc::clone(&calls));
    let intent = ForegroundBootstrap::confirm(route(), 1).unwrap();
    let attempt = intent.cancellation();
    assert!(provider.connect(&server(), &attempt).await.is_err());
    attempt.cancel();
    assert!(provider.preflight(route(), &attempt).await.is_err());
    assert_eq!(native.writes.load(Ordering::Acquire), 0);
    assert_eq!(calls.load(Ordering::Acquire), 0);
}
