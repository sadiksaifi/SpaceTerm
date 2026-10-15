//! Runs runtime clipboard requests through the desktop clipboard adapter.
use gpui::FutureExt as _;

use super::clipboard::{ClipboardPreferences, TextClipboard};
use crate::terminal::osc52::{MAX_OSC52_CONTENT_BYTES, Osc52Operation};
use crate::terminal::session::{ClipboardCompletion, ClipboardRequest};

impl ClipboardRequest {
    /// Consults the native clipboard only while the originating focus grant is current. A write
    /// completes before this returns. A read completes when the native clipboard answers or the
    /// request deadline passes, whichever comes first, so a slow owner cannot hold the UI or the
    /// next request. The worker revalidates the grant before replying, and dropping the returned
    /// task replies as denied.
    pub(crate) fn perform(
        mut self,
        preferences: ClipboardPreferences,
        clipboard: &dyn TextClipboard,
        focused: bool,
        cx: &mut gpui::App,
    ) -> gpui::Task<()> {
        let Some(remaining) = self.remaining().filter(|_| focused) else {
            self.complete(ClipboardCompletion::Denied);
            return gpui::Task::ready(());
        };
        let target = match self.operation() {
            Osc52Operation::Write { target, text } if preferences.allow_write => {
                let completion = clipboard
                    .write(clipboard.resolve(*target), text, cx)
                    .map_or(ClipboardCompletion::Denied, |()| {
                        ClipboardCompletion::Written
                    });
                self.complete(completion);
                return gpui::Task::ready(());
            }
            Osc52Operation::Read { target, .. } if preferences.allow_read => *target,
            _ => {
                self.complete(ClipboardCompletion::Denied);
                return gpui::Task::ready(());
            }
        };
        let read = clipboard
            .read(clipboard.resolve(target), cx)
            .with_timeout(remaining, cx.background_executor());
        cx.foreground_executor().spawn(async move {
            let completion = match read.await {
                Ok(Ok(text))
                    if text
                        .as_ref()
                        .is_none_or(|text| text.len() <= MAX_OSC52_CONTENT_BYTES) =>
                {
                    ClipboardCompletion::Text(text.unwrap_or_default())
                }
                _ => ClipboardCompletion::Denied,
            };
            self.complete(completion);
        })
    }
}
