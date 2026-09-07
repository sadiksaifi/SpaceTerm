//! Nonblocking object retention. All policy lives in Local Filesystem Authority.
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use super::local_filesystem::{
    LocalFilesystemError, LocalIdentityObservation, LocalIdentitySource, LocalObjectIdentity,
    LocalObjectKind, classify_io_error,
};

pub(super) struct MacosLocalIdentity;

impl LocalIdentitySource for MacosLocalIdentity {
    fn identify(&self, path: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
        // Retain identity without reading contents. O_NONBLOCK prevents a substituted FIFO from
        // stalling resolution; O_NOCTTY prevents acquiring a controlling terminal. Standard Rust
        // opening supplies close-on-exec. The open follows the exact selected symlink spelling.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(path)
            .map_err(classify_io_error)?;
        let metadata = file.metadata().map_err(classify_io_error)?;
        let kind = if metadata.is_dir() {
            LocalObjectKind::Directory
        } else if metadata.is_file() {
            LocalObjectKind::File
        } else {
            LocalObjectKind::Other
        };
        Ok(LocalIdentityObservation {
            identity: LocalObjectIdentity::from_file(file)?,
            kind,
        })
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn special_file_identity_does_not_wait_for_a_fifo_writer() {
        let path =
            std::env::temp_dir().join(format!("spaceterm-identity-fifo-{}", std::process::id()));
        let name = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: name is a live NUL-terminated path. This creates only this test's private FIFO.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = mpsc::channel();
        let identified_path = path.clone();
        let worker = std::thread::spawn(move || {
            let result = MacosLocalIdentity
                .identify(&identified_path)
                .map(|value| value.kind);
            let _ = sender.send(result);
        });
        let result = receiver.recv_timeout(Duration::from_secs(2));
        std::fs::remove_file(path).unwrap();
        assert_eq!(result.unwrap(), Ok(LocalObjectKind::Other));
        worker.join().unwrap();
    }
}
