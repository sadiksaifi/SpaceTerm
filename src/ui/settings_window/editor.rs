//! Instant-apply editing over the retained Settings document.
//!
//! Every change previews live and commits shortly after the last change, so the Settings Window has
//! no unsaved state and needs no save or cancel action. The editor owns the authoritative draft
//! because [`UserSettings`] refuses a preview update while a commit is in flight and retires a
//! token once the committed revision moves; keeping the draft here means an edit made during either
//! window is re-pushed rather than lost.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use gpui::{Context, Task};

use crate::appearance::{
    AppearanceDocument, ResetTarget, SchemeCatalog, SchemeId, SchemeKind, SchemeSummary,
};
use crate::settings::storage::StorageError;
use crate::settings::{
    CommitOutcome, ImportReceipt, PreviewToken, SchemeImport, SettingsError, UserSettings,
};

use super::SettingsWindow;

/// How long the editor waits for the next change before writing.
///
/// Long enough that holding a stepper or dragging through a list produces one write, short enough
/// that a change feels saved by the time attention moves elsewhere.
pub(super) const COMMIT_DELAY: Duration = Duration::from_millis(500);

/// What the Settings Window reports about the retained document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SaveStatus {
    /// Every change reached the retained document.
    Saved,
    /// A change is previewing and has not been written yet.
    Saving,
    /// A write failed. The change is still previewing and can be retried.
    Failed(SettingsError),
    /// The document cannot be written until it is reloaded, so editing is refused.
    Unavailable(SettingsError),
}

pub(super) struct SettingsEditor {
    settings: UserSettings,
    draft: Arc<AppearanceDocument>,
    preview: Option<PreviewToken>,
    pending: Option<Task<()>>,
    status: SaveStatus,
    /// A draft change that could not reach the live preview and must be re-pushed.
    resync: bool,
    /// Retires a superseded scheduled write so a late completion cannot report stale status.
    generation: u64,
}

impl SettingsEditor {
    pub(super) fn new(settings: UserSettings) -> Self {
        let snapshot = settings.snapshot();
        let status = match snapshot.status {
            Some(error) => SaveStatus::Unavailable(error),
            None => SaveStatus::Saved,
        };
        Self {
            settings,
            draft: snapshot.candidate,
            preview: None,
            pending: None,
            status,
            resync: false,
            generation: 0,
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

    /// Applies one edit to the draft, previews it, and schedules the write.
    pub(super) fn edit(
        &mut self,
        edit: impl FnOnce(&mut AppearanceDocument),
        cx: &mut Context<SettingsWindow>,
    ) {
        if !self.editable() {
            return;
        }
        let mut draft = (*self.draft).clone();
        edit(&mut draft);
        if draft == *self.draft {
            return;
        }
        self.draft = Arc::new(draft);
        self.apply_preview();
        self.schedule(cx);
    }

    /// Restores one preference group or field to its default.
    pub(super) fn reset(&mut self, target: ResetTarget, cx: &mut Context<SettingsWindow>) {
        self.edit(
            |draft| {
                // A reset that the document rejects leaves the draft untouched, so the comparison
                // in `edit` discards it rather than scheduling an empty write.
                let _ = draft.reset(target);
            },
            cx,
        );
    }

    /// Installs parsed schemes without selecting any of them.
    pub(super) fn import(
        &mut self,
        source: SchemeImport<'_>,
        replace: &BTreeSet<SchemeId>,
        cx: &mut Context<SettingsWindow>,
    ) -> Result<ImportReceipt, SettingsError> {
        if !self.editable() {
            return Err(SettingsError::Busy);
        }
        // Import validates and installs against the live transaction, so the preview must hold the
        // draft before the catalog revision is captured.
        self.apply_preview();
        let token = self.preview.as_ref().ok_or(SettingsError::Busy)?;
        let catalog_revision = self.settings.snapshot().catalog_revision;
        let receipt = self
            .settings
            .import_preview(token, catalog_revision, source, replace)?;
        self.draft = self.settings.snapshot().candidate;
        self.schedule(cx);
        Ok(receipt)
    }

    pub(super) fn remove_custom_scheme(
        &mut self,
        id: &SchemeId,
        cx: &mut Context<SettingsWindow>,
    ) -> Result<(), SettingsError> {
        if !self.editable() {
            return Err(SettingsError::Busy);
        }
        self.apply_preview();
        let token = self.preview.as_ref().ok_or(SettingsError::Busy)?;
        let catalog_revision = self.settings.snapshot().catalog_revision;
        self.settings
            .remove_custom_scheme_preview(token, catalog_revision, id)?;
        self.draft = self.settings.snapshot().candidate;
        self.schedule(cx);
        Ok(())
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
        self.settings.export_document()
    }

    pub(super) fn export_schemes(
        &self,
        schemes: &[(SchemeKind, SchemeId)],
    ) -> Result<String, SettingsError> {
        self.settings.export_schemes(schemes)
    }

    pub(super) fn list_import_candidates(
        bytes: &[u8],
    ) -> Result<Vec<crate::appearance::ImportCandidate>, SettingsError> {
        UserSettings::list_import_candidates(bytes)
    }

    /// Writes any scheduled change immediately and synchronously.
    ///
    /// Called while the window is closing, when an asynchronous write would be cancelled with the
    /// window that owns its task. The document is one small file, so the pause is imperceptible and
    /// it is the only way a change made moments before closing survives.
    pub(super) fn flush(&mut self, cx: &mut Context<SettingsWindow>) {
        self.pending = None;
        if !self.has_unwritten_changes() {
            return;
        }
        match self.prepare_commit() {
            Ok(job) => {
                let generation = self.generation;
                let result = job.run();
                self.settle(generation, result, cx);
            }
            Err(error) => self.report(error, cx),
        }
    }

    /// Writes a change that a previous attempt could not.
    pub(super) fn retry(&mut self, cx: &mut Context<SettingsWindow>) {
        self.pending = None;
        self.start_commit(self.generation, cx);
    }

    /// Re-reads the retained document, discarding any live preview.
    pub(super) fn reload(&mut self, cx: &mut Context<SettingsWindow>) {
        self.pending = None;
        self.resync = false;
        if let Some(token) = self.preview.take() {
            let _ = self.settings.cancel_preview(&token);
        }
        match self.settings.reload() {
            Ok(()) => {
                self.draft = self.settings.snapshot().committed;
                self.status = SaveStatus::Saved;
            }
            Err(error) => self.status = SaveStatus::Unavailable(error),
        }
        cx.notify();
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

    fn has_unwritten_changes(&self) -> bool {
        self.resync
            || self.pending.is_some()
            || matches!(self.status, SaveStatus::Saving | SaveStatus::Failed(_))
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

    /// Replaces any scheduled write with a fresh one, which is the debounce.
    fn schedule(&mut self, cx: &mut Context<SettingsWindow>) {
        self.status = SaveStatus::Saving;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        // Assigning the field drops the previous task, cancelling its timer.
        self.pending = Some(cx.spawn(async move |window, cx| {
            cx.background_executor().timer(COMMIT_DELAY).await;
            let _ = window.update(cx, |window, cx| {
                window.editor.start_commit(generation, cx);
            });
        }));
        cx.notify();
    }

    fn start_commit(&mut self, generation: u64, cx: &mut Context<SettingsWindow>) {
        if generation != self.generation {
            return;
        }
        self.pending = None;
        match self.prepare_commit() {
            Ok(job) => {
                self.pending = Some(cx.spawn(async move |window, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { job.run() })
                        .await;
                    let _ = window.update(cx, |window, cx| {
                        window.editor.settle(generation, result, cx);
                    });
                }));
                cx.notify();
            }
            Err(SettingsError::Busy) => {
                // Another writer owns the transaction. Take the next turn instead of failing.
                self.schedule(cx);
            }
            Err(error) => self.report(error, cx),
        }
    }

    fn prepare_commit(&mut self) -> Result<crate::settings::CommitJob, SettingsError> {
        if self.resync {
            self.apply_preview();
        }
        match self.preview.as_ref() {
            Some(token) => self.settings.commit_preview(token),
            None => {
                let revision = self.settings.snapshot().committed.revision;
                self.settings
                    .update_committed(revision, (*self.draft).clone())
            }
        }
    }

    fn settle(
        &mut self,
        generation: u64,
        result: Result<CommitOutcome, SettingsError>,
        cx: &mut Context<SettingsWindow>,
    ) {
        self.pending = None;
        match result {
            Ok(outcome) if outcome.reload_required => {
                // The published document has no verifiable identity, so the next write could
                // replace content this session never read.
                self.preview = None;
                self.resync = false;
                self.status =
                    SaveStatus::Unavailable(SettingsError::Storage(StorageError::Conflict));
            }
            Ok(_) => {
                // The token is retired with the revision it captured. The transaction is already
                // idle, so releasing it cancels nothing.
                self.preview = None;
                self.draft = self.settings.snapshot().committed;
                if generation == self.generation && !self.resync {
                    self.status = SaveStatus::Saved;
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
            self.schedule(cx);
        }
        cx.notify();
    }

    fn report(&mut self, error: SettingsError, cx: &mut Context<SettingsWindow>) {
        self.status = match error {
            SettingsError::Storage(StorageError::Conflict) | SettingsError::Invalid => {
                SaveStatus::Unavailable(error)
            }
            _ => SaveStatus::Failed(error),
        };
        cx.notify();
    }
}

impl SaveStatus {
    /// Content-free wording for the footer. No path, no raw native error.
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::Saved => "All changes saved",
            Self::Saving => "Saving…",
            Self::Failed(_) => "Could not save your changes",
            Self::Unavailable(SettingsError::Storage(StorageError::Conflict)) => {
                "Your settings changed outside SpaceTerm"
            }
            Self::Unavailable(SettingsError::Storage(StorageError::Unsafe)) => {
                "Your settings file is not in a safe location"
            }
            Self::Unavailable(SettingsError::Storage(StorageError::TooLarge)) => {
                "Your settings file is too large to read"
            }
            Self::Unavailable(SettingsError::Invalid) => "Your settings could not be read",
            Self::Unavailable(_) => "Your settings are unavailable",
        }
    }

    /// The explanation a banner adds above the content, when one is warranted.
    pub(super) fn explanation(self) -> Option<&'static str> {
        match self {
            Self::Saved | Self::Saving => None,
            Self::Failed(_) => {
                Some("The change is still applied. Retry to write it to your settings file.")
            }
            Self::Unavailable(SettingsError::Storage(StorageError::Conflict)) => Some(
                "Reload to pick up the change. Editing is paused so SpaceTerm does not replace it.",
            ),
            Self::Unavailable(SettingsError::Invalid) => Some(
                "SpaceTerm is showing defaults and has changed nothing. Fix or remove the file, then reload.",
            ),
            Self::Unavailable(_) => {
                Some("Editing is paused until SpaceTerm can read and write your settings.")
            }
        }
    }
}
