use super::*;
use spaceterm_core::operations::OperationId;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

// Restricted revocation retries are slower than ordinary connection retries.
const REVOCATION_RETRY_MINIMUM: Duration = Duration::from_secs(1);
const REVOCATION_RETRY_MAXIMUM: Duration = Duration::from_secs(15 * 60);

struct Claim(Arc<AtomicBool>);
impl Drop for Claim {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Runtime ownership is separate from notices or the ordinary connection registry.
/// Shutdown drains the exact driver's in-progress native work, not a duplicate
/// cleanup attempt. Dropping a waiter does not drop the worker's join ownership.
pub struct PendingRuntime {
    stop: watch::Sender<bool>,
    done: watch::Receiver<Option<Result<(), ClientFailure>>>,
}
impl PendingRuntime {
    pub async fn shutdown(self) -> Result<(), ClientFailure> {
        self.stop.send_replace(true);
        let mut done = self.done.clone();
        loop {
            if let Some(result) = *done.borrow_and_update() {
                return result;
            }
            done.changed()
                .await
                .map_err(|_| ClientFailure::new(ClientFailureClass::Cancelled))?;
        }
    }
}
impl Drop for PendingRuntime {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

pub(super) fn start(
    tasks: PendingTasks,
    attempt: impl Fn(OperationId) -> ClientFuture<'static, bool> + Send + 'static,
) -> Result<PendingRuntime, ClientFailure> {
    start_with_enrollment(tasks, attempt, None)
}

pub(super) type EnrollmentAcknowledgementAttempt =
    Arc<dyn Fn(OperationId) -> ClientFuture<'static, ()> + Send + Sync>;

pub(super) fn start_with_enrollment(
    tasks: PendingTasks,
    attempt: impl Fn(OperationId) -> ClientFuture<'static, bool> + Send + 'static,
    enrollment: Option<EnrollmentAcknowledgementAttempt>,
) -> Result<PendingRuntime, ClientFailure> {
    let claim = Claim(tasks.journal.claim_scheduler()?);
    let (stop, stopped) = watch::channel(false);
    let (finished, done) = watch::channel(None);
    let worker = tokio::spawn(async move {
        let _claim = claim;
        run(tasks, attempt, enrollment, stopped).await
    });
    tokio::spawn(async move {
        let result = worker
            .await
            .map_err(|_| ClientFailure::new(ClientFailureClass::Cancelled))
            .and_then(|result| result);
        finished.send_replace(Some(result));
    });
    Ok(PendingRuntime { stop, done })
}

async fn run(
    tasks: PendingTasks,
    attempt: impl Fn(OperationId) -> ClientFuture<'static, bool> + Send + 'static,
    enrollment: Option<EnrollmentAcknowledgementAttempt>,
    mut stopped: watch::Receiver<bool>,
) -> Result<(), ClientFailure> {
    let mut revisions = tasks.journal.revisions();
    let mut deadlines = BTreeMap::<OperationId, Instant>::new();
    let mut enrollment_attempts = BTreeMap::<OperationId, u32>::new();
    loop {
        if *stopped.borrow() {
            return Ok(());
        }
        let snapshot = tasks.journal.snapshot()?;
        deadlines.retain(|id, _| {
            snapshot
                .revocations()
                .iter()
                .any(|record| record.task.parts().original.operation() == *id)
                || enrollment.is_some()
                    && snapshot
                        .pairing()
                        .acknowledgements()
                        .iter()
                        .any(|work| work.prepared.enrollment.id == *id)
        });
        enrollment_attempts.retain(|id, _| deadlines.contains_key(id));
        if let Some(enrollment) = &enrollment {
            for work in snapshot.pairing().acknowledgements() {
                let id = work.prepared.enrollment.id;
                let deadline = deadlines.entry(id).or_insert_with(Instant::now);
                if *deadline > Instant::now() {
                    continue;
                }
                // Acknowledge the original durable receipt before any revocation
                // can delete its retained authentication credential.
                let result = enrollment(id).await;
                if *stopped.borrow() {
                    return Ok(());
                }
                tasks.journal.ensure_healthy()?;
                if result.is_ok() {
                    deadlines.remove(&id);
                    enrollment_attempts.remove(&id);
                } else {
                    let attempts = enrollment_attempts.entry(id).or_default();
                    *attempts = attempts.saturating_add(1);
                    deadlines.insert(id, Instant::now() + delay(id, *attempts));
                }
            }
        }
        for record in snapshot.revocations() {
            let id = record.task.parts().original.operation();
            let deadline = deadlines.entry(id).or_insert_with(|| {
                let now = Instant::now();
                if record.attempts != 0
                    && matches!(record.phase, RevocationPhase::AwaitingConfirmation)
                {
                    now + delay(id, record.attempts)
                } else {
                    now
                }
            });
            if *deadline > Instant::now() {
                continue;
            }
            // Do not cancel a driver halfway through owned native cleanup. The
            // driver bounds network readiness and joins its exact native effects.
            let result = attempt(id).await;
            if *stopped.borrow() {
                return Ok(());
            }
            tasks.journal.ensure_healthy()?;
            if matches!(result, Ok(true)) {
                deadlines.remove(&id);
            } else {
                let attempts = tasks
                    .journal
                    .snapshot()?
                    .revocations()
                    .iter()
                    .find(|record| record.task.parts().original.operation() == id)
                    .map_or(record.attempts, |record| record.attempts);
                deadlines.insert(id, Instant::now() + delay(id, attempts));
            }
        }
        let deadline = deadlines.values().min().copied();
        tokio::select! {
            _ = stopped.changed() => {},
            _ = revisions.changed() => {},
            _ = async {
                if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; }
            } => {},
        }
    }
}
fn delay(id: OperationId, attempts: u32) -> Duration {
    let multiplier = 1u64 << attempts.saturating_sub(1).min(10);
    let base = (REVOCATION_RETRY_MINIMUM.as_millis() as u64 * multiplier)
        .min(REVOCATION_RETRY_MAXIMUM.as_millis() as u64);
    let jitter = u64::from(id.stream().server().bytes()[0]) * base / 2560;
    Duration::from_millis((base + jitter).min(REVOCATION_RETRY_MAXIMUM.as_millis() as u64))
}
