//! Shared headless native journal. Desktop and companion delegate to this owner.
//!
//! The singleton projection has one versioned purpose and one full-width revision.
//! Domain validation lives in the portable journal, rather than a second SQL truth.
//! Metadata and changed comparison records commit in one revision-CAS transaction.
//! A revision-checked cache avoids decoding unchanged history during live use.
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
const HISTORY_SCHEMA: &str = "UPDATE client_journal SET payload=CASE WHEN \
    json_type(CAST(payload AS TEXT),'$.streams')='array' AND \
    json_type(CAST(payload AS TEXT),'$.tombstones')='array' AND \
    json_type(CAST(payload AS TEXT),'$.generations')='array' AND \
    (json_type(CAST(payload AS TEXT),'$.pairing') IS NULL OR \
     json_type(CAST(payload AS TEXT),'$.pairing.slots')='array') \
    THEN payload ELSE NULL END;\
CREATE TABLE client_journal_history(\
    kind INTEGER NOT NULL CHECK(kind BETWEEN 0 AND 3),\
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 4095),\
    payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) BETWEEN 1 AND 33554432 AND json_valid(payload)),\
    PRIMARY KEY(kind,position)\
) STRICT, WITHOUT ROWID;\
INSERT INTO client_journal_history SELECT 0,CAST(j.key AS INTEGER),j.value \
    FROM client_journal,json_each(CAST(payload AS TEXT),'$.streams') j;\
INSERT INTO client_journal_history SELECT 1,CAST(j.key AS INTEGER),j.value \
    FROM client_journal,json_each(CAST(payload AS TEXT),'$.tombstones') j;\
INSERT INTO client_journal_history SELECT 2,CAST(j.key AS INTEGER),j.value \
    FROM client_journal,json_each(CAST(payload AS TEXT),'$.pairing.slots') j;\
INSERT INTO client_journal_history SELECT 3,CAST(j.key AS INTEGER),j.value \
    FROM client_journal,json_each(CAST(payload AS TEXT),'$.generations') j;\
UPDATE client_journal SET payload=CAST(json_insert(CAST(payload AS TEXT),'$.pairing',\
    json('{\"generation_high_water\":[0,0,0,0,0,0,0,0],\"slots\":[],\"credential_preparations\":[],\"prepared\":[],\"acknowledgements\":[]}')) AS BLOB);\
UPDATE client_journal SET payload=CAST(json_set(CAST(payload AS TEXT),\
    '$.streams',json('[]'),'$.tombstones',json('[]'),'$.pairing.slots',json('[]'),'$.generations',json('[]')) AS BLOB)";
const MIGRATIONS: &[Migration<'_>] = &[
    Migration { sql: SCHEMA },
    Migration {
        sql: HISTORY_SCHEMA,
    },
];

struct Cache {
    state: RecoveredClientState,
    lengths: [Vec<usize>; 4],
}
struct RecordUpdate {
    kind: usize,
    position: usize,
    payload: Option<Vec<u8>>,
}

struct Owner {
    database: Database,
    cache: Option<Cache>,
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
            let mut owner = Owner { database, cache: None, codec, verifier, limits, closed: Arc::clone(&closed) };
            read(&mut owner)?;
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
                let mut owner = owner
                    .lock()
                    .map_err(|_| ClientFailure::new(ClientFailureClass::RecoveryRequired))?;
                read(&mut owner).cloned()
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
                let state = match read(&mut owner) { Ok(state) => state.clone(), Err(error) => return Ok(JournalCommitDisposition::NotApplied(journal_failure(error))) };
                if transaction.expected_revision != state.revision() { return Ok(JournalCommitDisposition::NotApplied(JournalFailure::Conflict)); }
                let candidate = match state.apply(&transaction, owner.codec.as_ref(), owner.verifier.as_ref(), owner.limits) {
                    Ok(candidate) => candidate,
                    Err(_) => return Ok(JournalCommitDisposition::NotApplied(JournalFailure::InvalidState)),
                };
                let payload = match encode(&candidate.persistence_metadata(), owner.limits) { Ok(payload) => payload, Err(error) => return Ok(JournalCommitDisposition::NotApplied(journal_failure(error))) };
                let Some(cache) = owner.cache.as_ref() else { return Err(ClientFailure::new(ClientFailureClass::RecoveryRequired)); };
                let (updates, lengths) = match changed_records(&state, &candidate, &cache.lengths, owner.limits) {
                    Ok(records) => records,
                    Err(error) => return Ok(JournalCommitDisposition::NotApplied(journal_failure(error))),
                };
                let retained_bytes = lengths.iter().try_fold(payload.len(), |total, records| {
                    records.iter().try_fold(total, |sum, length| sum.checked_add(*length))
                        .and_then(|sum| sum.checked_add(records.len().saturating_sub(1)))
                });
                if retained_bytes.is_none_or(|size| size > owner.limits.retained_bytes) {
                    return Ok(JournalCommitDisposition::NotApplied(JournalFailure::InvalidState));
                }
                let expected = state.revision().get().to_be_bytes();
                let revision = candidate.revision().get().to_be_bytes();
                let result = owner.database.transaction(|tx| {
                    let rows = tx.execute("UPDATE client_journal SET revision=?1,payload=?2 WHERE singleton=1 AND purpose='client-journal-v1' AND revision=?3", (revision.as_slice(), payload.as_slice(), expected.as_slice()))?;
                    if rows != 1 { return Ok(false); }
                    for update in &updates {
                        let kind = update.kind as i64;
                        let position = update.position as i64;
                        match &update.payload {
                            Some(payload) => {
                                let text = std::str::from_utf8(payload).map_err(|_| StorageFailure::InvalidState)?;
                                tx.execute("INSERT INTO client_journal_history(kind,position,payload) VALUES(?1,?2,?3) ON CONFLICT(kind,position) DO UPDATE SET payload=excluded.payload", (kind, position, text))?;
                            },
                            None => { tx.execute("DELETE FROM client_journal_history WHERE kind=?1 AND position=?2", (kind, position))?; },
                        }
                    }
                    Ok(true)
                });
                Ok(match result {
                    CommitDisposition::Durable(true) => {
                        let receipt = JournalReceipt::new(candidate.revision());
                        owner.cache = Some(Cache { state: candidate, lengths });
                        JournalCommitDisposition::Durable(receipt)
                    },
                    CommitDisposition::Durable(false) => JournalCommitDisposition::NotApplied(JournalFailure::Conflict),
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

fn read(owner: &mut Owner) -> Result<&RecoveredClientState, ClientFailure> {
    if owner.closed.is_closed() {
        return Err(ClientFailure::new(ClientFailureClass::RecoveryRequired));
    }
    // The native query still verifies held filesystem authority and observes other
    // SQLite owners. A matching revision reuses only our last durable projection.
    let (purpose, revision): (bool, Option<Vec<u8>>) = owner.database.query_row(
        "SELECT purpose='client-journal-v1',CASE WHEN length(revision)=8 THEN revision ELSE NULL END FROM client_journal WHERE singleton=1", [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(classify)?;
    if !purpose {
        return Err(ClientFailure::new(ClientFailureClass::InvalidStorage));
    }
    let revision = ClientJournalRevision::new(u64::from_be_bytes(
        revision
            .ok_or_else(invalid_storage)?
            .try_into()
            .map_err(|_| invalid_storage())?,
    ));
    if owner
        .cache
        .as_ref()
        .is_none_or(|cache| cache.state.revision() != revision)
    {
        owner.cache = Some(read_snapshot(owner)?);
    }
    owner
        .cache
        .as_ref()
        .map(|cache| &cache.state)
        .ok_or_else(invalid_storage)
}
fn invalid_storage() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::InvalidStorage)
}
fn read_snapshot(owner: &mut Owner) -> Result<Cache, ClientFailure> {
    let limit = i64::try_from(owner.limits.retained_bytes).map_err(|_| invalid_storage())?;
    let (revision, payload, lengths) = owner.database.read_transaction(|tx| {
        // Check counts, contiguity and total byte residency before assembling or
        // allocating the retained document, even if foreign tables lack checks.
        let valid: bool = tx.query_row(
            "SELECT count(*)<=16384 AND coalesce(sum(length(CAST(payload AS BLOB))+1),0)-count(DISTINCT kind)+(SELECT length(payload) FROM client_journal WHERE singleton=1)<=?1 AND coalesce(min(kind),0)>=0 AND coalesce(max(kind),0)<=3 AND NOT EXISTS(SELECT 1 FROM client_journal_history GROUP BY kind HAVING min(position)!=0 OR max(position)!=count(*)-1 OR count(*)>4096) FROM client_journal_history", [limit], |row| row.get(0),
        )?;
        if !valid { return Err(StorageFailure::InvalidState); }
        let (revision, payload): (Vec<u8>, Vec<u8>) = tx.query_row(
            "SELECT revision,CAST(json_set(CAST(payload AS TEXT),'$.streams',json((SELECT json_group_array(json(payload)) FROM (SELECT payload FROM client_journal_history WHERE kind=0 ORDER BY position))),'$.tombstones',json((SELECT json_group_array(json(payload)) FROM (SELECT payload FROM client_journal_history WHERE kind=1 ORDER BY position))),'$.pairing.slots',json((SELECT json_group_array(json(payload)) FROM (SELECT payload FROM client_journal_history WHERE kind=2 ORDER BY position))),'$.generations',json((SELECT json_group_array(json(payload)) FROM (SELECT payload FROM client_journal_history WHERE kind=3 ORDER BY position)))) AS BLOB) FROM client_journal WHERE singleton=1 AND purpose='client-journal-v1' AND length(revision)=8 AND json_type(CAST(payload AS TEXT),'$.streams')='array' AND json_array_length(CAST(payload AS TEXT),'$.streams')=0 AND json_type(CAST(payload AS TEXT),'$.tombstones')='array' AND json_array_length(CAST(payload AS TEXT),'$.tombstones')=0 AND json_type(CAST(payload AS TEXT),'$.pairing.slots')='array' AND json_array_length(CAST(payload AS TEXT),'$.pairing.slots')=0 AND json_type(CAST(payload AS TEXT),'$.generations')='array' AND json_array_length(CAST(payload AS TEXT),'$.generations')=0", [], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut lengths: [Vec<usize>; 4] = Default::default();
        for (kind, values) in lengths.iter_mut().enumerate() {
            let encoded: String = tx.query_row(
                "SELECT json_group_array(length(CAST(payload AS BLOB))) FROM (SELECT payload FROM client_journal_history WHERE kind=?1 ORDER BY position)", [kind as i64], |row| row.get(0),
            )?;
            *values = serde_json::from_str(&encoded).map_err(|_| StorageFailure::InvalidState)?;
        }
        Ok((revision, payload, lengths))
    }).map_err(classify)?;
    if payload.len() > owner.limits.retained_bytes {
        return Err(invalid_storage());
    }
    let state: RecoveredClientState =
        serde_json::from_slice(&payload).map_err(|_| invalid_storage())?;
    let revision = ClientJournalRevision::new(u64::from_be_bytes(
        revision.try_into().map_err(|_| invalid_storage())?,
    ));
    if state.revision() != revision {
        return Err(invalid_storage());
    }
    state.validate(owner.codec.as_ref(), owner.verifier.as_ref(), owner.limits)?;
    Ok(Cache { state, lengths })
}
fn changed_records(
    old: &RecoveredClientState,
    next: &RecoveredClientState,
    previous_lengths: &[Vec<usize>; 4],
    limits: JournalLimits,
) -> Result<(Vec<RecordUpdate>, [Vec<usize>; 4]), ClientFailure> {
    fn collect<T: serde::Serialize + PartialEq>(
        old: &[T],
        next: &[T],
        kind: usize,
        lengths: &mut Vec<usize>,
        updates: &mut Vec<RecordUpdate>,
        limits: JournalLimits,
    ) -> Result<(), ClientFailure> {
        for (position, record) in next.iter().enumerate() {
            if old.get(position) != Some(record) {
                let payload = encode(record, limits)?;
                if position < lengths.len() {
                    lengths[position] = payload.len();
                } else {
                    lengths.push(payload.len());
                }
                updates.push(RecordUpdate {
                    kind,
                    position,
                    payload: Some(payload),
                });
            }
        }
        for position in next.len()..old.len() {
            updates.push(RecordUpdate {
                kind,
                position,
                payload: None,
            });
        }
        lengths.truncate(next.len());
        Ok(())
    }
    let mut lengths = previous_lengths.clone();
    let mut updates = Vec::new();
    collect(
        old.streams(),
        next.streams(),
        0,
        &mut lengths[0],
        &mut updates,
        limits,
    )?;
    collect(
        old.tombstones(),
        next.tombstones(),
        1,
        &mut lengths[1],
        &mut updates,
        limits,
    )?;
    collect(
        old.pairing().slots(),
        next.pairing().slots(),
        2,
        &mut lengths[2],
        &mut updates,
        limits,
    )?;
    collect(
        old.generations(),
        next.generations(),
        3,
        &mut lengths[3],
        &mut updates,
        limits,
    )?;
    Ok((updates, lengths))
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
fn encode(state: &impl serde::Serialize, limits: JournalLimits) -> Result<Vec<u8>, ClientFailure> {
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
                Migration {
                    sql: "CREATE TABLE future_projection(value INTEGER) STRICT",
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
            &[
                Migration { sql: "" },
                Migration { sql: "" },
                Migration { sql: "" },
            ],
        )
        .unwrap();
        assert_eq!(database.schema_version().unwrap(), 3);
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
    async fn native_journal_migrates_schema_one_without_losing_comparison_records() {
        use spaceterm_client::journal::*;
        let directory = Directory::new();
        let state = RecoveredClientState::empty()
            .apply(
                &ClientJournalTransaction {
                    expected_revision: ClientJournalRevision::new(0),
                    changes: vec![ClientJournalChange::SaveBinding(Box::new(saved()))],
                },
                &Codec,
                &Verifier,
                JournalLimits::default(),
            )
            .unwrap();
        for legacy_pairing in [false, true] {
            let mut database = Database::open(&directory.0, &[Migration { sql: SCHEMA }]).unwrap();
            let mut document = serde_json::to_value(&state).unwrap();
            if legacy_pairing {
                document.as_object_mut().unwrap().remove("pairing");
            }
            let expected: RecoveredClientState = serde_json::from_value(document.clone()).unwrap();
            let payload = serde_json::to_vec(&document).unwrap();
            let revision = state.revision().get().to_be_bytes();
            assert_eq!(database.transaction(|tx| {
                tx.execute("INSERT INTO client_journal(singleton,purpose,revision,payload) VALUES(1,'client-journal-v1',?1,?2)", (revision.as_slice(), payload.as_slice()))?;
                Ok(())
            }), CommitDisposition::Durable(()));
            drop(database);
            let journal = NativeClientJournal::open_existing(
                directory.0.clone(),
                Arc::new(Codec),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap();
            assert_eq!(journal.recover().await.unwrap(), expected);
            drop(journal);
            for entry in std::fs::read_dir(&directory.0).unwrap() {
                std::fs::remove_file(entry.unwrap().path()).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn native_journal_migration_does_not_repair_missing_history() {
        for field in ["streams", "tombstones", "generations"] {
            let directory = Directory::new();
            let mut database = Database::open(&directory.0, &[Migration { sql: SCHEMA }]).unwrap();
            let mut document = serde_json::to_value(RecoveredClientState::empty()).unwrap();
            document.as_object_mut().unwrap().remove(field);
            let payload = serde_json::to_vec(&document).unwrap();
            assert_eq!(database.transaction(|tx| {
                tx.execute("INSERT INTO client_journal(singleton,purpose,revision,payload) VALUES(1,'client-journal-v1',?1,?2)", ([0u8; 8].as_slice(), payload.as_slice()))?;
                Ok(())
            }), CommitDisposition::Durable(()));
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
            let database =
                Database::open_existing(&directory.0, &[Migration { sql: SCHEMA }]).unwrap();
            assert_eq!(database.schema_version().unwrap(), 1);
            let retained: Vec<u8> = database
                .query_row("SELECT payload FROM client_journal", [], |row| row.get(0))
                .unwrap();
            assert_eq!(retained, payload);
        }
    }

    #[tokio::test]
    async fn native_journal_history_write_failure_rolls_back_metadata_and_cache() {
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
        let mut database = Database::open_existing(&directory.0, MIGRATIONS).unwrap();
        assert_eq!(database.transaction(|tx| tx.execute_batch(
            "CREATE TRIGGER reject_second_stream BEFORE INSERT ON client_journal_history WHEN NEW.kind=0 AND NEW.position=1 BEGIN SELECT RAISE(ABORT,'fixture'); END"
        )), CommitDisposition::Durable(()));
        drop(database);
        let tx = ClientJournalTransaction {
            expected_revision: ClientJournalRevision::new(0),
            changes: vec![ClientJournalChange::SaveBinding(Box::new(saved()))],
        };
        assert_eq!(
            journal.commit(&tx).await.unwrap(),
            JournalCommitDisposition::NotApplied(JournalFailure::InvalidState)
        );
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
    async fn native_journal_metadata_commit_does_not_rewrite_comparison_history() {
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
        let binding = saved();
        let tx = ClientJournalTransaction {
            expected_revision: ClientJournalRevision::new(0),
            changes: vec![ClientJournalChange::SaveBinding(Box::new(binding.clone()))],
        };
        assert!(matches!(
            journal.commit(&tx).await.unwrap(),
            JournalCommitDisposition::Durable(_)
        ));
        drop(journal);
        let mut database = Database::open_existing(&directory.0, MIGRATIONS).unwrap();
        assert_eq!(database.transaction(|tx| tx.execute_batch(
            "CREATE TABLE observed_history_writes(value INTEGER NOT NULL) STRICT;             INSERT INTO observed_history_writes VALUES(0);             CREATE TRIGGER observe_history_insert AFTER INSERT ON client_journal_history BEGIN UPDATE observed_history_writes SET value=value+1; END;             CREATE TRIGGER observe_history_update AFTER UPDATE ON client_journal_history BEGIN UPDATE observed_history_writes SET value=value+1; END;             CREATE TRIGGER observe_history_delete AFTER DELETE ON client_journal_history BEGIN UPDATE observed_history_writes SET value=value+1; END"
        )), CommitDisposition::Durable(()));
        drop(database);
        let journal = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let tx = ClientJournalTransaction {
            expected_revision: ClientJournalRevision::new(1),
            changes: vec![ClientJournalChange::ReconcileGrants {
                local_pairing: binding.binding().unwrap().target().local_pairing().unwrap(),
                epoch: spaceterm_core::authority::ServerEpoch::new(1),
                revision: spaceterm_core::authority::OrganizationRevision::new(1),
                grants: vec![],
            }],
        };
        assert!(matches!(
            journal.commit(&tx).await.unwrap(),
            JournalCommitDisposition::Durable(_)
        ));
        assert_eq!(journal.recover().await.unwrap().streams().len(), 2);
        drop(journal);
        let database = Database::open_existing(&directory.0, MIGRATIONS).unwrap();
        let writes: u64 = database
            .query_row("SELECT value FROM observed_history_writes", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(writes, 0);
    }

    #[tokio::test]
    async fn native_journal_unchanged_recovery_does_not_reverify_the_snapshot() {
        struct CountingVerifier(std::sync::atomic::AtomicU64);
        impl ReceiptVerifier for CountingVerifier {
            fn verify(
                &self,
                pin: ServerPin,
                receipt: &SignedPairingReceipt,
            ) -> Result<(), ClientFailure> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Verifier.verify(pin, receipt)
            }
        }
        let directory = Directory::new();
        let verifier = Arc::new(CountingVerifier(0.into()));
        let journal = NativeClientJournal::create(
            directory.0.clone(),
            Arc::new(Codec),
            verifier.clone(),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let tx = ClientJournalTransaction {
            expected_revision: ClientJournalRevision::new(0),
            changes: vec![spaceterm_client::journal::ClientJournalChange::SaveBinding(
                Box::new(saved()),
            )],
        };
        assert!(matches!(
            journal.commit(&tx).await.unwrap(),
            JournalCommitDisposition::Durable(_)
        ));
        verifier.0.store(0, Ordering::Relaxed);
        for _ in 0..16 {
            assert_eq!(journal.recover().await.unwrap().revision().get(), 1);
        }
        assert_eq!(verifier.0.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn native_journal_recovery_observes_another_owners_commit() {
        use spaceterm_client::journal::*;
        let directory = Directory::new();
        let first = NativeClientJournal::create(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let second = NativeClientJournal::open_existing(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let binding = saved();
        let tx = ClientJournalTransaction {
            expected_revision: ClientJournalRevision::new(0),
            changes: vec![ClientJournalChange::SaveBinding(Box::new(binding.clone()))],
        };
        assert!(matches!(
            first.commit(&tx).await.unwrap(),
            JournalCommitDisposition::Durable(_)
        ));
        assert_eq!(
            second.recover().await.unwrap(),
            first.recover().await.unwrap()
        );
        let tx = ClientJournalTransaction {
            expected_revision: ClientJournalRevision::new(1),
            changes: vec![ClientJournalChange::ReconcileGrants {
                local_pairing: binding.binding().unwrap().target().local_pairing().unwrap(),
                epoch: spaceterm_core::authority::ServerEpoch::new(1),
                revision: spaceterm_core::authority::OrganizationRevision::new(1),
                grants: vec![],
            }],
        };
        assert!(matches!(
            second.commit(&tx).await.unwrap(),
            JournalCommitDisposition::Durable(_)
        ));
        assert_eq!(first.recover().await.unwrap().revision().get(), 2);
        assert_eq!(
            first.commit(&tx).await.unwrap(),
            JournalCommitDisposition::NotApplied(JournalFailure::Conflict)
        );
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
        let expected = journal.snapshot().unwrap();
        let mut payload = serde_json::to_value(expected.persistence_metadata()).unwrap();
        drop(journal);
        drop(native);
        // The unmodified fixture must recover before the digest-only corruption.
        // Otherwise a physical-schema error can mask missing digest validation.
        let mut database = Database::open_existing(&directory.0, MIGRATIONS).unwrap();
        let valid_payload = serde_json::to_vec(&payload).unwrap();
        assert_eq!(
            database.transaction(|tx| tx
                .execute(
                    "UPDATE client_journal SET payload=?1",
                    [valid_payload.as_slice()]
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
        assert_eq!(reopened.recover().await.unwrap(), expected);
        drop(reopened);
        payload["revocations"][0]["task"]["request_digest"] =
            serde_json::to_value([99u8; 32]).unwrap();
        let payload = serde_json::to_vec(&payload).unwrap();
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
    async fn native_journal_recovery_rejects_non_array_history_placeholders() {
        let directory = Directory::new();
        let native = NativeClientJournal::create(
            directory.0.clone(),
            Arc::new(Codec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let state = native.recover().await.unwrap();
        let metadata = serde_json::to_value(state.persistence_metadata()).unwrap();
        drop(native);
        for pointer in ["/streams", "/tombstones", "/pairing/slots", "/generations"] {
            for value in [
                serde_json::json!({}),
                serde_json::json!(1),
                serde_json::Value::Null,
            ] {
                let mut corrupted = metadata.clone();
                *corrupted.pointer_mut(pointer).unwrap() = value;
                let payload = serde_json::to_vec(&corrupted).unwrap();
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
                    result.err().map(|error| error.class()),
                    Some(ClientFailureClass::InvalidStorage),
                    "accepted non-array history placeholder {pointer}"
                );
            }
        }
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
                    ssh_capture: None,
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
                    ssh_capture: None,
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
