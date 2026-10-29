//! Answers `Operation::ScrollbackPage` for the channel's controlling client.
//!
//! Each client caches Scrollback and reads it locally, so the server keeps no reading state: no
//! viewport, Selection or Find per client. A page is read on the Pane's terminal worker after
//! the output it already received, encoded there with page-local tables, and replied through the
//! connection's outbox. The `PaneService` lock is never held while rows are read or encoded.
use super::*;
use crate::terminal::session::ScrollbackReply;
use spaceterm_server::protocol::Page;
use spaceterm_server::protocol::limits::MAX_PAGE_ROWS;

/// One `ScrollbackPage` request: up to `rows` rows from position `start` of history `epoch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollbackRead {
    pub epoch: u64,
    pub start: u64,
    pub rows: u16,
}

/// Receives the encoded page, or the refusal, on the terminal worker.
pub type PageReply = Box<dyn FnOnce(Result<Page, Refusal>) + Send>;

impl DevelopmentPaneHost {
    /// Reads a page of the Pane `lease` controls, under the same lease checks as its other
    /// commands.
    pub(super) fn scrollback_page(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        lease: &Lease,
        read: ScrollbackRead,
        reply: PageReply,
    ) -> Result<(), Refusal> {
        let pane = self.pane.as_mut().ok_or(Refusal::Missing)?;
        if Some(device) != pane.device
            || connection != pane.lease.connection
            || *lease != pane.lease
            || !pane.connected
        {
            return Err(Refusal::Stale);
        }
        pane.scrollback_page(read, reply)
    }
}

impl OwnedPane {
    /// Starts reading the page. `reply` runs exactly once, on the terminal worker: with the page,
    /// or `Refusal::Stale` when the epoch ended or the alternate screen hides history. Rows the
    /// engine already discarded are left out, so the page may start later than requested.
    pub(super) fn scrollback_page(
        &self,
        read: ScrollbackRead,
        reply: PageReply,
    ) -> Result<(), Refusal> {
        if self.ended || self.failed {
            return Err(Refusal::Missing);
        }
        if !(1..=MAX_PAGE_ROWS).contains(&read.rows) {
            return Err(Refusal::Unsupported);
        }
        self.runtime.read_scrollback(
            read.epoch,
            read.start,
            read.rows,
            ScrollbackReply::new(move |page| {
                reply(page.ok_or(Refusal::Stale).map(|page| {
                    screen_encoder::encode_page(
                        page.epoch,
                        page.start,
                        page.rows
                            .iter()
                            .map(|(cells, wrapped)| (cells.as_ref(), *wrapped)),
                    )
                }));
            }),
        );
        Ok(())
    }
}
