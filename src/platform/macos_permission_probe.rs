//! Reads permission authorization from a child process, the way a program started now reads it.
//!
//! The system attributes a child's privacy checks to its responsible application, which is
//! SpaceTerm, and a fresh process holds no cached answer. SpaceTerm's own Screen Recording read
//! keeps its launch value until SpaceTerm reopens, so only a child follows a grant made since.

use std::io::{Read as _, Write as _};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::permission_access::{PermissionAccessError, PermissionAuthorization, SystemPermission};

/// Selects the probe role. Only the exact value does, so an unrelated inherited value never turns
/// a launch into a probe.
const PROBE_ENV: &str = "SPACETERM_PERMISSION_PROBE";
const PROBE_ROLE: &str = "report";
/// A probe answers within milliseconds. A slower one is stuck and is stopped.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const CHILD_POLL: Duration = Duration::from_millis(5);

/// Both authorizations one probe read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProbeReport {
    screen_recording: bool,
    accessibility: bool,
}

impl ProbeReport {
    pub(super) fn read(read: impl Fn(SystemPermission) -> bool) -> Self {
        Self {
            screen_recording: read(SystemPermission::ScreenRecording),
            accessibility: read(SystemPermission::Accessibility),
        }
    }

    /// This report with `permission` not granted and the other permission as it was.
    pub(super) fn revoking(self, permission: SystemPermission) -> Self {
        Self::read(|read| {
            read != permission && self.authorization(read) == PermissionAuthorization::Granted
        })
    }

    pub(super) fn authorization(self, permission: SystemPermission) -> PermissionAuthorization {
        let granted = match permission {
            SystemPermission::ScreenRecording => self.screen_recording,
            SystemPermission::Accessibility => self.accessibility,
        };
        if granted {
            PermissionAuthorization::Granted
        } else {
            PermissionAuthorization::NotGranted
        }
    }

    fn encode(self) -> String {
        format!(
            "screen-recording={} accessibility={}\n",
            u8::from(self.screen_recording),
            u8::from(self.accessibility)
        )
    }

    fn decode(output: &[u8]) -> Option<Self> {
        [false, true]
            .into_iter()
            .flat_map(|screen_recording| {
                [false, true].map(|accessibility| Self {
                    screen_recording,
                    accessibility,
                })
            })
            .find(|report| report.encode().as_bytes() == output)
    }
}

/// Runs the probe role when this process was started as a probe, returning its exit code.
pub(crate) fn dispatch_probe_from_environment() -> Option<i32> {
    if std::env::var_os(PROBE_ENV)? != PROBE_ROLE {
        return None;
    }
    let report = ProbeReport::read(super::macos_permission_access::in_process_granted);
    let mut stdout = std::io::stdout().lock();
    let written = stdout
        .write_all(report.encode().as_bytes())
        .and_then(|()| stdout.flush());
    Some(if written.is_ok() { 0 } else { 1 })
}

/// Starts this executable as a probe and waits for its report.
///
/// It blocks for the probe's lifetime, so callers run it off the main thread.
pub(super) fn run_probe() -> Result<ProbeReport, PermissionAccessError> {
    let executable =
        std::env::current_exe().map_err(|_| PermissionAccessError::PlatformUnavailable)?;
    let mut child = Command::new(executable)
        .env_clear()
        .env(PROBE_ENV, PROBE_ROLE)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| PermissionAccessError::PlatformUnavailable)?;
    let status = wait_bounded(&mut child, PROBE_TIMEOUT)
        .ok_or(PermissionAccessError::PlatformUnavailable)?;
    let mut output = Vec::new();
    let read = child
        .stdout
        .take()
        .map(|mut stdout| stdout.read_to_end(&mut output));
    match read {
        Some(Ok(_)) if status.success() => {
            ProbeReport::decode(&output).ok_or(PermissionAccessError::PlatformRejected)
        }
        _ => Err(PermissionAccessError::PlatformRejected),
    }
}

/// Waits up to `timeout` for `child` to exit. A child still running then is stuck, so it is
/// stopped and reaped, and the wait returns `None`.
pub(super) fn wait_bounded(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(CHILD_POLL),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bounded_wait_stops_a_stuck_child() {
        let mut stuck = Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("a child");
        let started = Instant::now();

        assert_eq!(wait_bounded(&mut stuck, Duration::from_millis(50)), None);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            stuck.try_wait().expect("a reaped child").is_some(),
            "the stuck child is stopped and reaped"
        );

        let mut finished = Command::new("/usr/bin/true").spawn().expect("a child");
        assert!(
            wait_bounded(&mut finished, Duration::from_secs(5))
                .is_some_and(|status| status.success())
        );
    }

    #[test]
    fn a_report_round_trips_through_its_exact_encoding() {
        for screen_recording in [false, true] {
            for accessibility in [false, true] {
                let report = ProbeReport {
                    screen_recording,
                    accessibility,
                };
                assert_eq!(
                    ProbeReport::decode(report.encode().as_bytes()),
                    Some(report)
                );
            }
        }
        assert_eq!(
            ProbeReport {
                screen_recording: true,
                accessibility: false,
            }
            .encode(),
            "screen-recording=1 accessibility=0\n"
        );
    }

    #[test]
    fn a_report_rejects_any_other_output() {
        for output in [
            &b""[..],
            b"screen-recording=1 accessibility=0",
            b"screen-recording=2 accessibility=0\n",
            b"accessibility=0 screen-recording=1\n",
            b"screen-recording=1 accessibility=0\nextra",
        ] {
            assert_eq!(ProbeReport::decode(output), None);
        }
    }

    #[test]
    fn a_report_answers_each_permission_from_its_own_read() {
        let report = ProbeReport::read(|permission| permission == SystemPermission::Accessibility);

        assert_eq!(
            [
                report.authorization(SystemPermission::ScreenRecording),
                report.authorization(SystemPermission::Accessibility),
            ],
            [
                PermissionAuthorization::NotGranted,
                PermissionAuthorization::Granted,
            ]
        );
    }
}
