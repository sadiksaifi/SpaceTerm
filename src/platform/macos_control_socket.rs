//! macOS local-socket mechanism for Control Connection endpoint probing.

use std::os::unix::net::UnixListener;
use std::path::Path;

use super::control_socket::{ControlSocketProbe, ControlSocketUnavailable};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MacosControlSocketProbe;

impl ControlSocketProbe for MacosControlSocketProbe {
    fn probe(&self, endpoint: &Path) -> Result<(), ControlSocketUnavailable> {
        let listener = UnixListener::bind(endpoint).map_err(|_| ControlSocketUnavailable)?;
        drop(listener);
        Ok(())
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn probe_should_create_and_release_the_exact_endpoint() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("spaceterm-control-probe-{nonce}"));
        fs::create_dir(&directory).unwrap();
        let endpoint = directory.join("c");

        MacosControlSocketProbe.probe(&endpoint).unwrap();

        assert!(endpoint.exists());
        fs::remove_file(&endpoint).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
