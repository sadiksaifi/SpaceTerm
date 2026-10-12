//! Cooperative cancellation for blocking host reads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone)]
/// Cloneable cooperative cancellation shared by host and desktop workers.
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    observed: Arc<[Arc<AtomicBool>]>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            observed: Arc::from([]),
        }
    }
}

impl CancellationToken {
    /// Construct an already-cancelled token.
    pub fn cancelled() -> Self {
        let token = Self::default();
        token.cancel();
        token
    }

    /// Cancel this token and every token observing it.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Observe an external cancellation flag without mutating it.
    pub fn observing(flag: Arc<AtomicBool>) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            observed: Arc::from([flag]),
        }
    }

    /// Whether this token or any linked source has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self
                .observed
                .iter()
                .any(|flag| flag.load(Ordering::Acquire))
    }

    /// Observe both tokens while retaining an independent cancellation flag.
    pub fn linked(left: &Self, right: &Self) -> Self {
        let mut observed = Vec::with_capacity(left.observed.len() + right.observed.len() + 2);
        observed.push(Arc::clone(&left.cancelled));
        observed.extend(left.observed.iter().cloned());
        observed.push(Arc::clone(&right.cancelled));
        observed.extend(right.observed.iter().cloned());
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            observed: observed.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_flag_should_cancel_without_polling_or_mutating_the_source() {
        let source = Arc::new(AtomicBool::new(false));
        let token = CancellationToken::observing(Arc::clone(&source));

        source.store(true, Ordering::Release);

        assert!(token.is_cancelled() && source.load(Ordering::Acquire));
    }
}
