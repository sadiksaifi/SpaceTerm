//! Answers `Operation::ScrollbackPage` for the channel's controlling client.
//!
//! Each client caches Scrollback and reads it locally, so the server keeps no reading state: no
//! viewport, Selection or Find per client. A page is read on the Pane's terminal worker after
//! the output it already received, encoded there with page-local tables, and replied through the
//! connection's outbox. The `PaneService` lock is never held while rows are read or encoded.
use super::*;
use crate::terminal::emulator::HistoryPage;
use crate::terminal::session::ScrollbackReply;
use spaceterm_server::protocol::Page;
use spaceterm_server::protocol::limits::MAX_PAGE_ROWS;

impl OwnedPane {
    /// Starts reading rows `start..start + rows` of history `epoch`. `reply` runs exactly once,
    /// on the terminal worker: with the page, or `Refusal::Stale` when the epoch ended or the
    /// alternate screen hides history. Rows the engine already discarded are left out, so the
    /// page may start later than `start`.
    pub(super) fn scrollback_page(
        &self,
        epoch: u64,
        start: u64,
        rows: u16,
        reply: impl FnOnce(Result<Page, Refusal>) + Send + 'static,
    ) -> Result<(), Refusal> {
        if self.ended || self.failed {
            return Err(Refusal::Missing);
        }
        if !(1..=MAX_PAGE_ROWS).contains(&rows) {
            return Err(Refusal::Unsupported);
        }
        self.runtime.read_scrollback(
            epoch,
            start,
            rows,
            ScrollbackReply::new(move |page| {
                reply(page.ok_or(Refusal::Stale).and_then(encode_page));
            }),
        );
        Ok(())
    }
}

/// Encodes history rows as a page with page-local style and link tables.
///
/// This becomes a call to `screen_encoder::encode_page(page.epoch, page.start, rows)` once the
/// encoding change merges. Until then a page is refused rather than encoded twice.
fn encode_page(page: HistoryPage) -> Result<Page, Refusal> {
    let _ = page;
    Err(Refusal::Unsupported)
}
