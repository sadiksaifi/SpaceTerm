use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;

use super::admission::require_linux_filesystem;
use super::linux_evidence::WorkBudget;
use crate::protected_directory::ProtectedDirectoryError;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct FilesystemVersion {
    pub(super) filesystem_type: u64,
    pub(super) filesystem_id: libc::c_ulong,
    pub(super) mount_flags: libc::c_ulong,
}

impl FilesystemVersion {
    pub(super) fn require_proven_semantics(self) -> Result<(), ProtectedDirectoryError> {
        require_linux_filesystem(self.filesystem_type)
    }
}

pub(super) fn filesystem_version(
    file: &File,
    work: &mut WorkBudget,
) -> Result<FilesystemVersion, ProtectedDirectoryError> {
    let mut filesystem = MaybeUninit::<libc::statfs>::uninit();
    work.retry(|| {
        // SAFETY: the borrowed File owns a live descriptor and filesystem is
        // writable native storage. fstatfs transfers no descriptor ownership.
        if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })?;
    // SAFETY: fstatfs initialized the output on success.
    let filesystem = unsafe { filesystem.assume_init() };

    // f_flags is not exposed by libc's statfs on every Linux ABI. fstatvfs supplies
    // mount flags and a filesystem ID, neither of which proves ownership mapping.
    let mut mount = MaybeUninit::<libc::statvfs>::uninit();
    work.retry(|| {
        // SAFETY: file remains borrowed and mount is writable native storage.
        if unsafe { libc::fstatvfs(file.as_raw_fd(), mount.as_mut_ptr()) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })?;
    // SAFETY: fstatvfs initialized the output on success.
    let mount = unsafe { mount.assume_init() };
    Ok(FilesystemVersion {
        // Linux filesystem magic is 32 bits, even when __fsword_t is signed.
        filesystem_type: u64::from(filesystem.f_type as u32),
        filesystem_id: mount.f_fsid,
        mount_flags: mount.f_flag,
    })
}
