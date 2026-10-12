//! Retained directory authority, with host ownership and ACL inspection injected per handle.
//!
//! Operations compare current protection and retained identities and observe covered
//! native mutations. Snapshot and event equality cannot certify immutable contents,
//! an atomic filesystem snapshot, or continuously safe ACLs. Managed native mutation
//! must be serialized separately. Approved copies, immutable protected installations,
//! and final execution binding require authentication and exclusion above this module.
//!
//! Linux inotify omits mmap/msync/munmap writes, fileattr flags and over-mounts.
//! XNU vnode notifications cover successful operations; failed partial mutations and
//! writable-map stores before pageout may escape observation. Neither backend is a
//! barrier against arbitrary concurrent writers.

use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::path::Path;

use thiserror::Error;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod native;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) mod observation;

/// Content-free failures from protected directory operations.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProtectedDirectoryError {
    /// Native traversal and retention are unavailable for this host.
    #[error("native protected directory support is unavailable")]
    NativeUnavailable,
    /// The absolute directory path or single entry name is invalid.
    #[error("the protected directory path or entry name is invalid")]
    InvalidPath,
    /// An expected entry is absent.
    #[error("a protected entry is missing")]
    Missing,
    /// An entry type, ownership rule, or access rule is unsafe.
    #[error("a protected entry is unsafe")]
    Unsafe,
    /// A retained link, sampled entry state, or covered native mutation changed an operation.
    #[error("a protected entry changed")]
    Changed,
    /// The file exceeds the admitted byte bound or the bound cannot be represented.
    #[error("the protected file exceeds its byte bound")]
    TooLarge,
    /// Native storage or allocation could not complete the operation.
    #[error("protected storage is unavailable")]
    Unavailable,
}

/// Host-specific ownership and ACL policy on already opened native handles.
///
/// This adapter does not traverse paths, open entries, or establish entry identities.
/// Those mechanics belong to [`RetainedProtectedDirectory`]. Production inspection
/// must exclude owning-account writes, including ACL grants, on every ancestor and file.
/// A fixture inspector proves only the rules of its controlled fixture.
pub trait NativeDirectoryProtection {
    /// Checks host protection on one opened directory, including filesystem root.
    fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError>;

    /// Checks host protection on an opened regular file before and after reading.
    fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError>;
}

/// Opaque, noncloneable authority for a directory and its entire ancestor chain.
///
/// Retains opened handles and parent-child identities from filesystem root. No
/// operation canonicalizes or reopens the caller's absolute path.
///
/// Each operation enrolls held handles in a private, bounded native observation
/// before inspecting them. It compares sampled protection and links after callbacks,
/// then rejects covered inode attribute, rename, deletion and file-write events or
/// observation failures. Each retained child has its own observation. Unrelated child
/// activity is admissible when current protection and retained identities remain valid.
/// Observation silence is not authentication or history proof.
///
/// Traversal admits at most 255 directory handles, including root, and reserves one
/// observation for a read file. Linux descriptor binding assumes trusted host procfs
/// and a trusted mount namespace. This module does not authenticate caller-controlled
/// namespaces.
pub struct RetainedProtectedDirectory<'protection> {
    _protection: &'protection dyn NativeDirectoryProtection,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    native: native::RetainedDirectory,
}

impl<'protection> RetainedProtectedDirectory<'protection> {
    /// Opens an absolute directory by no-follow traversal from filesystem root.
    ///
    /// No path canonicalization or creation is permitted. The injected inspector
    /// checks each opened handle, while this module owns traversal and link retention.
    /// Explicit current-directory and parent-directory components are invalid.
    ///
    /// # Errors
    /// Rejects invalid paths, symlinks, unsafe protection, and changed links. Missing
    /// entries and unavailable storage have distinct classifications. Observation
    /// setup or resource-bound failures fail closed. Unsupported hosts or required
    /// native observation return [`ProtectedDirectoryError::NativeUnavailable`].
    pub fn open(
        absolute_path: &Path,
        protection: &'protection dyn NativeDirectoryProtection,
    ) -> Result<Self, ProtectedDirectoryError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let native = native::RetainedDirectory::open(absolute_path, protection)?;
            Ok(Self {
                _protection: protection,
                native,
            })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (absolute_path, protection);
            Err(ProtectedDirectoryError::NativeUnavailable)
        }
    }

    /// Rechecks protection and every retained ancestor link without reopening by path.
    ///
    /// # Errors
    /// Returns [`ProtectedDirectoryError::Changed`] for replaced or missing links,
    /// changed sampled protection, or covered native events,
    /// [`ProtectedDirectoryError::Unsafe`] for unsafe protection, or a typed native
    /// observation/storage failure. Unsupported hosts fail closed.
    pub fn verify(&self) -> Result<(), ProtectedDirectoryError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.native.verify(self._protection)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(ProtectedDirectoryError::NativeUnavailable)
        }
    }

    /// Reads one no-follow regular entry relative to the retained directory.
    ///
    /// `name` must be one normal component without a NUL byte. Reads verify the
    /// ancestor chain, protection, and file version before and after reading. File
    /// type and hard-link inspection belong to this module, not the protection adapter.
    /// Returned contents and requested allocation never exceed `maximum_bytes`.
    /// Oversize detection reads at most one additional byte without retaining it.
    /// One observation spans both inspection rounds without resetting event history.
    /// The returned buffer is not an atomic content snapshot or execution approval.
    ///
    /// # Errors
    /// Rejects invalid names, unsafe or changed entries, and files exceeding the byte
    /// bound. Bounds whose one-byte probe overflows return `TooLarge`. Missing entries
    /// and unavailable storage have distinct classifications. Unsupported hosts fail closed.
    pub fn read_regular_file(
        &self,
        name: &OsStr,
        maximum_bytes: usize,
    ) -> Result<ProtectedFileContents, ProtectedDirectoryError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let bytes = self
                .native
                .read_regular_file(name, maximum_bytes, self._protection)?;
            Ok(ProtectedFileContents { bytes })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (name, maximum_bytes);
            Err(ProtectedDirectoryError::NativeUnavailable)
        }
    }
}

impl fmt::Debug for RetainedProtectedDirectory<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RetainedProtectedDirectory(..)")
    }
}

/// Bounded observed file bytes whose diagnostics never reveal the payload.
///
/// This buffer is unauthenticated. Execution approval must authenticate the exact
/// bytes and separately retain the approved installation through execution and
/// resource use.
pub struct ProtectedFileContents {
    bytes: Vec<u8>,
}

impl ProtectedFileContents {
    /// Borrows the admitted contents for decoding, not diagnostics.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for ProtectedFileContents {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProtectedFileContents(..)")
    }
}
