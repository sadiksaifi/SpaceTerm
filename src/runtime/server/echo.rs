//! An accepted key batch's reply carries the Pane's next publication, so typed text can echo in
//! the same exchange. Polling stays the baseline: a reply without a screen leaves the next
//! output check to deliver it.
use super::*;
use crate::terminal::session::Publications;
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
}

/// Waits for a publication without the service lock and without consuming any event.
struct Publication {
    publications: Publications,
    seen: u64,
}
impl PendingReply for Publication {
    fn wait(&mut self, deadline: Instant) {
        self.publications.wait_after(self.seen, deadline);
    }
}

impl DevelopmentPaneHost {
    /// Called under the service lock after the batch was accepted. A Pane that cannot present
    /// to this exact controller answers the acceptance now, without waiting.
    pub(super) fn await_echo(
        &mut self,
        device: [u8; 32],
        lease: Lease,
        generation: u64,
    ) -> Handled {
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
        let Ok(bytes) = nonce() else {
            return Handled::Now(Reply::Unchanged);
        };
        let token = u64::from_ne_bytes(std::array::from_fn(|index| bytes[index]));
        let publications = pane.subscription.publications();
        let seen = publications.current();
        pane.echo = Some(Echo {
            token,
            device,
            lease,
            focus_generation: generation,
        });
        Handled::Later {
            pending: Box::new(Publication { publications, seen }),
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
        pane.drain();
        if pane.ended || pane.failed {
            return Some(Reply::Unchanged);
        }
        // The encoder's own baseline is the one the client holds after its previous reply.
        let base = pane.screen_encoder.baseline();
        Some(pane.present(Some(base)).unwrap_or(Reply::Unchanged))
    }
}
