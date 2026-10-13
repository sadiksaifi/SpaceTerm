//! Bounded worker joins and retained Session cleanup, shared by both runtime factories.
use super::budget::{BudgetLease, RuntimeRegistration};
use super::ports::{PendingPtyCleanup, RuntimeFailure, RuntimeStage};
use spaceterm_core::organization::FailureClass;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SERVICE_TICK: Duration = Duration::from_millis(10);
const PROOF_SPACING: Duration = Duration::from_millis(250);
const SESSION_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const MECHANICAL_POLL_INTERVAL: Duration = Duration::from_millis(100);

enum Capacity {
    Legacy(BudgetLease),
    Server(RuntimeRegistration),
}
/// Dropping an unresolved claim preserves accounting without leaking its Arc.
pub(crate) struct RetainedCapacity(Option<Capacity>);
impl RetainedCapacity {
    pub fn legacy(lease: BudgetLease) -> Self {
        Self(Some(Capacity::Legacy(lease)))
    }
    pub fn server(mut registration: RuntimeRegistration) -> Self {
        registration.quarantine();
        Self(Some(Capacity::Server(registration)))
    }
    pub fn release(mut self) {
        if let Some(Capacity::Server(registration)) = &mut self.0 {
            registration.confirm_cleanup();
        }
        drop(self.0.take());
    }
    pub fn combine_legacy(&mut self, mut other: Self) {
        if let Some(Capacity::Legacy(lease)) = &mut self.0
            && let Some(Capacity::Legacy(other)) = other.0.take()
        {
            *lease = std::mem::take(lease).combine(other);
        }
    }
}
impl Drop for RetainedCapacity {
    fn drop(&mut self) {
        if let Some(Capacity::Legacy(lease)) = self.0.take() {
            lease.quarantine();
        }
        // A quarantined RuntimeRegistration disarms its grid lease on Drop.
    }
}

/// Original PTY resources retain the native Session identity until both the
/// absence certificate and actual mechanical completion permit discharge.
pub(crate) struct CleanupObligation {
    mechanics: Option<PendingPtyCleanup>,
    capacity: RetainedCapacity,
    confirmed: Box<dyn FnOnce() + Send>,
    next_attempt: Option<Instant>,
    next_poll: Instant,
}
impl CleanupObligation {
    pub fn new(
        mechanics: Option<PendingPtyCleanup>,
        capacity: RetainedCapacity,
        confirmed: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            mechanics,
            capacity,
            confirmed: Box::new(confirmed),
            next_attempt: Some(Instant::now() + SESSION_RETRY_INTERVAL),
            next_poll: Instant::now(),
        }
    }
    fn poll_mechanics(&mut self, now: Instant) -> bool {
        let Some(mechanics) = &mut self.mechanics else {
            return false;
        };
        if self.next_poll <= now {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mechanics.poll_mechanics()))
                .is_err()
            {
                mechanics.fail_mechanics();
            }
            self.next_poll = Instant::now() + MECHANICAL_POLL_INTERVAL;
        }
        mechanics.can_release()
    }
    fn due(&self, now: Instant) -> bool {
        self.mechanics
            .as_ref()
            .is_some_and(PendingPtyCleanup::can_retry)
            && self.next_attempt.is_some_and(|attempt| attempt <= now)
    }
    fn retry(&mut self) -> bool {
        let Some(mechanics) = &mut self.mechanics else {
            return false;
        };
        mechanics.retry();
        self.next_attempt = mechanics
            .can_retry()
            .then(|| Instant::now() + SESSION_RETRY_INTERVAL);
        mechanics.can_release()
    }
    fn dispose_completed(&mut self) -> bool {
        // Native destructors are outside the shutdown gate and precede any
        // capacity discharge. An unwind leaves this obligation fail-closed.
        let mechanics = self.mechanics.take();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(mechanics))).is_ok()
    }
    fn discharge(self) {
        self.capacity.release();
        (self.confirmed)();
    }
}

pub(crate) struct WorkerRegistration {
    pub thread: JoinHandle<()>,
    /// Must not retain the factory or a registry sender. `false` never authorizes reproof.
    pub complete: Box<dyn FnOnce(bool) -> Option<CleanupObligation> + Send>,
}
enum Entry {
    Worker(WorkerRegistration),
    Cleanup(CleanupObligation),
}
struct Sender {
    sender: mpsc::SyncSender<Entry>,
    stopped: Arc<Mutex<bool>>,
}
impl Drop for Sender {
    fn drop(&mut self) {
        // The service never holds this gate during a native callback. Shutdown
        // does not join the service, and a late receipt cannot discharge a claim.
        *self.stopped.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }
}
#[derive(Clone)]
pub(crate) struct CleanupRegistry(Arc<Sender>);
impl CleanupRegistry {
    pub fn new(capacity: usize) -> Result<Self, RuntimeFailure> {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let stopped = Arc::new(Mutex::new(false));
        let service_stopped = Arc::clone(&stopped);
        thread::Builder::new()
            .name("terminal-legacy-cleanup".into())
            .spawn(move || service(receiver, service_stopped))
            .map_err(|_| {
                RuntimeFailure::new(FailureClass::RuntimeUnavailable, RuntimeStage::Cleanup)
            })?;
        Ok(Self(Arc::new(Sender { sender, stopped })))
    }
    pub fn register_worker(&self, registration: WorkerRegistration) -> Result<(), RuntimeFailure> {
        if let Err(error) = self.0.sender.send(Entry::Worker(registration)) {
            if let Entry::Worker(registration) = error.0 {
                let joined = registration.thread.join().is_ok();
                drop((registration.complete)(joined));
            }
            return Err(RuntimeFailure::new(
                FailureClass::RuntimeUnavailable,
                RuntimeStage::Cleanup,
            ));
        }
        Ok(())
    }
    pub fn retain(&self, obligation: CleanupObligation) {
        // Capacity bounds both queued and retained entries. Send failure drops
        // the obligation, which preserves rather than discharges its claims.
        let _ = self.0.sender.send(Entry::Cleanup(obligation));
    }
}
fn service(receiver: mpsc::Receiver<Entry>, stopped: Arc<Mutex<bool>>) {
    let mut workers = Vec::<WorkerRegistration>::new();
    let mut obligations = std::collections::VecDeque::<CleanupObligation>::new();
    let mut next_proof = Instant::now();
    loop {
        if *stopped.lock().unwrap_or_else(|e| e.into_inner()) {
            break;
        }
        match receiver.recv_timeout(SERVICE_TICK) {
            Ok(Entry::Worker(worker)) => workers.push(worker),
            Ok(Entry::Cleanup(obligation)) => obligations.push_back(obligation),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let mut index = 0;
        while index < workers.len() {
            if !workers[index].thread.is_finished() {
                index += 1;
                continue;
            }
            let registration = workers.swap_remove(index);
            let joined = registration.thread.join().is_ok();
            if let Some(obligation) = (registration.complete)(joined) {
                obligations.push_back(obligation);
            }
        }
        let now = Instant::now();
        for _ in 0..obligations.len() {
            let Some(mut obligation) = obligations.pop_front() else {
                break;
            };
            if obligation.poll_mechanics(now) {
                finish_confirmed(obligation, &stopped, &mut obligations);
            } else {
                obligations.push_back(obligation);
            }
        }
        if now < next_proof {
            continue;
        }
        // Round-robin search visits a bounded number of capacity-backed entries.
        // At most one native attempt runs in a service turn, with global spacing.
        for _ in 0..obligations.len() {
            let Some(mut obligation) = obligations.pop_front() else {
                break;
            };
            if !obligation.due(now) {
                obligations.push_back(obligation);
                continue;
            }
            let confirmed = obligation.retry();
            next_proof = Instant::now() + PROOF_SPACING;
            let stopping = stopped.lock().unwrap_or_else(|e| e.into_inner());
            if *stopping {
                obligations.push_front(obligation);
                break;
            }
            drop(stopping);
            if confirmed {
                finish_confirmed(obligation, &stopped, &mut obligations);
            } else {
                obligations.push_back(obligation);
            }
            break;
        }
    }
    // Do not wait for a native callback or an unfinished worker on shutdown.
    // Every remaining capacity wrapper drops fail-closed.
    for entry in receiver.try_iter() {
        match entry {
            Entry::Worker(registration) => workers.push(registration),
            Entry::Cleanup(obligation) => obligations.push_back(obligation),
        }
    }
    for registration in workers {
        let joined = registration.thread.is_finished() && registration.thread.join().is_ok();
        if let Some(obligation) = (registration.complete)(joined) {
            obligations.push_back(obligation);
        }
    }
    // Publish every worker completion before disposing any retained native
    // resource. Native disposal stays on this existing background thread.
    drop(obligations);
}

fn finish_confirmed(
    mut obligation: CleanupObligation,
    stopped: &Mutex<bool>,
    pending: &mut std::collections::VecDeque<CleanupObligation>,
) {
    if *stopped.lock().unwrap_or_else(|e| e.into_inner()) {
        // Preserve observer-first shutdown disposal when stopping is already known.
        pending.push_back(obligation);
        return;
    }
    let disposed = obligation.dispose_completed();
    let stopping = stopped.lock().unwrap_or_else(|e| e.into_inner());
    if disposed && !*stopping {
        obligation.discharge();
    } else {
        // Failed disposal permanently disables reproof; shutdown never accepts
        // a receipt that completed while native disposal was in flight.
        pending.push_back(obligation);
    }
}
