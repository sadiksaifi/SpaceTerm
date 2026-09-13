//! Owns committed preferences, preview lifetime, and serialized identity-aware writes.

#![cfg_attr(
    not(any(test, feature = "appearance-exerciser")),
    allow(
        dead_code,
        reason = "the Settings Window edits through a draft and the preview transaction, so the direct-commit, field-reset, and recoverable-candidate operations remain available but unused"
    )
)]

pub(crate) mod storage;
#[cfg(test)]
mod storage_tests;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, MutexGuard, Weak},
};

use crate::appearance::{
    AppearanceDocument, AppearanceDocumentError, CatalogError, CustomScheme, ImportCandidate,
    ImportError, ResetTarget, SchemeCatalog, SchemeId, SchemeKind, ZedImportKind, export_settings,
    parse_settings,
};
use crate::platform::secure_filesystem::SecureEntryIdentity;
use storage::{Durability, SettingsStorage, StorageError};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum SettingsError {
    #[error("appearance settings are busy")]
    Busy,
    #[error("appearance settings revision is stale")]
    Stale,
    #[error("appearance settings revision is exhausted")]
    RevisionExhausted,
    #[error("appearance settings are invalid")]
    Invalid,
    #[error(transparent)]
    Import(#[from] ImportError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

impl From<AppearanceDocumentError> for SettingsError {
    fn from(_: AppearanceDocumentError) -> Self {
        Self::Invalid
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreviewPhase {
    Idle,
    Previewing,
    Committing,
}

#[derive(Clone)]
pub(crate) struct SettingsSnapshot {
    pub(crate) committed: Arc<AppearanceDocument>,
    pub(crate) candidate: Arc<AppearanceDocument>,
    /// A failed direct save remains recoverable without applying it as a live preview.
    pub(crate) recoverable_candidate: Option<Arc<AppearanceDocument>>,
    /// Retires import/replacement plans whenever the live editing state changes.
    pub(crate) catalog_revision: u64,
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
    pub(crate) installed: Vec<SchemeId>,
    pub(crate) catalog_revision: u64,
}

/// Clones share the same transaction and storage authority.
#[derive(Clone)]
pub(crate) struct UserSettings(Arc<SettingsInner>);

struct SettingsInner {
    state: Mutex<State>,
    storage: Arc<dyn SettingsStorage>,
    changed: async_channel::Sender<()>,
}

struct State {
    committed: Arc<AppearanceDocument>,
    recoverable_candidate: Option<Arc<AppearanceDocument>>,
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
        candidate: Arc<AppearanceDocument>,
    },
    Committing {
        id: Option<u64>,
        candidate: Arc<AppearanceDocument>,
        owner_alive: bool,
    },
}

/// Dropping the last lease cancels a preview; a started commit owns its immutable candidate.
pub(crate) struct PreviewToken {
    owner: Weak<SettingsInner>,
    id: u64,
    revision: u64,
}

/// Parsing is bounded and color-only. Installing never selects the imported schemes.
pub(crate) enum SchemeImport<'a> {
    SpaceTerm(&'a [u8]),
    Zed {
        bytes: &'a [u8],
        candidate_index: usize,
        kinds: &'a [ZedImportKind],
    },
}

impl SchemeImport<'_> {
    fn parse(self) -> Result<Vec<CustomScheme>, ImportError> {
        match self {
            Self::SpaceTerm(bytes) => Ok(crate::appearance::parse_color_document(bytes)?.schemes),
            Self::Zed {
                bytes,
                candidate_index,
                kinds,
            } => crate::appearance::import_zed(bytes, candidate_index, kinds),
        }
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
        let _ = self.changed.try_send(());
    }
}

impl UserSettings {
    /// Loads without creating a missing file or replacing an invalid one.
    pub(crate) fn load(storage: Arc<dyn SettingsStorage>) -> (Self, async_channel::Receiver<()>) {
        let (changed, receiver) = async_channel::bounded(1);
        let mut state = State {
            committed: Arc::new(AppearanceDocument::default()),
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
        (
            Self(Arc::new(SettingsInner {
                state: Mutex::new(state),
                storage,
                changed,
            })),
            receiver,
        )
    }

    pub(crate) fn snapshot(&self) -> SettingsSnapshot {
        let state = self.0.lock();
        let (candidate, phase) = match &state.transaction {
            Transaction::Idle => (Arc::clone(&state.committed), PreviewPhase::Idle),
            Transaction::Preview { candidate, .. } => {
                (Arc::clone(candidate), PreviewPhase::Previewing)
            }
            Transaction::Committing { id, candidate, .. } => (
                if id.is_some() {
                    Arc::clone(candidate)
                } else {
                    Arc::clone(&state.committed)
                },
                PreviewPhase::Committing,
            ),
        };
        SettingsSnapshot {
            committed: Arc::clone(&state.committed),
            candidate,
            recoverable_candidate: state.recoverable_candidate.clone(),
            catalog_revision: state.catalog_revision,
            phase,
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
        candidate: AppearanceDocument,
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
        candidate: AppearanceDocument,
    ) -> Result<CommitJob, SettingsError> {
        let mut state = self.0.lock();
        state.require_idle(revision)?;
        self.prepare_direct_commit(&mut state, candidate)
    }

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

    #[allow(
        dead_code,
        reason = "direct settings operations are available without a preview UI"
    )]
    pub(crate) fn reset_committed(
        &self,
        revision: u64,
        target: ResetTarget,
    ) -> Result<CommitJob, SettingsError> {
        let mut state = self.0.lock();
        state.require_idle(revision)?;
        let mut candidate = (*state.committed).clone();
        candidate.reset(target)?;
        self.prepare_direct_commit(&mut state, candidate)
    }

    pub(crate) fn import_preview(
        &self,
        token: &PreviewToken,
        catalog_revision: u64,
        source: SchemeImport<'_>,
        replace: &BTreeSet<SchemeId>,
    ) -> Result<ImportReceipt, SettingsError> {
        let mut state = self.0.lock();
        self.require_token(&state, token)?;
        state.require_catalog_revision(catalog_revision)?;
        let Transaction::Preview { candidate, .. } = &state.transaction else {
            return Err(SettingsError::Stale);
        };
        let (candidate, installed) = install_schemes(candidate, source.parse()?, replace)?;
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

    /// The receipt describes the installed candidate and becomes authoritative only if the
    /// paired commit job succeeds.
    #[allow(
        dead_code,
        reason = "direct settings operations are available without a preview UI"
    )]
    pub(crate) fn import_committed(
        &self,
        revision: u64,
        catalog_revision: u64,
        source: SchemeImport<'_>,
        replace: &BTreeSet<SchemeId>,
    ) -> Result<(ImportReceipt, CommitJob), SettingsError> {
        let mut state = self.0.lock();
        state.require_idle(revision)?;
        state.require_catalog_revision(catalog_revision)?;
        let (candidate, installed) = install_schemes(&state.committed, source.parse()?, replace)?;
        let resulting_catalog_revision = state
            .catalog_revision
            .checked_add(1)
            .ok_or(SettingsError::RevisionExhausted)?;
        let job = self.prepare_direct_commit(&mut state, candidate)?;
        Ok((
            ImportReceipt {
                installed,
                catalog_revision: resulting_catalog_revision,
            },
            job,
        ))
    }

    pub(crate) fn remove_custom_scheme_preview(
        &self,
        token: &PreviewToken,
        catalog_revision: u64,
        id: &SchemeId,
    ) -> Result<u64, SettingsError> {
        let mut state = self.0.lock();
        self.require_token(&state, token)?;
        state.require_catalog_revision(catalog_revision)?;
        let Transaction::Preview { candidate, .. } = &state.transaction else {
            return Err(SettingsError::Stale);
        };
        let candidate = remove_custom_scheme(candidate, id)?;
        state.transaction = Transaction::Preview {
            id: token.id,
            candidate: Arc::new(candidate),
        };
        state.retire_catalog_revision();
        let catalog_revision = state.catalog_revision;
        self.0.notify();
        Ok(catalog_revision)
    }

    #[allow(
        dead_code,
        reason = "direct custom scheme deletion is available without a preview UI"
    )]
    pub(crate) fn remove_custom_scheme_committed(
        &self,
        revision: u64,
        catalog_revision: u64,
        id: &SchemeId,
    ) -> Result<CommitJob, SettingsError> {
        let mut state = self.0.lock();
        state.require_idle(revision)?;
        state.require_catalog_revision(catalog_revision)?;
        let candidate = remove_custom_scheme(&state.committed, id)?;
        self.prepare_direct_commit(&mut state, candidate)
    }

    #[allow(
        dead_code,
        reason = "complete scheme listing remains available to interchange surfaces"
    )]
    pub(crate) fn list_schemes(&self) -> Result<Vec<CustomScheme>, SettingsError> {
        let snapshot = self.snapshot();
        Ok(SchemeCatalog::from_custom_schemes(&snapshot.candidate.custom_schemes)?.schemes())
    }

    pub(crate) fn list_import_candidates(
        bytes: &[u8],
    ) -> Result<Vec<ImportCandidate>, SettingsError> {
        Ok(crate::appearance::list_zed_candidates(bytes)?)
    }

    pub(crate) fn export_schemes(
        &self,
        schemes: &[(SchemeKind, SchemeId)],
    ) -> Result<String, SettingsError> {
        let snapshot = self.snapshot();
        let catalog = SchemeCatalog::from_custom_schemes(&snapshot.candidate.custom_schemes)?;
        Ok(crate::appearance::export_schemes(&catalog, schemes)?)
    }

    pub(crate) fn export_document(&self) -> Result<String, SettingsError> {
        Ok(export_settings(&self.snapshot().candidate)?)
    }

    fn edit_preview(
        &self,
        token: &PreviewToken,
        edit: impl FnOnce(&mut AppearanceDocument) -> Result<(), SettingsError>,
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
        candidate: AppearanceDocument,
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
        match read_document(self.0.storage.as_ref()) {
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
        candidate: Arc<AppearanceDocument>,
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
    candidate: Arc<AppearanceDocument>,
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

fn install_schemes(
    document: &AppearanceDocument,
    schemes: Vec<CustomScheme>,
    replace: &BTreeSet<SchemeId>,
) -> Result<(AppearanceDocument, Vec<SchemeId>), SettingsError> {
    let mut catalog = SchemeCatalog::from_custom_schemes(&document.custom_schemes)?;
    let installed = catalog.install_batch(&schemes, catalog.revision(), replace)?;
    let mut candidate = document.clone();
    candidate
        .custom_schemes
        .retain(|scheme| !replace.contains(scheme.id()));
    candidate.custom_schemes.extend(schemes);
    Ok((validate_candidate(candidate, document.revision)?, installed))
}

#[allow(
    dead_code,
    reason = "shared validation for the optional preview and direct deletion operations"
)]
fn remove_custom_scheme(
    document: &AppearanceDocument,
    id: &SchemeId,
) -> Result<AppearanceDocument, SettingsError> {
    if id.is_reserved() {
        return Err(CatalogError::ReservedId.into());
    }
    let mut candidate = document.clone();
    let before = candidate.custom_schemes.len();
    candidate.custom_schemes.retain(|scheme| scheme.id() != id);
    if candidate.custom_schemes.len() == before {
        return Err(CatalogError::UnknownReplacement.into());
    }
    validate_candidate(candidate, document.revision)
}

fn validate_candidate(
    candidate: AppearanceDocument,
    revision: u64,
) -> Result<AppearanceDocument, SettingsError> {
    if candidate.revision != revision {
        return Err(SettingsError::Stale);
    }
    // Serialization and import use the same strict contract, including catalog/selection checks.
    let bytes = export_settings(&candidate)?;
    Ok(parse_settings(bytes.as_bytes())?)
}

fn read_document(
    storage: &dyn SettingsStorage,
) -> Result<(AppearanceDocument, Option<SecureEntryIdentity>), SettingsError> {
    let Some(snapshot) = storage.read()? else {
        return Ok((AppearanceDocument::default(), None));
    };
    Ok((parse_settings(&snapshot.bytes)?, Some(snapshot.identity)))
}
