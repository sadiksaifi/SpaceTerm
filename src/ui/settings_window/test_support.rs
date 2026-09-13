//! Shared storage Adapter for Settings draft and window tests.

use std::sync::{Arc, Mutex};

use crate::appearance::{AppearanceDocument, export_settings};
use crate::platform::secure_filesystem::{PrivateFileSnapshot, SecureEntryIdentity};
use crate::settings::storage::{Durability, SettingsStorage, StorageCommit, StorageError};

/// In-memory Settings storage that counts writes and can be made to fail on demand.
#[derive(Default)]
pub(super) struct MemoryStorage(Mutex<MemoryState>);

#[derive(Default)]
struct MemoryState {
    snapshot: Option<(Vec<u8>, u64)>,
    writes: usize,
    write_failure: Option<StorageError>,
    read_failure: Option<StorageError>,
    /// Publishes without a verifiable identity, which forces a reload before the next write.
    drop_identity: bool,
}

impl MemoryStorage {
    pub(super) fn with_document(document: &AppearanceDocument) -> Arc<Self> {
        let storage = Arc::new(Self::default());
        let bytes = export_settings(document)
            .expect("fixture document")
            .into_bytes();
        storage.0.lock().unwrap().snapshot = Some((bytes, 1));
        storage
    }

    pub(super) fn writes(&self) -> usize {
        self.0.lock().unwrap().writes
    }

    pub(super) fn document(&self) -> Option<AppearanceDocument> {
        let state = self.0.lock().unwrap();
        let (bytes, _) = state.snapshot.as_ref()?;
        crate::appearance::parse_settings(bytes).ok()
    }

    pub(super) fn fail_writes(&self, error: Option<StorageError>) {
        self.0.lock().unwrap().write_failure = error;
    }

    pub(super) fn corrupt(&self) {
        self.0.lock().unwrap().snapshot = Some((b"{ not settings".to_vec(), 1));
    }

    pub(super) fn repair(&self) {
        let bytes = export_settings(&AppearanceDocument::default())
            .expect("default document")
            .into_bytes();
        self.0.lock().unwrap().snapshot = Some((bytes, 2));
    }

    pub(super) fn drop_identity(&self, drop: bool) {
        self.0.lock().unwrap().drop_identity = drop;
    }
}

impl SettingsStorage for MemoryStorage {
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
