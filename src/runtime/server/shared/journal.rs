//! Organization commits leave the `PaneService` lock as soon as they are ordered.
//!
//! A commit changes the in-memory state under the lock and hands the journal its envelope. The
//! write and its syncs happen later in [`Journal::sync`], off the lock. Each envelope is a whole
//! state that carries every earlier revision, so a writer writes only the newest pending envelope
//! and revisions become durable in submission order.
//!
//! A failed write leaves the retained record uncertain. Nothing more is written until the service
//! reads the record back with [`Journal::recover`]. Every revision submitted before that read and
//! not yet written is lost.
use spaceterm_server::{
    ServerError,
    organization::{Organization, OrganizationStore},
};
use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex, MutexGuard};

/// Where the organization is retained: the server's [`OrganizationStore`], or a test double.
pub(in crate::runtime::server) trait Retention: Send + Sync {
    /// Publishes `organization` durably, or fails with the record's state uncertain.
    fn write(&self, organization: &Organization) -> Result<(), ServerError>;
    /// Reads the retained organization and confirms that it is durable.
    fn read(&self) -> Result<Option<Organization>, ServerError>;
    /// Fences Worktree recovery while a Git write runs.
    fn worktree_lock(&self) -> Result<std::fs::File, ServerError>;
}

impl Retention for OrganizationStore {
    fn write(&self, organization: &Organization) -> Result<(), ServerError> {
        OrganizationStore::write(self, Some(organization))
    }
    fn read(&self) -> Result<Option<Organization>, ServerError> {
        OrganizationStore::read(self)
    }
    fn worktree_lock(&self) -> Result<std::fs::File, ServerError> {
        OrganizationStore::worktree_lock(self)
    }
}

/// How many recoveries keep their verdicts. A revision older than these is reported uncertain.
const RECOVERIES: usize = 16;

/// A commit's place in submission order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runtime::server) struct Revision {
    epoch: u64,
    number: u64,
}

/// The revision may or may not be retained. The service reads the record back before relying on
/// any retained state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runtime::server) struct Uncertain;

pub(in crate::runtime::server) struct Journal {
    store: Box<dyn Retention>,
    progress: Mutex<Progress>,
    written: Condvar,
}

#[derive(Default)]
struct Progress {
    /// Starts again after each recovery.
    epoch: u64,
    submitted: u64,
    /// The newest envelope no writer has taken yet.
    pending: Option<(u64, Organization)>,
    writing: bool,
    /// Every revision of this epoch up to here is retained.
    durable: u64,
    failed: bool,
    /// The last durable revision of each earlier epoch. Later revisions of that epoch were lost.
    ended: BTreeMap<u64, u64>,
}

impl Journal {
    pub(in crate::runtime::server) fn new(store: Box<dyn Retention>) -> Self {
        Self {
            store,
            progress: Mutex::default(),
            written: Condvar::new(),
        }
    }

    pub(in crate::runtime::server) fn worktree_lock(&self) -> Result<std::fs::File, ServerError> {
        self.store.worktree_lock()
    }

    fn progress(&self) -> MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Orders `envelope` after every earlier commit, without touching the disk.
    pub(in crate::runtime::server) fn submit(&self, envelope: Organization) -> Revision {
        let mut progress = self.progress();
        progress.submitted += 1;
        progress.pending = Some((progress.submitted, envelope));
        Revision {
            epoch: progress.epoch,
            number: progress.submitted,
        }
    }

    /// The newest submitted revision. Every outcome already in the state is retained with it.
    pub(in crate::runtime::server) fn latest(&self) -> Revision {
        let progress = self.progress();
        Revision {
            epoch: progress.epoch,
            number: progress.submitted,
        }
    }

    /// Returns once `revision` is retained. Writes the newest pending envelope unless another
    /// caller is already writing, in which case it waits for that write and checks again.
    pub(in crate::runtime::server) fn sync(&self, revision: Revision) -> Result<(), Uncertain> {
        let mut progress = self.progress();
        loop {
            if revision.epoch != progress.epoch {
                let durable = progress.ended.get(&revision.epoch).ok_or(Uncertain)?;
                return (revision.number <= *durable).then_some(()).ok_or(Uncertain);
            }
            if revision.number <= progress.durable {
                return Ok(());
            }
            if progress.failed {
                return Err(Uncertain);
            }
            if progress.writing {
                progress = self
                    .written
                    .wait(progress)
                    .unwrap_or_else(|e| e.into_inner());
                continue;
            }
            let (number, envelope) = progress.pending.take().ok_or(Uncertain)?;
            progress.writing = true;
            drop(progress);
            let result = self.store.write(&envelope);
            progress = self.progress();
            progress.writing = false;
            match result {
                Ok(()) => progress.durable = number,
                Err(_) => progress.failed = true,
            }
            self.written.notify_all();
        }
    }

    /// Whether nothing waits to be written and the last write succeeded.
    pub(in crate::runtime::server) fn settled(&self) -> bool {
        let progress = self.progress();
        !progress.failed && !progress.writing && progress.pending.is_none()
    }

    /// Whether a write failed and the record must be read back.
    pub(in crate::runtime::server) fn uncertain(&self) -> bool {
        self.progress().failed
    }

    /// Reads the record back after a failed write and confirms its durability. Call it only while
    /// [`Self::uncertain`]: no write starts then, so the read sees the final record.
    pub(in crate::runtime::server) fn recover(&self) -> Result<Option<Organization>, ServerError> {
        let record = self.store.read()?;
        let mut progress = self.progress();
        let (epoch, durable) = (progress.epoch, progress.durable);
        progress.ended.insert(epoch, durable);
        while progress.ended.len() > RECOVERIES {
            progress.ended.pop_first();
        }
        progress.epoch += 1;
        progress.submitted = 0;
        progress.durable = 0;
        progress.pending = None;
        progress.failed = false;
        Ok(record)
    }
}

#[cfg(test)]
pub(in crate::runtime::server) mod memory {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;

    /// Retains organization in memory. Writes can fail or wait for a release.
    #[derive(Default)]
    pub(in crate::runtime::server) struct Memory {
        record: Mutex<Option<Organization>>,
        pub writes: AtomicUsize,
        pub fail: AtomicBool,
        gate: Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl Memory {
        /// Holds the next write until the returned sender sends or drops.
        pub fn hold(&self) -> mpsc::Sender<()> {
            let (release, gate) = mpsc::channel();
            *self.gate.lock().unwrap() = Some(gate);
            release
        }
        pub fn retained(&self) -> Option<Organization> {
            self.record.lock().unwrap().clone()
        }
    }

    pub(in crate::runtime::server) struct Shared(pub Arc<Memory>);

    impl Retention for Shared {
        fn write(&self, organization: &Organization) -> Result<(), ServerError> {
            if let Some(gate) = self.0.gate.lock().unwrap().take() {
                let _ = gate.recv();
            }
            self.0.writes.fetch_add(1, Ordering::SeqCst);
            if self.0.fail.load(Ordering::SeqCst) {
                return Err(ServerError::StorageUnavailable);
            }
            *self.0.record.lock().unwrap() = Some(organization.clone());
            Ok(())
        }
        fn read(&self) -> Result<Option<Organization>, ServerError> {
            if self.0.fail.load(Ordering::SeqCst) {
                return Err(ServerError::StorageUnavailable);
            }
            Ok(self.0.retained())
        }
        fn worktree_lock(&self) -> Result<std::fs::File, ServerError> {
            std::fs::File::open("/dev/null").map_err(|_| ServerError::StorageUnavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memory::{Memory, Shared};
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    fn journal() -> (Arc<Journal>, Arc<Memory>) {
        let memory = Arc::new(Memory::default());
        (
            Arc::new(Journal::new(Box::new(Shared(Arc::clone(&memory))))),
            memory,
        )
    }

    fn named(name: &str) -> Organization {
        let mut organization = Organization::first("/".into()).unwrap();
        organization.name = name.into();
        organization
    }

    #[test]
    fn one_write_retains_every_revision_submitted_before_it_in_order() {
        let (journal, memory) = journal();
        let first = journal.submit(named("first"));
        let second = journal.submit(named("second"));
        let third = journal.submit(named("third"));
        assert!(!journal.settled());
        assert_eq!(journal.sync(first), Ok(()));
        assert_eq!(memory.writes.load(Ordering::SeqCst), 1);
        assert_eq!(memory.retained().unwrap().name, "third");
        assert_eq!(journal.sync(third), Ok(()));
        assert_eq!(journal.sync(second), Ok(()));
        assert_eq!(
            memory.writes.load(Ordering::SeqCst),
            1,
            "later revisions coalesce"
        );
        assert!(journal.settled());
    }

    #[test]
    fn a_revision_submitted_during_a_write_waits_for_its_own_write() {
        let (journal, memory) = journal();
        let release = memory.hold();
        let first = journal.submit(named("first"));
        let writer = {
            let journal = Arc::clone(&journal);
            std::thread::spawn(move || journal.sync(first))
        };
        while !journal.progress().writing {
            std::thread::yield_now();
        }
        let second = journal.submit(named("second"));
        let waiter = {
            let journal = Arc::clone(&journal);
            std::thread::spawn(move || journal.sync(second))
        };
        release.send(()).unwrap();
        assert_eq!(writer.join().unwrap(), Ok(()));
        assert_eq!(waiter.join().unwrap(), Ok(()));
        assert_eq!(memory.writes.load(Ordering::SeqCst), 2);
        assert_eq!(memory.retained().unwrap().name, "second");
    }

    #[test]
    fn a_failed_write_loses_unwritten_revisions_until_the_record_is_read_back() {
        let (journal, memory) = journal();
        let kept = journal.submit(named("kept"));
        journal.sync(kept).unwrap();
        memory.fail.store(true, Ordering::SeqCst);
        let failed = journal.submit(named("failed"));
        assert_eq!(journal.sync(failed), Err(Uncertain));
        let queued = journal.submit(named("queued"));
        assert!(journal.uncertain());
        assert_eq!(
            journal.sync(queued),
            Err(Uncertain),
            "nothing is written while uncertain"
        );
        assert_eq!(memory.writes.load(Ordering::SeqCst), 2);
        assert!(
            journal.recover().is_err(),
            "an unreadable record stays uncertain"
        );
        memory.fail.store(false, Ordering::SeqCst);
        assert_eq!(journal.recover().unwrap().unwrap().name, "kept");
        assert!(journal.settled());
        let later = journal.submit(named("later"));
        journal.sync(later).unwrap();
        assert_eq!(journal.sync(kept), Ok(()));
        assert_eq!(journal.sync(failed), Err(Uncertain));
        assert_eq!(
            journal.sync(queued),
            Err(Uncertain),
            "a later write never confirms a lost one"
        );
        assert_eq!(journal.sync(journal.latest()), Ok(()));
    }
}
