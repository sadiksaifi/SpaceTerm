//! Facts about the owning account that do not depend on the process environment.
use std::{ffi::CStr, os::unix::ffi::OsStrExt, path::PathBuf};

/// The account's home directory from the user database.
pub fn home() -> Option<PathBuf> {
    let mut buffer = vec![0u8; 4096];
    // SAFETY: passwd is plain data that getpwuid_r fills.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    // SAFETY: Every pointer refers to live storage of the stated size for the call's duration.
    let status = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            &mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded, so pw_dir is a NUL-terminated string within `buffer`.
    let home = unsafe { CStr::from_ptr(entry.pw_dir) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes())))
}
