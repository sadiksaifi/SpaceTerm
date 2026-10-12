//! Fixed-root filesystem protection for already opened native handles.
//!
//! Filesystem protection does not authenticate signatures or approve execution.

use std::fs::File;
#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt;

use crate::protected_directory::{NativeDirectoryProtection, ProtectedDirectoryError};

#[cfg(any(target_os = "macos", target_os = "linux", test))]
mod admission;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod linux_context;
#[cfg(target_os = "linux")]
mod linux_evidence;
#[cfg(target_os = "linux")]
mod linux_native;
#[cfg(any(target_os = "macos", test))]
mod macos;

/// Fixed-root protection adapter, with no caller-selected root, UID or trust flag.
///
/// Requires UID zero and no group or other write permission. On macOS, admits only
/// local APFS/HFS mounts with ownership enforcement and independently confirmed
/// extended ACL support. Every macOS ACL mutation ALLOW is rejected, regardless of
/// principal or order. Unknown filesystem or ACL semantics fail closed.
///
/// On Linux, apparent root ownership is insufficient. This uncontextualized adapter
/// rejects unsafe ownership, modes and handle types, then returns
/// [`ProtectedDirectoryError::NativeUnavailable`]. The private Linux context also
/// requires descriptor-bound non-idmapped evidence and independently issued per-inode
/// publication coverage excluding account-write-authorizing aliases, including
/// foreign namespaces and detached fresh-open handles. No production issuer exists.
/// Root mode, namespace ownership, marker files and observation silence cannot
/// manufacture either proof. Linux 5.15 layouts without genuine provenance remain
/// unavailable; optional newer kernel queries do not raise the supported baseline.
///
/// The adapter borrows handles. Traversal, retained links, hard links and bounded
/// reads remain owned by the protected-directory boundary. Contextual Linux queries
/// use fixed 256-byte statx and 512-byte basic statmount output, at most three
/// backends per round, four interruptions and 32 syscall attempts per inspection,
/// with at most 256 mount witnesses and 256 inode publication records. Temporary
/// descriptor closes consume that budget, independently of native observation.
///
/// Sampled metadata, mount facts and covered events are not mutation generations,
/// immutable contents or continuously safe ACLs. This boundary neither authenticates
/// code nor approves execution, and cannot exclude historical writable descriptors
/// or mappings. Authenticated exact copies, immutable approved resources, trusted
/// publication/mutation control and final execution binding remain separate
/// obligations. Unsupported hosts fail closed.
#[derive(Debug, Default)]
pub struct NativeRootProtection;

impl NativeDirectoryProtection for NativeRootProtection {
    fn verify_directory(&self, directory: &File) -> Result<(), ProtectedDirectoryError> {
        #[cfg(target_os = "linux")]
        {
            linux_context::verify_without_authority(directory, linux_context::HandleKind::Directory)
        }
        #[cfg(not(target_os = "linux"))]
        {
            verify_handle(directory)
        }
    }

    fn verify_regular_file(&self, file: &File) -> Result<(), ProtectedDirectoryError> {
        #[cfg(target_os = "linux")]
        {
            linux_context::verify_without_authority(file, linux_context::HandleKind::RegularFile)
        }
        #[cfg(not(target_os = "linux"))]
        {
            verify_handle(file)
        }
    }
}

#[cfg(target_os = "macos")]
fn verify_handle(file: &File) -> Result<(), ProtectedDirectoryError> {
    let before = ProtectionVersion::of(file)?;
    admission::require_root_mode(before.owner, before.mode)?;
    let mount_before = macos::filesystem_version(file)?;
    let inspection = mount_before
        .require_proven_semantics()
        .and_then(|()| macos::inspect_acl(file));
    // Recheck even when ACL admission failed, so a changed inspection is not
    // mistaken for a stable protection snapshot.
    let mount_after = macos::filesystem_version(file)?;
    let after = ProtectionVersion::of(file)?;
    if before != after || mount_before != mount_after {
        return Err(ProtectedDirectoryError::Changed);
    }
    inspection
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn verify_handle(_file: &File) -> Result<(), ProtectedDirectoryError> {
    Err(ProtectedDirectoryError::NativeUnavailable)
}

#[cfg(target_os = "macos")]
#[derive(Eq, PartialEq)]
struct ProtectionVersion {
    owner: u32,
    group: u32,
    mode: u32,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(target_os = "macos")]
impl ProtectionVersion {
    fn of(file: &File) -> Result<Self, ProtectedDirectoryError> {
        let metadata = file
            .metadata()
            .map_err(|_| ProtectedDirectoryError::Unavailable)?;
        Ok(Self {
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.mode(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}
