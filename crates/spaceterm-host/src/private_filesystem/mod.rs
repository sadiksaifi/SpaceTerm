//! Account-private files using the desktop's descriptor-relative secure mechanics.
//!
//! Existing directories are opened without permission repair. These helpers are
//! not used for SQLite files, which retain their separate descriptor authority.

#[cfg(target_os = "linux")]
#[allow(
    dead_code,
    reason = "Shared source also supplies desktop updater operations."
)]
#[path = "../../../../src/platform/linux_atomic_rename.rs"]
mod linux_atomic_rename;
#[cfg(target_os = "macos")]
#[path = "../../../../src/platform/macos_atomic_rename.rs"]
mod macos_atomic_rename;
#[allow(dead_code, reason = "Shared source retains desktop-only operations.")]
#[path = "../../../../src/platform/secure_filesystem.rs"]
mod secure_filesystem;
#[allow(dead_code, reason = "Shared source retains desktop-only operations.")]
#[path = "unix.rs"]
mod unix_secure_filesystem;

use std::{ffi::OsStr, path::Path};

use secure_filesystem::{
    SecureCommitOutcome, SecureDirectory, SecureEntryIdentity, SecureFilesystem,
    SecureFilesystemError,
};
use unix_secure_filesystem::UnixSecureFilesystem;
use zeroize::Zeroizing;

// Storage uses the same atomic no-replace primitive for publishing its own
// exclusive-created inode. Database descriptors remain owned by storage/VFS.
pub(crate) fn rename_noreplace_at(
    directory: &std::fs::File,
    from: &std::ffi::CStr,
    to: &std::ffi::CStr,
) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    use linux_atomic_rename as atomic_rename;
    #[cfg(target_os = "macos")]
    use macos_atomic_rename as atomic_rename;
    atomic_rename::rename_noreplace_at(directory, from, to)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Failure {
    Missing,
    AlreadyExists,
    Conflict,
    LimitExceeded,
    Unsafe,
    Unavailable,
    Indeterminate,
    Busy,
}

impl From<SecureFilesystemError> for Failure {
    fn from(error: SecureFilesystemError) -> Self {
        match error {
            SecureFilesystemError::Missing => Self::Missing,
            SecureFilesystemError::AlreadyExists => Self::AlreadyExists,
            SecureFilesystemError::Conflict => Self::Conflict,
            SecureFilesystemError::TooLarge => Self::LimitExceeded,
            SecureFilesystemError::Unsafe => Self::Unsafe,
            SecureFilesystemError::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Directory(
    SecureDirectory,
    std::sync::Arc<unix_secure_filesystem::InitializationAuthority>,
);

pub(crate) struct Lease(unix_secure_filesystem::PrivateLease);
impl Lease {
    pub(crate) fn verify(&self) -> Result<(), Failure> {
        self.0.verify()
    }
}

pub(crate) struct Snapshot {
    pub(crate) bytes: Zeroizing<Vec<u8>>,
    pub(crate) identity: SecureEntryIdentity,
}

impl Directory {
    pub(crate) fn open(path: &Path) -> Result<Option<Self>, Failure> {
        #[cfg(feature = "test-support")]
        test_support::opening_directory(path);
        unix_secure_filesystem::admit_existing(path)?
            .map(|(value, authority)| {
                let directory = Self(value, std::sync::Arc::new(authority));
                directory.verify()?;
                Ok(directory)
            })
            .transpose()
    }

    // This authority remains closed to ordinary reads and writes. Only the
    // explicit original-phase recovery operations may use its retained descriptor.
    pub(crate) fn open_for_recovery(path: &Path) -> Result<Option<Self>, Failure> {
        unix_secure_filesystem::admit_existing(path)?
            .map(|(handle, authority)| Self(handle, std::sync::Arc::new(authority)))
            .map_or(Ok(None), |directory| {
                directory.1.verify()?;
                Ok(Some(directory))
            })
    }
    pub(crate) fn read_for_recovery(
        &self,
        name: &str,
        maximum: usize,
    ) -> Result<Option<Snapshot>, Failure> {
        self.1.verify()?;
        unix_secure_filesystem::read_for_recovery(&self.0, OsStr::new(name), maximum)
    }

    pub(crate) fn create(path: &Path) -> Result<Self, Failure> {
        // Never use ensure to repair an existing directory. The shared helper
        // intentionally repairs desktop settings directories on explicit writes.
        if let Some(directory) = Self::open(path)? {
            return Ok(directory);
        }
        let created = unix_secure_filesystem::create_without_repair(path)?;
        let directory = Self::open(path)?.ok_or(Failure::Indeterminate)?;
        unix_secure_filesystem::verify_same_directory(&created, &directory.0)?;
        Ok(directory)
    }

    pub(crate) fn verify(&self) -> Result<(), Failure> {
        self.1.verify()?;
        unix_secure_filesystem::verify_directory_acl(&self.0)
    }

    pub(crate) fn verify_established(&self) -> Result<(), Failure> {
        self.1.verify()?;
        unix_secure_filesystem::verify_established(&self.0)
    }

    pub(crate) fn read(&self, name: &str, maximum: usize) -> Result<Option<Snapshot>, Failure> {
        self.verify()?;
        let snapshot = unix_secure_filesystem::read_zeroizing(&self.0, OsStr::new(name), maximum)?;
        if snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.bytes.len() > maximum)
        {
            return Err(Failure::LimitExceeded);
        }
        Ok(snapshot)
    }

    pub(crate) fn publish(
        &self,
        name: &str,
        bytes: &[u8],
        expected: Option<&Snapshot>,
    ) -> Result<(), Failure> {
        self.publish_retained(name, bytes, expected).map(|_| ())
    }

    pub(crate) fn publish_retained(
        &self,
        name: &str,
        bytes: &[u8],
        expected: Option<&Snapshot>,
    ) -> Result<Snapshot, Failure> {
        self.verify()?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| Failure::Unavailable)?;
        let mut mutation = unix_secure_filesystem::begin_publication(
            &self.0,
            OsStr::new(name),
            bytes,
            expected.map(|snapshot| &snapshot.identity),
        )?;
        #[cfg(feature = "test-support")]
        test_support::publication_before_effect()?;
        let prepared =
            UnixSecureFilesystem.prepare_private_file(&self.0, OsStr::new(name), bytes, nonce)?;
        unix_secure_filesystem::verify_prepared_acl(&prepared)?;
        let result = UnixSecureFilesystem
            .commit_private_file(prepared, expected.map(|snapshot| &snapshot.identity))?;
        #[cfg(feature = "test-support")]
        let result = {
            let mut result = result;
            if result.outcome == SecureCommitOutcome::Committed
                && test_support::publication_unsynced()
            {
                result.outcome = SecureCommitOutcome::CommittedButUnsynced;
            }
            result
        };
        #[cfg(feature = "test-support")]
        if result.outcome == SecureCommitOutcome::Committed {
            test_support::published();
        }
        match result.outcome {
            SecureCommitOutcome::Committed => {
                let identity = result.published_identity.ok_or(Failure::Indeterminate)?;
                unix_secure_filesystem::finish_mutation(&mut mutation)?;
                Ok(Snapshot {
                    bytes: Zeroizing::new(Vec::new()),
                    identity,
                })
            }
            SecureCommitOutcome::Conflict => {
                // Record the proven no-effect result before checkpoint completion.
                unix_secure_filesystem::abort_publication(&mut mutation)?;
                Err(Failure::Conflict)
            }
            SecureCommitOutcome::CommittedButUnsynced => Err(Failure::Indeterminate),
        }
    }

    pub(crate) fn lease(&self, name: &str) -> Result<Lease, Failure> {
        self.verify()?;
        unix_secure_filesystem::private_lease(&self.0, OsStr::new(name)).map(Lease)
    }

    // Explicit recovery is never invoked by read admission. The validator checks
    // original application bindings before a fresh publication establishes durability.
    pub(crate) fn reconcile_publication(
        path: &Path,
        name: &str,
        maximum: usize,
        validate: impl FnOnce(&[u8]) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        unix_secure_filesystem::reconcile_publication(path, OsStr::new(name), maximum, validate)
    }

    pub(crate) fn reconcile_retained_publication(
        &self,
        name: &str,
        maximum: usize,
        validate: impl FnOnce(&[u8]) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        self.1.verify()?;
        unix_secure_filesystem::reconcile_retained_publication(
            &self.0,
            OsStr::new(name),
            maximum,
            validate,
        )?;
        self.1.verify()
    }

    pub(crate) fn reconcile_initialization(path: &Path) -> Result<(), Failure> {
        unix_secure_filesystem::reconcile_initialization(path)
    }
    pub(crate) fn reconcile_removal(
        &self,
        name: &str,
        maximum: usize,
        validate: impl FnOnce(&[u8]) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        self.1.verify()?;
        unix_secure_filesystem::reconcile_removal(&self.0, OsStr::new(name), maximum, validate)
    }

    pub(crate) fn remove(&self, name: &str, expected: &Snapshot) -> Result<(), Failure> {
        self.verify()?;
        let mut mutation =
            unix_secure_filesystem::begin_removal(&self.0, OsStr::new(name), &expected.identity)?;
        let result =
            unix_secure_filesystem::remove_and_sync(&self.0, OsStr::new(name), &expected.identity);
        if matches!(result, Ok(()) | Err(Failure::Missing | Failure::Conflict)) {
            unix_secure_filesystem::finish_mutation(&mut mutation)?;
        }
        result
    }
}

#[cfg(feature = "test-support")]
pub(crate) mod test_support {
    use std::cell::RefCell;

    thread_local! {
        static AFTER_PUBLICATION: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static FAIL_PUBLICATION_BEFORE_EFFECT: RefCell<bool> = const { RefCell::new(false) };
        static FAIL_REMOVAL_SYNC: RefCell<bool> = const { RefCell::new(false) };
        static FAIL_PUBLICATION_DURABILITY: RefCell<bool> = const { RefCell::new(false) };
        static FAIL_CREATION_BEFORE_EFFECT: RefCell<bool> = const { RefCell::new(false) };
        static FAIL_CREATION_PARENT_SYNC: RefCell<bool> = const { RefCell::new(false) };
        static FAIL_MUTATION_COMPLETION_SYNC: RefCell<bool> = const { RefCell::new(false) };
        static FAIL_COMPLETION_AFTER_SYNC: RefCell<bool> = const { RefCell::new(false) };
        static COMPLETION_IO_FAILED: RefCell<bool> = const { RefCell::new(false) };
        static OPEN_DIRECTORY: RefCell<Option<DirectoryOpenHook>> = const { RefCell::new(None) };
    }

    struct DirectoryOpenHook {
        path: std::path::PathBuf,
        operation: Box<dyn FnOnce()>,
    }

    pub(crate) fn before_next_directory_open(
        path: &std::path::Path,
        operation: impl FnOnce() + 'static,
    ) {
        OPEN_DIRECTORY.with(|slot| {
            assert!(slot.borrow().is_none(), "directory hook already armed");
            *slot.borrow_mut() = Some(DirectoryOpenHook {
                path: path.to_owned(),
                operation: Box::new(operation),
            });
        });
    }

    pub(super) fn opening_directory(path: &std::path::Path) {
        let operation = OPEN_DIRECTORY.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|hook| hook.path == path) {
                slot.take()
            } else {
                None
            }
        });
        if let Some(hook) = operation {
            (hook.operation)();
        }
    }

    pub(crate) fn fail_next_completion_and_rollback_write() {
        fail_next_mutation_completion_sync();
        FAIL_COMPLETION_AFTER_SYNC.with(|slot| *slot.borrow_mut() = true);
    }

    pub(super) struct CompletionIoScope;
    impl Drop for CompletionIoScope {
        fn drop(&mut self) {
            FAIL_MUTATION_COMPLETION_SYNC.with(|slot| slot.replace(false));
            FAIL_COMPLETION_AFTER_SYNC.with(|slot| slot.replace(false));
            COMPLETION_IO_FAILED.with(|slot| slot.replace(false));
        }
    }

    pub(super) fn mutation_completion_write() -> std::io::Result<()> {
        if COMPLETION_IO_FAILED.with(|slot| *slot.borrow()) {
            Err(std::io::Error::from_raw_os_error(libc::EIO))
        } else {
            Ok(())
        }
    }

    pub(crate) fn after_next_publication(operation: impl FnOnce() + 'static) {
        AFTER_PUBLICATION.with(|slot| {
            assert!(slot.borrow().is_none(), "publication hook already armed");
            *slot.borrow_mut() = Some(Box::new(operation));
        });
    }

    pub(super) fn published() {
        let operation = AFTER_PUBLICATION.with(|slot| slot.borrow_mut().take());
        if let Some(operation) = operation {
            operation();
        }
    }

    pub(crate) fn fail_next_publication_before_effect() {
        FAIL_PUBLICATION_BEFORE_EFFECT.with(|slot| *slot.borrow_mut() = true);
    }
    pub(super) fn publication_before_effect() -> Result<(), super::Failure> {
        if FAIL_PUBLICATION_BEFORE_EFFECT.with(|slot| slot.replace(false)) {
            Err(super::Failure::Indeterminate)
        } else {
            Ok(())
        }
    }
    pub(crate) fn fail_next_removal_sync() {
        FAIL_REMOVAL_SYNC.with(|slot| *slot.borrow_mut() = true);
    }

    pub(crate) fn fail_next_publication_durability() {
        FAIL_PUBLICATION_DURABILITY.with(|slot| *slot.borrow_mut() = true);
    }

    pub(super) fn publication_unsynced() -> bool {
        FAIL_PUBLICATION_DURABILITY.with(|slot| slot.replace(false))
    }

    pub(crate) fn fail_next_creation_before_effect() {
        FAIL_CREATION_BEFORE_EFFECT.with(|slot| *slot.borrow_mut() = true);
    }
    pub(super) fn creation_before_effect() -> Result<(), super::Failure> {
        if FAIL_CREATION_BEFORE_EFFECT.with(|slot| slot.replace(false)) {
            Err(super::Failure::Indeterminate)
        } else {
            Ok(())
        }
    }

    pub(crate) fn fail_next_creation_parent_sync() {
        FAIL_CREATION_PARENT_SYNC.with(|slot| *slot.borrow_mut() = true);
    }

    pub(super) fn creation_parent_sync() -> std::io::Result<()> {
        if FAIL_CREATION_PARENT_SYNC.with(|slot| slot.replace(false)) {
            Err(std::io::Error::from_raw_os_error(libc::EIO))
        } else {
            Ok(())
        }
    }

    pub(crate) fn fail_next_mutation_completion_sync() {
        FAIL_MUTATION_COMPLETION_SYNC.with(|slot| *slot.borrow_mut() = true);
    }

    pub(super) fn mutation_completion_sync() -> std::io::Result<()> {
        let failed = FAIL_MUTATION_COMPLETION_SYNC.with(|slot| slot.replace(false));
        if failed && FAIL_COMPLETION_AFTER_SYNC.with(|slot| slot.replace(false)) {
            // Every subsequent checkpoint write/sync, including any attempted
            // rollback, fails until this native completion transaction ends.
            COMPLETION_IO_FAILED.with(|slot| slot.replace(true));
        }
        if failed || COMPLETION_IO_FAILED.with(|slot| *slot.borrow()) {
            Err(std::io::Error::from_raw_os_error(libc::EIO))
        } else {
            Ok(())
        }
    }

    pub(super) fn removal_sync() -> std::io::Result<()> {
        if FAIL_REMOVAL_SYNC.with(|slot| slot.replace(false)) {
            Err(std::io::Error::from_raw_os_error(libc::EIO))
        } else {
            Ok(())
        }
    }
}

impl std::fmt::Debug for Directory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrivateDirectory(..)")
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrivateSnapshot(..)")
    }
}
