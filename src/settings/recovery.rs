//! Resets malformed Settings while retaining the original file as a backup.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum RecoveryError {
    #[error("settings are not malformed")]
    NotMalformed,
    #[error("settings are busy")]
    Busy,
    #[error("settings revisions are exhausted")]
    RevisionExhausted,
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryReceipt {
    pub(crate) durability: Durability,
}

impl Settings {
    pub(crate) fn recover_by_reset(&self) -> Result<RecoveryReceipt, RecoveryError> {
        let mut state = self.0.lock();
        if !matches!(state.transaction, Transaction::Idle) {
            return Err(RecoveryError::Busy);
        }
        if state.storage_ready || !state.status.is_some_and(SettingsError::is_malformed) {
            return Err(RecoveryError::NotMalformed);
        }
        let revision = state
            .committed
            .revision
            .checked_add(1)
            .filter(|_| state.catalog_revision < u64::MAX)
            .ok_or(RecoveryError::RevisionExhausted)?;
        let mut defaults = SettingsDocument::default();
        let bytes = export_settings(&defaults).expect("default Settings document is valid");
        let result = self
            .0
            .storage
            .quarantine()
            .and_then(|()| self.0.storage.write(bytes.as_bytes(), None));
        match result {
            Ok(commit) => {
                // Retire locally captured edits independently of the default file revision.
                defaults.revision = revision;
                state.committed = Arc::new(defaults);
                state.recoverable_candidate = None;
                state.expected = commit.identity;
                state.storage_ready = state.expected.is_some();
                state.status = (!state.storage_ready)
                    .then_some(SettingsError::Storage(StorageError::Conflict));
                state.retire_catalog_revision();
                self.0.notify();
                Ok(RecoveryReceipt {
                    durability: commit.durability,
                })
            }
            Err(error) => {
                state.status = Some(SettingsError::Storage(error));
                self.0.notify();
                Err(error.into())
            }
        }
    }
}
