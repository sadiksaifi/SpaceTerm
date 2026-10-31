//! Bounded search of an inherited executable PATH.
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

const MAXIMUM_PATH_BYTES: usize = 16 * 1024;
const MAXIMUM_PATH_ENTRIES: usize = 128;

pub(super) fn directories(path: Option<&OsStr>) -> impl Iterator<Item = PathBuf> {
    path.filter(|path| {
        let bytes = path.as_encoded_bytes();
        bytes.len() <= MAXIMUM_PATH_BYTES && !bytes.iter().any(u8::is_ascii_control)
    })
    .into_iter()
    .flat_map(std::env::split_paths)
    .take(MAXIMUM_PATH_ENTRIES)
    .filter(|directory| directory.is_absolute())
}

pub(super) fn is_executable_file(path: &Path) -> bool {
    if !path.is_absolute() || !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: the NUL-terminated local path stays live during this non-mutating access check.
    unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0 }
}
