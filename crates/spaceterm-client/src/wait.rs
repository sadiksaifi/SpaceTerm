use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub(crate) const CANCELLATION_POLL: Duration = Duration::from_millis(10);

/// An absolute deadline and optional caller cancellation for a blocking client operation.
/// Set the flag to cancel the wait. Leave it set for the lifetime of that operation.
#[derive(Clone)]
pub struct WaitLimit {
    deadline: Instant,
    cancellation: Option<Arc<AtomicBool>>,
}

impl WaitLimit {
    pub fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            cancellation: None,
        }
    }

    pub fn with_cancellation(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancellation = Some(flag);
        self
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    pub(crate) fn ended(&self) -> bool {
        self.is_cancelled() || Instant::now() >= self.deadline
    }

    pub(crate) fn cancellation(&self) -> Option<Arc<AtomicBool>> {
        self.cancellation.clone()
    }

    pub(crate) fn earlier(&self, deadline: Instant) -> Self {
        Self {
            deadline: self.deadline.min(deadline),
            cancellation: self.cancellation(),
        }
    }

    pub(crate) fn next_poll(&self) -> Instant {
        if self.cancellation.is_some() {
            self.deadline.min(Instant::now() + CANCELLATION_POLL)
        } else {
            self.deadline
        }
    }
}

impl From<Instant> for WaitLimit {
    fn from(deadline: Instant) -> Self {
        Self::new(deadline)
    }
}
