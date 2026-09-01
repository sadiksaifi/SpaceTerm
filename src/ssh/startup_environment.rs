use std::ffi::{OsStr, OsString};

const SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE: &str = "SSH_AUTH_SOCK";

trait StartupSshEnvironmentReader {
    fn environment_variable(&mut self, key: &OsStr) -> Option<OsString>;
}

struct ProcessStartupSshEnvironmentReader;

impl StartupSshEnvironmentReader for ProcessStartupSshEnvironmentReader {
    fn environment_variable(&mut self, key: &OsStr) -> Option<OsString> {
        std::env::var_os(key)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StartupSshEnvironment {
    agent_socket: Option<OsString>,
}

impl StartupSshEnvironment {
    pub(crate) fn capture() -> Self {
        Self::capture_with(&mut ProcessStartupSshEnvironmentReader)
    }

    fn capture_with(reader: &mut impl StartupSshEnvironmentReader) -> Self {
        Self {
            agent_socket: reader
                .environment_variable(OsStr::new(SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE)),
        }
    }

    pub(crate) fn agent_socket(&self) -> Option<&OsStr> {
        self.agent_socket.as_deref()
    }

    pub(crate) fn cloned_agent_socket(&self) -> Option<OsString> {
        self.agent_socket.clone()
    }

    #[cfg(test)]
    pub(crate) fn for_test(agent_socket: Option<OsString>) -> Self {
        Self { agent_socket }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::ffi::OsStringExt;

    use super::*;

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

        fn read_count(&self, key: &str) -> usize {
            self.reads.get(OsStr::new(key)).copied().unwrap_or_default()
        }
    }

    impl StartupSshEnvironmentReader for TestStartupSshEnvironmentReader {
        fn environment_variable(&mut self, key: &OsStr) -> Option<OsString> {
            *self.reads.entry(key.to_os_string()).or_default() += 1;
            self.environment.get(key).cloned()
        }
    }

    #[test]
    fn capture_should_read_agent_socket_exactly_once() {
        let mut reader = TestStartupSshEnvironmentReader::default().with_environment(
            SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE,
            "/private/tmp/ssh-agent.sock",
        );

        let captured = StartupSshEnvironment::capture_with(&mut reader);

        assert_eq!(
            (
                captured.agent_socket(),
                reader.read_count(SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE)
            ),
            (Some(OsStr::new("/private/tmp/ssh-agent.sock")), 1)
        );
    }

    #[test]
    fn capture_should_preserve_an_unset_agent_socket() {
        let mut reader = TestStartupSshEnvironmentReader::default();

        let captured = StartupSshEnvironment::capture_with(&mut reader);

        assert_eq!(captured.agent_socket(), None);
    }

    #[test]
    fn capture_should_preserve_raw_non_utf8_agent_socket_bytes() {
        let raw = OsString::from_vec(vec![b'/', b'p', b'r', b'i', b'v', b'a', b't', b'e', 0xff]);
        let mut reader = TestStartupSshEnvironmentReader::default()
            .with_environment(SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE, raw.clone());

        let captured = StartupSshEnvironment::capture_with(&mut reader);

        assert_eq!(captured.cloned_agent_socket(), Some(raw));
    }

    #[test]
    fn capture_should_not_normalize_an_empty_agent_socket() {
        let mut reader = TestStartupSshEnvironmentReader::default()
            .with_environment(SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE, OsString::new());

        let captured = StartupSshEnvironment::capture_with(&mut reader);

        assert_eq!(captured.agent_socket(), Some(OsStr::new("")));
    }
}
