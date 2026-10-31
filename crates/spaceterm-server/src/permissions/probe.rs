//! Reads authorization from a child of the server, the way a program started now in a Terminal
//! Session reads it.
//!
//! A child's privacy checks belong to the server it inherits responsibility from, and a fresh
//! process holds no cached answer. The server's own Screen Recording read keeps its launch value
//! until the server restarts, so only a child follows a grant made since.

use std::io::Read as _;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::{PermissionError, PermissionState, SystemPermission};

/// The hidden command that runs the probe role.
const PROBE_ARGUMENTS: [&str; 3] = ["server", "provision", "permission-report"];
/// A probe answers within milliseconds. A slower one is stuck and is stopped.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const CHILD_POLL: Duration = Duration::from_millis(5);
const MAX_REPORT: usize = 256;

/// Every permission's authorization one probe read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Probe {
    states: Vec<(SystemPermission, PermissionState)>,
}

impl Probe {
    pub(super) fn read(read: impl Fn(SystemPermission) -> PermissionState) -> Self {
        Self {
            states: SystemPermission::ALL
                .iter()
                .map(|&permission| (permission, read(permission)))
                .collect(),
        }
    }

    /// Reads this process's own authorization.
    pub(super) fn read_in_process() -> Self {
        Self::read(super::macos::in_process_state)
    }

    pub(super) fn states(&self) -> &[(SystemPermission, PermissionState)] {
        &self.states
    }

    pub(super) fn state(&self, permission: SystemPermission) -> PermissionState {
        self.states
            .iter()
            .find_map(|(read, state)| (*read == permission).then_some(*state))
            .unwrap_or(PermissionState::NotGranted)
    }

    /// One line naming each permission and state in [`SystemPermission::ALL`] order.
    pub(super) fn encode(&self) -> String {
        let mut line = self
            .states
            .iter()
            .map(|(permission, state)| format!("{}={}", permission.as_str(), state_name(*state)))
            .collect::<Vec<_>>()
            .join(" ");
        line.push('\n');
        line
    }

    /// Accepts only the exact encoding of a report.
    fn decode(output: &[u8]) -> Option<Self> {
        let line = std::str::from_utf8(output).ok()?.strip_suffix('\n')?;
        let mut fields = line.split(' ');
        let mut states = Vec::new();
        for permission in SystemPermission::ALL {
            let (name, state) = fields.next()?.split_once('=')?;
            if name != permission.as_str() {
                return None;
            }
            states.push((permission, state_from_name(state)?));
        }
        let probe = Self { states };
        (fields.next().is_none() && probe.encode() == format!("{line}\n")).then_some(probe)
    }
}

const STATES: [(PermissionState, &str); 4] = [
    (PermissionState::NotRequested, "not_requested"),
    (PermissionState::Granted, "granted"),
    (PermissionState::NotGranted, "not_granted"),
    (PermissionState::Restricted, "restricted"),
];

fn state_name(state: PermissionState) -> &'static str {
    STATES
        .iter()
        .find_map(|(known, name)| (*known == state).then_some(*name))
        .unwrap_or("not_granted")
}

fn state_from_name(name: &str) -> Option<PermissionState> {
    STATES
        .iter()
        .find_map(|(state, known)| (*known == name).then_some(*state))
}

/// Starts this executable as a probe and waits for its report. It blocks for the probe's life,
/// so callers run it on a lane connection's thread.
pub(super) fn run() -> Result<Probe, PermissionError> {
    let executable = std::env::current_exe().map_err(|_| PermissionError::Unavailable)?;
    let mut child = Command::new(executable)
        .args(PROBE_ARGUMENTS)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| PermissionError::Unavailable)?;
    let status = wait_bounded(&mut child, PROBE_TIMEOUT).ok_or(PermissionError::Unavailable)?;
    let mut output = Vec::new();
    let read = child
        .stdout
        .take()
        .map(|stdout| stdout.take(MAX_REPORT as u64 + 1).read_to_end(&mut output));
    match read {
        Some(Ok(length)) if status.success() && length <= MAX_REPORT => {
            Probe::decode(&output).ok_or(PermissionError::Rejected)
        }
        _ => Err(PermissionError::Rejected),
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
        let probe = Probe::read(|permission| match permission {
            SystemPermission::Microphone => PermissionState::NotRequested,
            SystemPermission::ScreenRecording => PermissionState::Granted,
            SystemPermission::Accessibility => PermissionState::NotGranted,
        });
        let line = "microphone=not_requested screen_recording=granted accessibility=not_granted\n";
        assert_eq!(probe.encode(), line);
        assert_eq!(Probe::decode(line.as_bytes()), Some(probe));
    }

    #[test]
    fn a_report_rejects_any_other_output() {
        for output in [
            &b""[..],
            b"microphone=granted screen_recording=granted accessibility=granted",
            b"microphone=granted screen_recording=granted accessibility=maybe\n",
            b"screen_recording=granted microphone=granted accessibility=granted\n",
            b"microphone=granted screen_recording=granted accessibility=granted extra\n",
            b"microphone=granted  screen_recording=granted accessibility=granted\n",
        ] {
            assert_eq!(Probe::decode(output), None, "{output:?}");
        }
    }

    #[test]
    fn a_report_answers_each_permission_from_its_own_read() {
        let probe = Probe::read(|permission| {
            if permission == SystemPermission::Accessibility {
                PermissionState::Granted
            } else {
                PermissionState::NotGranted
            }
        });
        assert_eq!(
            SystemPermission::ALL.map(|permission| probe.state(permission)),
            [
                PermissionState::NotGranted,
                PermissionState::NotGranted,
                PermissionState::Granted,
            ]
        );
    }
}
