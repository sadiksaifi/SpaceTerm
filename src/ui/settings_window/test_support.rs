//! Shared storage Adapter for Settings draft and window tests.

use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use crate::appearance::{SettingsDocument, export_settings};
use crate::platform::secure_filesystem::{PrivateFileSnapshot, SecureEntryIdentity};
use crate::settings::storage::{Durability, SettingsStorage, StorageCommit, StorageError};

/// The bytes [`MemoryStorage::corrupt`] retains: not a Settings Document.
pub(crate) const CORRUPT_DOCUMENT: &[u8] = b"{ not settings";

/// In-memory Settings storage that counts writes and can be made to fail on demand.
#[derive(Default)]
pub(crate) struct MemoryStorage(Mutex<MemoryState>, Mutex<Option<Arc<WriteGate>>>);

#[derive(Default)]
struct MemoryState {
    snapshot: Option<(Vec<u8>, u64)>,
    backup: Option<Vec<u8>>,
    writes: usize,
    write_failure: Option<StorageError>,
    read_failure: Option<StorageError>,
    /// Publishes without a verifiable identity, which forces a reload before the next write.
    drop_identity: bool,
}

#[derive(Default)]
struct WriteGate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

pub(crate) struct BlockedWrite(Arc<WriteGate>);

impl BlockedWrite {
    pub(crate) fn wait_until_started(&self) {
        let (state, timeout) = self
            .0
            .changed
            .wait_timeout_while(
                self.0.state.lock().unwrap(),
                Duration::from_secs(5),
                |state| !state.0,
            )
            .unwrap();
        assert!(state.0 && !timeout.timed_out(), "the write should start");
    }

    pub(crate) fn release(&self) {
        self.0.state.lock().unwrap().1 = true;
        self.0.changed.notify_all();
    }
}

impl Drop for BlockedWrite {
    fn drop(&mut self) {
        self.release();
    }
}

impl MemoryStorage {
    pub(crate) fn block_next_write(&self) -> BlockedWrite {
        let gate = Arc::new(WriteGate::default());
        *self.1.lock().unwrap() = Some(gate.clone());
        BlockedWrite(gate)
    }

    pub(crate) fn with_document(document: &SettingsDocument) -> Arc<Self> {
        let bytes = export_settings(document)
            .expect("fixture document")
            .into_bytes();
        Self::with_bytes(bytes)
    }

    /// Stores a Settings file as written, including content this build would not write.
    pub(crate) fn with_bytes(bytes: Vec<u8>) -> Arc<Self> {
        let storage = Arc::new(Self::default());
        storage.0.lock().unwrap().snapshot = Some((bytes, 1));
        storage
    }

    pub(crate) fn writes(&self) -> usize {
        self.0.lock().unwrap().writes
    }

    pub(crate) fn document(&self) -> Option<SettingsDocument> {
        let state = self.0.lock().unwrap();
        let (bytes, _) = state.snapshot.as_ref()?;
        crate::appearance::parse_settings(bytes).ok()
    }

    pub(super) fn fail_writes(&self, error: Option<StorageError>) {
        self.0.lock().unwrap().write_failure = error;
    }

    pub(crate) fn corrupt(&self) {
        self.0.lock().unwrap().snapshot = Some((CORRUPT_DOCUMENT.to_vec(), 1));
    }

    /// The bytes Settings Recovery moved aside, if it ran.
    pub(crate) fn backup(&self) -> Option<Vec<u8>> {
        self.0.lock().unwrap().backup.clone()
    }

    pub(crate) fn fail_reads(&self, error: Option<StorageError>) {
        self.0.lock().unwrap().read_failure = error;
    }

    /// Saves `document` as another program would, under a new identity.
    pub(crate) fn save_elsewhere(&self, document: &SettingsDocument) {
        self.save_bytes_elsewhere(
            export_settings(document)
                .expect("fixture document")
                .into_bytes(),
        );
    }

    /// Saves `bytes`, which need not be a Settings Document, as another program would.
    pub(crate) fn save_bytes_elsewhere(&self, bytes: Vec<u8>) {
        let mut state = self.0.lock().unwrap();
        let identity = state.snapshot.as_ref().map_or(0, |(_, identity)| *identity) + 100;
        state.snapshot = Some((bytes, identity));
    }

    pub(super) fn repair(&self) {
        let bytes = export_settings(&SettingsDocument::default())
            .expect("default document")
            .into_bytes();
        self.0.lock().unwrap().snapshot = Some((bytes, 2));
    }

    pub(super) fn drop_identity(&self, drop: bool) {
        self.0.lock().unwrap().drop_identity = drop;
    }
}

impl SettingsStorage for MemoryStorage {
    fn quarantine(&self) -> Result<(), StorageError> {
        let mut state = self.0.lock().unwrap();
        if let Some(error) = state.write_failure {
            return Err(error);
        }
        state.backup = Some(state.snapshot.take().ok_or(StorageError::Unavailable)?.0);
        Ok(())
    }
    fn read(&self) -> Result<Option<PrivateFileSnapshot>, StorageError> {
        let state = self.0.lock().unwrap();
        if let Some(error) = state.read_failure {
            return Err(error);
        }
        Ok(state
            .snapshot
            .as_ref()
            .map(|(bytes, identity)| PrivateFileSnapshot {
                bytes: bytes.clone(),
                identity: SecureEntryIdentity::from_opaque(*identity),
            }))
    }

    fn write(
        &self,
        bytes: &[u8],
        expected: Option<&SecureEntryIdentity>,
    ) -> Result<StorageCommit, StorageError> {
        let gate = self.1.lock().unwrap().take();
        if let Some(gate) = gate {
            let mut state = gate.state.lock().unwrap();
            state.0 = true;
            gate.changed.notify_all();
            drop(gate.changed.wait_while(state, |state| !state.1).unwrap());
        }
        let mut state = self.0.lock().unwrap();
        if let Some(error) = state.write_failure {
            return Err(error);
        }
        let expected = expected
            .and_then(|identity| identity.opaque_ref::<u64>())
            .copied();
        if expected != state.snapshot.as_ref().map(|(_, identity)| *identity) {
            return Err(StorageError::Conflict);
        }
        state.writes += 1;
        let identity = expected.unwrap_or_default() + 1;
        state.snapshot = Some((bytes.to_vec(), identity));
        let drop_identity = state.drop_identity;
        Ok(StorageCommit {
            durability: Durability::Synchronized,
            identity: (!drop_identity).then(|| SecureEntryIdentity::from_opaque(identity)),
        })
    }
}
