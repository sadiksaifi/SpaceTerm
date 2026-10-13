//! Shared headless native journal. Desktop and companion delegate to this owner.
//!
//! The singleton projection has one versioned purpose and one full-width revision.
//! Domain validation lives in the portable journal, rather than a second SQL truth.
//! Every change replaces the checked aggregate in one compare-and-swap transaction.
//! SQLite alone owns database and rollback-journal descriptors. Reopening is explicit.

use spaceterm_client::{
    failure::{ClientFailure, ClientFailureClass},
    journal::{
        ClientJournal, ClientJournalRevision, ClientJournalTransaction, JournalCommitDisposition,
        JournalFailure, JournalLimits, JournalReceipt, ReceiptVerifier, RecoveredClientState,
    },
    ports::ClientFuture,
};
use spaceterm_core::operation_ledger_contracts::LedgerCodec;
use spaceterm_host::storage::{CommitDisposition, Database, Migration, StorageFailure};
use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

const SCHEMA: &str = "CREATE TABLE client_journal(\
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),\
    purpose TEXT NOT NULL CHECK(purpose='client-journal-v1'),\
    revision BLOB NOT NULL CHECK(length(revision)=8),\
    payload BLOB NOT NULL CHECK(length(payload)>0 AND length(payload)<=33554432)\
) STRICT";
const MIGRATIONS: &[Migration<'_>] = &[Migration { sql: SCHEMA }];

struct Owner {
    database: Database,
    codec: Arc<dyn LedgerCodec>,
    verifier: Arc<dyn ReceiptVerifier>,
    limits: JournalLimits,
    closed: Arc<spaceterm_client::journal::JournalAdmission>,
}
/// The supplied directory is explicitly provisioned local client-state authority,
/// never route/path data received from a server. Production composition chooses
/// identity-isolated desktop or companion directories beneath client-connections.
/// No signing key, plaintext credential or terminal content enters this adapter.
#[derive(Clone)]
pub struct NativeClientJournal {
    owner: Arc<Mutex<Owner>>,
    fence: Arc<spaceterm_client::pairing::credentials::PairingAuthorityFence>,
    scheduler: Arc<std::sync::atomic::AtomicBool>,
    closed: Arc<spaceterm_client::journal::JournalAdmission>,
}
impl NativeClientJournal {
    /// Explicit first-use initialization. Existing state is never overwritten.
    /// The caller supplies an already-private dedicated client directory.
    pub async fn create(
        directory: PathBuf,
        codec: Arc<dyn LedgerCodec>,
        verifier: Arc<dyn ReceiptVerifier>,
        limits: JournalLimits,
    ) -> Result<Self, ClientFailure> {
        Self::open(directory, codec, verifier, limits, true).await
    }
    /// Missing, locked, malformed and unsupported state never grant creation.
    pub async fn open_existing(
        directory: PathBuf,
        codec: Arc<dyn LedgerCodec>,
        verifier: Arc<dyn ReceiptVerifier>,
        limits: JournalLimits,
    ) -> Result<Self, ClientFailure> {
        Self::open(directory, codec, verifier, limits, false).await
    }
    async fn open(
        directory: PathBuf,
        codec: Arc<dyn LedgerCodec>,
        verifier: Arc<dyn ReceiptVerifier>,
        limits: JournalLimits,
        create: bool,
    ) -> Result<Self, ClientFailure> {
        let limits = limits.validate()?;
        tokio::task::spawn_blocking(move || {
            let mut database = if create { Database::open(&directory, MIGRATIONS) } else { Database::open_existing(&directory, MIGRATIONS) }.map_err(classify)?;
            if create {
                let empty = RecoveredClientState::empty();
                let payload = encode(&empty, limits)?;
                let revision = empty.revision().get().to_be_bytes();
                let result = database.transaction(|tx| {
                    tx.execute("INSERT INTO client_journal(singleton,purpose,revision,payload) VALUES(1,'client-journal-v1',?1,?2)", (revision.as_slice(), payload.as_slice()))?;
                    Ok(())
                });
                match result { CommitDisposition::Durable(()) => {}, CommitDisposition::NotApplied(error) => return Err(classify(error)), CommitDisposition::Indeterminate => return Err(ClientFailure::new(ClientFailureClass::RecoveryRequired)) }
            }
            let closed = Arc::default();
            let owner = Owner { database, codec, verifier, limits, closed: Arc::clone(&closed) };
            read(&owner)?;
            Ok(Self { owner: Arc::new(Mutex::new(owner)), fence: Arc::default(), scheduler: Arc::default(), closed })
        }).await.map_err(|_| ClientFailure::new(ClientFailureClass::RecoveryRequired))?
    }
}
impl ClientJournal for NativeClientJournal {
    fn admission_latch(&self) -> Arc<spaceterm_client::journal::JournalAdmission> {
        Arc::clone(&self.closed)
    }
    fn scheduler_claim(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.scheduler)
    }
    fn authority_fence(&self) -> &spaceterm_client::pairing::credentials::PairingAuthorityFence {
        &self.fence
    }
    fn recover(&self) -> ClientFuture<'_, RecoveredClientState> {
        let owner = Arc::clone(&self.owner);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let owner = owner
                    .lock()
                    .map_err(|_| ClientFailure::new(ClientFailureClass::RecoveryRequired))?;
                read(&owner)
            })
            .await
            .map_err(|_| ClientFailure::new(ClientFailureClass::RecoveryRequired))?
        })
    }
    fn commit<'a>(
        &'a self,
        transaction: &'a ClientJournalTransaction,
    ) -> ClientFuture<'a, JournalCommitDisposition> {
        let owner = Arc::clone(&self.owner);
        let transaction = transaction.clone();
        let fence = Arc::clone(&self.fence);
        Box::pin(async move {
            for change in &transaction.changes {
                if let spaceterm_client::journal::ClientJournalChange::BeginRevocation(record) =
                    change
                {
                    spaceterm_client::pairing::journal::seal_journal_pairing(
                        fence.as_ref(),
                        &record.task.parts().local_pairing,
                    );
                }
            }
            // Dropping the async caller leaves this one blocking owner responsible
            // for the same physical commit. The portable controller drains it.
            tokio::task::spawn_blocking(move || {
                let mut owner = owner.lock().map_err(|_| ClientFailure::new(ClientFailureClass::RecoveryRequired))?;
                if owner.closed.is_closed() { return Ok(JournalCommitDisposition::NotApplied(JournalFailure::RecoveryRequired)); }
                let state = match read(&owner) { Ok(state) => state, Err(error) => return Ok(JournalCommitDisposition::NotApplied(journal_failure(error))) };
                if transaction.expected_revision != state.revision() { return Ok(JournalCommitDisposition::NotApplied(JournalFailure::Conflict)); }
                let candidate = match state.apply(&transaction, owner.codec.as_ref(), owner.verifier.as_ref(), owner.limits) {
                    Ok(candidate) => candidate,
                    Err(_) => return Ok(JournalCommitDisposition::NotApplied(JournalFailure::InvalidState)),
                };
                let payload = match encode(&candidate, owner.limits) { Ok(payload) => payload, Err(error) => return Ok(JournalCommitDisposition::NotApplied(journal_failure(error))) };
                let expected = state.revision().get().to_be_bytes();
                let revision = candidate.revision().get().to_be_bytes();
                let result = owner.database.transaction(|tx| {
                    let rows = tx.execute("UPDATE client_journal SET revision=?1,payload=?2 WHERE singleton=1 AND purpose='client-journal-v1' AND revision=?3", (revision.as_slice(), payload.as_slice(), expected.as_slice()))?;
                    if rows != 1 { return Err(StorageFailure::InvalidState); }
                    Ok(())
                });
                Ok(match result {
                    CommitDisposition::Durable(()) => JournalCommitDisposition::Durable(JournalReceipt::new(candidate.revision())),
                    CommitDisposition::NotApplied(error) => JournalCommitDisposition::NotApplied(journal_failure(classify(error))),
                    CommitDisposition::Indeterminate => {
                        owner.closed.close();
                        // The same fence seals cached native leases and delayed first issuance.
                        for binding in state.bindings() {
                            if let Ok(key) = binding.binding().and_then(|binding| binding.target().local_pairing()) {
                                spaceterm_client::pairing::journal::seal_journal_pairing(fence.as_ref(), &key);
                            }
                        }
                        JournalCommitDisposition::Indeterminate
                    },
                })
            }).await.map_err(|_| ClientFailure::new(ClientFailureClass::RecoveryRequired))?
        })
    }
}

fn read(owner: &Owner) -> Result<RecoveredClientState, ClientFailure> {
    if owner.closed.is_closed() {
        return Err(ClientFailure::new(ClientFailureClass::RecoveryRequired));
    }
    let limit = i64::try_from(owner.limits.retained_bytes)
        .map_err(|_| ClientFailure::new(ClientFailureClass::ResourceLimit))?;
    // SQL reduces purpose to a scalar and checks both blob lengths BEFORE the
    // row decoder allocates, including when a foreign table lacks our constraints.
    let (purpose, revision, payload): (bool, Option<Vec<u8>>, Option<Vec<u8>>) = owner.database.query_row("SELECT purpose='client-journal-v1',CASE WHEN length(revision)=8 THEN revision ELSE NULL END,CASE WHEN length(payload)<=?1 THEN payload ELSE NULL END FROM client_journal WHERE singleton=1", [limit], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(classify)?;
    if !purpose {
        return Err(ClientFailure::new(ClientFailureClass::InvalidStorage));
    }
    let revision = ClientJournalRevision::new(u64::from_be_bytes(
        revision
            .ok_or_else(|| ClientFailure::new(ClientFailureClass::InvalidStorage))?
            .try_into()
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidStorage))?,
    ));
    let payload = payload.ok_or_else(|| ClientFailure::new(ClientFailureClass::ResourceLimit))?;
    let state: RecoveredClientState = serde_json::from_slice(&payload)
        .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidStorage))?;
    if state.revision() != revision {
        return Err(ClientFailure::new(ClientFailureClass::InvalidStorage));
    }
    state.validate(owner.codec.as_ref(), owner.verifier.as_ref(), owner.limits)?;
    Ok(state)
}
struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > self.limit)
        {
            return Err(std::io::Error::from(std::io::ErrorKind::OutOfMemory));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(state: &RecoveredClientState, limits: JournalLimits) -> Result<Vec<u8>, ClientFailure> {
    let mut writer = BoundedWriter {
        bytes: vec![],
        limit: limits.retained_bytes,
    };
    serde_json::to_writer(&mut writer, state)
        .map_err(|_| ClientFailure::new(ClientFailureClass::ResourceLimit))?;
    Ok(writer.bytes)
}
fn classify(error: StorageFailure) -> ClientFailure {
    ClientFailure::new(match error {
        StorageFailure::Unavailable | StorageFailure::Io => ClientFailureClass::StorageUnavailable,
        StorageFailure::UnsafeStorage => ClientFailureClass::UnsafeStorage,
        StorageFailure::UnknownSchema { .. } => ClientFailureClass::UnknownSchema,
        StorageFailure::InvalidState => ClientFailureClass::InvalidStorage,
        StorageFailure::RecoveryRequired => ClientFailureClass::RecoveryRequired,
    })
}
fn journal_failure(error: ClientFailure) -> JournalFailure {
    match error.class() {
        ClientFailureClass::StorageUnavailable | ClientFailureClass::ResourceLimit => {
            JournalFailure::Unavailable
        }
        ClientFailureClass::UnsafeStorage => JournalFailure::UnsafeStorage,
        ClientFailureClass::UnknownSchema => JournalFailure::UnknownSchema,
        ClientFailureClass::RecoveryRequired => JournalFailure::RecoveryRequired,
        _ => JournalFailure::InvalidState,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::{operation_ledger_contracts::*, operations::*, pairing::*};
    use spaceterm_protocol::authentication::ServerPin;
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    // Stateless test-only JSON codec for the frozen LedgerCodec port. Production
    // composition must inject the ledger owner's separately versioned codec.
    struct Codec;
    impl LedgerCodec for Codec {
        fn canonical_request_digest(
            &self,
            _: &StreamBinding,
            _: OperationId,
            command: &Command,
        ) -> Result<RequestDigest, LedgerFailure> {
            RequestDigest::for_command(command)
                .map_err(|_| LedgerFailure::new(LedgerFailureClass::InvalidRequest))
        }
        fn encode_canonical_request(
            &self,
            _: &StreamBinding,
            operation: OperationId,
            command: &Command,
        ) -> Result<Vec<u8>, LedgerFailure> {
            serde_json::to_vec(&(operation, command))
                .map_err(|_| LedgerFailure::new(LedgerFailureClass::InvalidRequest))
        }
        fn decode_canonical_request(
            &self,
            bytes: &[u8],
        ) -> Result<CanonicalOperationRequest, LedgerFailure> {
            let (operation, command): (OperationId, Command) = serde_json::from_slice(bytes)
                .map_err(|_| LedgerFailure::new(LedgerFailureClass::InvalidRequest))?;
            CanonicalOperationRequest::new(operation, command, self)
        }
        fn encode_compact_outcome(&self, outcome: &Outcome) -> Result<Vec<u8>, LedgerFailure> {
            serde_json::to_vec(outcome)
                .map_err(|_| LedgerFailure::new(LedgerFailureClass::InvalidCompactOutcome))
        }
        fn decode_compact_outcome(&self, bytes: &[u8]) -> Result<Outcome, LedgerFailure> {
            serde_json::from_slice(bytes)
                .map_err(|_| LedgerFailure::new(LedgerFailureClass::InvalidCompactOutcome))
        }
    }
    struct Verifier;
    impl ReceiptVerifier for Verifier {
        fn verify(
            &self,
            pin: ServerPin,
            receipt: &SignedPairingReceipt,
        ) -> Result<(), ClientFailure> {
            if receipt.parts().claims.server().bytes() != pin.identity
                || receipt.parts().signature != vec![1; 64]
            {
                return Err(ClientFailure::new(ClientFailureClass::InvalidPeer));
            }
            Ok(())
        }
    }
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt;
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::current_dir().unwrap().join("target");
            std::fs::create_dir_all(&root).unwrap();
            let directory = root.join(format!(
                "client-journal-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
            Self(directory)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[tokio::test]
    async fn native_journal_fresh_reopen_preserves_identity_and_revision() {
        let directory = Directory::new();
        let journal = NativeClientJournal::create(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            journal.recover().await.unwrap(),
            RecoveredClientState::empty()
        );
        drop(journal);
        let reopened = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            reopened.recover().await.unwrap(),
            RecoveredClientState::empty()
        );
    }

    #[tokio::test]
    async fn native_journal_unknown_schema_refuses_reset() {
        let directory = Directory::new();
        let database = Database::open(
            &directory.0,
            &[
                Migration {
                    sql: "CREATE TABLE future_state(value INTEGER) STRICT",
                },
                Migration {
                    sql: "CREATE TABLE future_metadata(value INTEGER) STRICT",
                },
            ],
        )
        .unwrap();
        drop(database);
        let result = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await;
        assert_eq!(
            result.err().unwrap().class(),
            ClientFailureClass::UnknownSchema
        );
        let database = Database::open_existing(
            &directory.0,
            &[Migration { sql: "" }, Migration { sql: "" }],
        )
        .unwrap();
        assert_eq!(database.schema_version().unwrap(), 2);
    }

    #[tokio::test]
    async fn missing_native_journal_is_not_creation_authority() {
        let directory = Directory::new();
        let result = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await;
        assert_eq!(
            result.err().unwrap().class(),
            ClientFailureClass::StorageUnavailable
        );
        assert!(std::fs::read_dir(&directory.0).unwrap().next().is_none());
    }
    fn saved() -> spaceterm_client::journal::SavedBinding {
        use spaceterm_client::api::*;
        use spaceterm_core::{authority::StoreRevision, identity::*};
        let server = ServerId::new([1; 16]);
        let pairing = PairingId::new(server, 1).unwrap();
        let client = ClientId::new(server, 1).unwrap();
        let device = DeviceId::new([2; 16]);
        let target = ConnectionTarget::new(
            ServerPin {
                identity: server.bytes(),
                public_key: [3; 32],
            },
            device,
            pairing,
            client,
            RouteDescription::new(RouteParts::Ipc {
                directory: "/private/local".into(),
                name: "s".into(),
            })
            .unwrap(),
            CredentialReference::new([4; 16]),
            LocalPairingGeneration::new(1).unwrap(),
        )
        .unwrap();
        let streams = PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: client,
                pairing,
            },
            ordinary: OperationStreamId::new(
                OperationStreamKey::new(server, 1).unwrap(),
                pairing,
                client,
            )
            .unwrap(),
            self_revocation: OperationStreamId::new(
                OperationStreamKey::new(server, 2).unwrap(),
                pairing,
                client,
            )
            .unwrap(),
        })
        .unwrap();
        let proof = SignedPairingReceipt::new(SignedPairingReceiptParts {
            schema: 1,
            claims: PairingReceiptBody::ClientBinding {
                target: PairingTarget::new(PairingTargetParts {
                    server,
                    device,
                    pairing,
                })
                .unwrap(),
                keys: PairingKeys::new(PairingKeysParts {
                    device_identity_public_key: [5; 32],
                    pairing_authentication_public_key: [6; 32],
                })
                .unwrap(),
                client: streams.clone(),
                operation: OperationBinding {
                    id: OperationId::new(
                        streams.parts().ordinary,
                        OperationSequence::new(1).unwrap(),
                    ),
                    digest: RequestDigest::from_bytes([7; 32]),
                },
                revision: StoreRevision::new(1),
            },
            canonical_body: vec![8],
            signature: vec![1; 64],
        })
        .unwrap();
        spaceterm_client::journal::SavedBinding::verified(
            PairedServerBinding::new(target),
            streams,
            proof,
            &Verifier,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn native_journal_restart_preserves_stream_high_water_and_pending_tasks() {
        use spaceterm_client::journal::*;
        use spaceterm_core::{
            authority::FieldVersion, identity::WorkspaceId, organization::NameDraft,
        };
        let directory = Directory::new();
        let native = Arc::new(
            NativeClientJournal::create(
                directory.0.clone(),
                Arc::new(Codec),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        let journal = JournalController::recover(
            native.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let saved = saved();
        let binding = saved.binding().unwrap();
        journal
            .transact(vec![ClientJournalChange::SaveBinding(Box::new(
                saved.clone(),
            ))])
            .await
            .unwrap();
        let workspace = WorkspaceId::new(binding.target().server(), 1).unwrap();
        let request = CanonicalOperationRequest::new(
            OperationId::new(
                saved.streams().parts().ordinary,
                OperationSequence::new(1).unwrap(),
            ),
            Command::RenameWorkspace {
                workspace,
                expected_version: FieldVersion::new(1),
                draft: NameDraft::Automatic,
            },
            &Codec,
        )
        .unwrap();
        journal
            .transact(vec![
                ClientJournalChange::Prepare(JournalOperation {
                    binding: request.binding(),
                    request: request.encode(&Codec).unwrap(),
                    workspace: Some(workspace),
                    reserves_grant: false,
                    cleanup: false,
                    state: RetainedOperationState::Pending,
                }),
                ClientJournalChange::BeginDetach(DetachTask {
                    local_pairing: binding.target().local_pairing().unwrap(),
                    workspace,
                    grants: vec![],
                    dependencies: vec![],
                    releases: vec![],
                }),
            ])
            .await
            .unwrap();
        let original = PendingSelfRevocation::new(PendingParts {
            original_server: binding.target().server(),
            original_pairing: binding.target().pairing(),
            device: binding.target().device(),
            client: binding.target().client(),
            operation: OperationId::new(
                saved.streams().parts().self_revocation,
                OperationSequence::new(1).unwrap(),
            ),
            route: RevocationRouteReference::new([9; 16]),
            credential: binding.target().credential(),
            notice_dismissed: false,
        })
        .unwrap();
        let command = Command::Pairing(PairingCommand::SelfRevoke {
            target: PairingTarget::new(PairingTargetParts {
                server: original.server(),
                device: original.device(),
                pairing: original.pairing(),
            })
            .unwrap(),
        });
        let digest = CanonicalOperationRequest::new(original.operation(), command, &Codec)
            .unwrap()
            .binding()
            .digest;
        let task = PendingRevocationTask::new(PendingRevocationTaskParts {
            original,
            local_pairing: binding.target().local_pairing().unwrap(),
            pinned_server_public_key: binding.target().pin().public_key,
            request_digest: digest,
        })
        .unwrap();
        journal
            .transact(vec![ClientJournalChange::BeginRevocation(Box::new(
                JournalRevocation {
                    task: task.clone(),
                    target: SavedTarget::from_target(binding.target()),
                    phase: RevocationPhase::AwaitingConfirmation,
                    notice_dismissed: false,
                    attempts: 0,
                },
            ))])
            .await
            .unwrap();
        drop(journal);
        drop(native);
        let reopened = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let state = reopened.recover().await.unwrap();
        assert_eq!(
            state
                .stream(request.binding().id.stream())
                .unwrap()
                .high_water,
            1
        );
        assert_eq!(
            state.operation(request.binding().id).unwrap().binding,
            request.binding()
        );
        assert_eq!(state.detaches()[0].workspace, workspace);
        assert_eq!(state.revocations()[0].task, task);
        assert!(!state.ordinary_use_allowed(&binding));
        assert_eq!(
            state.stream(original.operation().stream()).unwrap().purpose,
            StreamPurpose::SelfRevocation
        );
    }

    #[tokio::test]
    async fn native_journal_full_width_revision_survives_reopen() {
        let directory = Directory::new();
        let journal = NativeClientJournal::create(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        drop(journal);
        let mut database = Database::open_existing(&directory.0, MIGRATIONS).unwrap();
        let mut payload = serde_json::to_value(RecoveredClientState::empty()).unwrap();
        payload["revision"] = serde_json::to_value(u64::MAX.to_be_bytes()).unwrap();
        let payload = serde_json::to_vec(&payload).unwrap();
        let revision = u64::MAX.to_be_bytes();
        assert_eq!(
            database.transaction(|tx| tx
                .execute(
                    "UPDATE client_journal SET revision=?1,payload=?2",
                    (revision.as_slice(), payload.as_slice())
                )
                .map(|_| ())),
            CommitDisposition::Durable(())
        );
        drop(database);
        let reopened = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(reopened.recover().await.unwrap().revision().get(), u64::MAX);
    }

    #[tokio::test]
    async fn native_journal_stale_revision_cannot_overwrite_committed_state() {
        use spaceterm_client::journal::*;
        let directory = Directory::new();
        let journal = NativeClientJournal::create(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let tx = ClientJournalTransaction {
            expected_revision: ClientJournalRevision::new(0),
            changes: vec![ClientJournalChange::SaveBinding(Box::new(saved()))],
        };
        assert!(matches!(
            journal.commit(&tx).await.unwrap(),
            JournalCommitDisposition::Durable(_)
        ));
        assert_eq!(
            journal.commit(&tx).await.unwrap(),
            JournalCommitDisposition::NotApplied(JournalFailure::Conflict)
        );
        assert_eq!(journal.recover().await.unwrap().revision().get(), 1);
    }
    #[tokio::test]
    async fn native_journal_restart_preserves_grant_reservation_after_result_ack() {
        use spaceterm_client::journal::*;
        use spaceterm_core::{
            authority::{
                ControlGeneration, ControlGrant, GrantedControl, OrganizationRevision, ServerEpoch,
            },
            control::CompareAcquireRequest,
            identity::{PaneId, WorkspaceId},
        };
        let directory = Directory::new();
        let native = Arc::new(
            NativeClientJournal::create(
                directory.0.clone(),
                Arc::new(Codec),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        let journal = JournalController::recover(
            native.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let saved = saved();
        let binding = saved.binding().unwrap();
        journal
            .transact(vec![ClientJournalChange::SaveBinding(Box::new(
                saved.clone(),
            ))])
            .await
            .unwrap();
        let workspace = WorkspaceId::new(binding.target().server(), 1).unwrap();
        let pane = PaneId::new(binding.target().server(), 1).unwrap();
        let control = GrantedControl::new(
            ServerEpoch::new(7),
            ControlGrant::new(pane, binding.target().client(), ControlGeneration::new(8)).unwrap(),
        )
        .unwrap();
        let request = CanonicalOperationRequest::new(
            OperationId::new(
                saved.streams().parts().ordinary,
                OperationSequence::new(1).unwrap(),
            ),
            Command::CompareAcquire(CompareAcquireRequest {
                epoch: control.epoch(),
                pane,
                client: binding.target().client(),
                expected_generation: ControlGeneration::new(0),
            }),
            &Codec,
        )
        .unwrap();
        journal
            .transact(vec![
                ClientJournalChange::Prepare(JournalOperation {
                    binding: request.binding(),
                    request: request.encode(&Codec).unwrap(),
                    workspace: Some(workspace),
                    reserves_grant: true,
                    cleanup: false,
                    state: RetainedOperationState::Pending,
                }),
                ClientJournalChange::Complete {
                    binding: request.binding(),
                    outcome: CompactOutcome::new(&Outcome::ControlGranted(control), &Codec)
                        .unwrap(),
                    publication_revision: OrganizationRevision::new(2),
                },
            ])
            .await
            .unwrap();
        journal
            .transact(vec![
                ClientJournalChange::ConfirmAcknowledgement {
                    stream: request.binding().id.stream(),
                    through: 1,
                },
                ClientJournalChange::ConsumeResult(request.binding().id),
            ])
            .await
            .unwrap();
        drop(journal);
        drop(native);
        let reopened = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let state = reopened.recover().await.unwrap();
        assert!(state.operations().is_empty());
        assert_eq!(state.grants()[0].control, control);
        assert_eq!(
            state
                .stream(request.binding().id.stream())
                .unwrap()
                .high_water,
            1
        );
    }
    #[tokio::test]
    async fn native_journal_recovery_rejects_changed_restricted_digest() {
        use spaceterm_client::journal::*;
        let directory = Directory::new();
        let native = Arc::new(
            NativeClientJournal::create(
                directory.0.clone(),
                Arc::new(Codec),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        let journal = JournalController::recover(
            native.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let saved = saved();
        let binding = saved.binding().unwrap();
        let target = binding.target();
        let original = PendingSelfRevocation::new(PendingParts {
            original_server: target.server(),
            original_pairing: target.pairing(),
            device: target.device(),
            client: target.client(),
            operation: OperationId::new(
                saved.streams().parts().self_revocation,
                OperationSequence::new(1).unwrap(),
            ),
            route: RevocationRouteReference::new([9; 16]),
            credential: target.credential(),
            notice_dismissed: false,
        })
        .unwrap();
        let command = Command::Pairing(PairingCommand::SelfRevoke {
            target: PairingTarget::new(PairingTargetParts {
                server: target.server(),
                device: target.device(),
                pairing: target.pairing(),
            })
            .unwrap(),
        });
        let request_digest = CanonicalOperationRequest::new(original.operation(), command, &Codec)
            .unwrap()
            .binding()
            .digest;
        let task = PendingRevocationTask::new(PendingRevocationTaskParts {
            original,
            local_pairing: target.local_pairing().unwrap(),
            pinned_server_public_key: target.pin().public_key,
            request_digest,
        })
        .unwrap();
        journal
            .transact(vec![
                ClientJournalChange::SaveBinding(Box::new(saved)),
                ClientJournalChange::BeginRevocation(Box::new(JournalRevocation {
                    task,
                    target: SavedTarget::from_target(target),
                    phase: RevocationPhase::AwaitingConfirmation,
                    notice_dismissed: false,
                    attempts: 0,
                })),
            ])
            .await
            .unwrap();
        let mut payload = serde_json::to_value(journal.snapshot().unwrap()).unwrap();
        payload["revocations"][0]["task"]["request_digest"] =
            serde_json::to_value([99u8; 32]).unwrap();
        let payload = serde_json::to_vec(&payload).unwrap();
        drop(journal);
        drop(native);
        let mut database = Database::open_existing(&directory.0, MIGRATIONS).unwrap();
        assert_eq!(
            database.transaction(|tx| tx
                .execute("UPDATE client_journal SET payload=?1", [payload.as_slice()])
                .map(|_| ())),
            CommitDisposition::Durable(())
        );
        drop(database);
        let result = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await;
        assert_eq!(
            result.err().unwrap().class(),
            ClientFailureClass::InvalidStorage
        );
    }
    #[tokio::test]
    async fn native_journal_restart_preserves_prepared_credentials_and_enrollment_ack_work() {
        use spaceterm_client::journal::*;
        use spaceterm_core::{authority::StoreRevision, identity::*};
        let directory = Directory::new();
        let native = Arc::new(
            NativeClientJournal::create(
                directory.0.clone(),
                Arc::new(Codec),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        let journal = JournalController::recover(
            native.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let existing = saved();
        let binding = existing.binding().unwrap();
        let target = binding.target();
        let keys = PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [5; 32],
            pairing_authentication_public_key: [6; 32],
        })
        .unwrap();
        let enrollment = OperationBinding {
            id: OperationId::new(
                OperationStreamId::new(
                    OperationStreamKey::new(target.server(), 3).unwrap(),
                    target.pairing(),
                    target.client(),
                )
                .unwrap(),
                OperationSequence::new(1).unwrap(),
            ),
            digest: RequestDigest::from_bytes([7; 32]),
        };
        let prepared = JournalEnrollment {
            target: SavedTarget::from_target(target),
            keys: keys.clone(),
            route_reference: RevocationRouteReference::new([8; 16]),
            enrollment,
            reservation: Some(JournalEnrollmentReservation {
                ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
                    server: target.server(),
                    nonce: [9; 16],
                })
                .unwrap(),
                claim_digest: RequestDigest::from_bytes([10; 32]),
            }),
        };
        let proof = SignedPairingReceipt::new(SignedPairingReceiptParts {
            schema: 1,
            claims: PairingReceiptBody::Enrollment {
                target: PairingTarget::new(PairingTargetParts {
                    server: target.server(),
                    pairing: target.pairing(),
                    device: target.device(),
                })
                .unwrap(),
                keys,
                client: existing.streams().clone(),
                enrollment,
                revision: StoreRevision::new(5),
            },
            canonical_body: vec![11],
            signature: vec![1; 64],
        })
        .unwrap();
        let saved = SavedBinding::verified(
            binding.clone(),
            existing.streams().clone(),
            proof.clone(),
            &Verifier,
        )
        .unwrap();
        let device_credential = CredentialReference::new([12; 16]);
        journal
            .transact(vec![
                ClientJournalChange::ReserveCredential(JournalCredentialSlot {
                    namespace: "dev.spaceterm.desktop".into(),
                    server: Some(target.server()),
                    reference: target.credential(),
                    public_key: None,
                    route_reference: None,
                    route: None,
                    generation: LocalPairingGeneration::new(1).unwrap(),
                }),
                ClientJournalChange::RecordCredentialPublicKey {
                    reference: target.credential(),
                    public_key: [6; 32],
                },
                ClientJournalChange::ReserveCredential(JournalCredentialSlot {
                    namespace: "dev.spaceterm.desktop".into(),
                    server: None,
                    reference: device_credential,
                    public_key: None,
                    route_reference: None,
                    route: None,
                    generation: LocalPairingGeneration::new(2).unwrap(),
                }),
                ClientJournalChange::RecordCredentialPublicKey {
                    reference: device_credential,
                    public_key: [5; 32],
                },
                ClientJournalChange::PrepareEnrollment(Box::new(prepared.clone())),
                ClientJournalChange::SaveEnrollment {
                    binding: Box::new(saved),
                    acknowledgement: Box::new(JournalEnrollmentAcknowledgement {
                        prepared: prepared.clone(),
                        device_credential,
                        receipt: proof,
                    }),
                },
            ])
            .await
            .unwrap();
        drop(journal);
        drop(native);
        let reopened = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let state = reopened.recover().await.unwrap();
        assert!(state.ordinary_use_allowed(&binding));
        assert_eq!(state.pairing().generation_high_water(), 2);
        assert_eq!(state.pairing().slots().len(), 2);
        assert_eq!(
            state.pairing().credential_preparations(),
            &[device_credential]
        );
        assert_eq!(state.pairing().acknowledgements()[0].prepared, prepared);
        assert_eq!(
            state
                .pairing()
                .slot(target.credential())
                .unwrap()
                .route_reference,
            Some(prepared.route_reference)
        );
    }
}
