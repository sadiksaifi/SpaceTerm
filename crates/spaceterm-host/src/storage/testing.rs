//! Real authority fault controls, compiled only for tests or test-support.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::directory::mutex;
use super::{Database, StorageFailure, vfs};

/// Actual I/O or commit boundary. A fault fires once after the selected skip count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultPoint {
    /// VFS xWrite, before the native write.
    Write,
    /// Main database xWrite, after its rollback journal is durable.
    DatabaseWrite,
    /// VFS xSync, before the native synchronization.
    Sync,
    /// Main database xSync, after the database pages have been written.
    DatabaseSync,
    /// Synchronizing a created or deleted journal entry.
    DirectorySync,
    /// macOS device barrier after journal deletion.
    #[cfg(target_os = "macos")]
    DeleteFullSync,
    /// All projection statements finished, before COMMIT.
    BeforeCommit,
    /// COMMIT finished, before the authority returns its receipt.
    AfterCommit,
}

pub(super) struct Barrier {
    pub(super) point: FaultPoint,
    pub(super) callback: Box<dyn FnOnce() + Send>,
}

/// Controls only this authority's retained VFS context. No pathname reopening.
#[derive(Clone)]
pub struct TestControl(Arc<vfs::Context>);
impl std::fmt::Debug for TestControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TestControl(..)")
    }
}
impl Database {
    /// Obtains fixture controls without extracting SQLite or filesystem handles.
    pub fn test_control(&self) -> TestControl {
        TestControl(self.registration.context.clone())
    }
}
impl TestControl {
    /// Fails the selected real boundary once, after `skip` matching invocations.
    pub fn fail_once(&self, point: FaultPoint, skip: usize) {
        *mutex(&self.0.faults) = Some((point, skip));
    }

    /// Runs a one-shot owned-fixture callback at the actual authority boundary.
    /// The callback must not panic across a VFS callback or reenter this authority.
    pub fn barrier(&self, point: FaultPoint, callback: impl FnOnce() + Send + 'static) {
        *mutex(&self.0.barrier) = Some(Barrier {
            point,
            callback: Box::new(callback),
        });
    }

    /// Whether the selected I/O failure has not yet fired.
    pub fn fault_pending(&self) -> bool {
        mutex(&self.0.faults).is_some()
    }

    /// Number of admitted callback SQL calls. Rejected writes cannot increment it.
    pub fn sql_calls(&self) -> u64 {
        self.0.sql_calls.load(Ordering::Relaxed)
    }

    /// Repeats real synchronization on the existing VFS-owned descriptor. This
    /// never clears uncertain durability or grants mutation/recovery authority.
    pub fn repeat_sync(&self) -> Result<(), StorageFailure> {
        self.0.directory.sync()?;
        let entry = mutex(&self.0.database)
            .upgrade()
            .ok_or(StorageFailure::Unavailable)?;
        entry.verify(&self.0.directory)?;
        // SAFETY: the VFS retains this regular-file descriptor throughout the call.
        if unsafe { libc::fsync(entry.fd()) } != 0 {
            return Err(super::directory::io_failure());
        }
        #[cfg(target_os = "macos")]
        if unsafe { libc::fcntl(entry.fd(), libc::F_FULLFSYNC) } != 0 {
            return Err(super::directory::io_failure());
        }
        Ok(())
    }
}
