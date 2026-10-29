use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone)]
pub(crate) struct CancellationToken {
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
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[cfg(any(feature = "desktop", test))]
    pub(crate) fn observing(flag: Arc<AtomicBool>) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            observed: Arc::from([flag]),
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self
                .observed
                .iter()
                .any(|flag| flag.load(Ordering::Acquire))
    }

    pub(crate) fn linked(left: &Self, right: &Self) -> Self {
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

#[cfg(feature = "desktop")]
impl spaceterm_connections::setup::Cancellation for CancellationToken {
    fn is_cancelled(&self) -> bool {
        self.is_cancelled()
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
