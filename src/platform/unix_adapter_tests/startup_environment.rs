//! Native Adapter integration evidence.
use super::*;
use std::os::unix::ffi::OsStringExt;

#[test]
fn capture_should_preserve_raw_non_utf8_agent_socket_bytes() {
    let raw = OsString::from_vec(vec![b'/', b'p', b'r', b'i', b'v', b'a', b't', b'e', 0xff]);
    let mut reader = TestStartupSshEnvironmentReader::default()
        .with_environment(SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE, raw.clone());

    let captured = StartupSshEnvironment::capture_with(&mut reader);

    assert_eq!(captured.agent_socket(), Some(raw.as_os_str()));
}

use std::collections::BTreeMap;
#[derive(Default)]
struct TestStartupSshEnvironmentReader {
    environment: BTreeMap<OsString, OsString>,
    reads: BTreeMap<OsString, usize>,
}

impl TestStartupSshEnvironmentReader {
    fn with_environment(mut self, key: &str, value: impl Into<OsString>) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }
}

impl StartupSshEnvironmentReader for TestStartupSshEnvironmentReader {
    fn environment_variable(&mut self, key: &OsStr) -> Option<OsString> {
        *self.reads.entry(key.to_os_string()).or_default() += 1;
        self.environment.get(key).cloned()
    }
}
