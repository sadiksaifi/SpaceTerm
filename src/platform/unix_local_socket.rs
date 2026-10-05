//! POSIX local-socket mechanism for Control Connection endpoint probing.

use std::os::unix::net::UnixListener;
use std::path::Path;

use super::control_socket::{ControlSocketProbe, ControlSocketUnavailable};

/// The longest local socket path, in bytes, that the host `sockaddr_un` accepts with its NUL
/// terminator: 103 on macOS and 107 on Linux.
pub(super) const LOCAL_IPC_PATH_MAXIMUM: usize = std::mem::size_of::<libc::sockaddr_un>()
    - std::mem::offset_of!(libc::sockaddr_un, sun_path)
    - 1;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct UnixControlSocketProbe;

impl ControlSocketProbe for UnixControlSocketProbe {
    fn probe(&self, endpoint: &Path) -> Result<(), ControlSocketUnavailable> {
        let listener = UnixListener::bind(endpoint).map_err(|_| ControlSocketUnavailable)?;
        drop(listener);
        Ok(())
    }
}

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn local_ipc_path_maximum_matches_the_host_socket_address() {
        #[cfg(target_os = "macos")]
        assert_eq!(LOCAL_IPC_PATH_MAXIMUM, 103);
        #[cfg(target_os = "linux")]
        assert_eq!(LOCAL_IPC_PATH_MAXIMUM, 107);
    }

    #[test]
    fn probe_should_create_and_release_the_exact_endpoint() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("spaceterm-control-probe-{nonce}"));
        fs::create_dir(&directory).unwrap();
        let endpoint = directory.join("c");

        UnixControlSocketProbe.probe(&endpoint).unwrap();

        assert!(endpoint.exists());
        // A concurrent fork can briefly inherit the listener before its child execs.
        let release_deadline = Instant::now() + Duration::from_millis(500);
        loop {
            assert!(
                Instant::now() < release_deadline,
                "the probe should release its listener within 500 ms"
            );
            match std::os::unix::net::UnixStream::connect(&endpoint) {
                Ok(connection) => drop(connection),
                Err(error) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        fs::remove_file(&endpoint).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
