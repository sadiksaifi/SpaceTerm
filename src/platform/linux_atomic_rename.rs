//! Linux atomic same-directory rename primitives for the shared POSIX secure filesystem.

use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

/// Atomically exchange two entries in one directory.
pub(super) fn exchange_at(directory: &File, first: &CStr, second: &CStr) -> io::Result<()> {
    rename_at(directory, first, second, libc::RENAME_EXCHANGE)
}

/// Atomically rename an entry within one directory, failing if the target exists.
pub(super) fn rename_noreplace_at(directory: &File, from: &CStr, to: &CStr) -> io::Result<()> {
    rename_at(directory, from, to, libc::RENAME_NOREPLACE)
}

fn rename_at(directory: &File, from: &CStr, to: &CStr, flags: libc::c_uint) -> io::Result<()> {
    // SAFETY: the descriptor and both NUL-terminated names remain valid for this call.
    let result = unsafe {
        libc::renameat2(
            directory.as_raw_fd(),
            from.as_ptr(),
            directory.as_raw_fd(),
            to.as_ptr(),
            flags,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(unsupported_filesystem(io::Error::last_os_error()))
    }
}

/// Filesystems without renameat2 flag support (NFS, CIFS, ecryptfs, some FUSE) report EINVAL,
/// ENOSYS, or EOPNOTSUPP. Those mean the store is unavailable here, not that an entry is unsafe.
fn unsupported_filesystem(error: io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP) => {
            io::Error::from(io::ErrorKind::Unsupported)
        }
        _ => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_rename_flag_rejection_is_classified_as_unsupported() {
        for code in [libc::EINVAL, libc::ENOSYS, libc::EOPNOTSUPP] {
            assert_eq!(
                unsupported_filesystem(io::Error::from_raw_os_error(code)).kind(),
                io::ErrorKind::Unsupported
            );
        }
        assert_eq!(
            unsupported_filesystem(io::Error::from_raw_os_error(libc::EEXIST)).kind(),
            io::ErrorKind::AlreadyExists
        );
    }
}
