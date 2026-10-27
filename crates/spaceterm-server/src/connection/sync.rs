//! One serialized worker per server for work that must not run under the `PaneService` lock or on
//! a connection's I/O thread: disk syncs and direct-network administration.
//!
//! Jobs run one at a time in submission order. Submitting under the `PaneService` lock therefore
//! fixes the order of syncs across every connection: a revision submitted first is renamed first.
//! A job replies through the thread-safe [`Outbox`], which wakes the I/O thread. I/O threads never
//! run these jobs.
use super::outbox::{Outbox, OutboxError};
use crate::protocol::{Refusal, ReplyBody, RequestId};
use std::sync::mpsc;

type Job = Box<dyn FnOnce() + Send>;

/// Clones share one worker thread, which ends when the last clone drops.
#[derive(Clone)]
pub struct SyncWorker {
    jobs: mpsc::Sender<Job>,
}

/// The worker thread is gone, so the job did not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the sync worker stopped")]
pub struct Stopped;

impl SyncWorker {
    pub fn spawn() -> std::io::Result<(Self, std::thread::JoinHandle<()>)> {
        let (jobs, queue) = mpsc::channel::<Job>();
        let thread = std::thread::Builder::new()
            .name("spaceterm-sync".into())
            .spawn(move || {
                for job in queue {
                    job();
                }
            })?;
        Ok((Self { jobs }, thread))
    }

    /// Queues `job` behind every job submitted before it.
    pub fn submit(&self, job: impl FnOnce() + Send + 'static) -> Result<(), Stopped> {
        self.jobs.send(Box::new(job)).map_err(|_| Stopped)
    }

    /// Runs `work` in order and queues its result as the reply to `id`. If the worker is gone,
    /// replies `Failed` at once, so the reply id is never lost.
    pub fn reply(
        &self,
        outbox: &Outbox,
        id: RequestId,
        work: impl FnOnce() -> Result<ReplyBody, Refusal> + Send + 'static,
    ) -> Result<(), OutboxError> {
        let replier = outbox.clone();
        let job = move || {
            // A closing connection no longer needs the reply.
            let _ = replier.reply_or_refuse(id, work());
        };
        match self.submit(job) {
            Ok(()) => Ok(()),
            Err(Stopped) => outbox.reply(id, Err(Refusal::Failed)),
        }
    }

    /// Blocks until every job submitted before this call has run, for example before shutdown.
    pub fn flush(&self) -> Result<(), Stopped> {
        let (done, finished) = mpsc::channel();
        self.submit(move || {
            let _ = done.send(());
        })?;
        finished.recv().map_err(|_| Stopped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn jobs_run_in_submission_order_off_the_caller_thread() {
        let (worker, thread) = SyncWorker::spawn().unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let caller = std::thread::current().id();
        for revision in 0..32 {
            let order = Arc::clone(&order);
            worker
                .submit(move || {
                    assert_ne!(std::thread::current().id(), caller);
                    order.lock().unwrap().push(revision);
                })
                .unwrap();
        }
        worker.flush().unwrap();
        assert_eq!(*order.lock().unwrap(), (0..32).collect::<Vec<_>>());
        drop(worker);
        thread.join().unwrap();
    }
}
