//! Reads Scrollback pages on the terminal worker for clients that cache history.
use super::*;
use crate::terminal::emulator::HistoryPage;

/// Receives one page exactly once, on the terminal worker: the rows read, or `None` when the
/// epoch ended, the alternate screen hides history, or the worker stopped first.
pub(crate) struct ScrollbackReply(Option<Box<dyn FnOnce(Option<HistoryPage>) + Send>>);

impl ScrollbackReply {
    pub(crate) fn new(reply: impl FnOnce(Option<HistoryPage>) + Send + 'static) -> Self {
        Self(Some(Box::new(reply)))
    }

    pub(super) fn send(mut self, page: Option<HistoryPage>) {
        if let Some(reply) = self.0.take() {
            reply(page);
        }
    }
}

impl Drop for ScrollbackReply {
    fn drop(&mut self) {
        if let Some(reply) = self.0.take() {
            reply(None);
        }
    }
}

/// One `ScrollbackPage` read.
pub(super) struct ScrollbackRead {
    pub(super) epoch: u64,
    pub(super) start: u64,
    pub(super) rows: u16,
    pub(super) reply: ScrollbackReply,
}

impl TerminalRuntime {
    /// Reads up to `rows` history rows from position `start` of history `epoch` on the worker,
    /// after the output it already received. Reading writes nothing to the PTY and moves no view.
    pub(crate) fn read_scrollback(
        &self,
        epoch: u64,
        start: u64,
        rows: u16,
        reply: ScrollbackReply,
    ) {
        if let Some(commands) = &self.commands {
            // A stopped worker drops the command, and the reply answers `None`.
            let _ = commands.send(Command::ReadScrollback(ScrollbackRead {
                epoch,
                start,
                rows,
                reply,
            }));
        }
    }
}

impl TerminalWorker {
    pub(super) fn read_scrollback(&mut self, read: ScrollbackRead) -> bool {
        match self
            .emulator
            .scrollback_page(read.epoch, read.start, read.rows)
        {
            Ok(page) => {
                read.reply.send(page);
                true
            }
            Err(error) => {
                read.reply.send(None);
                self.send_runtime_failure(error);
                false
            }
        }
    }
}
