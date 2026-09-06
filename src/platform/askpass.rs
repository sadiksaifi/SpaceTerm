//! Injected AskPass construction and lifetime authority. Transport stays private to the host.
use super::app_paths::AppPaths;
use gpui::{App, Window};
use std::ffi::OsStr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct AskPassUnavailable;
pub(crate) trait AskPassWindowFactory: Send + Sync {
    fn create(
        &self,
        window: &Window,
        cx: &mut App,
    ) -> Result<Arc<dyn AskPassAttemptFactory>, AskPassUnavailable>;
}
pub(crate) trait AskPassAttemptFactory: Send + Sync {
    fn start_attempt(&self, paths: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable>;
}
pub(crate) struct AskPassAttempt {
    pub(crate) lease: AskPassBrokerLease,
    pub(crate) observation: AskPassAttemptObservation,
}
pub(crate) trait AskPassLease: Send + Sync {
    fn entries(&self) -> Vec<(&'static str, &OsStr)>;
    fn cancel(&self);
}
#[derive(Clone)]
pub(crate) struct AskPassBrokerLease(Arc<dyn AskPassLease>);
impl AskPassBrokerLease {
    pub(crate) fn new(lease: Arc<dyn AskPassLease>) -> Self {
        Self(lease)
    }
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&'static str, &OsStr)> {
        self.0.entries().into_iter()
    }
    pub(crate) fn cancel(&self) {
        self.0.cancel();
    }
}
#[derive(Clone, Default)]
/// Content-free observation of authentication prompt activity and user cancellation.
///
/// It is scoped to one connection attempt and carries no prompt or response bytes.
pub(crate) struct AskPassAttemptObservation {
    pub(super) state: Arc<AskPassAttemptObservationState>,
}

#[derive(Default)]
pub(super) struct AskPassAttemptObservationState {
    pub(super) prompt_started: AtomicBool,
    pub(super) prompt_active: AtomicBool,
    pub(super) cancelled: Arc<AtomicBool>,
}

impl AskPassAttemptObservation {
    /// Reports whether any prompt in this attempt reached the presenter.
    #[cfg(test)]
    pub(crate) fn prompt_started(&self) -> bool {
        self.state.prompt_started.load(Ordering::Acquire)
    }

    /// Reports whether an authentication prompt is currently active for this attempt.
    pub(crate) fn prompt_active(&self) -> bool {
        self.state.prompt_active.load(Ordering::Acquire)
    }

    /// Reports whether any prompt in this attempt was cancelled.
    pub(crate) fn cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn cancellation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.state.cancelled)
    }
}
