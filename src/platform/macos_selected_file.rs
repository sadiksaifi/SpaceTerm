//! Nonblocking file opening for explicit local file selections.

use std::{fs::File, os::unix::fs::OpenOptionsExt, path::Path};

use super::selected_file::{SelectedFileOpenError, SelectedFileOpener};

pub(super) struct MacosSelectedFileOpener;

impl SelectedFileOpener for MacosSelectedFileOpener {
    fn open(&self, path: &Path) -> Result<File, SelectedFileOpenError> {
        // Rejecting special files requires a handle first. Opening must not wait for a FIFO
        // writer or acquire a controlling terminal. The explicitly selected symlink may be followed.
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(path)
            .map_err(|_| SelectedFileOpenError)
    }
}
