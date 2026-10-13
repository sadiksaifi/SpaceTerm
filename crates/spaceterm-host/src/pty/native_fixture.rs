//! Real PTY fixtures for native-test composition, including unavailable settlement inspection.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::native_pty::NativePtyAdapterFactory;
use super::unix_pty::{
    MasterReadError, ProcessCreationEpoch, ProcessIdentity, ProcessObservation,
    UnixNativePtyAdapterFactory, UnixPtyHost,
};
use crate::shell_launch::{PreparedShellLaunch, ShellLaunchPlanner};

/// Native processes with an injectable inspection failure, never a production provider.
#[derive(Clone)]
pub struct NativePtyFixture {
    host: Arc<FixtureHost>,
}

impl Default for NativePtyFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl NativePtyFixture {
    /// Select the real host adapter in explicitly native test composition.
    pub fn new() -> Self {
        #[cfg(target_os = "macos")]
        let native = super::macos_pty_host::MacosPtyHost;
        #[cfg(target_os = "linux")]
        let native = super::linux_pty_host::LinuxPtyHost;
        Self {
            host: Arc::new(FixtureHost {
                native: Arc::new(native),
                unavailable: AtomicBool::new(false),
            }),
        }
    }

    /// Supply the real native factory through the same port as application composition.
    pub fn factory(&self) -> Arc<dyn NativePtyAdapterFactory> {
        Arc::new(UnixNativePtyAdapterFactory::new(self.host.clone()))
    }

    /// Prepare a shell whose HOME and working directory are the caller's test fixture.
    pub fn shell_launch(&self, directory: &Path) -> PreparedShellLaunch {
        ShellLaunchPlanner::for_test("/bin/sh".into(), directory.into())
            .local(directory)
            .unwrap()
            .with_environment_value("HOME", directory.to_str().unwrap())
    }

    /// Prepare an exited leader with a live descendant that ignores HUP and TERM.
    /// The descendant closes every PTY slave and confirms its traps before the leader exits.
    pub fn exited_leader_launch(&self, directory: &Path) -> PreparedShellLaunch {
        let ready = directory.join("descendant-ready");
        let pid = directory.join("descendant-pid");
        let shell = directory.join("controlled-shell");
        let script = format!(
            "#!/bin/sh\ntrap '' HUP\n(trap '' HUP TERM; exec </dev/null >/dev/null 2>&1; echo ready > '{}'; while :; do sleep 1; done) &\nchild=$!\nwhile [ ! -f '{}' ]; do sleep 0.01; done\necho $child > '{}'\nexit 0\n",
            ready.display(),
            ready.display(),
            pid.display(),
        );
        std::fs::write(&shell, script).unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
        ShellLaunchPlanner::for_test(shell, directory.into())
            .local(directory)
            .unwrap()
    }

    /// Read the bounded numeric identity published by the controlled leader.
    pub fn descendant(&self, directory: &Path) -> i32 {
        std::fs::read_to_string(directory.join("descendant-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// Observe a real live process independently of the injected settlement failure.
    pub fn is_alive(&self, process: i32) -> bool {
        self.host.native.observe_process(process).is_some()
    }

    /// Keep signalling real processes but make settlement evidence unavailable.
    pub fn deny_settlement_inspection(&self) {
        self.host.unavailable.store(true, Ordering::Release);
    }
}

struct FixtureHost {
    native: Arc<dyn UnixPtyHost>,
    unavailable: AtomicBool,
}
impl UnixPtyHost for FixtureHost {
    fn observe_process(&self, process: i32) -> Option<ProcessObservation> {
        self.native.observe_process(process)
    }
    fn observe_spawned_child(&self, process: i32) -> Option<ProcessObservation> {
        self.native.observe_spawned_child(process)
    }
    fn inspect_process(&self, process: i32) -> io::Result<Option<ProcessObservation>> {
        if self.unavailable.load(Ordering::Acquire) {
            Err(io::ErrorKind::PermissionDenied.into())
        } else {
            self.native.inspect_process(process)
        }
    }
    fn process_birth(&self, process: i32) -> io::Result<Option<u64>> {
        self.native.process_birth(process)
    }
    fn inspect_session_member(
        &self,
        process: i32,
        leader: ProcessIdentity,
        leader_birth: Option<u64>,
    ) -> io::Result<Option<ProcessObservation>> {
        if self.unavailable.load(Ordering::Acquire) {
            Err(io::ErrorKind::PermissionDenied.into())
        } else {
            self.native
                .inspect_session_member(process, leader, leader_birth)
        }
    }
    fn process_ids(&self) -> io::Result<Vec<i32>> {
        self.native.process_ids()
    }
    fn process_creation_epoch(&self) -> io::Result<ProcessCreationEpoch> {
        self.native.process_creation_epoch()
    }
    fn settlement_process_ids(&self) -> io::Result<Vec<i32>> {
        self.native.settlement_process_ids()
    }
    fn master_read_error(&self, error: &io::Error) -> MasterReadError {
        self.native.master_read_error(error)
    }
}

/// Run a native PTY case in a fresh process before unrelated Foundation tests start threads.
/// Returns false only inside the selected child test.
pub fn isolate(name: &str) -> bool {
    const CHILD: &str = "SPACETERM_ISOLATED_TERMINAL_CLEANUP";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--test-threads=1", "--nocapture"])
        .env(CHILD, name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native cleanup test failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    true
}
