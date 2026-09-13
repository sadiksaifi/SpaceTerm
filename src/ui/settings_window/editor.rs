//! Schedules instant-apply Settings edits on the Settings Window's GPUI executor.
//!
//! Draft and preview ownership live in the framework-independent draft Module. This Adapter owns
//! debounce cancellation, background execution, synchronous close flushing, and view notification.

mod draft;

use std::{collections::BTreeSet, time::Duration};

use gpui::{Context, Task};

use crate::appearance::{AppearanceDocument, ResetTarget, SchemeId, SchemeKind, SchemeSummary};
use crate::settings::storage::StorageError;
use crate::settings::{CommitOutcome, ImportReceipt, SchemeImport, SettingsError, UserSettings};

use super::SettingsWindow;
pub(super) use draft::SaveStatus;
use draft::SettingsDraft;

/// How long the editor waits for the next change before writing.
///
/// Long enough that holding a stepper or dragging through a list produces one write, short enough
/// that a change feels saved by the time attention moves elsewhere.
pub(super) const COMMIT_DELAY: Duration = Duration::from_millis(500);

pub(super) struct SettingsEditor {
    draft: SettingsDraft,
    pending: Option<Task<()>>,
    /// Retires a superseded scheduled write so a late completion cannot report stale status.
    generation: u64,
}

impl SettingsEditor {
    pub(super) fn new(settings: UserSettings) -> Self {
        Self {
            draft: SettingsDraft::new(settings),
            pending: None,
            generation: 0,
        }
    }

    pub(super) fn document(&self) -> &AppearanceDocument {
        self.draft.document()
    }

    pub(super) fn editable(&self) -> bool {
        self.draft.editable()
    }

    pub(super) fn status(&self) -> SaveStatus {
        self.draft.status()
    }

    pub(super) fn edit(
        &mut self,
        edit: impl FnOnce(&mut AppearanceDocument),
        cx: &mut Context<SettingsWindow>,
    ) {
        if self.draft.edit(edit) {
            self.schedule(cx);
        }
    }

    pub(super) fn reset(&mut self, target: ResetTarget, cx: &mut Context<SettingsWindow>) {
        if self.draft.reset(target) {
            self.schedule(cx);
        }
    }

    pub(super) fn import(
        &mut self,
        source: SchemeImport<'_>,
        replace: &BTreeSet<SchemeId>,
        cx: &mut Context<SettingsWindow>,
    ) -> Result<ImportReceipt, SettingsError> {
        let receipt = self.draft.import(source, replace)?;
        self.schedule(cx);
        Ok(receipt)
    }

    pub(super) fn remove_custom_scheme(
        &mut self,
        id: &SchemeId,
        cx: &mut Context<SettingsWindow>,
    ) -> Result<(), SettingsError> {
        self.draft.remove_custom_scheme(id)?;
        self.schedule(cx);
        Ok(())
    }

    pub(super) fn scheme_summaries(
        &self,
        kind: SchemeKind,
    ) -> Result<Vec<SchemeSummary>, SettingsError> {
        self.draft.scheme_summaries(kind)
    }

    pub(super) fn export_document(&self) -> Result<String, SettingsError> {
        self.draft.export_document()
    }

    pub(super) fn export_schemes(
        &self,
        schemes: &[(SchemeKind, SchemeId)],
    ) -> Result<String, SettingsError> {
        self.draft.export_schemes(schemes)
    }

    pub(super) fn list_import_candidates(
        bytes: &[u8],
    ) -> Result<Vec<crate::appearance::ImportCandidate>, SettingsError> {
        SettingsDraft::list_import_candidates(bytes)
    }

    /// Writes any scheduled change immediately and synchronously.
    ///
    /// Called while the window is closing, when an asynchronous write would be cancelled with the
    /// window that owns its task. The document is one small file, so the pause is imperceptible and
    /// it is the only way a change made moments before closing survives.
    pub(super) fn flush(&mut self, cx: &mut Context<SettingsWindow>) {
        self.pending = None;
        if !self.draft.has_unwritten_changes() {
            return;
        }
        match self.draft.prepare_commit() {
            Ok(job) => {
                let generation = self.generation;
                let result = job.run();
                self.settle(generation, result, cx);
            }
            Err(error) => {
                self.draft.report(error);
                cx.notify();
            }
        }
    }

    /// Writes a change that a previous attempt could not.
    pub(super) fn retry(&mut self, cx: &mut Context<SettingsWindow>) {
        self.pending = None;
        self.start_commit(self.generation, cx);
    }

    pub(super) fn reload(&mut self, cx: &mut Context<SettingsWindow>) {
        self.pending = None;
        self.draft.reload();
        cx.notify();
    }

    pub(super) fn synchronize(&mut self) {
        if self.pending.is_none() {
            self.draft.synchronize();
        }
    }

    /// Replaces any scheduled write with a fresh one, which is the debounce.
    fn schedule(&mut self, cx: &mut Context<SettingsWindow>) {
        self.draft.mark_saving();
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
        match self.draft.prepare_commit() {
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
            Err(error) => {
                self.draft.report(error);
                cx.notify();
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
        if self.draft.settle(generation == self.generation, result) {
            self.schedule(cx);
        }
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
