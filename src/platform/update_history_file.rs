//! The desktop's update history, `update-history.json` in its private state directory, where
//! the desktop state root's migrations reach it.
use std::cell::RefCell;
use std::ffi::OsStr;
use std::sync::Arc;

use super::app_directories::AppDirectoryRoot;
use super::app_paths::AppPaths;
use super::secure_filesystem::{SecureCommitOutcome, SecureEntryIdentity};
use crate::updates::policy::UpdateHistory;

pub(crate) const HISTORY_NAME: &str = "update-history.json";
const MAX_HISTORY_BYTES: usize = 4096;

/// Update observations in the private state directory. They are never installation authority.
pub(crate) struct UpdateHistoryFile {
    paths: Option<Arc<AppPaths>>,
    observed: RefCell<Option<SecureEntryIdentity>>,
    written: RefCell<Option<Vec<u8>>>,
}

impl UpdateHistoryFile {
    pub(crate) fn new(paths: Option<Arc<AppPaths>>) -> Self {
        Self {
            paths,
            observed: RefCell::new(None),
            written: RefCell::new(None),
        }
    }

    pub(crate) fn load(&self) -> UpdateHistory {
        let Some(paths) = &self.paths else {
            return UpdateHistory::default();
        };
        let Ok(Some(directory)) = paths.open_secure_root(AppDirectoryRoot::State) else {
            return UpdateHistory::default();
        };
        let Ok(Some(snapshot)) = paths.filesystem().read_private_file(
            &directory,
            OsStr::new(HISTORY_NAME),
            MAX_HISTORY_BYTES,
        ) else {
            return UpdateHistory::default();
        };
        *self.observed.borrow_mut() = Some(snapshot.identity);
        if snapshot.bytes.len() > MAX_HISTORY_BYTES {
            return UpdateHistory::default();
        }
        let history = serde_json::from_slice(&snapshot.bytes).unwrap_or_default();
        *self.written.borrow_mut() = Some(snapshot.bytes);
        history
    }

    /// Each scheduler tick saves the history, so only a changed document reaches the disk.
    pub(crate) fn save(&self, history: &UpdateHistory) {
        let Some(paths) = &self.paths else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(history) else {
            return;
        };
        if bytes.len() > MAX_HISTORY_BYTES || self.written.borrow().as_ref() == Some(&bytes) {
            return;
        }
        let Ok(directory) = paths.ensure_secure_root(AppDirectoryRoot::State) else {
            return;
        };
        let filesystem = paths.filesystem();
        let name = OsStr::new(HISTORY_NAME);
        let mut nonce = [0; 16];
        if getrandom::fill(&mut nonce).is_err() {
            return;
        }
        let Ok(prepared) = filesystem.prepare_private_file(&directory, name, &bytes, nonce) else {
            return;
        };
        let expected = self.observed.borrow().clone();
        let Ok(commit) = filesystem.commit_private_file(prepared, expected.as_ref()) else {
            return;
        };
        if commit.outcome == SecureCommitOutcome::Conflict {
            // Another instance wrote meanwhile. Adopt its identity so the next save can replace it.
            *self.observed.borrow_mut() = filesystem
                .read_private_file(&directory, name, MAX_HISTORY_BYTES)
                .ok()
                .flatten()
                .map(|snapshot| snapshot.identity);
            return;
        }
        *self.observed.borrow_mut() = commit.published_identity;
        *self.written.borrow_mut() = Some(bytes);
    }
}

