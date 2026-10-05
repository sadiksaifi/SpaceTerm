//! macOS atomic same-directory rename primitives for the shared POSIX secure filesystem.

use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

/// Atomically exchange two entries in one directory.
pub(super) fn exchange_at(directory: &File, first: &CStr, second: &CStr) -> io::Result<()> {
    rename_at(directory, first, second, libc::RENAME_SWAP)
}

/// Atomically rename an entry within one directory, failing if the target exists.
pub(super) fn rename_noreplace_at(directory: &File, from: &CStr, to: &CStr) -> io::Result<()> {
    rename_at(directory, from, to, libc::RENAME_EXCL)
}

fn rename_at(directory: &File, from: &CStr, to: &CStr, flags: libc::c_uint) -> io::Result<()> {
    // SAFETY: the descriptor and both NUL-terminated names remain valid for this call.
    let result = unsafe {
        libc::renameatx_np(
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
        Err(io::Error::last_os_error())
    }
}
