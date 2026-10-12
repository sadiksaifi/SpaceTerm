//! Retained physical directory identity for Worktree confirmation and write coordination.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;

/// Content-free failure to retain or revalidate a directory.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DirectoryError {
    /// No filesystem entry exists at the location.
    #[error("the directory is missing")]
    Missing,
    /// The location holds an entry that is not a directory.
    #[error("the location is not a directory")]
    NotDirectory,
    /// The location no longer names the retained directory.
    #[error("the directory identity changed")]
    IdentityChanged,
    /// The retained-directory budget is exhausted.
    #[error("directory identity capacity is exhausted")]
    Capacity,
    /// Permission, malformed path, or another native failure.
    #[error("the directory could not be checked")]
    Unavailable,
}

/// A physical directory held open for the entire confirmation lifetime.
///
/// Implementations must retain the object, not only an inode number that can be reused after
/// deletion. The path is host-only authority and must never be serialized as a native proof.
pub trait RetainedDirectory: Send + Sync {
    /// The exact location selected for this lease.
    fn path(&self) -> &Path;
    /// Check that the location still names the retained physical directory.
    fn revalidate(&self) -> Result<(), DirectoryError>;
}

impl fmt::Debug for dyn RetainedDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RetainedDirectory(<redacted>)")
    }
}

/// Filesystem operations needed by Worktree removal, injected at host composition.
pub trait WorktreeFilesystem: Send + Sync {
    /// Retain a readable physical directory, following links in its selected location.
    fn validate_directory(&self, path: &Path)
    -> Result<Arc<dyn RetainedDirectory>, DirectoryError>;
    /// Whether any entry, including a dangling symbolic link, exists at the location.
    /// A failed observation can return false; use `is_absent` to establish absence.
    fn has_entry(&self, path: &Path) -> bool;

    /// Establish absence without treating inaccessible storage or a dangling link as missing.
    fn is_absent(&self, path: &Path) -> Result<bool, DirectoryError> {
        match self.validate_directory(path) {
            Err(DirectoryError::Missing) => Ok(!self.has_entry(path)),
            Ok(_) | Err(DirectoryError::NotDirectory) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// Native retained directory adapter on the supported server hosts.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeWorktreeFilesystem;

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl WorktreeFilesystem for NativeWorktreeFilesystem {
    fn validate_directory(
        &self,
        path: &Path,
    ) -> Result<Arc<dyn RetainedDirectory>, DirectoryError> {
        Ok(Arc::new(native::Directory::open(path)?))
    }

    fn has_entry(&self, path: &Path) -> bool {
        path.is_absolute() && std::fs::symlink_metadata(path).is_ok()
    }

    fn is_absent(&self, path: &Path) -> Result<bool, DirectoryError> {
        if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(DirectoryError::Unavailable);
        }
        match std::fs::symlink_metadata(path) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => Ok(false),
            Err(_) => Err(DirectoryError::Unavailable),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) mod native {
    use std::fs::{self, OpenOptions};
    use std::io;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{DirectoryError, RetainedDirectory};

    // Reserve one handle for short, serialized observations across every native Git adapter.
    // New leases cannot consume this headroom and prevent already-admitted writes from checking
    // their retained identities. Git commands and repository reads remain concurrent.
    const MAX_DIRECTORY_HANDLES: usize = 96;
    const MAX_RETAINED_DIRECTORY_HANDLES: usize = MAX_DIRECTORY_HANDLES - 1;
    static RETAINED_HANDLES: AtomicUsize = AtomicUsize::new(0);
    static OBSERVATION: Mutex<()> = Mutex::new(());

    struct Permit;

    impl Permit {
        fn reserve() -> Result<Self, DirectoryError> {
            RETAINED_HANDLES
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    (used < MAX_RETAINED_DIRECTORY_HANDLES).then_some(used + 1)
                })
                .map(|_| Self)
                .map_err(|_| DirectoryError::Capacity)
        }
    }

    impl Drop for Permit {
        fn drop(&mut self) {
            RETAINED_HANDLES.fetch_sub(1, Ordering::AcqRel);
        }
    }

    pub(in crate::git) struct Directory {
        path: PathBuf,
        pub(in crate::git) identity: Arc<same_file::Handle>,
        _permit: Permit,
    }

    impl Directory {
        pub(in crate::git) fn open(path: &Path) -> Result<Self, DirectoryError> {
            let permit = Permit::reserve()?;
            let identity = identify(path)?;
            let _observation = OBSERVATION
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            fs::read_dir(path).map_err(classify)?;
            if identify(path)? != identity {
                return Err(DirectoryError::IdentityChanged);
            }
            Ok(Self {
                path: path.to_owned(),
                identity: Arc::new(identity),
                _permit: permit,
            })
        }

        pub(in crate::git) fn revalidate_at(&self, path: &Path) -> Result<(), DirectoryError> {
            let _observation = OBSERVATION
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if identify(path)? != *self.identity {
                return Err(DirectoryError::IdentityChanged);
            }
            fs::read_dir(path).map(drop).map_err(classify)
        }
    }

    impl RetainedDirectory for Directory {
        fn path(&self) -> &Path {
            &self.path
        }

        fn revalidate(&self) -> Result<(), DirectoryError> {
            self.revalidate_at(&self.path)
        }
    }

    fn identify(path: &Path) -> Result<same_file::Handle, DirectoryError> {
        if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(DirectoryError::Unavailable);
        }
        // O_DIRECTORY rejects special files; O_NONBLOCK also prevents a swapped FIFO from
        // blocking the open. The retained handle prevents inode recycling during confirmation.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(classify)?;
        same_file::Handle::from_file(file).map_err(classify)
    }

    fn classify(error: io::Error) -> DirectoryError {
        match error.kind() {
            io::ErrorKind::NotFound => DirectoryError::Missing,
            io::ErrorKind::NotADirectory => DirectoryError::NotDirectory,
            _ => DirectoryError::Unavailable,
        }
    }
}
