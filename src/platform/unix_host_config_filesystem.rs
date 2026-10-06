//! POSIX filesystem mechanics for read-only SSH host discovery.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::ssh::host_config::{HostConfigFilesystem, HostConfigFilesystemError};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct UnixHostConfigFilesystem;

impl HostConfigFilesystem for UnixHostConfigFilesystem {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, HostConfigFilesystemError> {
        std::fs::canonicalize(path).map_err(classify)
    }

    fn read_file_limited(
        &self,
        path: &Path,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>, HostConfigFilesystemError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
            .open(path)
            .map_err(classify)?;
        if !file.metadata().map_err(classify)?.is_file() {
            return Err(HostConfigFilesystemError::Unavailable);
        }
        let mut contents = Vec::with_capacity(maximum_bytes.min(16 * 1024));
        file.take(maximum_bytes.saturating_add(1) as u64)
            .read_to_end(&mut contents)
            .map_err(classify)?;
        Ok(contents)
    }

    fn read_directory_limited(
        &self,
        path: &Path,
        maximum_entries: usize,
    ) -> Result<Vec<PathBuf>, HostConfigFilesystemError> {
        let mut entries = Vec::with_capacity(maximum_entries.saturating_add(1));
        for entry in std::fs::read_dir(path).map_err(classify)? {
            entries.push(entry.map_err(classify)?.path());
            if entries.len() > maximum_entries {
                return Ok(entries);
            }
        }
        entries.sort();
        Ok(entries)
    }
}

fn classify(error: std::io::Error) -> HostConfigFilesystemError {
    if error.kind() == std::io::ErrorKind::NotFound {
        HostConfigFilesystemError::Missing
    } else {
        HostConfigFilesystemError::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::host_config::{
        HostConfigIssueKind, HostConfigRoots, HostDiscoveryLimits, discover_ssh_hosts,
    };

    #[test]
    fn unix_host_discovery_rejects_fifo_and_device_includes_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        use std::sync::mpsc;
        use std::time::Duration;
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-host-discovery-fifo-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let fifo = directory.join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let config = directory.join("config");
        std::fs::write(
            &config,
            format!(
                "Include {} /dev/null\nHost reachable\n  HostName example.org\n",
                fifo.display()
            ),
        )
        .unwrap();
        let roots = HostConfigRoots {
            managed: directory.join("missing"),
            user: config,
            home: directory.to_owned(),
        };
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(discover_ssh_hosts(
                    &UnixHostConfigFilesystem,
                    &roots,
                    HostDiscoveryLimits::default(),
                ))
                .unwrap();
        });
        let discovered = receiver.recv_timeout(Duration::from_secs(2));
        // Unblock a blocking open on failure so the test leaves no worker.
        if discovered.is_err() {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo);
        }
        worker.join().unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        let discovered = discovered.expect("host discovery must not wait for a FIFO writer");
        assert_eq!(discovered.hosts.len(), 1);
        assert_eq!(discovered.issues.len(), 2);
        assert!(
            discovered
                .issues
                .iter()
                .all(|issue| issue.kind() == HostConfigIssueKind::Read)
        );
    }
}
