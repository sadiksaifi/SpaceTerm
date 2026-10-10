//! Owns committed preferences, preview lifetime, and serialized identity-aware writes.

mod document;
#[cfg(test)]
mod document_tests;
pub(crate) mod git;
#[cfg(test)]
mod schema_tests;

pub(crate) mod recovery;
pub(crate) mod storage;
#[cfg(test)]
mod storage_tests;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, MutexGuard, Weak},
};

#[cfg(any(test, feature = "developer-tools"))]
use crate::appearance::ResetTarget;
use crate::appearance::{
    CatalogError, ImportError, TerminalTheme, ThemeCatalog, ThemeId, ZedExtension,
    translate_zed_extension, translate_zed_family,
};
use crate::platform::secure_filesystem::SecureEntryIdentity;
pub(crate) use document::{
    SettingsDocument, SettingsDocumentError, export_settings, parse_settings,
};
use storage::{Durability, SettingsStorage, StorageError};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum SettingsError {
    #[error("settings are busy")]
    Busy,
    #[error("settings revision is stale")]
    Stale,
    #[error("settings revision is exhausted")]
    RevisionExhausted,
    #[error("settings are invalid")]
    Invalid,
    #[error(transparent)]
    Import(#[from] ImportError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

impl SettingsError {
    pub(crate) fn is_malformed(self) -> bool {
        matches!(self, Self::Invalid | Self::Storage(StorageError::TooLarge))
    }
}

impl From<SettingsDocumentError> for SettingsError {
    fn from(_: SettingsDocumentError) -> Self {
        Self::Invalid
    }
}

#[cfg(any(test, feature = "developer-tools"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreviewPhase {
    Idle,
    Previewing,
    Committing,
}

#[derive(Clone)]
pub(crate) struct SettingsSnapshot {
    pub(crate) committed: Arc<SettingsDocument>,
    pub(crate) candidate: Arc<SettingsDocument>,
    /// A failed direct save remains recoverable without applying it as a live preview.
    #[cfg(any(test, feature = "developer-tools"))]
    pub(crate) recoverable_candidate: Option<Arc<SettingsDocument>>,
    /// Retires import/replacement plans whenever the live editing state changes.
    pub(crate) catalog_revision: u64,
    #[cfg(any(test, feature = "developer-tools"))]
    pub(crate) phase: PreviewPhase,
    pub(crate) status: Option<SettingsError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommitOutcome {
    pub(crate) revision: u64,
    pub(crate) durability: Durability,
    pub(crate) reload_required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImportReceipt {
    pub(crate) installed: Vec<ThemeId>,
    pub(crate) catalog_revision: u64,
}

/// Clones share the same transaction and storage authority.
#[derive(Clone)]
pub(crate) struct Settings(Arc<SettingsInner>);

struct SettingsInner {
    state: Mutex<State>,
    storage: Arc<dyn SettingsStorage>,
    subscribers: Mutex<Vec<async_channel::Sender<()>>>,
}

struct State {
    committed: Arc<SettingsDocument>,
    recoverable_candidate: Option<Arc<SettingsDocument>>,
    catalog_revision: u64,
    expected: Option<SecureEntryIdentity>,
    storage_ready: bool,
    status: Option<SettingsError>,
    next_preview: u64,
    transaction: Transaction,
}

enum Transaction {
    Idle,
    Preview {
        id: u64,
        candidate: Arc<SettingsDocument>,
    },
    Committing {
        id: Option<u64>,
        candidate: Arc<SettingsDocument>,
        owner_alive: bool,
    },
}

/// Dropping the last lease cancels a preview; a started commit owns its immutable candidate.
pub(crate) struct PreviewToken {
    owner: Weak<SettingsInner>,
    id: u64,
    revision: u64,
}

/// A Zed theme source. Translation is bounded and color-only; installing never selects a theme.
#[derive(Clone, Copy)]
pub(crate) enum ThemeImport<'a> {
    /// One Zed theme family document the user selected. Reinstalling it replaces its themes.
    ZedFamily(&'a [u8]),
    /// A Zed theme extension from the registry. Installing it replaces every theme an earlier
    /// version of the same extension installed, including themes the new version no longer has.
    ZedExtension(&'a ZedExtension),
}

impl ThemeImport<'_> {
    fn translate(self) -> Result<Vec<TerminalTheme>, ImportError> {
        match self {
            Self::ZedFamily(bytes) => translate_zed_family(bytes),
            Self::ZedExtension(extension) => translate_zed_extension(extension),
        }
    }

    /// Installed themes this source supersedes.
    fn retired(self, document: &SettingsDocument) -> BTreeSet<ThemeId> {
        let Self::ZedExtension(extension) = self else {
            return BTreeSet::new();
        };
        document
            .terminal_themes
            .iter()
            .filter(|theme| {
                theme.metadata.origin.as_ref().is_some_and(|origin| {
                    origin.package_id.as_deref() == Some(extension.id.as_str())
                })
            })
            .map(|theme| theme.id.clone())
            .collect()
    }
}

impl Drop for PreviewToken {
    fn drop(&mut self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let mut state = owner.lock();
        match &mut state.transaction {
            Transaction::Preview { id, .. } if *id == self.id => {
                state.transaction = Transaction::Idle;
                state.retire_catalog_revision();
                owner.notify();
            }
            Transaction::Committing {
                id: Some(id),
                owner_alive,
                ..
            } if *id == self.id => {
                *owner_alive = false;
            }
            _ => {}
        }
    }
}

impl SettingsInner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn notify(&self) {
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|subscriber| {
                !matches!(
                    subscriber.try_send(()),
                    Err(async_channel::TrySendError::Closed(()))
                )
            });
    }
}

impl Settings {
    /// Loads without creating a missing file or replacing an invalid one.
    pub(crate) fn load(storage: Arc<dyn SettingsStorage>) -> Self {
        let mut state = State {
            committed: Arc::new(SettingsDocument::default()),
            recoverable_candidate: None,
            catalog_revision: 0,
            expected: None,
            storage_ready: false,
            status: None,
            next_preview: 0,
            transaction: Transaction::Idle,
        };
        match read_document(storage.as_ref()) {
            Ok((document, identity)) => {
                state.committed = Arc::new(document);
                state.expected = identity;
                state.storage_ready = true;
            }
            Err(error) => state.status = Some(error),
        }
        Self(Arc::new(SettingsInner {
            state: Mutex::new(state),
            storage,
            subscribers: Mutex::new(Vec::new()),
        }))
    }

    pub(crate) fn subscribe(&self) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        self.0
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(sender);
        receiver
    }

    /// The committed document and any candidate a preview or a started commit may still retain,
    /// including a commit whose preview owner has gone.
    pub(crate) fn retainable_documents(&self) -> Vec<Arc<SettingsDocument>> {
        let state = self.0.lock();
        let mut documents = vec![Arc::clone(&state.committed)];
        match &state.transaction {
            Transaction::Idle => {}
            Transaction::Preview { candidate, .. } | Transaction::Committing { candidate, .. } => {
                documents.push(Arc::clone(candidate));
            }
        }
        documents
    }

    pub(crate) fn snapshot(&self) -> SettingsSnapshot {
        let state = self.0.lock();
        let candidate = match &state.transaction {
            Transaction::Idle => Arc::clone(&state.committed),
            Transaction::Preview { candidate, .. } => Arc::clone(candidate),
            Transaction::Committing { id, candidate, .. } => {
                if id.is_some() {
                    Arc::clone(candidate)
                } else {
                    Arc::clone(&state.committed)
                }
            }
        };
        SettingsSnapshot {
            committed: Arc::clone(&state.committed),
            candidate,
            #[cfg(any(test, feature = "developer-tools"))]
            recoverable_candidate: state.recoverable_candidate.clone(),
            catalog_revision: state.catalog_revision,
            #[cfg(any(test, feature = "developer-tools"))]
            phase: match state.transaction {
                Transaction::Idle => PreviewPhase::Idle,
                Transaction::Preview { .. } => PreviewPhase::Previewing,
                Transaction::Committing { .. } => PreviewPhase::Committing,
            },
            status: state.status,
        }
    }

    pub(crate) fn begin_preview(&self, revision: u64) -> Result<PreviewToken, SettingsError> {
        let mut state = self.0.lock();
        state.require_idle(revision)?;
        let id = state
            .next_preview
            .checked_add(1)
            .ok_or(SettingsError::RevisionExhausted)?;
        state.next_preview = id;
        state.transaction = Transaction::Preview {
            id,
            candidate: Arc::clone(&state.committed),
        };
        state.retire_catalog_revision();
        self.0.notify();
        Ok(PreviewToken {
            owner: Arc::downgrade(&self.0),
            id,
            revision,
        })
    }

    pub(crate) fn update_preview(
        &self,
        token: &PreviewToken,
        candidate: SettingsDocument,
    ) -> Result<(), SettingsError> {
        let mut state = self.0.lock();
        self.require_token(&state, token)?;
        let candidate = validate_candidate(candidate, token.revision)?;
        if let Transaction::Preview {
            candidate: current, ..
        } = &mut state.transaction
        {
            *current = Arc::new(candidate);
        }
        state.retire_catalog_revision();
        self.0.notify();
        Ok(())
    }

    pub(crate) fn cancel_preview(&self, token: &PreviewToken) -> Result<(), SettingsError> {
        let mut state = self.0.lock();
        self.require_token(&state, token)?;
        state.transaction = Transaction::Idle;
        state.retire_catalog_revision();
        self.0.notify();
        Ok(())
    }

    /// Captures a single candidate. The caller executes the returned job off the UI thread.
    pub(crate) fn commit_preview(&self, token: &PreviewToken) -> Result<CommitJob, SettingsError> {
        let mut state = self.0.lock();
        self.require_token(&state, token)?;
        let Transaction::Preview { candidate, .. } = &state.transaction else {
            return Err(SettingsError::Stale);
        };
        let candidate = Arc::clone(candidate);
        self.prepare_commit(&mut state, Some(token.id), candidate)
    }

    pub(crate) fn update_committed(
        &self,
        revision: u64,
        candidate: SettingsDocument,
    ) -> Result<CommitJob, SettingsError> {
        let mut state = self.0.lock();
        state.require_idle(revision)?;
        self.prepare_direct_commit(&mut state, candidate)
    }

    #[cfg(any(test, feature = "developer-tools"))]
    pub(crate) fn reset_preview(
        &self,
        token: &PreviewToken,
        target: ResetTarget,
    ) -> Result<(), SettingsError> {
        self.edit_preview(token, |document| {
            document.reset(target)?;
            Ok(())
        })
    }

    pub(crate) fn import_preview(
        &self,
        token: &PreviewToken,
        catalog_revision: u64,
        source: ThemeImport<'_>,
    ) -> Result<ImportReceipt, SettingsError> {
        let mut state = self.0.lock();
        self.require_token(&state, token)?;
        state.require_catalog_revision(catalog_revision)?;
        let Transaction::Preview { candidate, .. } = &state.transaction else {
            return Err(SettingsError::Stale);
        };
        let (candidate, installed) = install_themes(candidate, source)?;
        state.transaction = Transaction::Preview {
            id: token.id,
            candidate: Arc::new(candidate),
        };
        state.retire_catalog_revision();
        let receipt = ImportReceipt {
            installed,
            catalog_revision: state.catalog_revision,
        };
        self.0.notify();
        Ok(receipt)
    }

    /// Removes installed themes together, so one extension leaves the catalog in one step.
    pub(crate) fn remove_themes_preview(
        &self,
        token: &PreviewToken,
        catalog_revision: u64,
        ids: &[ThemeId],
    ) -> Result<u64, SettingsError> {
        let mut state = self.0.lock();
        self.require_token(&state, token)?;
        state.require_catalog_revision(catalog_revision)?;
        let Transaction::Preview { candidate, .. } = &state.transaction else {
            return Err(SettingsError::Stale);
        };
        let candidate = remove_themes(candidate, ids)?;
        state.transaction = Transaction::Preview {
            id: token.id,
            candidate: Arc::new(candidate),
        };
        state.retire_catalog_revision();
        let catalog_revision = state.catalog_revision;
        self.0.notify();
        Ok(catalog_revision)
    }

    #[cfg(any(test, feature = "developer-tools"))]
    pub(crate) fn export_document(&self) -> Result<String, SettingsError> {
        Ok(export_settings(&self.snapshot().candidate)?)
    }

    #[cfg(any(test, feature = "developer-tools"))]
    fn edit_preview(
        &self,
        token: &PreviewToken,
        edit: impl FnOnce(&mut SettingsDocument) -> Result<(), SettingsError>,
    ) -> Result<(), SettingsError> {
        let mut state = self.0.lock();
        self.require_token(&state, token)?;
        let Transaction::Preview { candidate, .. } = &state.transaction else {
            return Err(SettingsError::Stale);
        };
        let mut candidate = (**candidate).clone();
        edit(&mut candidate)?;
        let candidate = validate_candidate(candidate, token.revision)?;
        state.transaction = Transaction::Preview {
            id: token.id,
            candidate: Arc::new(candidate),
        };
        state.retire_catalog_revision();
        self.0.notify();
        Ok(())
    }

    fn prepare_direct_commit(
        &self,
        state: &mut State,
        candidate: SettingsDocument,
    ) -> Result<CommitJob, SettingsError> {
        let candidate = Arc::new(validate_candidate(candidate, state.committed.revision)?);
        let result = self.prepare_commit(state, None, Arc::clone(&candidate));
        if result.is_err() {
            state.recoverable_candidate = Some(candidate);
            self.0.notify();
        }
        result
    }

    /// Explicit reload preserves the last committed state on malformed or unsafe input.
    pub(crate) fn reload(&self) -> Result<(), SettingsError> {
        let mut state = self.0.lock();
        if !matches!(state.transaction, Transaction::Idle) {
            return Err(SettingsError::Busy);
        }
        state.require_catalog_revision(state.catalog_revision)?;
        let read = read_document(self.0.storage.as_ref());
        self.adopt(&mut state, read)
    }

    /// Adopts the file another program changed, returning whether SpaceTerm read a new document.
    /// A live preview or write owns the transaction, so the caller retries once it ends.
    pub(crate) fn follow_file(&self) -> Result<bool, SettingsError> {
        let mut state = self.0.lock();
        if !matches!(state.transaction, Transaction::Idle) {
            return Err(SettingsError::Busy);
        }
        state.require_catalog_revision(state.catalog_revision)?;
        let snapshot = self.0.storage.read();
        let unchanged = state.storage_ready
            && state.status.is_none()
            && match &snapshot {
                Ok(Some(snapshot)) => state.expected.as_ref() == Some(&snapshot.identity),
                Ok(None) => state.expected.is_none(),
                Err(_) => false,
            };
        if unchanged {
            return Ok(false);
        }
        let read = snapshot.map_err(SettingsError::from).and_then(|snapshot| {
            let Some(snapshot) = snapshot else {
                return Ok((SettingsDocument::default(), None));
            };
            Ok((parse_settings(&snapshot.bytes)?, Some(snapshot.identity)))
        });
        self.adopt(&mut state, read).map(|()| true)
    }

    /// Returns the write that creates the settings file when none exists. An existing unreadable
    /// file is left as it is.
    pub(crate) fn ensure_file(&self) -> Result<Option<CommitJob>, SettingsError> {
        let mut state = self.0.lock();
        if !matches!(state.transaction, Transaction::Idle) {
            return Err(SettingsError::Busy);
        }
        if state.expected.is_some() || !state.storage_ready {
            return Ok(None);
        }
        let committed = (*state.committed).clone();
        self.prepare_direct_commit(&mut state, committed).map(Some)
    }

    fn adopt(
        &self,
        state: &mut State,
        read: Result<(SettingsDocument, Option<SecureEntryIdentity>), SettingsError>,
    ) -> Result<(), SettingsError> {
        match read {
            Ok((mut document, identity)) => {
                // File revisions belong to another writer. A reload retires every locally
                // captured revision even when the external document reused the same number.
                document.revision = document.revision.max(
                    state
                        .committed
                        .revision
                        .checked_add(1)
                        .ok_or(SettingsError::RevisionExhausted)?,
                );
                state.committed = Arc::new(document);
                state.expected = identity;
                state.storage_ready = true;
                state.status = None;
                state.retire_catalog_revision();
                self.0.notify();
                Ok(())
            }
            Err(error) => {
                state.storage_ready = false;
                state.status = Some(error);
                self.0.notify();
                Err(error)
            }
        }
    }

    fn require_token(&self, state: &State, token: &PreviewToken) -> Result<(), SettingsError> {
        state.require_catalog_revision(state.catalog_revision)?;
        if !Weak::ptr_eq(&token.owner, &Arc::downgrade(&self.0))
            || token.revision != state.committed.revision
        {
            return Err(SettingsError::Stale);
        }
        match state.transaction {
            Transaction::Preview { id, .. } if id == token.id => Ok(()),
            Transaction::Committing { .. } => Err(SettingsError::Busy),
            _ => Err(SettingsError::Stale),
        }
    }

    fn prepare_commit(
        &self,
        state: &mut State,
        id: Option<u64>,
        candidate: Arc<SettingsDocument>,
    ) -> Result<CommitJob, SettingsError> {
        if !state.storage_ready {
            return Err(state
                .status
                .unwrap_or(SettingsError::Storage(StorageError::Conflict)));
        }
        let mut candidate = (*candidate).clone();
        candidate.revision = state
            .committed
            .revision
            .checked_add(1)
            .ok_or(SettingsError::RevisionExhausted)?;
        let bytes = export_settings(&candidate)?.into_bytes();
        let candidate = Arc::new(candidate);
        state.transaction = Transaction::Committing {
            id,
            candidate: Arc::clone(&candidate),
            owner_alive: true,
        };
        self.0.notify();
        Ok(CommitJob {
            owner: Arc::clone(&self.0),
            candidate,
            bytes,
            expected: state.expected.clone(),
            completed: false,
        })
    }
}

impl State {
    fn require_catalog_revision(&self, revision: u64) -> Result<(), SettingsError> {
        if self.catalog_revision == u64::MAX {
            return Err(SettingsError::RevisionExhausted);
        }
        if revision != self.catalog_revision {
            return Err(SettingsError::Stale);
        }
        Ok(())
    }

    fn retire_catalog_revision(&mut self) {
        // Drop cannot return an error. Saturation retires the last revision and blocks edits.
        self.catalog_revision = self.catalog_revision.saturating_add(1);
    }

    fn require_idle(&self, revision: u64) -> Result<(), SettingsError> {
        if !matches!(self.transaction, Transaction::Idle) {
            return Err(SettingsError::Busy);
        }
        if self.committed.revision != revision {
            return Err(SettingsError::Stale);
        }
        self.require_catalog_revision(self.catalog_revision)?;
        Ok(())
    }
}

/// Owns a started write even after the originating preview/window is destroyed.
pub(crate) struct CommitJob {
    owner: Arc<SettingsInner>,
    candidate: Arc<SettingsDocument>,
    bytes: Vec<u8>,
    expected: Option<SecureEntryIdentity>,
    completed: bool,
}

impl CommitJob {
    pub(crate) fn run(mut self) -> Result<CommitOutcome, SettingsError> {
        let result = self
            .owner
            .storage
            .write(&self.bytes, self.expected.as_ref());
        let mut state = self.owner.lock();
        let outcome = match result {
            Ok(commit) => {
                let reload_required = commit.identity.is_none();
                state.committed = Arc::clone(&self.candidate);
                state.recoverable_candidate = None;
                state.expected = commit.identity;
                state.storage_ready = !reload_required;
                state.status =
                    reload_required.then_some(SettingsError::Storage(StorageError::Conflict));
                state.transaction = Transaction::Idle;
                state.retire_catalog_revision();
                Ok(CommitOutcome {
                    revision: self.candidate.revision,
                    durability: commit.durability,
                    reload_required,
                })
            }
            Err(error) => {
                state.status = Some(SettingsError::Storage(error));
                if error == StorageError::Conflict {
                    state.storage_ready = false;
                }
                restore_preview_after_failure(&mut state);
                Err(SettingsError::Storage(error))
            }
        };
        self.completed = true;
        self.owner.notify();
        outcome
    }
}

impl Drop for CommitJob {
    fn drop(&mut self) {
        if !self.completed {
            restore_preview_after_failure(&mut self.owner.lock());
            self.owner.notify();
        }
    }
}

fn restore_preview_after_failure(state: &mut State) {
    let transaction = std::mem::replace(&mut state.transaction, Transaction::Idle);
    state.retire_catalog_revision();
    if let Transaction::Committing {
        id,
        candidate,
        owner_alive,
    } = transaction
    {
        let mut candidate = (*candidate).clone();
        candidate.revision = state.committed.revision;
        let candidate = Arc::new(candidate);
        match id {
            Some(id) if owner_alive => {
                state.transaction = Transaction::Preview { id, candidate };
            }
            None => state.recoverable_candidate = Some(candidate),
            Some(_) => {}
        }
    }
}

fn install_themes(
    document: &SettingsDocument,
    source: ThemeImport<'_>,
) -> Result<(SettingsDocument, Vec<ThemeId>), SettingsError> {
    let themes = source.translate()?;
    let retired = source.retired(document);
    let mut catalog = ThemeCatalog::from_terminal_themes(&document.terminal_themes)?;
    let installed = catalog.install_batch(&themes, catalog.revision(), &retired)?;
    let mut candidate = document.clone();
    candidate
        .terminal_themes
        .retain(|theme| !retired.contains(&theme.id) && !installed.contains(&theme.id));
    candidate.terminal_themes.extend(themes);
    candidate.select_builtin_for_missing_themes();
    Ok((validate_candidate(candidate, document.revision)?, installed))
}

fn remove_themes(
    document: &SettingsDocument,
    ids: &[ThemeId],
) -> Result<SettingsDocument, SettingsError> {
    if ids.iter().any(ThemeId::is_reserved) {
        return Err(CatalogError::ReservedId.into());
    }
    let installed = |id: &ThemeId| document.terminal_themes.iter().any(|theme| &theme.id == id);
    if ids.is_empty() || !ids.iter().all(installed) {
        return Err(CatalogError::UnknownTheme.into());
    }
    let mut candidate = document.clone();
    candidate
        .terminal_themes
        .retain(|theme| !ids.contains(&theme.id));
    candidate.select_builtin_for_missing_themes();
    validate_candidate(candidate, document.revision)
}

fn validate_candidate(
    candidate: SettingsDocument,
    revision: u64,
) -> Result<SettingsDocument, SettingsError> {
    if candidate.revision != revision {
        return Err(SettingsError::Stale);
    }
    // Serialization and import use the same strict contract, including catalog/selection checks.
    let bytes = export_settings(&candidate)?;
    Ok(parse_settings(bytes.as_bytes())?)
}

fn read_document(
    storage: &dyn SettingsStorage,
) -> Result<(SettingsDocument, Option<SecureEntryIdentity>), SettingsError> {
    let Some(snapshot) = storage.read()? else {
        return Ok((SettingsDocument::default(), None));
    };
    Ok((parse_settings(&snapshot.bytes)?, Some(snapshot.identity)))
}
