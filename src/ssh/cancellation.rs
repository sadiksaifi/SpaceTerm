use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone)]
pub(crate) struct SshCancellationToken {
    cancelled: Arc<AtomicBool>,
    observed: Arc<[Arc<AtomicBool>]>,
}

impl Default for SshCancellationToken {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            observed: Arc::from([]),
        }
    }
}

impl SshCancellationToken {
    pub(crate) fn cancelled() -> Self {
        let token = Self::default();
        token.cancel();
        token
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
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
