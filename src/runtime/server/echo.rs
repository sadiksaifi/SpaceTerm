//! An accepted key batch's reply carries the Pane's next publication, so typed text can echo in
//! the same exchange. Polling stays the baseline: a reply without a screen leaves the next
//! output check to deliver it.
use super::*;
use crate::terminal::session::Publications;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::time::Instant;

/// How long an accepted key batch's reply waits for the Pane's next publication.
///
/// The terminal runtime publishes a change no earlier than its 8 ms accumulation interval and
/// no more often than once per 16.667 ms frame (`src/terminal/session/schedules.rs`). Output
/// that arrives just after a publication therefore publishes within one frame. One more
/// accumulation interval covers the shell reading the key and writing its echo. Their sum,
/// 24.667 ms, is rounded up to whole milliseconds.
pub(super) const KEY_ECHO_WAIT: Duration = Duration::from_millis(25);

/// The accepted batch whose reply may still carry a screen. Only the same device, lease and
/// focus generation can finish it, and only once.
pub(super) struct Echo {
    token: u64,
    device: [u8; 32],
    lease: Lease,
    focus_generation: u64,
    /// Set by the lock-free wait when the worker reports that the batch wrote nothing.
    wrote_nothing: Arc<AtomicBool>,
}

/// A key batch the terminal worker is applying.
pub(super) struct Applied {
    /// Publications counted before the worker received the batch.
    pub(super) seen: u64,
    /// Whether the batch wrote bytes to the PTY, once the worker has applied it.
    pub(super) written: std::sync::mpsc::Receiver<bool>,
}

/// Waits without the service lock and without consuming any event: first for the worker's
/// report on what the batch wrote, then, only if it wrote bytes, for the next publication.
struct Publication {
    publications: Publications,
    seen: u64,
    written: Option<std::sync::mpsc::Receiver<bool>>,
    wrote_nothing: Arc<AtomicBool>,
}
impl PendingReply for Publication {
    fn wait(&mut self, deadline: Instant) {
        if let Some(written) = self.written.take() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match written.recv_timeout(remaining) {
                Ok(true) => {}
                Ok(false) | Err(RecvTimeoutError::Disconnected) => {
                    self.wrote_nothing.store(true, Ordering::Release);
                    return;
                }
                Err(RecvTimeoutError::Timeout) => return,
            }
        }
        self.publications.wait_after(self.seen, deadline);
    }
}

impl DevelopmentPaneHost {
    /// Called under the service lock after the batch was accepted. A batch that wrote nothing
    /// to the PTY, or a Pane that cannot present to this exact controller, answers the
    /// acceptance now, without waiting.
    pub(super) fn await_echo(
        &mut self,
        device: [u8; 32],
        lease: Lease,
        generation: u64,
    ) -> Handled {
        let applied = self.pane.as_mut().and_then(|pane| pane.applied_keys.take());
        let Some(pane) = self.pane.as_mut().filter(|pane| {
            pane.lease == lease
                && pane.device == Some(device)
                && pane.connected
                && pane.presentable
                && pane.focus_generation == generation
                && !pane.ended
                && !pane.failed
        }) else {
            return Handled::Now(Reply::Unchanged);
        };
        let Some(Applied { seen, written }) = applied else {
            return Handled::Now(Reply::Unchanged);
        };
        // Keys that write nothing, such as releases and modifiers, produce no echo to wait for.
        // The service lock never waits for the worker; a report not yet made is awaited later.
        let written = match written.try_recv() {
            Ok(true) => None,
            Err(TryRecvError::Empty) => Some(written),
            Ok(false) | Err(TryRecvError::Disconnected) => return Handled::Now(Reply::Unchanged),
        };
        let Ok(bytes) = nonce() else {
            return Handled::Now(Reply::Unchanged);
        };
        let token = u64::from_ne_bytes(std::array::from_fn(|index| bytes[index]));
        let wrote_nothing = Arc::new(AtomicBool::new(false));
        pane.echo = Some(Echo {
            token,
            device,
            lease,
            focus_generation: generation,
            wrote_nothing: Arc::clone(&wrote_nothing),
        });
        Handled::Later {
            pending: Box::new(Publication {
                publications: pane.subscription.publications(),
                seen,
                written,
                wrote_nothing,
            }),
            deadline: Instant::now() + KEY_ECHO_WAIT,
            finish: token,
        }
    }

    /// Finishes this Pane's pending echo under the service lock. `None` means the token is not
    /// this Pane's. An echo that no longer matches its controller answers the acceptance alone.
    pub(super) fn finish_echo(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        token: u64,
    ) -> Option<Reply> {
        let pane = self.pane.as_mut()?;
        if pane.echo.as_ref().is_none_or(|echo| echo.token != token) {
            return None;
        }
        let echo = pane.echo.take()?;
        if echo.device != device
            || echo.lease.connection != connection
            || echo.lease != pane.lease
            || pane.device != Some(device)
            || !pane.connected
            || pane.focus_generation != echo.focus_generation
        {
            return Some(Reply::Unchanged);
        }
        // A batch that wrote nothing has no echo; its reply is the acceptance alone.
        if echo.wrote_nothing.load(Ordering::Acquire) {
            return Some(Reply::Unchanged);
        }
        pane.drain();
        if pane.ended || pane.failed {
            return Some(Reply::Unchanged);
        }
        // The encoder's own baseline is the one the client holds after its previous reply.
        let base = pane.screen_encoder.baseline();
        Some(pane.present(Some(base)).unwrap_or(Reply::Unchanged))
    }
}
