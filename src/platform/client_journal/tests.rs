use super::*;
use crate::platform::{
    app_directories::{AppDirectories, AppDirectoryEnvironment},
    unix_secure_filesystem::UnixSecureFilesystem,
};
use spaceterm_client::journal::ClientJournal;
use spaceterm_core::{operation_ledger_contracts::*, operations::*, pairing::*};
use spaceterm_protocol::authentication::ServerPin;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Codec;
impl LedgerCodec for Codec {
    fn canonical_request_digest(
        &self,
        _: &StreamBinding,
        _: OperationId,
        _: &Command,
    ) -> Result<RequestDigest, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::InvalidRequest))
    }
    fn encode_canonical_request(
        &self,
        _: &StreamBinding,
        _: OperationId,
        _: &Command,
    ) -> Result<Vec<u8>, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::InvalidRequest))
    }
    fn decode_canonical_request(
        &self,
        _: &[u8],
    ) -> Result<CanonicalOperationRequest, LedgerFailure> {
        Err(LedgerFailure::new(LedgerFailureClass::InvalidRequest))
    }
    fn encode_compact_outcome(&self, _: &Outcome) -> Result<Vec<u8>, LedgerFailure> {
        Err(LedgerFailure::new(
            LedgerFailureClass::InvalidCompactOutcome,
        ))
    }
    fn decode_compact_outcome(&self, _: &[u8]) -> Result<Outcome, LedgerFailure> {
        Err(LedgerFailure::new(
            LedgerFailureClass::InvalidCompactOutcome,
        ))
    }
}
struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, _: ServerPin, _: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        Err(ClientFailure::new(ClientFailureClass::InvalidPeer))
    }
}
struct Fixture {
    root: PathBuf,
    paths: AppPaths,
}
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "desktop-client-journal-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let environment = AppDirectoryEnvironment {
            home: Some(root.clone().into_os_string()),
            xdg_state_home: Some(root.join("state").into_os_string()),
            ..Default::default()
        };
        let directories =
            AppDirectories::resolve_xdg("journal-test-development", &environment, None).unwrap();
        let paths =
            AppPaths::from_directories(directories, 104, Arc::new(UnixSecureFilesystem)).unwrap();
        Self { root, paths }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test]
async fn desktop_adapter_uses_identity_scoped_state_not_settings() {
    let fixture = Fixture::new();
    let journal = open(
        &fixture.paths,
        JournalOpenMode::Initialize,
        Arc::new(Codec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    assert_eq!(journal.recover().await.unwrap().revision().get(), 0);
    assert!(
        fixture
            .paths
            .directories()
            .root(AppDirectoryRoot::State)
            .join("client-connections/desktop/organization.sqlite3")
            .exists()
    );
    assert!(
        !fixture
            .paths
            .directories()
            .root(AppDirectoryRoot::Config)
            .exists()
    );
    drop(journal);
    let reopened = open(
        &fixture.paths,
        JournalOpenMode::Existing,
        Arc::new(Codec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    assert!(reopened.recover().await.unwrap().bindings().is_empty());
}

#[tokio::test]
async fn desktop_journal_missing_state_does_not_initialize_a_replacement() {
    let fixture = Fixture::new();
    let error = open(
        &fixture.paths,
        JournalOpenMode::Existing,
        Arc::new(Codec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.class(), ClientFailureClass::StorageUnavailable);
    assert!(
        !fixture
            .paths
            .directories()
            .root(AppDirectoryRoot::State)
            .exists()
    );
}

#[tokio::test]
async fn desktop_journal_initialization_does_not_overwrite_existing_state() {
    let fixture = Fixture::new();
    let journal = open(
        &fixture.paths,
        JournalOpenMode::Initialize,
        Arc::new(Codec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let error = open(
        &fixture.paths,
        JournalOpenMode::Initialize,
        Arc::new(Codec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.class(), ClientFailureClass::UnsafeStorage);
    assert_eq!(journal.recover().await.unwrap().revision().get(), 0);
}

#[tokio::test]
async fn desktop_pairing_facade_shares_the_operation_journal_revision() {
    use spaceterm_client::pairing::{
        credentials::*,
        journal::{LocalPairingStore, reserve_native_credential},
    };
    let fixture = Fixture::new();
    let journal = open(
        &fixture.paths,
        JournalOpenMode::Initialize,
        Arc::new(Codec),
        Arc::new(Verifier),
        JournalLimits::default(),
    )
    .await
    .unwrap();
    let store = crate::platform::client_pairing_store::from_client_journal(journal.clone());
    let scope = CredentialScope::new(CredentialScopeParts {
        namespace: CredentialNamespace::new("dev.spaceterm.test".into()).unwrap(),
        purpose: CredentialPurpose::DeviceIdentity,
        reference: CredentialReference::new([44; 16]),
    })
    .unwrap();
    let reservation = reserve_native_credential(&store, scope.clone())
        .await
        .unwrap();
    let state = journal.snapshot().unwrap();
    assert_eq!(state.pairing().slots()[0].reference, scope.reference());
    assert_eq!(
        state.pairing().slots()[0].generation,
        reservation.generation()
    );
    assert_eq!(
        store.recover().await.unwrap().revision,
        state.revision().get()
    );
}
