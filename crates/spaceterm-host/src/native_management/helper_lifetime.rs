//! Independent retirement of the one-request privileged manager process.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::native_deployment::contracts::NativeFailure;

use super::controller::HELPER_LIFETIME;

/// One absolute monotonic deadline. Native calls must not restart its budget.
#[derive(Clone, Copy)]
pub struct HelperDeadline {
    expires: Instant,
}
impl HelperDeadline {
    /// Remaining time for a native poll, synchronization, or supervisor call.
    pub fn remaining(self) -> Result<Duration, NativeFailure> {
        self.expires
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(NativeFailure::TimedOut)
    }
    /// Reserves time before this helper's original exit deadline for exact child retirement.
    /// The returned budget is shorter and cannot extend the original lifetime.
    pub fn reserve_cleanup(self, reserve: Duration) -> Result<Self, NativeFailure> {
        let expires = self
            .expires
            .checked_sub(reserve)
            .ok_or(NativeFailure::TimedOut)?;
        let deadline = Self { expires };
        deadline.check()?;
        Ok(deadline)
    }
    /// Refuse another effect once the original request budget has elapsed.
    pub fn check(self) -> Result<(), NativeFailure> {
        self.remaining().map(|_| ())
    }
}

/// Retire this helper independently of a stalled synchronous native call.
///
/// This guard belongs only in the one-request manager process, never in the
/// server or a client facade. Timeout exits this process without signalling
/// unprivileged Git children. The existing journal remains unresolved; timeout
/// is not a completed transaction, rollback, or proof that an effect was absent.
/// Install it before reading or authorizing a request. Do not fork a replacement
/// helper, daemonize, or create another guard to extend the original deadline.
pub struct HelperLifetimeGuard {
    deadline: HelperDeadline,
    control: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}
impl HelperLifetimeGuard {
    /// Installs the manager's bounded wait before any potentially stalled call.
    pub fn install() -> Result<Self, NativeFailure> {
        Self::install_for(HELPER_LIFETIME)
    }
    /// Installs a bound chosen by trusted composition, never a request or environment value.
    pub fn install_for(lifetime: Duration) -> Result<Self, NativeFailure> {
        let expires = Instant::now()
            .checked_add(lifetime)
            .filter(|_| !lifetime.is_zero())
            .ok_or(NativeFailure::LimitExceeded)?;
        let deadline = HelperDeadline { expires };
        let control = Arc::new((Mutex::new(true), Condvar::new()));
        let worker_control = Arc::clone(&control);
        let worker = thread::Builder::new()
            .name("native-helper-deadline".into())
            .spawn(move || watch_deadline(deadline, &worker_control))
            .map_err(|_| NativeFailure::NativeUnavailable)?;
        Ok(Self {
            deadline,
            control,
            worker: Some(worker),
        })
    }
    /// The same deadline is supplied to every native backend call.
    pub fn deadline(&self) -> HelperDeadline {
        self.deadline
    }
}
impl Drop for HelperLifetimeGuard {
    fn drop(&mut self) {
        let (live, changed) = &*self.control;
        let Ok(mut live) = live.lock() else {
            retire(NativeFailure::NativeUnavailable);
        };
        *live = false;
        changed.notify_one();
        drop(live);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            retire(NativeFailure::NativeUnavailable);
        }
    }
}

fn watch_deadline(deadline: HelperDeadline, control: &(Mutex<bool>, Condvar)) {
    let (live, changed) = control;
    let Ok(mut live) = live.lock() else {
        retire(NativeFailure::NativeUnavailable);
    };
    while *live {
        let Ok(remaining) = deadline.remaining() else {
            retire(NativeFailure::TimedOut);
        };
        // Spurious wakeups never reset the absolute monotonic deadline.
        let Ok((next, _)) = changed.wait_timeout(live, remaining) else {
            retire(NativeFailure::NativeUnavailable);
        };
        live = next;
    }
}

fn retire(failure: NativeFailure) -> ! {
    // SAFETY: _exit retires only this process, without unwinding a blocked native
    // call, flushing inherited streams, signalling children, or fabricating a
    // durable outcome. No PID lookup or privilege-restoring action occurs here.
    unsafe { libc::_exit(failure as i32) }
}
