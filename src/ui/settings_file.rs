//! Follows the Settings Document file while another program edits it.
//!
//! A person edits settings.json in their own editor. Every save SpaceTerm did not make is adopted
//! the way an explicit reload adopts it, so every window follows the file and a malformed save
//! keeps the last valid settings until a valid one arrives.

#[cfg(test)]
#[path = "settings_file_tests.rs"]
mod tests;

use std::{rc::Rc, time::Duration};

use gpui::{App, AsyncApp, BorrowAppContext as _, Global, SharedString, Task};

use crate::platform::settings_file::{SettingsFileAccess, SettingsFileWatch};
use crate::settings::{SettingsError, UserSettings};

/// How long the file stays quiet before SpaceTerm reads it, because editors often save in steps.
pub(crate) const SETTLE_DELAY: Duration = Duration::from_millis(100);

/// The application's settings file: where it is, how to open it, and the task following it.
pub(crate) struct SettingsFile {
    access: Rc<dyn SettingsFileAccess>,
    changes: async_channel::Sender<()>,
    watch: Option<SettingsFileWatch>,
    _follow: Task<()>,
    /// Retries a watch that could not start, until one does.
    _start_watching: Option<Task<()>>,
}

impl Global for SettingsFile {}

impl SettingsFile {
    /// Starts following the file for the application's life.
    pub(crate) fn install(settings: UserSettings, access: Rc<dyn SettingsFileAccess>, cx: &mut App) {
        let (changes, received) = async_channel::bounded(1);
        let settings_changed = settings.subscribe();
        let follow = cx.spawn(async move |cx| follow(settings, received, cx).await);
        let mut file = Self {
            access,
            changes,
            watch: None,
            _follow: follow,
            _start_watching: None,
        };
        if !file.start_watching() {
            // Watching needs the file's directory, which SpaceTerm creates only when it first
            // writes. Every write notifies subscribers, so each notification retries the watch.
            file._start_watching = Some(cx.spawn(async move |cx| {
                while settings_changed.recv().await.is_ok() {
                    let started = cx.update(|cx| {
                        cx.update_global::<Self, _>(|file, _| file.start_watching())
                    });
                    if started {
                        break;
                    }
                }
            }));
        }
        cx.set_global(file);
    }

    /// Where the file lives, spelled for a person to read.
    pub(crate) fn location(cx: &App) -> Option<SharedString> {
        cx.try_global::<Self>().map(|file| file.access.location())
    }

    /// Opens the file in the program the Operating System assigns to it.
    ///
    /// The file must already exist. Its directory exists by then too, so a watch that could not
    /// start at launch starts here.
    pub(crate) fn open(cx: &mut App) -> bool {
        let Some(access) = cx.try_global::<Self>().map(|file| Rc::clone(&file.access)) else {
            return false;
        };
        cx.update_global::<Self, _>(|file, _| file.start_watching());
        access.open(cx);
        true
    }

    /// Returns whether the file is watched.
    fn start_watching(&mut self) -> bool {
        if self.watch.is_some() {
            return true;
        }
        let changes = self.changes.clone();
        self.watch = self
            .access
            .watch(Box::new(move || {
                // A full channel already holds a change that has not been read.
                let _ = changes.try_send(());
            }))
            .ok();
        self.watch.is_some()
    }
}

async fn follow(settings: UserSettings, changes: async_channel::Receiver<()>, cx: &mut AsyncApp) {
    let settings_changed = settings.subscribe();
    while changes.recv().await.is_ok() {
        cx.background_executor().timer(SETTLE_DELAY).await;
        while changes.try_recv().is_ok() {}
        loop {
            let reader = settings.clone();
            let result = cx
                .background_executor()
                .spawn(async move { reader.follow_file() })
                .await;
            if result != Err(SettingsError::Busy) {
                break;
            }
            // A preview or write owns the transaction. Its end notifies subscribers.
            if settings_changed.recv().await.is_err() {
                return;
            }
        }
    }
}
