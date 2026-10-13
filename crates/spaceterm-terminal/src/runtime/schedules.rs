//! Coalesced wakeups. Reader and timers never consume ordinary ingress slots.
use std::sync::{Condvar, Mutex};
use std::time::Duration;
#[derive(Default)]
pub(crate) struct Wake {
    pending: Mutex<bool>,
    changed: Condvar,
}
impl Wake {
    pub fn notify(&self) {
        *self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = true;
        self.changed.notify_one();
    }
    pub fn wait(&self, interval: Duration) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*pending {
            pending = self
                .changed
                .wait_timeout(pending, interval)
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
        *pending = false;
    }
}
