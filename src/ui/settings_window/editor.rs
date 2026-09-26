//! Schedules instant-apply Settings edits on the Settings Window's GPUI executor.
//!
//! Draft and preview ownership live in the framework-independent draft Module. This Adapter owns
//! debounce cancellation, retained background execution, close/quit flushing, and view notification.

mod draft;

#[cfg(test)]
use std::future::Future as _;

use std::{
    collections::BTreeSet,
    sync::{Arc, OnceLock},
    time::Duration,
};

use gpui::{Context, Task};

use crate::appearance::{ResetTarget, SchemeId, SchemeKind, SchemeSummary, SettingsDocument};
#[cfg(test)]
use crate::settings::CommitJob;
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
    in_flight: Option<ActiveCommit>,
    /// Retires a superseded scheduled write so a late completion cannot report stale status.
    generation: u64,
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
    pub(super) fn new(settings: UserSettings) -> Self {
        Self {
            draft: SettingsDraft::new(settings),
            pending: None,
            in_flight: None,
            generation: 0,
        }
    }

    pub(super) fn document(&self) -> &SettingsDocument {
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
        edit: impl FnOnce(&mut SettingsDocument),
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

    /// Restores every Setting, including the imported scheme catalog, to its default.
    pub(super) fn reset_all(&mut self, cx: &mut Context<SettingsWindow>) {
        if self.draft.reset_all() {
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

    pub(super) fn export_definitions(
        &self,
        schemes: &[(SchemeKind, SchemeId)],
    ) -> Result<String, SettingsError> {
        self.draft.export_definitions(schemes)
    }

    pub(super) fn export_appearance(
        &self,
        resolved: &crate::appearance::ResolvedAppearance,
    ) -> Result<String, SettingsError> {
        self.draft.export_appearance(resolved)
    }

    pub(super) fn list_import_candidates(
        bytes: &[u8],
    ) -> Result<Vec<crate::appearance::ImportCandidate>, SettingsError> {
        SettingsDraft::list_import_candidates(bytes)
    }

    /// Attempts to save before closing, keeping a running write and any failed draft alive.
    ///
    /// A running write completes through its retained callback. Otherwise the final small document
    /// is written synchronously, so a successful close never depends on a task owned by the window.
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
        if self.pending.is_none() && self.in_flight.is_none() {
            self.draft.synchronize();
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

    #[cfg(test)]
    pub(super) fn start_deferred_commit(
        &mut self,
        cx: &mut Context<SettingsWindow>,
    ) -> (
        CommitJob,
        async_channel::Sender<Result<CommitOutcome, SettingsError>>,
    ) {
        self.pending = None;
        let job = self.draft.prepare_commit().unwrap();
        let (sender, receiver) = async_channel::bounded(1);
        let result = cx
            .background_executor()
            .spawn(async move { receiver.recv().await.unwrap() });
        self.track_commit(self.generation, result, cx);
        (job, sender)
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
