//! Schedules instant-apply Settings edits on the Settings Window's GPUI executor.
//! The executor-independent `draft` Module owns the draft and preview transaction.

mod draft;

#[cfg(test)]
use std::future::Future as _;

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use gpui::{Context, Task};

use crate::appearance::{ResetTarget, ThemeId, ThemeSummary};
use crate::settings::SettingsDocument;
use crate::settings::recovery::RecoveryError;
use crate::settings::storage::StorageError;
use crate::settings::{CommitOutcome, ImportReceipt, Settings, SettingsError, ThemeImport};

use super::SettingsWindow;
pub(super) use draft::SaveStatus;
use draft::SettingsDraft;

/// How long the editor waits for the next change before writing, so holding a stepper produces one
/// write.
pub(super) const COMMIT_DELAY: Duration = Duration::from_millis(500);

pub(super) struct SettingsEditor {
    draft: SettingsDraft,
    pending: Option<Task<()>>,
    in_flight: Option<ActiveCommit>,
    /// Retires a superseded scheduled write so a late completion cannot report stale status.
    generation: u64,
    /// Advances when the draft's Background Image changes, or when a reset, reload, or recovery
    /// replaces the draft, so a choice begun before it no longer applies.
    background_image_changes: u64,
}

struct ActiveCommit {
    generation: u64,
    result: CommitCompletion,
    _worker: Task<()>,
    completion: Task<()>,
}

#[derive(Clone)]
struct CommitCompletion {
    result: Arc<OnceLock<Result<CommitOutcome, SettingsError>>>,
    finished: async_channel::Receiver<()>,
}

impl CommitCompletion {
    async fn wait(&self) -> Result<CommitOutcome, SettingsError> {
        // The sender closes only after publishing the result. Every waiter sees that closure;
        // foreground completion and shutdown never compete to consume one result message.
        let _ = self.finished.recv().await;
        self.result
            .get()
            .copied()
            .unwrap_or(Err(SettingsError::Storage(StorageError::Unavailable)))
    }
}

impl SettingsEditor {
    pub(super) fn new(settings: Settings) -> Self {
        Self {
            draft: SettingsDraft::new(settings),
            pending: None,
            in_flight: None,
            generation: 0,
            background_image_changes: 0,
        }
    }

    pub(super) fn background_image_changes(&self) -> u64 {
        self.background_image_changes
    }

    /// Runs `change` on the draft, and records a Background Image change when the image differs
    /// afterwards or when `replaces` says the draft was replaced as a whole.
    fn tracking_background_image<R>(
        &mut self,
        replaces: bool,
        change: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let before = self.document().appearance.window.background_image;
        let result = change(self);
        if replaces || self.document().appearance.window.background_image != before {
            self.background_image_changes = self.background_image_changes.wrapping_add(1);
        }
        result
    }

    pub(super) fn document(&self) -> &SettingsDocument {
        self.draft.document()
    }

    /// The draft itself, which is replaced rather than mutated, so holding it shows whether the
    /// document changed since.
    pub(super) fn shared_document(&self) -> Arc<SettingsDocument> {
        self.draft.shared_document()
    }

    pub(super) fn editable(&self) -> bool {
        self.draft.editable()
    }

    pub(super) fn status(&self) -> SaveStatus {
        self.draft.status()
    }

    pub(super) fn edit(
        &mut self,
        edit: impl FnOnce(&mut SettingsDocument),
        cx: &mut Context<SettingsWindow>,
    ) {
        if self.tracking_background_image(false, |editor| editor.draft.edit(edit)) {
            self.schedule(cx);
        }
    }

    pub(super) fn reset(&mut self, target: ResetTarget, cx: &mut Context<SettingsWindow>) {
        if self.tracking_background_image(false, |editor| editor.draft.reset(target)) {
            self.schedule(cx);
        }
    }

    /// Restores every Setting, including the imported theme catalog, to its default.
    pub(super) fn reset_all(&mut self, cx: &mut Context<SettingsWindow>) {
        if self.tracking_background_image(true, |editor| editor.draft.reset_all()) {
            self.schedule(cx);
        }
    }

    pub(super) fn import(
        &mut self,
        source: ThemeImport<'_>,
        cx: &mut Context<SettingsWindow>,
    ) -> Result<ImportReceipt, SettingsError> {
        let receipt = self.draft.import(source)?;
        self.schedule(cx);
        Ok(receipt)
    }

    pub(super) fn remove_themes(
        &mut self,
        ids: &[ThemeId],
        cx: &mut Context<SettingsWindow>,
    ) -> Result<(), SettingsError> {
        self.draft.remove_themes(ids)?;
        self.schedule(cx);
        Ok(())
    }

    pub(super) fn theme_summaries(&self) -> Result<Vec<ThemeSummary>, SettingsError> {
        self.draft.theme_summaries()
    }

    pub(super) fn export_document(&self) -> Result<String, SettingsError> {
        self.draft.export_document()
    }

    /// Attempts to save before closing, keeping a running write and any failed draft alive. The
    /// final document is written synchronously so a close never depends on a task owned by the
    /// window.
    pub(super) fn flush(&mut self, cx: &mut Context<SettingsWindow>) -> bool {
        self.pending = None;
        if self.in_flight.is_some() {
            return false;
        }
        if !self.draft.has_unwritten_changes() {
            return true;
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
        !self.draft.has_unwritten_changes()
    }

    /// Writes pending changes, then the document itself if no file holds it yet, so another program
    /// can open the file.
    pub(super) fn write_file(&mut self, cx: &mut Context<SettingsWindow>) {
        if !self.flush(cx) {
            return;
        }
        match self.draft.prepare_file() {
            Ok(Some(job)) => {
                let generation = self.generation;
                let result = job.run();
                self.settle(generation, result, cx);
            }
            // Another writer owns the transaction, and its write creates the file.
            Ok(None) | Err(SettingsError::Busy) => {}
            Err(error) => {
                self.draft.report(error);
                cx.notify();
            }
        }
    }

    /// Reads the settings file again, as another program left it. A change this window can still
    /// write is written first.
    pub(super) fn reload_file(&mut self, cx: &mut Context<SettingsWindow>) {
        if self.draft.editable() && !self.flush(cx) {
            return;
        }
        self.reload(cx);
    }

    pub(super) fn is_writing(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Native shutdown cannot be deferred by GPUI and its returned futures get only 100 ms.
    /// Drain the background result before returning, without awaiting any foreground callback.
    pub(super) fn flush_for_shutdown(&mut self, cx: &mut Context<SettingsWindow>) -> bool {
        self.pending = None;
        if let Some(active) = self.in_flight.take() {
            drop(active.completion);
            let wait = active.result.wait();
            #[cfg(test)]
            let result = {
                let dispatcher = cx.background_executor().dispatcher();
                let test = dispatcher.as_test().expect("test dispatcher");
                let mut wait = std::pin::pin!(wait);
                let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                loop {
                    if let std::task::Poll::Ready(result) = wait.as_mut().poll(&mut context) {
                        break result;
                    }
                    if !test.tick(true) {
                        std::thread::yield_now();
                    }
                }
            };
            #[cfg(not(test))]
            let result = pollster::block_on(wait);
            self.draft
                .settle(active.generation == self.generation, result);
        }
        self.flush(cx)
    }

    pub(super) fn retry(&mut self, cx: &mut Context<SettingsWindow>) {
        self.pending = None;
        self.start_commit(self.generation, cx);
    }

    pub(super) fn reload(&mut self, cx: &mut Context<SettingsWindow>) {
        self.pending = None;
        self.tracking_background_image(true, |editor| editor.draft.reload());
        cx.notify();
    }

    /// Replaces Malformed Settings with defaults, keeping the unreadable file as a backup.
    pub(super) fn recover_by_reset(
        &mut self,
        cx: &mut Context<SettingsWindow>,
    ) -> Result<(), RecoveryError> {
        self.pending = None;
        let result = self.tracking_background_image(true, |editor| editor.draft.recover_by_reset());
        cx.notify();
        result
    }

    pub(super) fn synchronize(&mut self) {
        if self.pending.is_none() && self.in_flight.is_none() {
            self.tracking_background_image(false, |editor| editor.draft.synchronize());
        }
    }

    /// Replaces any scheduled write with a fresh one, which is the debounce.
    fn schedule(&mut self, cx: &mut Context<SettingsWindow>) {
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
        if self.in_flight.is_some() {
            return;
        }
        match self.draft.prepare_commit() {
            Ok(job) => {
                let result = cx.background_executor().spawn(async move { job.run() });
                self.track_commit(generation, result, cx);
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

    fn track_commit(
        &mut self,
        generation: u64,
        result: Task<Result<CommitOutcome, SettingsError>>,
        cx: &mut Context<SettingsWindow>,
    ) {
        let (finished, waiting) = async_channel::bounded(1);
        let shared = Arc::new(OnceLock::new());
        let published = shared.clone();
        let worker = cx.background_executor().spawn(async move {
            let _ = published.set(result.await);
            drop(finished);
        });
        let result = CommitCompletion {
            result: shared,
            finished: waiting,
        };
        let completion = result.clone();
        let completion = cx.spawn(async move |window, cx| {
            let result = completion.wait().await;
            let _ = window.update(cx, |window, cx| {
                window.editor.settle(generation, result, cx);
                window.finish_close(cx);
            });
        });
        self.in_flight = Some(ActiveCommit {
            generation,
            result,
            _worker: worker,
            completion,
        });
    }

    fn settle(
        &mut self,
        generation: u64,
        result: Result<CommitOutcome, SettingsError>,
        cx: &mut Context<SettingsWindow>,
    ) {
        self.in_flight = None;
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

    pub(super) fn recoverable(self) -> bool {
        matches!(self, Self::Unavailable(error) if error.is_malformed())
    }

    /// The explanation a banner adds above the content, when one is warranted.
    pub(super) fn explanation(self) -> Option<&'static str> {
        match self {
            Self::Saved | Self::Saving => None,
            Self::Failed(_) => {
                Some("The change is still applied. Retry to write it to your settings file.")
            }
            status if status.recoverable() => Some(
                "SpaceTerm is showing defaults and has changed nothing. Reset Settings keeps the unreadable file as a backup and starts from defaults.",
            ),
            Self::Unavailable(SettingsError::Storage(StorageError::Conflict)) => Some(
                "Reload to pick up the change. Editing is paused so SpaceTerm does not replace it.",
            ),
            Self::Unavailable(_) => {
                Some("Editing is paused until SpaceTerm can read and write your settings.")
            }
        }
    }
}
