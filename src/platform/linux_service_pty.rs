//! Refresh only the account service's login agent when an explicit Pane launch creates a PTY.
use super::native_pty::{
    NativePtyAdapterConstructionFailure as Failure, NativePtyAdapterFactory, NativePtyAdapterParts,
    NativePtySize,
};
use super::shell_launch::PreparedShellLaunch;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

const AGENT: &str = "SSH_AUTH_SOCK";
type AgentLookup = dyn Fn() -> Result<Option<String>, Failure> + Send + Sync;

struct Factory {
    inner: Arc<dyn NativePtyAdapterFactory>,
    lookup: Arc<AgentLookup>,
}

pub(super) fn factory(inner: Arc<dyn NativePtyAdapterFactory>) -> Arc<dyn NativePtyAdapterFactory> {
    Arc::new(Factory {
        inner,
        lookup: Arc::new(agent_socket),
    })
}

impl NativePtyAdapterFactory for Factory {
    fn create(
        &self,
        launch: PreparedShellLaunch,
        size: NativePtySize,
    ) -> Result<NativePtyAdapterParts, Failure> {
        let agent = (self.lookup)()?;
        let mut launch = launch.without_environment_value(AGENT);
        if let Some(agent) = agent {
            launch = launch.with_environment_value(AGENT, &agent);
        }
        self.inner.create(launch, size)
    }
}

fn agent_socket() -> Result<Option<String>, Failure> {
    // Read the owning manager, never a client's bus address or environment. Its agent can
    // change after this service starts at boot, and disappear again after graphical logout.
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let mut child = Command::new("/usr/bin/busctl")
        .args([
            "--user",
            "--json=short",
            "--timeout=1s",
            "--auto-start=no",
            "--allow-interactive-authorization=no",
            "get-property",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "Environment",
        ])
        .env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path=/run/user/{uid}/bus"),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Failure::EnvironmentUnavailable)?;
    const MAX_OUTPUT: usize = 128 * 1024;
    let Some(output) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Failure::EnvironmentUnavailable);
    };
    let reader = std::thread::Builder::new()
        .name("service-agent-environment".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            output
                .take((MAX_OUTPUT + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| Failure::EnvironmentUnavailable)?;
            Ok(bytes)
        });
    let Ok(reader) = reader else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Failure::EnvironmentUnavailable);
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(Failure::EnvironmentUnavailable);
            }
        }
    };
    let bytes: Result<Vec<u8>, Failure> =
        reader.join().map_err(|_| Failure::EnvironmentUnavailable)?;
    result?;
    let bytes = bytes?;
    if bytes.len() > MAX_OUTPUT {
        return Err(Failure::EnvironmentUnavailable);
    }
    decode_agent(&bytes)
}

fn decode_agent(bytes: &[u8]) -> Result<Option<String>, Failure> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Property {
        #[serde(rename = "type")]
        signature: String,
        data: Vec<String>,
    }
    let property: Property =
        serde_json::from_slice(bytes).map_err(|_| Failure::EnvironmentUnavailable)?;
    if property.signature != "as" {
        return Err(Failure::EnvironmentUnavailable);
    }
    let mut agent = None;
    let mut found = false;
    for entry in property.data {
        if let Some(value) = entry.strip_prefix("SSH_AUTH_SOCK=") {
            if found || value.len() > 16 * 1024 || value.contains('\0') {
                return Err(Failure::EnvironmentUnavailable);
            }
            found = true;
            if !value.is_empty() {
                agent = Some(value.to_owned());
            }
        }
    }
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct PtyBoundary(Mutex<Vec<PreparedShellLaunch>>);
    impl NativePtyAdapterFactory for PtyBoundary {
        fn create(
            &self,
            launch: PreparedShellLaunch,
            _: NativePtySize,
        ) -> Result<NativePtyAdapterParts, Failure> {
            self.0.lock().unwrap().push(launch);
            Err(Failure::ProcessStartFailed)
        }
    }

    #[test]
    fn new_terminals_follow_login_agent_changes_without_restarting_the_service() {
        let current = Arc::new(Mutex::new(None));
        let lookup = Arc::clone(&current);
        let pty = Arc::new(PtyBoundary::default());
        let factory = Factory {
            inner: pty.clone(),
            lookup: Arc::new(move || Ok(lookup.lock().unwrap().clone())),
        };
        for expected in [None, Some("/fixture/new-agent".to_owned()), None] {
            *current.lock().unwrap() = expected.clone();
            let launch = PreparedShellLaunch::for_test("/fixture".into())
                .with_environment_value(AGENT, "/fixture/old-agent");
            let _ = factory.create(
                launch,
                NativePtySize {
                    rows: 24,
                    columns: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            );
            let launches = pty.0.lock().unwrap();
            let launch = launches.last().unwrap();
            let actual = launch
                .environment()
                .iter()
                .rev()
                .find(|(key, _)| key == AGENT);
            assert_eq!(
                actual.map(|(_, value)| value.to_str().unwrap()),
                expected.as_deref()
            );
            assert!(launch.environment_removals().iter().any(|key| key == AGENT));
        }
    }

    #[test]
    fn unavailable_manager_refuses_launch_instead_of_reusing_a_stale_agent() {
        let pty = Arc::new(PtyBoundary::default());
        let factory = Factory {
            inner: pty.clone(),
            lookup: Arc::new(|| Err(Failure::EnvironmentUnavailable)),
        };
        let result = factory.create(
            PreparedShellLaunch::for_test("/fixture".into())
                .with_environment_value(AGENT, "/fixture/stale-agent"),
            NativePtySize {
                rows: 24,
                columns: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        );
        assert!(matches!(result, Err(Failure::EnvironmentUnavailable)));
        assert!(pty.0.lock().unwrap().is_empty());
    }

    #[test]
    fn manager_property_exposes_only_the_current_agent_and_refuses_ambiguous_values() {
        assert_eq!(
            decode_agent(
                br#"{"type":"as","data":["UNRELATED=value","SSH_AUTH_SOCK=/fixture/agent"]}"#
            ),
            Ok(Some("/fixture/agent".into())),
        );
        assert_eq!(
            decode_agent(br#"{"type":"as","data":["SSH_AUTH_SOCK=/one","SSH_AUTH_SOCK=/two"]}"#),
            Err(Failure::EnvironmentUnavailable),
        );
    }
}
