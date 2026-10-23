//! Decides when a Pane worker exchanges, so input never waits for an output check.
use std::{
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

/// The output check for a visible Pane. Server output is not pushed yet, so this
/// remains the fallback discovery cadence; input and view changes wake the worker early.
const VISIBLE_POLL: Duration = Duration::from_millis(30);
/// A hidden Pane checks only for lifecycle and control changes.
const HIDDEN_POLL: Duration = Duration::from_millis(250);
/// Rechecks reading work the server is still applying, such as an operation's presentation
/// barrier or a selection being prepared. A quarter of a 60 Hz frame bounds each wait
/// without turning the worker into a busy loop.
const READING_RECHECK: Duration = Duration::from_millis(4);

/// Wakes a Pane worker. A notification before the wait is never lost.
#[derive(Default)]
pub(super) struct Wake {
    pending: Mutex<bool>,
    ready: Condvar,
}
impl Wake {
    pub(super) fn notify(&self) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.ready.notify_all();
    }

    /// Returns at the deadline or on the first notification since the previous wait.
    pub(super) fn wait_until(&self, deadline: Instant) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while !*pending {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            pending = self
                .ready
                .wait_timeout(pending, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *pending = false;
    }
}

/// One attached lease's output checks and visual demand.
pub(super) struct Cadence {
    next_poll: Instant,
    visible: bool,
    preparation: bool,
    demand_sent: Option<bool>,
}
impl Cadence {
    /// A new lease checks output immediately.
    pub(super) fn new(now: Instant, visible: bool) -> Self {
        Self {
            next_poll: now,
            visible,
            preparation: false,
            demand_sent: None,
        }
    }

    /// Records the view's visibility. Revealing a Pane checks output now.
    pub(super) fn set_visible(&mut self, visible: bool, now: Instant) {
        if visible && !self.visible {
            self.next_poll = now;
        }
        self.visible = visible;
    }

    pub(super) fn visible(&self) -> bool {
        self.visible
    }

    pub(super) fn set_preparation(&mut self, prepare: bool) {
        self.preparation = prepare;
    }

    /// The visual demand the server has not been told yet. Sending it precedes the next check.
    pub(super) fn take_demand(&mut self) -> Option<bool> {
        let demand = self.visible || self.preparation;
        (self.demand_sent != Some(demand)).then(|| {
            self.demand_sent = Some(demand);
            demand
        })
    }

    /// Whether to check output now; a due check schedules the next one.
    pub(super) fn poll_due(&mut self, now: Instant) -> bool {
        if now < self.next_poll {
            return false;
        }
        self.next_poll = now
            + if self.visible {
                VISIBLE_POLL
            } else {
                HIDDEN_POLL
            };
        true
    }

    /// Makes the output check due now, for a screen that reading work already produced.
    pub(super) fn poll_now(&mut self, now: Instant) {
        self.next_poll = self.next_poll.min(now);
    }

    pub(super) fn deadline(&self) -> Instant {
        self.next_poll
    }

    /// The wait while the server is still applying reading work.
    pub(super) fn recheck_deadline(&self, now: Instant) -> Instant {
        self.next_poll.min(now + READING_RECHECK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notification_before_the_wait_ends_it_immediately_and_only_once() {
        let wake = Wake::default();
        wake.notify();
        let started = Instant::now();
        wake.wait_until(started + Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
        let started = Instant::now();
        wake.wait_until(started + Duration::from_millis(20));
        assert!(
            started.elapsed() >= Duration::from_millis(20),
            "a consumed notification must not end a later wait"
        );
    }

    #[test]
    fn a_waiting_worker_wakes_for_input_before_its_output_check() {
        let wake = std::sync::Arc::new(Wake::default());
        let waiter = {
            let wake = std::sync::Arc::clone(&wake);
            std::thread::spawn(move || {
                let started = Instant::now();
                wake.wait_until(started + Duration::from_secs(5));
                started.elapsed()
            })
        };
        std::thread::sleep(Duration::from_millis(10));
        wake.notify();
        assert!(waiter.join().unwrap() < Duration::from_secs(1));
    }

    #[test]
    fn revealing_a_hidden_pane_demands_and_checks_its_screen_at_once() {
        let start = Instant::now();
        let mut cadence = Cadence::new(start, false);
        assert_eq!(cadence.take_demand(), Some(false));
        assert!(cadence.poll_due(start));
        assert!(!cadence.poll_due(start + VISIBLE_POLL));
        let revealed = start + Duration::from_millis(40);
        cadence.set_visible(true, revealed);
        assert_eq!(cadence.take_demand(), Some(true));
        assert_eq!(cadence.take_demand(), None);
        assert!(cadence.poll_due(revealed));
        assert_eq!(cadence.deadline(), revealed + VISIBLE_POLL);
    }
}
