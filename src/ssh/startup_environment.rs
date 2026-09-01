use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

const FALLBACK_PATH: &str = "/usr/bin:/bin";
const PATH_ENVIRONMENT_VARIABLE: &str = "PATH";
const SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE: &str = "SSH_AUTH_SOCK";
const MAXIMUM_CAPTURED_ENVIRONMENT_VALUE_BYTES: usize = 16 * 1024;
const LOCALE_ENVIRONMENT_VARIABLES: [&str; 8] = [
    "LANG",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
];

trait StartupSshEnvironmentReader {
    fn environment_variable(&mut self, key: &OsStr) -> Option<OsString>;
}

struct ProcessStartupSshEnvironmentReader;

impl StartupSshEnvironmentReader for ProcessStartupSshEnvironmentReader {
    fn environment_variable(&mut self, key: &OsStr) -> Option<OsString> {
        std::env::var_os(key)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StartupSshEnvironment {
    path: OsString,
    locale: Vec<(&'static str, OsString)>,
    agent_socket: Option<OsString>,
}

impl Default for StartupSshEnvironment {
    fn default() -> Self {
        Self {
            path: FALLBACK_PATH.into(),
            locale: Vec::new(),
            agent_socket: None,
        }
    }
}

impl StartupSshEnvironment {
    pub(crate) fn capture() -> Self {
        Self::capture_with(&mut ProcessStartupSshEnvironmentReader)
    }

    fn capture_with(reader: &mut impl StartupSshEnvironmentReader) -> Self {
        let path = reader
            .environment_variable(OsStr::new(PATH_ENVIRONMENT_VARIABLE))
            .filter(|value| safe_environment_value(value))
            .unwrap_or_else(|| FALLBACK_PATH.into());
        let locale = LOCALE_ENVIRONMENT_VARIABLES
            .into_iter()
            .filter_map(|name| {
                reader
                    .environment_variable(OsStr::new(name))
                    .filter(|value| safe_environment_value(value))
                    .map(|value| (name, value))
            })
            .collect();
        Self {
            path,
            locale,
            agent_socket: reader
                .environment_variable(OsStr::new(SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE)),
        }
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (&'static str, &OsStr)> {
        std::iter::once((PATH_ENVIRONMENT_VARIABLE, self.path.as_os_str()))
            .chain(
                self.locale
                    .iter()
                    .map(|(name, value)| (*name, value.as_os_str())),
            )
            .chain(
                self.agent_socket
                    .iter()
                    .map(|value| (SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE, value.as_os_str())),
            )
    }

    pub(crate) fn agent_socket(&self) -> Option<&OsStr> {
        self.agent_socket.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn for_test(agent_socket: Option<OsString>) -> Self {
        Self {
            agent_socket,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_path(path: OsString, agent_socket: Option<OsString>) -> Self {
        Self {
            path: if safe_environment_value(&path) {
                path
            } else {
                FALLBACK_PATH.into()
            },
            locale: Vec::new(),
            agent_socket,
        }
    }
}

fn safe_environment_value(value: &OsStr) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAXIMUM_CAPTURED_ENVIRONMENT_VALUE_BYTES
        && !bytes.iter().any(u8::is_ascii_control)
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

        assert_eq!(captured.agent_socket(), Some(raw.as_os_str()));
    }

    #[test]
    fn capture_should_not_normalize_an_empty_agent_socket() {
        let mut reader = TestStartupSshEnvironmentReader::default()
            .with_environment(SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE, OsString::new());

        let captured = StartupSshEnvironment::capture_with(&mut reader);

        assert_eq!(captured.agent_socket(), Some(OsStr::new("")));
    }

    #[test]
    fn capture_should_keep_only_the_explicit_path_locale_and_agent_allowlist() {
        let mut reader = TestStartupSshEnvironmentReader::default()
            .with_environment(PATH_ENVIRONMENT_VARIABLE, "/opt/local/bin:/usr/bin:/bin")
            .with_environment("LANG", "en_US.UTF-8")
            .with_environment("LC_CTYPE", "UTF-8")
            .with_environment(
                SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE,
                "/private/tmp/agent.sock",
            )
            .with_environment("LC_SECRET", "locale-shaped-secret")
            .with_environment("SPACETERM_UNRELATED_SECRET", "secret");

        let captured = StartupSshEnvironment::capture_with(&mut reader);

        assert_eq!(
            captured.entries().collect::<Vec<_>>(),
            vec![
                (
                    PATH_ENVIRONMENT_VARIABLE,
                    OsStr::new("/opt/local/bin:/usr/bin:/bin")
                ),
                ("LANG", OsStr::new("en_US.UTF-8")),
                ("LC_CTYPE", OsStr::new("UTF-8")),
                (
                    SSH_AUTH_SOCK_ENVIRONMENT_VARIABLE,
                    OsStr::new("/private/tmp/agent.sock")
                ),
            ]
        );
        assert_eq!(reader.read_count("LC_SECRET"), 0);
        assert_eq!(reader.read_count("SPACETERM_UNRELATED_SECRET"), 0);
    }

    #[test]
    fn capture_should_fall_back_when_path_is_missing_empty_control_bearing_or_oversized() {
        let cases = [
            None,
            Some(OsString::new()),
            Some(OsString::from("/usr/bin\n/attacker")),
            Some(OsString::from(
                "x".repeat(MAXIMUM_CAPTURED_ENVIRONMENT_VALUE_BYTES + 1),
            )),
        ];

        for path in cases {
            let mut reader = TestStartupSshEnvironmentReader::default();
            if let Some(path) = path {
                reader = reader.with_environment(PATH_ENVIRONMENT_VARIABLE, path);
            }
            let captured = StartupSshEnvironment::capture_with(&mut reader);

            assert_eq!(
                captured.entries().next(),
                Some((PATH_ENVIRONMENT_VARIABLE, OsStr::new(FALLBACK_PATH)))
            );
        }
    }
}
