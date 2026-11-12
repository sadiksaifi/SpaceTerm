//! Cancellable backoff for proven temporary local startup conditions.
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub(crate) enum WaitingFor {
    ClientViewLock,
    AttachmentsLock,
    WorkspaceOrderLock,
    LocalAdmission,
}

pub(crate) struct Backoff {
    reason: WaitingFor,
    delay: Duration,
    attempts: u32,
}

impl Backoff {
    pub(crate) fn new(reason: WaitingFor) -> Self {
        Self {
            reason,
            delay: Duration::from_millis(10),
            attempts: 0,
        }
    }

    pub(crate) fn pause(&mut self, cancelled: &dyn Fn() -> bool) -> bool {
        self.attempts = self.attempts.saturating_add(1);
        if self.attempts == 1 {
            eprintln!(
                "spaceterm_startup_wait classification={:?} attempts=1",
                self.reason
            );
        }
        let until = Instant::now() + self.delay;
        loop {
            if cancelled() {
                return false;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(10)));
        }
        self.delay = (self.delay * 2).min(Duration::from_millis(250));
        true
    }
}

impl Drop for Backoff {
    fn drop(&mut self) {
        if self.attempts > 0 {
            eprintln!(
                "spaceterm_startup_wait_end classification={:?} attempts={}",
                self.reason, self.attempts
            );
        }
    }
}
