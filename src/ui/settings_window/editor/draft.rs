//! Owns the Settings Window draft and preview transaction independently of its task executor.
//!
//! The scheduling Adapter supplies commit completion and whether it still belongs to the current
//! edit. This Module retains the draft, token, resynchronization policy, and save classification.
//!
//! The draft is authoritative because [`UserSettings`] refuses preview updates during a commit
//! and retires tokens when the committed revision moves. Edits remain here until they can reach
//! the preview transaction again.

use std::{collections::BTreeSet, sync::Arc};

use crate::appearance::{
    AppearanceDocument, ResetTarget, SchemeCatalog, SchemeId, SchemeKind, SchemeSummary,
};
use crate::settings::storage::StorageError;
use crate::settings::{
    CommitOutcome, ImportReceipt, PreviewToken, SchemeImport, SettingsError, UserSettings,
};

/// What the Settings Window reports about the retained document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui::settings_window) enum SaveStatus {
    /// Every change reached the retained document.
    Saved,
    /// A change is previewing and has not been written yet.
    Saving,
    /// A write failed. The change is still previewing and can be retried.
    Failed(SettingsError),
    /// The document cannot be written until it is reloaded, so editing is refused.
    Unavailable(SettingsError),
}

pub(super) struct SettingsDraft {
    settings: UserSettings,
    draft: Arc<AppearanceDocument>,
    preview: Option<PreviewToken>,
    status: SaveStatus,
    unwritten: bool,
    /// A draft change that could not reach the live preview and must be re-pushed.
    resync: bool,
}

impl SettingsDraft {
    pub(super) fn new(settings: UserSettings) -> Self {
        let snapshot = settings.snapshot();
        let status = match snapshot.status {
            Some(error) => SaveStatus::Unavailable(error),
            None => SaveStatus::Saved,
        };
        Self {
            settings,
            draft: snapshot.committed,
            preview: None,
            status,
            unwritten: false,
            resync: false,
        }
    }

    /// The values every control renders from.
    pub(super) fn document(&self) -> &AppearanceDocument {
        &self.draft
    }

    /// Whether controls may request changes.
    ///
    /// A document that could not be read or that changed underneath SpaceTerm is not editable: the
    /// first write would replace content this session never saw.
    pub(super) fn editable(&self) -> bool {
        !matches!(self.status, SaveStatus::Unavailable(_))
    }

    pub(super) fn status(&self) -> SaveStatus {
        self.status
    }

    /// Applies and previews one edit, returning whether the Adapter should schedule a write.
    pub(super) fn edit(&mut self, edit: impl FnOnce(&mut AppearanceDocument)) -> bool {
        if !self.editable() {
            return false;
        }
        let mut draft = (*self.draft).clone();
        edit(&mut draft);
        if draft == *self.draft {
            return false;
        }
        self.draft = Arc::new(draft);
        self.apply_preview();
        self.unwritten = true;
        self.status = SaveStatus::Saving;
        true
    }

    /// Restores one preference group or field to its default.
    pub(super) fn reset(&mut self, target: ResetTarget) -> bool {
        self.edit(|draft| {
            // A reset that the document rejects leaves the draft untouched, so the comparison
            // in `edit` discards it rather than scheduling an empty write.
            let _ = draft.reset(target);
        })
    }

    /// Installs parsed schemes without selecting any of them.
    pub(super) fn import(
        &mut self,
        source: SchemeImport<'_>,
        replace: &BTreeSet<SchemeId>,
    ) -> Result<ImportReceipt, SettingsError> {
        self.edit_catalog(|settings, token, revision| {
            settings.import_preview(token, revision, source, replace)
        })
    }

    pub(super) fn remove_custom_scheme(&mut self, id: &SchemeId) -> Result<(), SettingsError> {
        self.edit_catalog(|settings, token, revision| {
            settings
                .remove_custom_scheme_preview(token, revision, id)
                .map(|_| ())
        })
    }

    fn edit_catalog<T>(
        &mut self,
        edit: impl FnOnce(&UserSettings, &PreviewToken, u64) -> Result<T, SettingsError>,
    ) -> Result<T, SettingsError> {
        if !self.editable() {
            return Err(SettingsError::Busy);
        }
        let owns_changes = self.unwritten || self.preview.is_some();
        // Validate catalog changes against the authoritative draft before capturing its revision.
        self.apply_preview();
        let result = self
            .preview
            .as_ref()
            .ok_or(SettingsError::Busy)
            .and_then(|token| {
                edit(
                    &self.settings,
                    token,
                    self.settings.snapshot().catalog_revision,
                )
            });
        match result {
            Ok(value) => {
                self.draft = self.settings.snapshot().candidate;
                self.unwritten = true;
                self.status = SaveStatus::Saving;
                Ok(value)
            }
            Err(error) => {
                if !owns_changes {
                    // Rejection from an idle draft must not reserve the shared transaction. A
                    // previously owned preview or pending edit remains intact on the same error.
                    if let Some(token) = self.preview.take() {
                        let _ = self.settings.cancel_preview(&token);
                    }
                    self.resync = false;
                    self.draft = self.settings.snapshot().committed;
                }
                Err(error)
            }
        }
    }

    /// Lists one kind's selectable schemes from the draft, so a freshly imported scheme appears
    /// before it has been written.
    pub(super) fn scheme_summaries(
        &self,
        kind: SchemeKind,
    ) -> Result<Vec<SchemeSummary>, SettingsError> {
        Ok(SchemeCatalog::from_custom_schemes(&self.draft.custom_schemes)?.summaries(kind))
    }

    pub(super) fn export_document(&self) -> Result<String, SettingsError> {
        Ok(crate::appearance::export_settings(&self.draft)?)
    }

    pub(super) fn export_appearance(
        &self,
        resolved: &crate::appearance::ResolvedAppearance,
    ) -> Result<String, SettingsError> {
        let catalog = SchemeCatalog::from_custom_schemes(&self.draft.custom_schemes)?;
        Ok(crate::appearance::export_resolved_schemes(
            &catalog, resolved,
        )?)
    }

    pub(super) fn export_definitions(
        &self,
        schemes: &[(SchemeKind, SchemeId)],
    ) -> Result<String, SettingsError> {
        let catalog = SchemeCatalog::from_custom_schemes(&self.draft.custom_schemes)?;
        Ok(crate::appearance::export_schemes(&catalog, schemes)?)
    }

    pub(super) fn list_import_candidates(
        bytes: &[u8],
    ) -> Result<Vec<crate::appearance::ImportCandidate>, SettingsError> {
        UserSettings::list_import_candidates(bytes)
    }

    /// Re-reads the retained document, discarding any live preview.
    pub(super) fn reload(&mut self) {
        self.resync = false;
        if let Some(token) = self.preview.take() {
            let _ = self.settings.cancel_preview(&token);
        }
        match self.settings.reload() {
            Ok(()) => {
                self.draft = self.settings.snapshot().committed;
                self.unwritten = false;
                self.status = SaveStatus::Saved;
            }
            Err(error) => self.status = SaveStatus::Unavailable(error),
        }
    }

    /// Adopts a committed document this editor did not write.
    ///
    /// Another surface may commit while the window is open. With nothing of its own outstanding,
    /// the window should present what is retained rather than a stale draft.
    pub(super) fn synchronize(&mut self) {
        if self.has_unwritten_changes() || self.preview.is_some() {
            return;
        }
        let committed = self.settings.snapshot().committed;
        if committed.revision != self.draft.revision {
            self.draft = committed;
        }
    }

    pub(super) fn has_unwritten_changes(&self) -> bool {
        self.unwritten
    }

    /// Pushes the draft into the live preview so every window repaints at once.
    fn apply_preview(&mut self) {
        let committed_revision = self.settings.snapshot().committed.revision;
        let mut candidate = (*self.draft).clone();
        // The revision is the document's own write bookkeeping, not a preference, so a draft
        // always rebases onto whatever is committed now.
        candidate.revision = committed_revision;
        self.draft = Arc::new(candidate.clone());
        if self.preview.is_none() {
            match self.settings.begin_preview(committed_revision) {
                Ok(token) => self.preview = Some(token),
                Err(_) => {
                    self.resync = true;
                    return;
                }
            }
        }
        let token = self
            .preview
            .as_ref()
            .expect("a token is held or was just established");
        match self.settings.update_preview(token, candidate.clone()) {
            Ok(()) => self.resync = false,
            Err(SettingsError::Stale) => {
                // The committed revision moved underneath the token. Retire it and take one more
                // turn rather than looping against a transaction we do not own.
                self.preview = None;
                self.resync = true;
                let revision = self.settings.snapshot().committed.revision;
                if let Ok(token) = self.settings.begin_preview(revision) {
                    let mut candidate = candidate;
                    candidate.revision = revision;
                    if self
                        .settings
                        .update_preview(&token, candidate.clone())
                        .is_ok()
                    {
                        self.draft = Arc::new(candidate);
                        self.preview = Some(token);
                        self.resync = false;
                    }
                }
            }
            Err(_) => self.resync = true,
        }
    }

    pub(super) fn prepare_commit(&mut self) -> Result<crate::settings::CommitJob, SettingsError> {
        if self.resync {
            self.apply_preview();
        }
        let result = match self.preview.as_ref() {
            Some(token) => self.settings.commit_preview(token),
            None => {
                let revision = self.settings.snapshot().committed.revision;
                self.settings
                    .update_committed(revision, (*self.draft).clone())
            }
        };
        // A busy transaction defers the write. A retry that starts successfully keeps its previous
        // failure visible until completion, matching the existing Settings Window feedback.
        if matches!(result, Err(SettingsError::Busy)) {
            self.status = SaveStatus::Saving;
        }
        result
    }

    /// Incorporates a completed write, returning whether the draft needs another scheduled write.
    pub(super) fn settle(
        &mut self,
        current_generation: bool,
        result: Result<CommitOutcome, SettingsError>,
    ) -> bool {
        match result {
            Ok(outcome) if outcome.reload_required => {
                // The published document has no verifiable identity, so the next write could
                // replace content this session never read.
                self.preview = None;
                self.unwritten = !current_generation || self.resync;
                if !self.unwritten {
                    self.draft = self.settings.snapshot().committed;
                }
                self.resync = false;
                self.status =
                    SaveStatus::Unavailable(SettingsError::Storage(StorageError::Conflict));
            }
            Ok(_) => {
                if current_generation && !self.resync {
                    self.preview = None;
                    self.draft = self.settings.snapshot().committed;
                    self.unwritten = false;
                    self.status = SaveStatus::Saved;
                } else {
                    // A newer edit may already own a fresh preview. Preserve both its document
                    // and token; apply_preview can retire an old token if it is still held.
                    self.resync = true;
                }
            }
            Err(error) => {
                // A failed commit restores the preview for a live owner, so the change stays
                // visible and the retained token can carry the retry.
                self.status = match error {
                    SettingsError::Storage(StorageError::Conflict) => {
                        self.preview = None;
                        SaveStatus::Unavailable(error)
                    }
                    _ => SaveStatus::Failed(error),
                };
            }
        }
        if self.resync && self.editable() {
            self.apply_preview();
            self.status = SaveStatus::Saving;
            return true;
        }
        false
    }

    pub(super) fn report(&mut self, error: SettingsError) {
        self.status = match error {
            SettingsError::Storage(StorageError::Conflict) | SettingsError::Invalid => {
                SaveStatus::Unavailable(error)
            }
            _ => SaveStatus::Failed(error),
        };
    }
}

#[cfg(test)]
mod tests;
