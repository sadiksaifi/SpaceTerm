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

struct InitializationHook {
    boundary: u8,
    operation: Box<dyn FnOnce()>,
}
thread_local! {
    static INITIALIZATION_INTERRUPT: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
    static INITIALIZATION_HOOK: std::cell::RefCell<Option<InitializationHook>> = const { std::cell::RefCell::new(None) };
}
/// Interrupts only one original first-install storage/seed boundary.
/// 0: bound creation, 1: before migration, 2: migrated, 3: before seed,
/// 4: seeded, 5: creation intent, 6: created before identity publication.
pub fn interrupt_initialization_at(boundary: u8) {
    INITIALIZATION_INTERRUPT.set(Some(boundary));
}
/// Runs a controlled filesystem fault at one original creation boundary.
pub fn after_initialization_boundary(boundary: u8, operation: impl FnOnce() + 'static) {
    INITIALIZATION_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(InitializationHook {
            boundary,
            operation: Box::new(operation),
        })
    });
}
pub(crate) fn initialization_boundary(boundary: u8) -> Result<(), StorageFailure> {
    let operation = INITIALIZATION_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        if hook.as_ref().is_some_and(|hook| hook.boundary == boundary) {
            hook.take()
        } else {
            None
        }
    });
    if let Some(hook) = operation {
        (hook.operation)();
    }
    if INITIALIZATION_INTERRUPT.get() == Some(boundary) {
        INITIALIZATION_INTERRUPT.set(None);
        Err(StorageFailure::Io)
    } else {
        Ok(())
    }
}
