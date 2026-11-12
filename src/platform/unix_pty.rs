use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
#[cfg(all(test, feature = "native-tests"))]
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Error as AnyError;
use portable_pty::{
    Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system,
};
use spaceterm_core::diagnostics::TerminalCounters;
use spaceterm_core::pane::LineMode;
use thiserror::Error;

use crate::platform::native_pty::{
    NativePtyAdapter, NativePtyAdapterConstructionFailure, NativePtyAdapterFactory,
    NativePtyAdapterParts, NativePtyExit, NativePtyOperationFailure, NativePtySize,
    NativePtyTermination, NativePtyWaitFailure, SessionProbe,
};
use crate::platform::shell_launch::PreparedShellLaunch;
mod freeze;
#[cfg(all(test, feature = "native-tests"))]
use crate::platform::shell_launch::ShellLaunchPlanner;

const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const FORCED_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
// Real PTY cases fork nested test executables and login shells while asserting subsecond process-
// group cleanup. Serial execution keeps those OS-backed cases independent of runner scheduling.
#[cfg(all(test, feature = "native-tests"))]
static REAL_PTY_TEST_LOCK: Mutex<()> = Mutex::new(());
#[cfg(all(test, feature = "native-tests"))]
pub(crate) fn lock_real_pty_test() -> std::sync::MutexGuard<'static, ()> {
    REAL_PTY_TEST_LOCK
        .lock()
        .expect("a previous real PTY test panicked while it owned OS process resources")
}

#[cfg(all(test, feature = "native-tests"))]
pub(crate) fn isolate_real_pty_test(test_name: &str) -> bool {
    const CHILD_TEST: &str = "SPACETERM_ISOLATED_REAL_PTY_TEST";
    if std::env::var(CHILD_TEST).as_deref() == Ok(test_name) {
        eprintln!("SPACETERM_PTY_TEST_ENTERED={test_name}");
        return false;
    }

    // portable-pty forks before exec. The parallel native test process also runs Foundation
    // code, which can leave a forked child with corrupt os_once state. Run the PTY case in a
    // fresh test process before that process starts unrelated test threads.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--test-threads=1", "--nocapture"])
        .env(CHILD_TEST, test_name)
        .output()
        .expect("failed to start isolated real PTY test");
    assert!(
        output.status.success(),
        "isolated real PTY test failed: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .any(|line| line == format!("SPACETERM_PTY_TEST_ENTERED={test_name}")),
        "isolated real PTY test body did not execute: {test_name}"
    );
    true
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShutdownDisposition {
    NotRequested,
    Graceful,
    Forced,
}

#[derive(Debug)]
struct ShellExit {
    status: ExitStatus,
    shutdown: ShutdownDisposition,
}

struct TerminationTarget {
    host: Arc<dyn UnixPtyHost>,
    process_group: Option<i32>,
    session: Option<ProcessSessionIdentity>,
    fallback: Box<dyn ChildKiller + Send + Sync>,
}

/// Operating-System process facts that the shared POSIX PTY owner needs to supervise a Shell
/// Process session without trusting reusable process identifiers.
pub(super) trait UnixPtyHost: Send + Sync + 'static {
    fn pin_process(&self, _identity: ProcessIdentity) -> io::Result<Box<dyn ProcessControl>> {
        Err(io::Error::other("stable process signalling is unavailable"))
    }
    fn complete_process_ids(&self) -> io::Result<Vec<i32>> {
        self.process_ids()
    }
    fn holds_slave(&self, _process: i32, _slave: &std::path::Path) -> io::Result<bool> {
        Err(io::Error::other("PTY slave holders are unavailable"))
    }
    fn slave_holders(&self, slave: &std::path::Path) -> io::Result<Vec<ProcessIdentity>> {
        let mut ids = self.complete_process_ids()?;
        ids.sort_unstable();
        let mut holders = Vec::new();
        for &pid in &ids {
            if pid < 1 {
                continue;
            }
            let Some(before) = self.observe_exactly(pid)? else {
                continue;
            };
            let holds = self.holds_slave(pid, slave)?;
            if self.observe_exactly(pid)? != Some(before) {
                return Err(io::Error::other(
                    "process changed during descriptor enumeration",
                ));
            }
            if holds {
                holders.push(before.identity);
            }
        }
        let mut after = self.complete_process_ids()?;
        after.sort_unstable();
        if ids != after {
            return Err(io::Error::other("incomplete process enumeration"));
        }
        Ok(holders)
    }
    fn foreground(&self, master: RawFd) -> io::Result<i32> {
        // SAFETY: the caller retains the live PTY master throughout this query.
        let group = unsafe { libc::tcgetpgrp(master) };
        if group > 0 {
            Ok(group)
        } else {
            Err(io::Error::last_os_error())
        }
    }
    fn pending_output(&self, master: RawFd) -> io::Result<bool> {
        let mut count: libc::c_int = 0;
        // SAFETY: master is live and count is writable ioctl storage.
        if unsafe { libc::ioctl(master, libc::FIONREAD, &mut count) } != 0 || count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count != 0 {
            return Ok(true);
        }
        // Darwin can report zero here while master output is already readable. Treat any
        // readiness or hangup as work too; never read it past the receipt gate to find out.
        let mut ready = libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: master is retained and ready is writable storage for one descriptor.
        let result = unsafe { libc::poll(&mut ready, 1, 0) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result != 0)
        }
    }
    /// Observe one live process. Returns `None` for a missing, exited, or unstable process.
    fn observe_process(&self, process: i32) -> Option<ProcessObservation>;

    /// Capture the identity of a child this owner just spawned and has not reaped. A child may
    /// already have exited before the parent runs again, while its process identity is retained.
    fn observe_spawned_child(&self, process: i32) -> Option<ProcessObservation> {
        self.observe_process(process)
    }

    /// List every process identifier currently visible to this user.
    fn process_ids(&self) -> io::Result<Vec<i32>>;

    /// Observe one process for a decision that must not miss one. `Ok(None)` only when the
    /// process is proven gone; an error when it could not be read or changed while it was read.
    /// A host that cannot tell the two apart reports every process as unreadable.
    fn observe_exactly(&self, _process: i32) -> io::Result<Option<ProcessObservation>> {
        Err(io::Error::other("this host cannot prove a process gone"))
    }

    /// Every process that may belong to the session `leader` leads: a superset of its members
    /// that [`Self::observe_exactly`] can read. By default every visible process.
    fn session_candidates(&self, _leader: i32) -> io::Result<Vec<i32>> {
        self.process_ids()
    }

    fn master_read_error(&self, error: &io::Error) -> MasterReadError;
}
/// An OS-bound process identity. Signals must never fall back to a reusable PID.
pub(super) trait ProcessControl: Send + Sync {
    fn identity(&self) -> ProcessIdentity;
    fn stopped(&self) -> io::Result<bool>;
    fn signal(&self, signal: i32) -> io::Result<()>;
}

/// How the shared PTY reader treats an Operating-System read failure on the PTY master.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MasterReadError {
    /// Every slave descriptor closed; the terminal output ended.
    Hangup,
    /// The read failed for another reason.
    Failure,
}

/// A process start instant in host-defined units; equal starts identify the same process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProcessStart {
    pub(super) coarse: u64,
    pub(super) fine: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProcessIdentity {
    pub(super) process: i32,
    pub(super) start: ProcessStart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessSessionIdentity {
    session: i32,
    leader: ProcessIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProcessObservation {
    pub(super) identity: ProcessIdentity,
    pub(super) process_group: i32,
    pub(super) session: i32,
}

impl TerminationTarget {
    fn owns_process_group(&self) -> bool {
        self.process_group.is_some()
    }

    fn hang_up(&mut self) -> io::Result<()> {
        self.signal_owned_process_groups(libc::SIGHUP)
    }

    fn force(&mut self) -> io::Result<()> {
        self.signal_owned_process_groups(libc::SIGKILL)
    }

    fn signal_owned_process_groups(&mut self, signal: i32) -> io::Result<()> {
        let Some(owner_group) = self.process_group else {
            return self.fallback.kill();
        };
        let Some(session) = self.session else {
            return signal_process_group(owner_group, signal);
        };
        let mut process_groups = process_groups_in_session(self.host.as_ref(), &session)?;
        let owner_members = process_groups.remove(&owner_group).unwrap_or_default();
        let mut first_error = None;
        for (process_group, members) in process_groups {
            if let Err(error) = signal_owned_process_group(
                self.host.as_ref(),
                process_group,
                &members,
                &session,
                signal,
            ) {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = signal_owned_process_group(
            self.host.as_ref(),
            owner_group,
            &owner_members,
            &session,
            signal,
        ) {
            first_error.get_or_insert(error);
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn is_alive(&mut self) -> io::Result<bool> {
        let Some(process_group) = self.process_group else {
            return Ok(true);
        };
        match self.session {
            Some(session) => {
                let process_groups = process_groups_in_session(self.host.as_ref(), &session)?;
                Ok(!process_groups.is_empty())
            }
            None => process_group_is_alive(process_group),
        }
    }
}

struct ChildTermination {
    target: Mutex<Option<TerminationTarget>>,
    requested: AtomicBool,
    forced: AtomicBool,
}

impl ChildTermination {
    #[cfg(all(test, feature = "native-tests"))]
    fn new(
        host: Arc<dyn UnixPtyHost>,
        process_group: Option<i32>,
        fallback: Box<dyn ChildKiller + Send + Sync>,
    ) -> Self {
        Self {
            target: Mutex::new(Some(TerminationTarget {
                host,
                process_group,
                session: None,
                fallback,
            })),
            requested: AtomicBool::new(false),
            forced: AtomicBool::new(false),
        }
    }

    fn new_with_session(
        host: Arc<dyn UnixPtyHost>,
        process_group: i32,
        session: ProcessSessionIdentity,
        fallback: Box<dyn ChildKiller + Send + Sync>,
    ) -> Self {
        Self {
            target: Mutex::new(Some(TerminationTarget {
                host,
                process_group: Some(process_group),
                session: Some(session),
                fallback,
            })),
            requested: AtomicBool::new(false),
            forced: AtomicBool::new(false),
        }
    }

    fn signal(&self) -> io::Result<()> {
        if self.requested.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut target = self.lock_target();
        if self.requested.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        match target.as_mut() {
            Some(target) => target.hang_up(),
            None => Ok(()),
        }
    }

    fn lock_target(&self) -> std::sync::MutexGuard<'_, Option<TerminationTarget>> {
        match self.target.lock() {
            Ok(target) => target,
            Err(poisoned) => {
                eprintln!("terminal child-termination coordination lock was poisoned; recovering");
                poisoned.into_inner()
            }
        }
    }

    fn revoke(&self) {
        self.lock_target().take();
    }

    fn complete_process_group(
        &self,
        graceful_deadline: Instant,
    ) -> io::Result<ShutdownDisposition> {
        let mut target_slot = self.lock_target();
        if !self.requested.load(Ordering::Acquire) {
            // A queued close may reach the termination supervisor after the session leader is
            // reaped. Keep stable session authority so that close can still find its descendants.
            let retains_session_authority = target_slot
                .as_ref()
                .and_then(|target| target.session)
                .is_some();
            if !retains_session_authority {
                target_slot.take();
            }
            return Ok(ShutdownDisposition::NotRequested);
        }

        let Some(target) = target_slot.as_mut() else {
            return Ok(if self.forced.load(Ordering::Acquire) {
                ShutdownDisposition::Forced
            } else {
                ShutdownDisposition::Graceful
            });
        };
        if !target.owns_process_group() {
            target_slot.take();
            return Ok(ShutdownDisposition::Graceful);
        }

        while Instant::now() < graceful_deadline {
            match target.is_alive() {
                Ok(false) => {
                    target_slot.take();
                    return Ok(if self.forced.load(Ordering::Acquire) {
                        ShutdownDisposition::Forced
                    } else {
                        ShutdownDisposition::Graceful
                    });
                }
                Ok(true) => thread::sleep(CHILD_EXIT_POLL_INTERVAL),
                Err(error) => {
                    eprintln!(
                        "failed to inspect shell process group during shutdown: {:?}",
                        error.kind()
                    );
                    break;
                }
            }
        }

        target.force()?;
        self.forced.store(true, Ordering::Release);
        // Signal delivery does not prove that every owned process has exited. Descendants can
        // outlive the reaped leader while the kernel schedules forced exit.
        let forced_deadline = Instant::now() + FORCED_SHUTDOWN_TIMEOUT;
        loop {
            if !target.is_alive()? {
                target_slot.take();
                break;
            }
            let remaining = forced_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            thread::sleep(CHILD_EXIT_POLL_INTERVAL.min(remaining));
        }
        Ok(ShutdownDisposition::Forced)
    }

    fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

fn signal_process_group(process_group: i32, signal: i32) -> io::Result<()> {
    // SAFETY: a negative PID addresses the process group created for this PTY.
    if unsafe { libc::kill(-process_group, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn process_group_is_alive(process_group: i32) -> io::Result<bool> {
    // SAFETY: signal zero performs only an existence/permission check.
    if unsafe { libc::kill(-process_group, 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

/// The shell's terminal session, read through the host's process facts.
struct ShellSession {
    host: Arc<dyn UnixPtyHost>,
    session: ProcessSessionIdentity,
    foreground: std::sync::Weak<OwnedFd>,
    activity: Arc<Mutex<Option<spaceterm_core::idle_restart::Activity>>>,
    slave: Option<std::path::PathBuf>,
    frozen: Mutex<Vec<std::sync::Weak<dyn ProcessControl>>>,
}

impl SessionProbe for ShellSession {
    fn freeze(&self) -> io::Result<Box<dyn spaceterm_core::idle_restart::Frozen>> {
        let master = self
            .foreground
            .upgrade()
            .ok_or_else(|| io::Error::other("the PTY is gone"))?;
        let slave = self
            .slave
            .as_deref()
            .ok_or_else(|| io::Error::other("the PTY slave is unknown"))?;
        let population = freeze::Population::capture(
            self.host.clone(),
            self.session,
            &master,
            slave,
            Instant::now() + Duration::from_millis(250),
        )?;
        *self.frozen.lock().unwrap_or_else(|e| e.into_inner()) = population.controls();
        Ok(Box::new(population))
    }
    fn activity(&self) -> Option<spaceterm_core::idle_restart::Activity> {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn shell_foreground(&self) -> io::Result<bool> {
        let descriptor = self
            .foreground
            .upgrade()
            .ok_or_else(|| io::Error::other("the PTY foreground is unavailable"))?;
        // SAFETY: the probe owns this live PTY master descriptor.
        let foreground = unsafe { libc::tcgetpgrp(descriptor.as_raw_fd()) };
        if foreground < 1 {
            return Err(io::Error::last_os_error());
        }
        Ok(foreground == self.session.leader.process)
    }

    /// Reads every visible process exactly, unlike cleanup's best-effort enumeration: a
    /// process that could not be read is an error, never a process that is not there.
    fn others(&self) -> io::Result<bool> {
        let leader = self
            .host
            .observe_exactly(self.session.session)?
            .ok_or_else(|| io::Error::other("the shell process is gone"))?;
        if leader.identity != self.session.leader || leader.session != self.session.session {
            return Err(io::Error::other("shell process ownership is unresolved"));
        }
        for process in self.host.session_candidates(self.session.session)? {
            if process < 1 || process == self.session.leader.process {
                continue;
            }
            match self.host.observe_exactly(process)? {
                Some(observation) if observation.session == self.session.session => {
                    return Ok(true);
                }
                _ => {}
            }
        }
        if self.host.observe_exactly(self.session.session)? != Some(leader) {
            return Err(io::Error::other("shell process ownership changed"));
        }
        Ok(false)
    }
}

fn process_groups_in_session(
    host: &dyn UnixPtyHost,
    session: &ProcessSessionIdentity,
) -> io::Result<BTreeMap<i32, Vec<ProcessIdentity>>> {
    let leader = host.observe_process(session.session);
    if leader.is_some_and(|leader| {
        leader.identity != session.leader || leader.session != session.session
    }) {
        return Err(io::Error::other("shell process ownership is unresolved"));
    }

    let mut process_groups = BTreeMap::<_, Vec<_>>::new();
    for process in host
        .process_ids()?
        .into_iter()
        .filter(|process| *process > 0)
    {
        let Some(observation) = host.observe_process(process) else {
            continue;
        };
        if observation.session != session.session {
            continue;
        }
        process_groups
            .entry(observation.process_group)
            .or_default()
            .push(observation.identity);
    }
    Ok(process_groups)
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    /// Process facts by PID: `Ok(None)` gone, `Err(())` unreadable.
    struct Facts(Vec<(i32, Result<Option<ProcessObservation>, ()>)>);
    impl UnixPtyHost for Facts {
        fn observe_process(&self, _: i32) -> Option<ProcessObservation> {
            panic!("the session probe reads processes exactly")
        }
        fn process_ids(&self) -> io::Result<Vec<i32>> {
            Ok(self.0.iter().map(|(process, _)| *process).collect())
        }
        fn observe_exactly(&self, process: i32) -> io::Result<Option<ProcessObservation>> {
            match self.0.iter().find(|(id, _)| *id == process) {
                Some((_, Ok(observation))) => Ok(*observation),
                Some((_, Err(()))) => Err(io::Error::other("unreadable")),
                None => Ok(None),
            }
        }
        fn master_read_error(&self, _: &io::Error) -> MasterReadError {
            MasterReadError::Failure
        }
    }

    fn observed(process: i32, process_group: i32, session: i32) -> ProcessObservation {
        ProcessObservation {
            identity: ProcessIdentity {
                process,
                start: ProcessStart { coarse: 1, fine: 0 },
            },
            process_group,
            session,
        }
    }

    fn probe(others: Vec<(i32, Result<Option<ProcessObservation>, ()>)>) -> io::Result<bool> {
        let shell = observed(100, 100, 100);
        let mut facts = vec![(100, Ok(Some(shell)))];
        facts.extend(others);
        ShellSession {
            host: Arc::new(Facts(facts)),
            foreground: std::sync::Weak::new(),
            activity: Arc::default(),
            slave: None,
            frozen: Mutex::new(Vec::new()),
            session: ProcessSessionIdentity {
                session: 100,
                leader: shell.identity,
            },
        }
        .others()
    }

    #[test]
    fn a_readable_shell_with_an_unreadable_process_beside_it_is_not_alone() {
        assert!(probe(vec![(7, Err(()))]).is_err());
        assert!(probe(vec![(200, Ok(Some(observed(200, 200, 200)))), (7, Err(()))]).is_err());
    }

    #[test]
    fn the_session_probe_finds_members_in_every_process_group_of_the_session() {
        assert!(!probe(vec![(200, Ok(Some(observed(200, 200, 200))))]).unwrap());
        assert!(
            !probe(vec![(201, Ok(None))]).unwrap(),
            "a process proven gone"
        );
        // A stopped or background job in its own process group, as job control places it.
        assert!(probe(vec![(101, Ok(Some(observed(101, 101, 100))))]).unwrap());
        assert!(probe(vec![(102, Ok(Some(observed(102, 100, 100))))]).unwrap());
    }

    #[test]
    fn a_shell_that_is_gone_or_replaced_is_not_alone() {
        let shell = observed(100, 100, 100);
        let reused = ProcessObservation {
            identity: ProcessIdentity {
                start: ProcessStart { coarse: 2, fine: 0 },
                ..shell.identity
            },
            ..shell
        };
        for leader in [Ok(None), Ok(Some(reused)), Err(())] {
            let probe = ShellSession {
                host: Arc::new(Facts(vec![(100, leader)])),
                foreground: std::sync::Weak::new(),
                activity: Arc::default(),
                slave: None,
                frozen: Mutex::new(Vec::new()),
                session: ProcessSessionIdentity {
                    session: 100,
                    leader: shell.identity,
                },
            };
            assert!(probe.others().is_err());
        }
    }
    struct ReusedLeader(ProcessObservation);
    impl UnixPtyHost for ReusedLeader {
        fn observe_process(&self, _: i32) -> Option<ProcessObservation> {
            Some(self.0)
        }
        fn process_ids(&self) -> io::Result<Vec<i32>> {
            Ok(vec![self.0.identity.process])
        }
        fn master_read_error(&self, _: &io::Error) -> MasterReadError {
            MasterReadError::Failure
        }
    }
    #[test]
    fn a_reused_session_leader_is_unresolved_cleanup() {
        let leader = ProcessIdentity {
            process: 123,
            start: ProcessStart { coarse: 1, fine: 0 },
        };
        let host = ReusedLeader(ProcessObservation {
            identity: ProcessIdentity {
                start: ProcessStart { coarse: 2, fine: 0 },
                ..leader
            },
            process_group: 123,
            session: 123,
        });
        assert!(
            process_groups_in_session(
                &host,
                &ProcessSessionIdentity {
                    session: 123,
                    leader
                }
            )
            .is_err()
        );
    }
}

fn signal_owned_process_group(
    host: &dyn UnixPtyHost,
    process_group: i32,
    members: &[ProcessIdentity],
    session: &ProcessSessionIdentity,
    signal: i32,
) -> io::Result<()> {
    let still_owned = members.iter().any(|member| {
        host.observe_process(member.process)
            .is_some_and(|observation| {
                observation.identity == *member
                    && observation.process_group == process_group
                    && observation.session == session.session
            })
    });
    if still_owned {
        signal_process_group(process_group, signal)
    } else {
        Ok(())
    }
}

fn child_already_reaped_error() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "shell process was already reaped")
}

/// Presents a PTY master hangup as end of terminal output on every host.
struct MasterReader {
    inner: Box<dyn Read + Send>,
    host: Arc<dyn UnixPtyHost>,
}

impl Read for MasterReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.inner.read(buffer) {
            Err(error) if self.host.master_read_error(&error) == MasterReadError::Hangup => Ok(0),
            result => result,
        }
    }
}

struct PtyTerminator {
    termination: Arc<ChildTermination>,
}

impl PtyTerminator {
    fn new(termination: Arc<ChildTermination>) -> Self {
        Self { termination }
    }

    fn terminate(&self) -> io::Result<()> {
        self.termination.signal()
    }
}

struct SpawnedPty {
    master: Box<dyn MasterPty + Send>,
    reader: Option<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    reader_stop: Option<OwnedFd>,
    closing: Arc<AtomicBool>,
    /// The shell's process group, which leads its own session.
    shell_process_group: libc::pid_t,
    probe: Arc<ShellSession>,
    /// The probe borrows this descriptor. It closes before waiting for PTY teardown, even
    /// when an observer retains the probe after the owner ends.
    _foreground: Option<Arc<OwnedFd>>,
    // Fields drop in declaration order. Close the PTY handles before waiting for a child whose
    // kernel exit can be waiting for terminal teardown.
    process: SpawnedChild,
}

struct SpawnedChild {
    child: Option<Box<dyn Child + Send + Sync>>,
    termination: Arc<ChildTermination>,
}

struct PtyReader {
    activity: Arc<Mutex<Option<spaceterm_core::idle_restart::Activity>>>,
    reader: MasterReader,
    readiness: OwnedFd,
    stop: OwnedFd,
    closing: Arc<AtomicBool>,
    /// Counts every native read, including each one retried after it would block.
    counters: Arc<TerminalCounters>,
}

impl Read for PtyReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.closing.load(Ordering::Acquire) {
                return Ok(0);
            }
            let activity = self
                .activity
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let mut read = || {
                let result = self.reader.read(buffer);
                self.counters.record_pty_read(&result);
                result
            };
            let result = match activity {
                Some(activity) => activity.read(read),
                None => read(),
            };
            match result {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let mut descriptors = [
                        libc::pollfd {
                            fd: self.readiness.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: self.stop.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];
                    // SAFETY: both descriptors are owned and the result array is writable.
                    if unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) } < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    } else if descriptors[1].revents != 0 {
                        return Ok(0);
                    }
                }
                result => return result,
            }
        }
    }
}

impl Drop for SpawnedPty {
    fn drop(&mut self) {
        // Terminal teardown is the destructive Stop boundary. Kill the exact frozen processes
        // before closing masters: kernel hangup can send SIGCONT along with SIGHUP.
        for process in self
            .probe
            .frozen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .filter_map(|process| process.upgrade())
        {
            if let Err(error) = process.signal(libc::SIGKILL)
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                eprintln!(
                    "failed to end a frozen terminal process: {:?}",
                    error.kind()
                );
            }
        }
        // An outside slave holder can suppress master EOF on Linux. Wake the reader through
        // a separate descriptor so it closes its masters without reading past the grant.
        self.closing.store(true, Ordering::Release);
        drop(self.reader_stop.take());
    }
}

impl SpawnedPty {
    fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
        self.reader
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PTY reader was already taken"))
    }

    fn resize(&self, size: PtySize) -> Result<(), AnyError> {
        self.master.resize(size)
    }

    fn line_mode(&self) -> io::Result<LineMode> {
        let descriptor = self
            .master
            .as_raw_fd()
            .ok_or_else(|| io::Error::other("PTY master descriptor is unavailable"))?;
        let termios = read_termios(descriptor)?;
        // SAFETY: descriptor is a live PTY master.
        let foreground = unsafe { libc::tcgetpgrp(descriptor) };
        if foreground == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(termios_line_mode(
            &termios,
            foreground == self.shell_process_group,
        ))
    }
}

impl SpawnedChild {
    fn wait_for_child(&mut self, timeout: Duration) -> io::Result<ShellExit> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait_for_child()? {
                let shutdown = self
                    .termination
                    .complete_process_group(Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT)?;
                return Ok(ShellExit { status, shutdown });
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "timed out after {} ms waiting for the shell process to exit",
                        timeout.as_millis()
                    ),
                ));
            }
            thread::sleep(CHILD_EXIT_POLL_INTERVAL.min(remaining));
        }
    }

    fn try_wait_for_child(&mut self) -> io::Result<Option<ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Err(child_already_reaped_error());
        };
        let status = child.try_wait()?;
        if status.is_some() {
            self.child.take();
        }
        Ok(status)
    }

    fn cleanup_child(&mut self) {
        let child = self.child.take();
        let Some(mut child) = child else {
            if self.termination.requested()
                && let Err(error) = self
                    .termination
                    .complete_process_group(Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT)
            {
                eprintln!(
                    "failed to retry shell process group shutdown: {:?}",
                    error.kind()
                );
            }
            return;
        };

        match child.try_wait() {
            Ok(Some(_)) => {
                if let Err(error) = self
                    .termination
                    .complete_process_group(Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT)
                {
                    eprintln!(
                        "failed to complete shell process group shutdown: {:?}",
                        error.kind()
                    );
                }
                return;
            }
            Ok(None) => {}
            Err(error) => eprintln!(
                "failed to inspect shell process during cleanup: {:?}",
                error.kind()
            ),
        }

        let owns_process_group = self
            .termination
            .lock_target()
            .as_ref()
            .is_some_and(TerminationTarget::owns_process_group);
        if owns_process_group {
            self.cleanup_process_group(child.as_mut());
            return;
        }

        self.termination.revoke();

        if let Err(error) = child.kill() {
            eprintln!(
                "failed to terminate shell process during cleanup: {:?}",
                error.kind()
            );
            Self::reap_after_failed_termination(child.as_mut());
            return;
        }

        if let Err(error) = child.wait() {
            eprintln!(
                "failed to reap shell process during cleanup: {:?}",
                error.kind()
            );
        }
    }

    fn cleanup_process_group(&self, child: &mut dyn Child) {
        if !self.termination.requested()
            && let Err(error) = self.termination.signal()
        {
            eprintln!(
                "failed to gracefully terminate shell process group: {:?}",
                error.kind()
            );
        }

        let graceful_deadline = Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT;
        if Self::poll_child_until(child, graceful_deadline) {
            if let Err(error) = self.termination.complete_process_group(graceful_deadline) {
                eprintln!(
                    "failed to complete shell process group shutdown: {:?}",
                    error.kind()
                );
            }
            return;
        }

        if let Err(error) = self.termination.complete_process_group(graceful_deadline) {
            eprintln!(
                "failed to force shell process group shutdown: {:?}",
                error.kind()
            );
        }
        let forced_deadline = Instant::now() + FORCED_SHUTDOWN_TIMEOUT;
        if !Self::poll_child_until(child, forced_deadline) {
            eprintln!("shell process did not become reapable after forced process-group shutdown");
            if let Err(error) = child.wait() {
                eprintln!(
                    "failed to reap shell process after bounded shutdown: {:?}",
                    error.kind()
                );
            }
        }
        if let Err(error) = self.termination.complete_process_group(Instant::now()) {
            eprintln!(
                "failed to retry shell process group shutdown: {:?}",
                error.kind()
            );
        }
    }

    fn poll_child_until(child: &mut dyn Child, deadline: Instant) -> bool {
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => {}
                Err(error) => {
                    eprintln!(
                        "failed to inspect shell process during shutdown: {:?}",
                        error.kind()
                    );
                    return false;
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            thread::sleep(CHILD_EXIT_POLL_INTERVAL.min(remaining));
        }
    }

    fn reap_after_failed_termination(child: &mut dyn Child) {
        match child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                if let Err(error) = child.wait() {
                    eprintln!(
                        "failed to reap shell process after termination failed: {:?}",
                        error.kind()
                    );
                }
            }
            Err(error) => {
                eprintln!(
                    "failed to recheck shell process after termination failed: {:?}",
                    error.kind()
                );
                if let Err(wait_error) = child.wait() {
                    eprintln!(
                        "failed to reap shell process after inspection failed: {:?}",
                        wait_error.kind()
                    );
                }
            }
        }
    }
}

impl Write for SpawnedPty {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.writer.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl NativePtyAdapter for SpawnedPty {
    fn bind_activity(&mut self, activity: Option<spaceterm_core::idle_restart::Activity>) {
        *self
            .probe
            .activity
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = activity;
    }

    fn take_reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
        Self::take_reader(self)
    }

    fn resize(&self, size: NativePtySize) -> Result<(), NativePtyOperationFailure> {
        Self::resize(self, portable_size(size)).map_err(|error| {
            NativePtyOperationFailure::new(
                error
                    .downcast_ref::<io::Error>()
                    .map_or(io::ErrorKind::Other, io::Error::kind),
            )
        })
    }

    fn line_mode(&self) -> Result<LineMode, NativePtyOperationFailure> {
        Self::line_mode(self).map_err(|error| NativePtyOperationFailure::new(error.kind()))
    }

    fn session_probe(&self) -> Option<Arc<dyn SessionProbe>> {
        Some(Arc::clone(&self.probe) as Arc<dyn SessionProbe>)
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Result<NativePtyExit, NativePtyWaitFailure> {
        let exit = self
            .process
            .wait_for_child(timeout)
            .map_err(|error| NativePtyWaitFailure::new(error.kind()))?;
        Ok(native_exit(exit))
    }
}

fn native_exit(exit: ShellExit) -> NativePtyExit {
    match exit.shutdown {
        ShutdownDisposition::Graceful => NativePtyExit::GracefulShutdown,
        ShutdownDisposition::Forced => NativePtyExit::ForcedShutdown,
        ShutdownDisposition::NotRequested => match exit.status.signal() {
            Some(signal) => NativePtyExit::Signal(signal.to_owned()),
            None if exit.status.success() => NativePtyExit::Success,
            None => NativePtyExit::ExitCode(exit.status.exit_code()),
        },
    }
}

impl NativePtyTermination for PtyTerminator {
    fn cleanup_confirmed(&self) -> bool {
        self.termination
            .lock_target()
            .as_mut()
            .is_none_or(|target| matches!(target.is_alive(), Ok(false)))
    }

    fn request_termination(&self) -> io::Result<()> {
        self.terminate()
    }
}

impl Drop for SpawnedChild {
    fn drop(&mut self) {
        self.cleanup_child();
    }
}

#[derive(Debug, Error)]
pub(super) enum PtyError {
    #[error("failed to open the pseudo-terminal: {0}")]
    Open(#[source] AnyError),
    #[error("the pseudo-terminal did not expose its native descriptor")]
    MissingDescriptor,
    #[error("failed to read the pseudo-terminal attributes: {0}")]
    ReadTermios(#[source] io::Error),
    #[error("failed to enable UTF-8 input on the pseudo-terminal: {0}")]
    ConfigureTermios(#[source] io::Error),
    #[error("failed to configure nonblocking pseudo-terminal input")]
    ConfigureInput,
    #[error("failed to apply the initial macOS pseudo-terminal size: {0}")]
    InitialResize(#[source] AnyError),
    #[error("failed to start Shell Process")]
    SpawnShell,
    #[error("the shell process did not expose its process-group identifier")]
    MissingProcessGroup,
    #[error("failed to identify the shell process session: {0}")]
    InspectSession(#[source] io::Error),
    #[error("the shell process did not establish an isolated process session")]
    UnisolatedSession,
    #[error("failed to clone the pseudo-terminal reader: {0}")]
    CloneReader(#[source] AnyError),
    #[error("failed to acquire the pseudo-terminal writer: {0}")]
    TakeWriter(#[source] AnyError),
}

/// The application-selected POSIX Native PTY construction implementation.
#[derive(Clone)]
pub(crate) struct UnixNativePtyAdapterFactory {
    host: Arc<dyn UnixPtyHost>,
    counters: Arc<TerminalCounters>,
}

impl UnixNativePtyAdapterFactory {
    /// Each PTY it makes counts its native reads in `counters`.
    pub(super) fn new(host: Arc<dyn UnixPtyHost>, counters: Arc<TerminalCounters>) -> Self {
        Self { host, counters }
    }
}

impl NativePtyAdapterFactory for UnixNativePtyAdapterFactory {
    fn create(
        &self,
        launch: PreparedShellLaunch,
        size: NativePtySize,
    ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure> {
        spawn_native_pty(
            Arc::clone(&self.host),
            launch,
            size,
            Arc::clone(&self.counters),
        )
        .map_err(classify_pty_construction_failure)
    }
}

fn classify_pty_construction_failure(error: PtyError) -> NativePtyAdapterConstructionFailure {
    match error {
        PtyError::Open(_) => NativePtyAdapterConstructionFailure::ResourceCreationFailed,
        PtyError::MissingDescriptor
        | PtyError::ReadTermios(_)
        | PtyError::ConfigureTermios(_)
        | PtyError::ConfigureInput
        | PtyError::InitialResize(_) => {
            NativePtyAdapterConstructionFailure::ResourceConfigurationFailed
        }
        PtyError::SpawnShell => NativePtyAdapterConstructionFailure::ProcessStartFailed,
        PtyError::MissingProcessGroup
        | PtyError::InspectSession(_)
        | PtyError::UnisolatedSession => {
            NativePtyAdapterConstructionFailure::ProcessOwnershipFailed
        }
        PtyError::CloneReader(_) | PtyError::TakeWriter(_) => {
            NativePtyAdapterConstructionFailure::InputOutputUnavailable
        }
    }
}

fn spawn_native_pty(
    host: Arc<dyn UnixPtyHost>,
    launch: PreparedShellLaunch,
    size: NativePtySize,
    counters: Arc<TerminalCounters>,
) -> Result<NativePtyAdapterParts, PtyError> {
    let command = command_from_launch(&launch);
    let (pty, termination) = spawn_command_in_pty(host, portable_size(size), command, counters)?;
    Ok(NativePtyAdapterParts {
        adapter: Box::new(pty),
        termination: Arc::new(termination),
    })
}

const fn portable_size(size: NativePtySize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.columns,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}

fn spawn_command_in_pty(
    host: Arc<dyn UnixPtyHost>,
    size: PtySize,
    command: CommandBuilder,
    counters: Arc<TerminalCounters>,
) -> Result<(SpawnedPty, PtyTerminator), PtyError> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(size).map_err(PtyError::Open)?;
    initialize_pty(pair.master.as_ref(), size)?;
    let readiness = pair.master.as_raw_fd().ok_or(PtyError::MissingDescriptor)?;
    // SAFETY: pair.master owns this live descriptor throughout cloning.
    let readiness = unsafe { BorrowedFd::borrow_raw(readiness) }
        .try_clone_to_owned()
        .map_err(|error| PtyError::CloneReader(error.into()))?;

    let (reader_stop, owner_stop) = std::os::unix::net::UnixStream::pair()
        .map_err(|error| PtyError::CloneReader(error.into()))?;
    let closing = Arc::new(AtomicBool::new(false));
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|_| PtyError::SpawnShell)?;

    // The application never uses the slave side after spawning the shell.
    drop(pair.slave);

    let reader = match pair.master.try_clone_reader() {
        Ok(reader) => MasterReader {
            inner: reader,
            host: Arc::clone(&host),
        },
        Err(source) => {
            terminate_after_startup_failure(child.as_mut());
            return Err(PtyError::CloneReader(source));
        }
    };
    // portable-pty's writer sends newline and EOF on drop. Closing a Terminal Session must
    // only close its descriptor, without submitting a pending command or confirmation.
    let writer: Box<dyn Write + Send> = match readiness.try_clone() {
        Ok(writer) => Box::new(std::fs::File::from(writer)),
        Err(source) => {
            terminate_after_startup_failure(child.as_mut());
            return Err(PtyError::TakeWriter(source.into()));
        }
    };
    let activity = Arc::new(Mutex::new(None));
    let reader: Box<dyn Read + Send> = Box::new(PtyReader {
        activity: Arc::clone(&activity),
        reader,
        readiness,
        stop: reader_stop.into(),
        closing: closing.clone(),
        counters,
    });

    let process_group = child
        .process_id()
        .and_then(|process_id| i32::try_from(process_id).ok());
    let Some(process_group) = process_group else {
        terminate_after_startup_failure(child.as_mut());
        return Err(PtyError::MissingProcessGroup);
    };
    let Some(observation) = host.observe_spawned_child(process_group) else {
        terminate_after_startup_failure(child.as_mut());
        return Err(PtyError::InspectSession(io::Error::other(
            "the child process identity was unavailable",
        )));
    };
    if observation.session != process_group || observation.process_group != process_group {
        terminate_after_startup_failure(child.as_mut());
        return Err(PtyError::UnisolatedSession);
    }
    let session = ProcessSessionIdentity {
        session: observation.session,
        leader: observation.identity,
    };
    let foreground = pair.master.as_raw_fd().and_then(|descriptor| {
        // SAFETY: pair.master owns this live descriptor throughout cloning.
        unsafe { BorrowedFd::borrow_raw(descriptor) }
            .try_clone_to_owned()
            .ok()
            .map(Arc::new)
    });
    let probe = Arc::new(ShellSession {
        host: Arc::clone(&host),
        session,
        // Failure leaves the read unavailable, which counts as work.
        foreground: foreground.as_ref().map(Arc::downgrade).unwrap_or_default(),
        activity,
        slave: pair.master.tty_name(),
        frozen: Mutex::new(Vec::new()),
    });
    let termination = Arc::new(ChildTermination::new_with_session(
        host,
        process_group,
        session,
        child.clone_killer(),
    ));
    let terminator = PtyTerminator::new(Arc::clone(&termination));

    Ok((
        SpawnedPty {
            master: pair.master,
            reader: Some(Box::new(reader)),
            writer,
            reader_stop: Some(owner_stop.into()),
            closing,
            shell_process_group: process_group,
            probe,
            _foreground: foreground,
            process: SpawnedChild {
                child: Some(child),
                termination,
            },
        },
        terminator,
    ))
}

fn initialize_pty(master: &dyn MasterPty, size: PtySize) -> Result<(), PtyError> {
    let descriptor = master.as_raw_fd().ok_or(PtyError::MissingDescriptor)?;
    let mut termios = read_termios(descriptor).map_err(PtyError::ReadTermios)?;
    termios.c_iflag |= libc::IUTF8;
    // SAFETY: descriptor remains live and termios contains attributes read from this PTY.
    if unsafe { libc::tcsetattr(descriptor, libc::TCSANOW, &termios) } == -1 {
        return Err(PtyError::ConfigureTermios(io::Error::last_os_error()));
    }
    // Cloned master descriptors share these flags. The reader waits through poll, while input
    // writes return immediately so child backpressure cannot block Terminal Session commands.
    // SAFETY: descriptor remains owned by master and F_GETFL takes no additional argument.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    // SAFETY: descriptor remains live and F_SETFL accepts these descriptor status flags.
    if flags == -1
        || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(PtyError::ConfigureInput);
    }
    master.resize(size).map_err(PtyError::InitialResize)
}

fn read_termios(descriptor: RawFd) -> io::Result<libc::termios> {
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: descriptor is a live PTY master and termios points to writable storage.
    if unsafe { libc::tcgetattr(descriptor, termios.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: tcgetattr initialized termios after succeeding above.
    Ok(unsafe { termios.assume_init() })
}

const fn termios_line_mode(termios: &libc::termios, shell_foreground: bool) -> LineMode {
    LineMode {
        canonical: termios.c_lflag & libc::ICANON != 0,
        echo: termios.c_lflag & libc::ECHO != 0,
        signals: termios.c_lflag & libc::ISIG != 0,
        shell_foreground,
    }
}

/// Translate already-decided launch values into the private process library representation.
fn command_from_launch(launch: &PreparedShellLaunch) -> CommandBuilder {
    let mut command = CommandBuilder::new(launch.executable());
    command.args(launch.arguments());
    if !launch.inherit_environment() {
        command.env_clear();
    }
    for name in launch.environment_removals() {
        command.env_remove(name);
    }
    for (name, value) in launch.environment() {
        command.env(name, value);
    }
    command.cwd(launch.working_directory());
    command
}

#[cfg(all(test, feature = "native-tests"))]
pub(crate) fn test_factory() -> UnixNativePtyAdapterFactory {
    UnixNativePtyAdapterFactory::new(test_host(), Arc::default())
}

/// The host adapter the running Operating System composes, for OS-backed verification.
#[cfg(all(test, feature = "native-tests"))]
fn test_host() -> Arc<dyn UnixPtyHost> {
    #[cfg(target_os = "macos")]
    let host = super::macos_pty_host::MacosPtyHost;
    #[cfg(target_os = "linux")]
    let host = super::linux_pty_host::LinuxPtyHost;
    Arc::new(host)
}

fn terminate_after_startup_failure(child: &mut dyn Child) {
    if let Err(error) = child.kill() {
        eprintln!(
            "failed to terminate shell after PTY setup failed: {:?}",
            error.kind()
        );
        return;
    }
    if let Err(error) = child.wait() {
        eprintln!(
            "failed to reap shell after PTY setup failed: {:?}",
            error.kind()
        );
    }
}

#[cfg(test)]
mod reader_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::os::unix::net::UnixStream;

    /// Reports `EIO` on the PTY master as a hangup, as both hosts do.
    struct HangupOnIo;
    impl UnixPtyHost for HangupOnIo {
        fn observe_process(&self, _: i32) -> Option<ProcessObservation> {
            None
        }
        fn process_ids(&self) -> io::Result<Vec<i32>> {
            Ok(Vec::new())
        }
        fn master_read_error(&self, error: &io::Error) -> MasterReadError {
            if error.raw_os_error() == Some(libc::EIO) {
                MasterReadError::Hangup
            } else {
                MasterReadError::Failure
            }
        }
    }

    /// Returns each scripted result once, in order.
    struct Scripted(VecDeque<io::Result<usize>>);
    impl Read for Scripted {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let result = self.0.pop_front().expect("a scripted read");
            if let Ok(read) = &result {
                buffer[..*read].fill(b'x');
            }
            result
        }
    }

    #[test]
    fn closing_the_owner_wakes_a_reader_even_while_an_outside_holder_suppresses_eof() {
        struct NoOutput(Option<std::sync::mpsc::Sender<()>>);
        impl Read for NoOutput {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                if let Some(attempted) = self.0.take() {
                    attempted.send(()).unwrap();
                    Err(io::ErrorKind::WouldBlock.into())
                } else {
                    Ok(0)
                }
            }
        }

        let (attempted, first_read) = std::sync::mpsc::channel();
        let (finished, result) = std::sync::mpsc::channel();
        let (readiness, mut outside_holder) = UnixStream::pair().unwrap();
        let (stop, owner_stop) = UnixStream::pair().unwrap();
        let counters = Arc::new(TerminalCounters::default());
        let mut reader = PtyReader {
            activity: Arc::default(),
            reader: MasterReader {
                inner: Box::new(NoOutput(Some(attempted))),
                host: Arc::new(HangupOnIo),
            },
            readiness: readiness.into(),
            stop: stop.into(),
            closing: Arc::new(AtomicBool::new(false)),
            counters: counters.clone(),
        };
        let worker = std::thread::spawn(move || {
            finished.send(reader.read(&mut [0; 16])).unwrap();
        });
        first_read
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        drop(owner_stop);
        let stopped = result.recv_timeout(std::time::Duration::from_secs(3));
        // Also join the worker if a regression leaves it waiting for master readiness.
        if stopped.is_err() {
            let _ = outside_holder.write_all(&[1]);
        }
        worker.join().unwrap();
        assert_eq!(
            stopped
                .expect("owner closure must wake the reader")
                .unwrap(),
            0
        );
        assert_eq!(counters.snapshot().pty_read_attempts, 1);
    }

    #[test]
    fn every_native_read_counts_as_an_attempt_and_only_data_counts_bytes() {
        let counters = Arc::new(TerminalCounters::default());
        // The readiness descriptor always polls readable, so a read that would block retries
        // at once.
        let (readiness, mut ready) = UnixStream::pair().unwrap();
        ready.write_all(&[1]).unwrap();
        let (stop, _keep_stop_open) = UnixStream::pair().unwrap();
        let mut reader = PtyReader {
            activity: Arc::default(),
            reader: MasterReader {
                inner: Box::new(Scripted(VecDeque::from([
                    Err(io::ErrorKind::WouldBlock.into()),
                    Ok(5),
                    Err(io::Error::from_raw_os_error(libc::EINTR)),
                    Ok(3),
                    Err(io::Error::from_raw_os_error(libc::EIO)),
                ]))),
                host: Arc::new(HangupOnIo),
            },
            readiness: readiness.into(),
            stop: stop.into(),
            closing: Arc::new(AtomicBool::new(false)),
            counters: Arc::clone(&counters),
        };
        let mut buffer = [0; 16];

        assert_eq!(reader.read(&mut buffer).unwrap(), 5);
        assert_eq!(
            reader.read(&mut buffer).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(reader.read(&mut buffer).unwrap(), 3);
        assert_eq!(reader.read(&mut buffer).unwrap(), 0);

        assert_eq!(
            counters.snapshot(),
            spaceterm_core::diagnostics::TerminalCounterSnapshot {
                pty_read_attempts: 5,
                pty_data_reads: 2,
                pty_read_bytes: 8,
                ..Default::default()
            }
        );
    }
}

#[cfg(all(test, feature = "native-tests"))]
#[path = "unix_pty_test_program.rs"]
pub(crate) mod test_program;

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use std::collections::HashMap;
    use std::env;
    use std::ffi::CString;
    use std::fmt;
    use std::io::{self, BufRead, BufReader};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::time::Instant;

    use portable_pty::{ChildKiller, ExitStatus};

    use super::*;

    /// A controlled process inventory for these isolated PTYs. All process handles, FD
    /// queries, stop acknowledgements, foreground and output reads still use the real OS.
    /// The portable refusal matrix separately exercises unreadable global inventories.
    struct KnownProcesses {
        real: Arc<dyn UnixPtyHost>,
        known: Mutex<std::collections::BTreeSet<i32>>,
    }
    impl UnixPtyHost for KnownProcesses {
        fn observe_process(&self, pid: i32) -> Option<ProcessObservation> {
            self.real.observe_process(pid)
        }
        fn observe_exactly(&self, pid: i32) -> io::Result<Option<ProcessObservation>> {
            self.real.observe_exactly(pid)
        }
        fn observe_spawned_child(&self, pid: i32) -> Option<ProcessObservation> {
            let observation = self.real.observe_spawned_child(pid)?;
            self.known.lock().unwrap().insert(pid);
            Some(observation)
        }
        fn process_ids(&self) -> io::Result<Vec<i32>> {
            self.real.process_ids()
        }
        fn session_candidates(&self, sid: i32) -> io::Result<Vec<i32>> {
            self.real.session_candidates(sid)
        }
        fn complete_process_ids(&self) -> io::Result<Vec<i32>> {
            Ok(self.known.lock().unwrap().iter().copied().collect())
        }
        fn holds_slave(&self, pid: i32, slave: &std::path::Path) -> io::Result<bool> {
            self.real.holds_slave(pid, slave)
        }
        fn pin_process(&self, identity: ProcessIdentity) -> io::Result<Box<dyn ProcessControl>> {
            self.real.pin_process(identity)
        }
        fn master_read_error(&self, error: &io::Error) -> MasterReadError {
            self.real.master_read_error(error)
        }
    }
    fn freeze_test_host() -> Arc<dyn UnixPtyHost> {
        Arc::new(KnownProcesses {
            real: test_host(),
            known: Mutex::new([std::process::id() as i32].into_iter().collect()),
        })
    }
    struct RealIdle {
        activity: spaceterm_core::idle_restart::Activity,
        probe: Arc<dyn SessionProbe>,
    }
    impl spaceterm_core::idle_restart::Session for RealIdle {
        fn activity(&self) -> spaceterm_core::idle_restart::Activity {
            self.activity.clone()
        }
        fn working(&self, _: Instant) -> bool {
            false
        }
        fn freeze(
            &self,
            _: Instant,
        ) -> Result<
            Box<dyn spaceterm_core::idle_restart::Frozen>,
            spaceterm_core::idle_restart::Restarting,
        > {
            self.probe
                .freeze()
                .map_err(|_| spaceterm_core::idle_restart::Restarting)
        }
    }
    fn raw_idle_lease(
        pty: &SpawnedPty,
        activity: spaceterm_core::idle_restart::Activity,
    ) -> spaceterm_core::idle_restart::Lease {
        let sessions: Vec<Arc<dyn spaceterm_core::idle_restart::Session>> =
            vec![Arc::new(RealIdle {
                activity,
                probe: NativePtyAdapter::session_probe(pty).unwrap(),
            })];
        spaceterm_core::idle_restart::Lease::scan(
            &sessions,
            Instant::now() + Duration::from_secs(7200),
            Duration::from_secs(3600),
        )
        .unwrap()
    }
    #[test]
    fn an_outside_slave_holder_refuses_the_grant_without_stopping_the_holder() {
        const NAME: &str = "platform::unix_pty::tests::an_outside_slave_holder_refuses_the_grant_without_stopping_the_holder";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "printf READY; read line"]);
        let (mut pty, terminator) = spawn_command_in_pty(
            freeze_test_host(),
            PtySize::default(),
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = pty.take_reader().unwrap();
        read_output_marker(&mut reader, b"READY");
        let outside = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(pty.probe.slave.as_ref().unwrap())
            .unwrap();
        let shell = test_host().pin_process(pty.probe.session.leader).unwrap();
        assert!(pty.probe.freeze().is_err());
        let until = Instant::now() + Duration::from_secs(3);
        while shell.stopped().unwrap() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
        // The caller is the outside holder. It continues executing and its descriptor is intact.
        assert!(unsafe { libc::fcntl(outside.as_raw_fd(), libc::F_GETFD) } >= 0);
        drop(outside);
        let frozen = pty.probe.freeze().unwrap();
        frozen.verify().unwrap();
        drop(frozen);
        terminator.terminate().unwrap();
        drop(reader);
        drop(pty);
    }

    fn adopt_native_owner(
        pty: SpawnedPty,
        terminator: PtyTerminator,
        activity: spaceterm_core::idle_restart::Activity,
    ) -> (
        crate::platform::native_pty::NativePtyOwner,
        crate::platform::native_pty::NativePtyCloseHandle,
    ) {
        use crate::platform::native_pty::{
            NativePtyCloseHandle, NativePtyOutput, NativePtyOutputSink, NativePtyOwner,
        };
        struct Parts(Mutex<Option<NativePtyAdapterParts>>);
        impl NativePtyAdapterFactory for Parts {
            fn create(
                &self,
                _: PreparedShellLaunch,
                _: NativePtySize,
            ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure> {
                Ok(self.0.lock().unwrap().take().unwrap())
            }
        }
        struct Output(spaceterm_core::idle_restart::Activity);
        impl NativePtyOutputSink for Output {
            fn activity(&self) -> Option<spaceterm_core::idle_restart::Activity> {
                Some(self.0.clone())
            }
            fn publish(&self, event: NativePtyOutput) -> bool {
                drop(self.0.output_receipt());
                !matches!(event, NativePtyOutput::Stopped(_))
            }
        }
        let factory = Parts(Mutex::new(Some(NativePtyAdapterParts {
            adapter: Box::new(pty),
            termination: Arc::new(terminator),
        })));
        let close = NativePtyCloseHandle::default();
        let owner = NativePtyOwner::start(
            &factory,
            PreparedShellLaunch::for_test("/tmp".into()),
            NativePtySize::default(),
            Arc::new(Output(activity)),
            &close,
            Arc::default(),
        )
        .unwrap();
        (owner, close)
    }
    fn finish_native_owner(owner: crate::platform::native_pty::NativePtyOwner) {
        let (done, ended) = std::sync::mpsc::channel();
        thread::spawn(move || {
            drop(owner);
            let _ = done.send(());
        });
        ended.recv_timeout(Duration::from_secs(3)).expect(
            "native Stop must join the reader even when an outside process keeps the slave open",
        );
    }

    #[test]
    fn native_freeze_waits_for_the_exact_process_and_thaws_on_release() {
        const NAME: &str = "platform::unix_pty::tests::native_freeze_waits_for_the_exact_process_and_thaws_on_release";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "printf READY; read line"]);
        let (mut pty, terminator) = spawn_command_in_pty(
            freeze_test_host(),
            PtySize::default(),
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = pty.take_reader().unwrap();
        read_output_marker(&mut reader, b"READY");
        let control = test_host().pin_process(pty.probe.session.leader).unwrap();
        let frozen = pty.probe.freeze().unwrap();
        assert!(control.stopped().unwrap());
        frozen.verify().unwrap();
        drop(frozen);
        let until = Instant::now() + Duration::from_secs(3);
        while control.stopped().unwrap() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
        terminator.terminate().unwrap();
        drop(reader);
        drop(pty);
    }
    #[test]
    fn an_outside_sigcont_or_job_before_stop_revokes_the_grant() {
        const NAME: &str =
            "platform::unix_pty::tests::an_outside_sigcont_or_job_before_stop_revokes_the_grant";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        for fork in [false, true] {
            let fixture = crate::terminal::testing::ShellResourcesFixture::new();
            let marker = fixture.path().join("forked");
            let mut command = CommandBuilder::new("/bin/sh");
            command.args(["-c", "trap 'sleep 30 & printf yes > \"$1\"' USR1; printf READY; while :; do read line; done", "outside-resume"]);
            command.arg(&marker);
            let (mut pty, terminator) = spawn_command_in_pty(
                freeze_test_host(),
                PtySize::default(),
                command,
                Arc::default(),
            )
            .unwrap();
            let mut reader = pty.take_reader().unwrap();
            read_output_marker(&mut reader, b"READY");
            let activity = spaceterm_core::idle_restart::Activity::new(Instant::now());
            let lease = raw_idle_lease(&pty, activity.clone());
            lease.freeze_and_grant().unwrap();
            let process = test_host().pin_process(pty.probe.session.leader).unwrap();
            process.signal(libc::SIGCONT).unwrap();
            if fork {
                process.signal(libc::SIGUSR1).unwrap();
                let until = Instant::now() + Duration::from_secs(3);
                while !marker.exists() {
                    assert!(Instant::now() < until);
                    thread::sleep(Duration::from_millis(1));
                }
            }
            let until = Instant::now() + Duration::from_secs(3);
            while process.stopped().unwrap() {
                assert!(Instant::now() < until);
                thread::sleep(Duration::from_millis(1));
            }
            assert!(lease.verify_frozen().is_err());
            assert!(!lease.granted());
            assert!(activity.input(Instant::now()).is_ok());
            drop(lease);
            assert!(
                !process.stopped().unwrap(),
                "refusal leaves the session running"
            );
            terminator.terminate().unwrap();
            drop(reader);
            drop(pty);
        }
    }
    #[test]
    fn pending_native_output_before_its_core_read_refuses_freeze_and_thaws() {
        const NAME: &str = "platform::unix_pty::tests::pending_native_output_before_its_core_read_refuses_freeze_and_thaws";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "trap 'printf PENDING' USR1; printf READY; while :; do read line; done",
        ]);
        let (mut pty, terminator) = spawn_command_in_pty(
            freeze_test_host(),
            PtySize::default(),
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = pty.take_reader().unwrap();
        read_output_marker(&mut reader, b"READY");
        let process = test_host().pin_process(pty.probe.session.leader).unwrap();
        process.signal(libc::SIGUSR1).unwrap();
        let mut ready = libc::pollfd {
            fd: pty._foreground.as_ref().unwrap().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the master is live; the reader is intentionally paused before its Core read.
        assert_eq!(unsafe { libc::poll(&mut ready, 1, 3000) }, 1);
        assert!(pty.probe.freeze().is_err(), "kernel-pending output is work");
        let until = Instant::now() + Duration::from_secs(3);
        while process.stopped().unwrap() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
        read_output_marker(&mut reader, b"PENDING");
        terminator.terminate().unwrap();
        drop(reader);
        drop(pty);
    }
    #[test]
    fn a_frozen_native_owner_can_finish_stop_and_join_its_reader() {
        const NAME: &str =
            "platform::unix_pty::tests::a_frozen_native_owner_can_finish_stop_and_join_its_reader";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        let fixture = crate::terminal::testing::ShellResourcesFixture::new();
        let marker = fixture.path().join("hangup-trap-ran");
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "trap 'printf bad > \"$1\"' HUP; printf READY; while :; do read line; done",
            "frozen-stop",
        ]);
        command.arg(&marker);
        let (mut pty, terminator) = spawn_command_in_pty(
            freeze_test_host(),
            PtySize::default(),
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = pty.take_reader().unwrap();
        read_output_marker(&mut reader, b"READY");
        pty.reader = Some(reader);
        let activity = spaceterm_core::idle_restart::Activity::new(Instant::now());
        let lease = raw_idle_lease(&pty, activity.clone());
        let (owner, _close) = adopt_native_owner(pty, terminator, activity);
        lease.freeze_and_grant().unwrap();
        lease.verify_frozen().unwrap();
        finish_native_owner(owner);
        assert!(
            !marker.exists(),
            "terminal hangup must not resume the frozen shell into its trap"
        );
        drop(lease);
    }

    #[test]
    fn launch_command_preserves_arguments_directory_environment_and_controlling_terminal() {
        let launch = ShellLaunchPlanner::for_test(
            "/bin/zsh".into(),
            "/spaceterm-conformance-missing-resources".into(),
        )
        .local(Path::new("/tmp"))
        .unwrap();
        let command = command_from_launch(&launch);
        assert_eq!(
            command.get_argv(),
            &[std::ffi::OsString::from("/bin/zsh"), "-l".into()]
        );
        assert_eq!(
            command.get_cwd().map(std::ffi::OsString::as_os_str),
            Some(std::ffi::OsStr::new("/tmp"))
        );
        for (name, value) in [
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "ghostty"),
            ("SPACETERM", "1"),
        ] {
            assert_eq!(
                command.get_env(name),
                Some(std::ffi::OsStr::new(value)),
                "{name}"
            );
        }
        assert!(command.get_controlling_tty());
    }

    #[test]
    fn platform_neutral_size_preserves_rows_columns_and_pixels() {
        assert_eq!(
            portable_size(NativePtySize {
                rows: 31,
                columns: 97,
                pixel_width: 1_164,
                pixel_height: 620,
            }),
            PtySize {
                rows: 31,
                cols: 97,
                pixel_width: 1_164,
                pixel_height: 620,
            }
        );
    }

    #[test]
    fn platform_neutral_exit_preserves_status_and_shutdown_classification() {
        let cases = [
            (
                ShellExit {
                    status: ExitStatus::with_exit_code(0),
                    shutdown: ShutdownDisposition::NotRequested,
                },
                NativePtyExit::Success,
            ),
            (
                ShellExit {
                    status: ExitStatus::with_exit_code(23),
                    shutdown: ShutdownDisposition::NotRequested,
                },
                NativePtyExit::ExitCode(23),
            ),
            (
                ShellExit {
                    status: ExitStatus::with_signal("TERM"),
                    shutdown: ShutdownDisposition::NotRequested,
                },
                NativePtyExit::Signal("TERM".to_owned()),
            ),
            (
                ShellExit {
                    status: ExitStatus::with_exit_code(0),
                    shutdown: ShutdownDisposition::Graceful,
                },
                NativePtyExit::GracefulShutdown,
            ),
            (
                ShellExit {
                    status: ExitStatus::with_exit_code(0),
                    shutdown: ShutdownDisposition::Forced,
                },
                NativePtyExit::ForcedShutdown,
            ),
        ];

        for (exit, expected) in cases {
            assert_eq!(native_exit(exit), expected);
        }
    }

    #[test]
    fn line_mode_reads_hidden_input_and_the_line_editor_from_termios() {
        // SAFETY: termios is a plain C data structure whose zero value is valid for flag tests.
        let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
        termios.c_lflag = libc::ICANON | libc::ECHO | libc::ISIG;
        let cooked = termios_line_mode(&termios, true);
        assert!(!cooked.hidden_input() && !cooked.line_editor());

        termios.c_lflag = libc::ICANON | libc::ISIG;
        let password = termios_line_mode(&termios, false);
        assert!(password.hidden_input() && !password.line_editor());

        termios.c_lflag = libc::ISIG;
        assert!(termios_line_mode(&termios, true).line_editor());
        assert!(!termios_line_mode(&termios, false).line_editor());

        termios.c_lflag = 0;
        let raw = termios_line_mode(&termios, true);
        assert!(!raw.hidden_input() && !raw.line_editor());
    }

    const CONTROLLED_CHILD_ENV: &str = "SPACETERM_CONTROLLED_PTY_CHILD";
    const CONTROLLED_CHILD_ACK: &str = "SPACETERM_ACK\n";

    #[test]
    fn unread_native_pty_input_returns_backpressure_without_blocking() {
        const NAME: &str = "platform::unix_pty::tests::unread_native_pty_input_returns_backpressure_without_blocking";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "stty raw -echo; printf 'READY'; exec sleep 30"]);
        let (mut pty, terminator) =
            spawn_command_in_pty(test_host(), PtySize::default(), command, Arc::default()).unwrap();
        // Unblock an accidentally blocking writer so this regression can fail without hanging.
        let (cancel, rescue) = mpsc::channel();
        let watchdog = thread::spawn(move || {
            if rescue.recv_timeout(Duration::from_secs(1)).is_err() {
                terminator.terminate().unwrap();
            }
        });
        let mut reader = pty.take_reader().unwrap();
        read_output_marker(&mut reader, b"READY");
        let result = pty.write_all(&vec![b'x'; 1024 * 1024]);
        let _ = cancel.send(());
        watchdog.join().unwrap();
        drop(reader);
        drop(pty);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn the_session_probe_sees_a_background_job_beside_the_shell() {
        const NAME: &str =
            "platform::unix_pty::tests::the_session_probe_sees_a_background_job_beside_the_shell";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        for (script, others) in [
            ("printf READY; exec sleep 30", false),
            ("sleep 30 & printf READY; wait", true),
            // Job control puts the job in its own process group, and stops it. The shell then
            // reads its terminal, because dash's `wait` returns at once for a stopped job: the
            // shell would exit and the kernel would end the orphaned job before the probe reads.
            (
                "set -m; sleep 30 & kill -STOP $!; printf READY; read line",
                true,
            ),
        ] {
            let mut command = CommandBuilder::new("/bin/sh");
            command.args(["-c", script]);
            let (mut pty, terminator) =
                spawn_command_in_pty(test_host(), PtySize::default(), command, Arc::default())
                    .unwrap();
            let mut reader = pty.take_reader().unwrap();
            read_output_marker(&mut reader, b"READY");
            let probe = NativePtyAdapter::session_probe(&pty).unwrap();
            assert_eq!(probe.others().unwrap(), others, "{script}");
            terminator.terminate().unwrap();
            drop(reader);
            drop(pty);
        }
    }

    #[test]
    fn the_session_probe_sees_a_member_that_dropped_its_controlling_terminal() {
        const NAME: &str = "platform::unix_pty::tests::the_session_probe_sees_a_member_that_dropped_its_controlling_terminal";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "set -m; /usr/bin/python3 -c 'import fcntl, os, termios, time; fcntl.ioctl(os.open(\"/dev/tty\", os.O_RDWR), termios.TIOCNOTTY); print(\"READY\", flush=True); time.sleep(30)' & read line"]);
        let (mut pty, terminator) =
            spawn_command_in_pty(test_host(), PtySize::default(), command, Arc::default()).unwrap();
        let mut reader = pty.take_reader().unwrap();
        read_output_marker(&mut reader, b"READY");
        let probe = NativePtyAdapter::session_probe(&pty).unwrap();
        assert!(probe.shell_foreground().unwrap());
        assert!(probe.others().unwrap());
        terminator.terminate().unwrap();
        drop(reader);
        drop(pty);
        assert!(probe.shell_foreground().is_err(), "the PTY owner ended");
    }

    #[test]
    #[ignore = "child of queued_slave_descriptor_receiver_survives_stop_but_its_terminal_writes_fail"]
    fn queued_slave_descriptor_child() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;
        let Some(path) = env::var_os("SPACETERM_TEST_RIGHTS_SOCKET") else {
            return;
        };
        let socket = UnixStream::connect(path).unwrap();
        let mut byte = [1_u8];
        let mut vector = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
        // SAFETY: the control storage is aligned and sized for one descriptor; all pointers
        // remain live until sendmsg completes. Sending duplicates stdout into the socket queue.
        unsafe {
            let space = libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as _) as usize;
            let mut control = vec![0_usize; space.div_ceil(std::mem::size_of::<usize>())];
            let mut message: libc::msghdr = std::mem::zeroed();
            message.msg_iov = &mut vector;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = space as _;
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as _) as _;
            libc::CMSG_DATA(header)
                .cast::<libc::c_int>()
                .write(libc::STDOUT_FILENO);
            assert_eq!(libc::sendmsg(socket.as_raw_fd(), &message, 0), 1);
        }
        std::io::stdout().write_all(b"QUEUED_READY").unwrap();
        std::io::stdout().flush().unwrap();
        loop {
            // SAFETY: wait for a signal until the owning test terminates this child.
            unsafe {
                libc::pause();
            }
        }
    }

    #[test]
    fn a_quiet_job_can_start_after_the_probe_without_terminal_input_or_output() {
        const NAME: &str = "platform::unix_pty::tests::a_quiet_job_can_start_after_the_probe_without_terminal_input_or_output";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _isolation = lock_real_pty_test();
        let fixture = crate::terminal::testing::ShellResourcesFixture::new();
        let marker = fixture.path().join("trap-ran");
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "trap 'sleep 30 & printf yes > \"$1\"' USR1; printf READY; while :; do read line; done",
            "quiet-trap",
        ]);
        command.arg(&marker);
        let (mut pty, terminator) =
            spawn_command_in_pty(test_host(), PtySize::default(), command, Arc::default()).unwrap();
        let mut reader = pty.take_reader().unwrap();
        read_output_marker(&mut reader, b"READY");
        assert!(!pty.probe.others().unwrap());
        // SAFETY: this is the test's unreaped child. No terminal input or output delivers this signal.
        assert_eq!(
            unsafe { libc::kill(pty.probe.session.leader.process, libc::SIGUSR1) },
            0
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "the shell trap did not run");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            pty.probe.others().unwrap(),
            "a quiet child appeared after the negative probe"
        );
        let mut ready = libc::pollfd {
            fd: pty._foreground.as_ref().unwrap().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: ready is one live retained master descriptor.
        assert_eq!(
            unsafe { libc::poll(&mut ready, 1, 0) },
            0,
            "the trap printed nothing"
        );
        assert!(
            pty.probe.freeze().is_err(),
            "a new quiet job refuses the native grant"
        );
        let shell = test_host().pin_process(pty.probe.session.leader).unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        while shell.stopped().unwrap() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
        terminator.terminate().unwrap();
        drop(reader);
        drop(pty);
    }

    #[test]
    fn queued_slave_descriptor_receiver_survives_stop_but_its_terminal_writes_fail() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::net::UnixListener;
        const NAME: &str = "platform::unix_pty::tests::queued_slave_descriptor_receiver_survives_stop_but_its_terminal_writes_fail";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _isolation = lock_real_pty_test();
        let fixture = crate::terminal::testing::ShellResourcesFixture::new();
        let path = fixture.path().join("rights");
        let listener = UnixListener::bind(&path).unwrap();
        let mut command = CommandBuilder::new(env::current_exe().unwrap());
        command.args([
            "--exact",
            "platform::unix_pty::tests::queued_slave_descriptor_child",
            "--ignored",
            "--nocapture",
        ]);
        command.env("SPACETERM_TEST_RIGHTS_SOCKET", &path);
        let (mut pty, terminator) = spawn_command_in_pty(
            freeze_test_host(),
            PtySize::default(),
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = pty.take_reader().unwrap();
        let (socket, _) = listener.accept().unwrap();
        read_output_marker(&mut reader, b"QUEUED_READY");
        let sid = pty.probe.session.session;
        let activity = spaceterm_core::idle_restart::Activity::new(Instant::now());
        let lease = raw_idle_lease(&pty, activity.clone());
        pty.reader = Some(reader);
        let (owner, _close) = adopt_native_owner(pty, terminator, activity);
        lease.freeze_and_grant().unwrap();
        lease.verify_frozen().unwrap(); // descriptor receipt happens after the final check
        let mut byte = [0_u8];
        let mut vector = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
        // SAFETY: aligned control storage holds one descriptor. recvmsg installs a new owned
        // descriptor; no process outside the SID held that descriptor before this receive.
        let descriptor = unsafe {
            let space = libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as _) as usize;
            let mut control = vec![0_usize; space.div_ceil(std::mem::size_of::<usize>())];
            let mut message: libc::msghdr = std::mem::zeroed();
            message.msg_iov = &mut vector;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = space as _;
            assert_eq!(libc::recvmsg(socket.as_raw_fd(), &mut message, 0), 1);
            let header = libc::CMSG_FIRSTHDR(&message);
            assert!(!header.is_null());
            assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
            OwnedFd::from_raw_fd(libc::CMSG_DATA(header).cast::<libc::c_int>().read())
        };
        let mut writer = std::fs::File::from(descriptor);
        // The receiver belongs to another session and is never stopped or ended.
        // SAFETY: getsid is a read-only identity query for this process.
        assert_ne!(unsafe { libc::getsid(0) }, sid);
        finish_native_owner(owner);
        assert!(
            writer.write_all(b"AFTER_STOP").is_err(),
            "the old terminal no longer accepts writes"
        );
        // SAFETY: writer still owns a valid descriptor; failure came from terminal hangup.
        assert!(unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFD) } >= 0);
        assert!(
            test_host()
                .observe_exactly(std::process::id() as i32)
                .unwrap()
                .is_some()
        );
        drop(lease);
    }

    #[test]
    fn fresh_session_probes_preserve_quiet_programs_and_jobs() {
        const NAME: &str =
            "platform::unix_pty::tests::fresh_session_probes_preserve_quiet_programs_and_jobs";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        for (case, script, shell_foreground, others) in [
            ("bare shell", "printf READY; read line", true, false),
            (
                "editor-like input",
                "set -m; /bin/sh -c 'stty -icanon -echo; printf READY; read line'; read line",
                false,
                true,
            ),
            (
                "SSH-like foreground",
                "set -m; /bin/sh -c 'printf READY; exec sleep 30'; read line",
                false,
                true,
            ),
            (
                "nested program",
                "set -m; /bin/sh -c '/bin/sh -c \"printf READY; read line\"; read line'; read line",
                false,
                true,
            ),
            (
                "password prompt",
                "set -m; /bin/sh -c 'stty -echo; printf READY; read line'; read line",
                false,
                true,
            ),
            (
                "stopped job",
                "set -m; sleep 30 & kill -STOP $!; printf READY; read line",
                true,
                true,
            ),
            (
                "background job",
                "set -m; sleep 30 & printf READY; read line",
                true,
                true,
            ),
        ] {
            let mut command = CommandBuilder::new("/bin/sh");
            command.args(["-c", script]);
            let (mut pty, terminator) =
                spawn_command_in_pty(test_host(), PtySize::default(), command, Arc::default())
                    .unwrap();
            let mut reader = pty.take_reader().unwrap();
            read_output_marker(&mut reader, b"READY");
            let probe = NativePtyAdapter::session_probe(&pty).unwrap();
            assert_eq!(
                probe.shell_foreground().unwrap(),
                shell_foreground,
                "{case}"
            );
            assert_eq!(probe.others().unwrap(), others, "{case}");
            terminator.terminate().unwrap();
            drop(reader);
            drop(pty);
        }
    }

    #[test]
    fn closing_a_native_pty_does_not_submit_pending_canonical_input() {
        const NAME: &str = "platform::unix_pty::tests::closing_a_native_pty_does_not_submit_pending_canonical_input";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        let fixture = crate::terminal::testing::ShellResourcesFixture::new();
        let accepted = fixture.path().join("unrequested-input");
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "trap '' HUP TERM; stty icanon -echo; printf READY; \
             IFS= read -r line || exit; printf 'ACCEPTED:%s:READY-TO-CLOSE' \"$line\"; \
             if IFS= read -r line; then printf '%s' \"$line\" > \"$1\"; fi; exec sleep 30",
            "spaceterm-close-check",
        ]);
        command.arg(&accepted);
        let (mut pty, terminator) =
            spawn_command_in_pty(test_host(), PtySize::default(), command, Arc::default()).unwrap();
        let mut reader = pty.take_reader().unwrap();
        read_output_marker(&mut reader, b"READY");
        pty.write_all(b"confirmed\n").unwrap();
        read_output_marker(&mut reader, b"ACCEPTED:confirmed:READY-TO-CLOSE");
        pty.write_all(b"unsubmitted").unwrap();

        // Keep a master reader alive so closing the owner cannot hide injected input behind
        // terminal hangup. The child ignores graceful signals and must be forcefully reaped.
        drop(pty);
        drop(reader);

        assert!(terminator.cleanup_confirmed());
        assert!(
            !accepted.exists(),
            "closing the PTY submitted a line the user never confirmed"
        );
    }

    #[test]
    #[ignore = "runs only as a child of the controlled PTY integration tests"]
    fn controlled_pty_child_reports_runtime_configuration() {
        if env::var_os(CONTROLLED_CHILD_ENV).is_none() {
            return;
        }

        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        let mut window_size = std::mem::MaybeUninit::<libc::winsize>::uninit();
        // SAFETY: stdin is the PTY slave installed by portable-pty. Both calls initialize their
        // output structs on success, which is asserted before assume_init.
        let termios_result = unsafe { libc::tcgetattr(libc::STDIN_FILENO, termios.as_mut_ptr()) };
        // SAFETY: TIOCGWINSZ writes a winsize to the valid pointer supplied here.
        let window_result = unsafe {
            libc::ioctl(
                libc::STDIN_FILENO,
                libc::TIOCGWINSZ,
                window_size.as_mut_ptr(),
            )
        };
        assert_eq!(termios_result, 0);
        assert_eq!(window_result, 0);
        // SAFETY: the successful system calls above initialized both values.
        let termios = unsafe { termios.assume_init() };
        // SAFETY: the successful ioctl above initialized the window size.
        let window_size = unsafe { window_size.assume_init() };
        let cwd = env::current_dir().unwrap();

        // SAFETY: these process and terminal identity queries have no pointer preconditions.
        let (pid, group, session, foreground, is_tty) = unsafe {
            (
                libc::getpid(),
                libc::getpgrp(),
                libc::getsid(0),
                libc::tcgetpgrp(libc::STDIN_FILENO),
                libc::isatty(libc::STDIN_FILENO),
            )
        };
        println!(
            "SPACETERM_PTY_REPORT pid={pid} group={group} session={session} foreground={foreground} tty={is_tty} utf8={} rows={} cols={} pixel_width={} pixel_height={} cwd={} marker={}",
            usize::from(termios.c_iflag & libc::IUTF8 != 0),
            window_size.ws_row,
            window_size.ws_col,
            window_size.ws_xpixel,
            window_size.ws_ypixel,
            cwd.display(),
            env::var("SPACETERM_PTY_MARKER").unwrap(),
        );
        // Keep the child alive until the parent has observed its process session.
        let mut acknowledgment = String::new();
        io::stdin().read_line(&mut acknowledgment).unwrap();
        assert_eq!(acknowledgment, CONTROLLED_CHILD_ACK);
    }

    fn controlled_child_command(working_directory: &Path, marker: &str) -> CommandBuilder {
        let mut command = CommandBuilder::new(env::current_exe().unwrap());
        command.args([
            "--ignored",
            "--exact",
            "platform::unix_pty::tests::controlled_pty_child_reports_runtime_configuration",
            "--nocapture",
        ]);
        command.env(CONTROLLED_CHILD_ENV, "1");
        command.env("SPACETERM_PTY_MARKER", marker);
        command.cwd(working_directory);
        command
    }

    fn read_controlled_report(pty: &mut SpawnedPty) -> HashMap<String, String> {
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        let mut output = String::new();
        let report = loop {
            let mut line = String::new();
            assert_ne!(
                reader.read_line(&mut line).unwrap(),
                0,
                "controlled PTY report was missing from: {output}"
            );
            output.push_str(&line);
            if line.starts_with("SPACETERM_PTY_REPORT ") {
                break line;
            }
        };
        // The slave echoes input. Acknowledge only after the complete report, or the echoed
        // acknowledgment can interleave with a field while the child is still writing it.
        pty.write_all(CONTROLLED_CHILD_ACK.as_bytes()).unwrap();
        pty.flush().unwrap();
        reader.read_to_string(&mut output).unwrap();
        let status = pty.process.wait_for_child(Duration::from_secs(2)).unwrap();
        assert!(
            status.status.success(),
            "controlled PTY child failed ({}): {output}",
            status.status,
        );
        report
            .split_whitespace()
            .skip(1)
            .map(|field| {
                let (key, value) = field.split_once('=').unwrap();
                (key.to_owned(), value.to_owned())
            })
            .collect()
    }

    #[test]
    fn controlled_child_should_observe_initialized_terminal_state_before_interaction() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "platform::unix_pty::tests::controlled_child_should_observe_initialized_terminal_state_before_interaction",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let size = PtySize {
            rows: 31,
            cols: 97,
            pixel_width: 1_164,
            pixel_height: 682,
        };
        let command = controlled_child_command(&working_directory, "initialized");

        let (mut pty, _terminator) =
            spawn_command_in_pty(test_host(), size, command, Arc::default()).unwrap();
        let report = read_controlled_report(&mut pty);

        assert_eq!(
            (
                &report["pid"],
                &report["group"],
                &report["session"],
                &report["foreground"],
                report["tty"].as_str(),
                report["utf8"].as_str(),
                report["rows"].as_str(),
                report["cols"].as_str(),
                report["pixel_width"].as_str(),
                report["pixel_height"].as_str(),
                report["cwd"].as_str(),
                report["marker"].as_str(),
            ),
            (
                &report["pid"],
                &report["pid"],
                &report["pid"],
                &report["pid"],
                "1",
                "1",
                "31",
                "97",
                "1164",
                "682",
                working_directory.to_str().unwrap(),
                "initialized",
            )
        );
    }

    #[test]
    fn concurrent_terminal_sessions_should_own_distinct_process_groups() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "platform::unix_pty::tests::concurrent_terminal_sessions_should_own_distinct_process_groups",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 640,
            pixel_height: 480,
        };
        let (mut first, _first_terminator) = spawn_command_in_pty(
            test_host(),
            size,
            controlled_child_command(&working_directory, "first"),
            Arc::default(),
        )
        .unwrap();
        let (mut second, _second_terminator) = spawn_command_in_pty(
            test_host(),
            size,
            controlled_child_command(&working_directory, "second"),
            Arc::default(),
        )
        .unwrap();

        let first_report = read_controlled_report(&mut first);
        let second_report = read_controlled_report(&mut second);

        assert_eq!(
            (
                first_report["group"] != second_report["group"],
                first_report["session"] != second_report["session"],
                first_report["marker"].as_str(),
                second_report["marker"].as_str(),
            ),
            (true, true, "first", "second")
        );
    }

    #[test]
    fn stubborn_process_group_should_receive_bounded_forced_shutdown() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "platform::unix_pty::tests::stubborn_process_group_should_receive_bounded_forced_shutdown",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "trap '' HUP; (trap '' HUP; exec </dev/null >/dev/null 2>&1; while :; do sleep 1; done) & child=$!; echo SPACETERM_SHUTDOWN_REPORT leader=$$ child=$child; exec </dev/null >/dev/null 2>&1; while :; do sleep 1; done",
        ]);
        command.cwd(&working_directory);
        let (mut pty, terminator) = spawn_command_in_pty(
            test_host(),
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        let mut report = String::new();
        reader.read_line(&mut report).unwrap();
        let fields: HashMap<_, _> = report
            .split_whitespace()
            .skip_while(|field| *field != "SPACETERM_SHUTDOWN_REPORT")
            .skip(1)
            .map(|field| field.split_once('=').unwrap())
            .collect();
        let leader = fields["leader"].parse::<i32>().unwrap();
        let child = fields["child"].parse::<i32>().unwrap();
        // SAFETY: getpgid performs a read-only process identity query.
        assert_eq!(unsafe { libc::getpgid(child) }, leader);

        let started = Instant::now();
        terminator.terminate().unwrap();
        pty.process.cleanup_child();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= GRACEFUL_SHUTDOWN_TIMEOUT,
            "forced shutdown skipped the grace window: {elapsed:?}"
        );
        assert!(
            elapsed <= GRACEFUL_SHUTDOWN_TIMEOUT + FORCED_SHUTDOWN_TIMEOUT + Duration::from_secs(1),
            "forced shutdown exceeded its bound: {elapsed:?}"
        );
        assert!(pty.process.termination.forced.load(Ordering::Acquire));
        assert_process_disappears(leader);
        assert_process_disappears(child);
    }

    #[test]
    fn interactive_foreground_process_group_should_be_forced_down_with_its_shell() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "platform::unix_pty::tests::interactive_foreground_process_group_should_be_forced_down_with_its_shell",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let mut command = CommandBuilder::new("/bin/zsh");
        command.args(["-f", "-i"]);
        command.cwd(&working_directory);
        command.env("PS1", "SPACETERM_READY ");
        let (mut pty, terminator) = spawn_command_in_pty(
            test_host(),
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        read_output_marker(&mut reader, b"SPACETERM_READY");
        pty.write_all(
            b"sh -c 'trap \"\" HUP; sleep 30 & child=$!; echo SPACETERM_FOREGROUND leader=$$ child=$child; wait'\n",
        )
        .unwrap();
        pty.flush().unwrap();

        let (leader, child) = read_process_report(&mut reader, "SPACETERM_FOREGROUND");
        let shell_process_group = pty
            .process
            .termination
            .lock_target()
            .as_ref()
            .and_then(|target| target.process_group)
            .unwrap();
        assert_ne!(leader, shell_process_group);
        let drain = thread::spawn(move || {
            let mut sink = io::sink();
            io::copy(&mut reader, &mut sink)
        });

        terminator.terminate().unwrap();
        pty.process.cleanup_child();
        assert!(pty.process.termination.forced.load(Ordering::Acquire));
        drop(pty);
        drain.join().unwrap().unwrap();

        assert_process_disappears(leader);
        assert_process_disappears(child);
    }

    #[test]
    fn interactive_background_process_group_should_be_forced_down_with_its_shell() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "platform::unix_pty::tests::interactive_background_process_group_should_be_forced_down_with_its_shell",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let readiness = env::temp_dir().join(format!(
            "spaceterm-background-pty-ready-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&readiness);
        let mut command = CommandBuilder::new("/bin/zsh");
        command.args(["-f", "-i"]);
        command.cwd(&working_directory);
        command.env("PS1", "SPACETERM_READY ");
        command.env("SPACETERM_BACKGROUND_READY", &readiness);
        let (mut pty, terminator) = spawn_command_in_pty(
            test_host(),
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        read_output_marker(&mut reader, b"SPACETERM_READY");
        pty.write_all(
            b"sh -c 'trap \"\" HUP; : > \"$SPACETERM_BACKGROUND_READY\"; while :; do sleep 1; done' & background=$!; while [ ! -f \"$SPACETERM_BACKGROUND_READY\" ]; do sleep 0.01; done; echo SPACETERM_BACKGROUND leader=$background child=$background\n",
        )
        .unwrap();
        pty.flush().unwrap();

        let (leader, child) = read_process_report(&mut reader, "SPACETERM_BACKGROUND");
        std::fs::remove_file(&readiness).unwrap();
        let shell_process_group = pty
            .process
            .termination
            .lock_target()
            .as_ref()
            .and_then(|target| target.process_group)
            .unwrap();
        assert_ne!(leader, shell_process_group);
        let session = pty
            .process
            .termination
            .lock_target()
            .as_ref()
            .and_then(|target| target.session)
            .unwrap();
        assert!(
            process_groups_in_session(test_host().as_ref(), &session)
                .unwrap()
                .contains_key(&leader)
        );
        let drain = thread::spawn(move || {
            let mut sink = io::sink();
            io::copy(&mut reader, &mut sink)
        });

        terminator.terminate().unwrap();
        pty.process.cleanup_child();
        assert!(pty.process.termination.forced.load(Ordering::Acquire));
        drop(pty);
        drain.join().unwrap().unwrap();

        assert_process_disappears(leader);
        assert_process_disappears(child);
    }

    /// Observes one process exit through the host kernel's process event facility.
    struct ProcessExitWatch(OwnedFd);

    impl ProcessExitWatch {
        #[cfg(target_os = "macos")]
        fn register(process: i32) -> Self {
            // SAFETY: kqueue has no input pointers and returns an owned descriptor on success.
            let queue = unsafe { libc::kqueue() };
            assert!(queue >= 0);
            // SAFETY: the successful kqueue descriptor has exactly one owner.
            let queue = unsafe { OwnedFd::from_raw_fd(queue) };
            let exit_event = libc::kevent {
                ident: process as usize,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ENABLE,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: queue and the initialized change remain live for this registration call.
            assert_eq!(
                unsafe {
                    libc::kevent(
                        queue.as_raw_fd(),
                        &exit_event,
                        1,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null(),
                    )
                },
                0
            );
            Self(queue)
        }

        #[cfg(target_os = "macos")]
        fn wait(&self, timeout: Duration) -> bool {
            // SAFETY: kevent is a plain C structure that the successful call below initializes.
            let mut exit_event = unsafe { std::mem::zeroed::<libc::kevent>() };
            let timeout = libc::timespec {
                tv_sec: timeout.as_secs() as libc::time_t,
                tv_nsec: 0,
            };
            // SAFETY: the queue is open and exit_event is writable for one returned event.
            let returned = unsafe {
                libc::kevent(
                    self.0.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut exit_event,
                    1,
                    &timeout,
                )
            };
            returned == 1 && exit_event.fflags & libc::NOTE_EXIT != 0
        }

        #[cfg(target_os = "linux")]
        fn register(process: i32) -> Self {
            // SAFETY: pidfd_open takes a process identifier and flags and returns an owned
            // descriptor on success.
            let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, process, 0) };
            assert!(descriptor >= 0);
            // SAFETY: the successful pidfd_open descriptor has exactly one owner.
            Self(unsafe { OwnedFd::from_raw_fd(descriptor as i32) })
        }

        #[cfg(target_os = "linux")]
        fn wait(&self, timeout: Duration) -> bool {
            let mut event = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
            // SAFETY: event points to one initialized pollfd whose pidfd remains open.
            unsafe { libc::poll(&mut event, 1, timeout) == 1 && event.revents & libc::POLLIN != 0 }
        }
    }

    #[test]
    fn hup_handler_process_handoff_should_be_forced_down_with_its_shell() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "platform::unix_pty::tests::hup_handler_process_handoff_should_be_forced_down_with_its_shell",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let readiness = env::temp_dir().join(format!(
            "spaceterm-handoff-pty-ready-{}",
            std::process::id()
        ));
        let replacement_report = env::temp_dir().join(format!(
            "spaceterm-handoff-pty-child-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&readiness);
        let _ = std::fs::remove_file(&replacement_report);
        let report_name = CString::new(replacement_report.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: report_name is a live NUL-terminated path to this test's private FIFO.
        assert_eq!(unsafe { libc::mkfifo(report_name.as_ptr(), 0o600) }, 0);
        let mut report_reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&replacement_report)
            .unwrap();
        // Keep the FIFO writable so poll waits for the handler's report instead of seeing EOF.
        let report_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&replacement_report)
            .unwrap();
        let mut command = CommandBuilder::new("/bin/zsh");
        command.args(["-f", "-i"]);
        command.cwd(&working_directory);
        command.env("PS1", "SPACETERM_READY ");
        command.env("SPACETERM_HANDOFF_READY", &readiness);
        command.env("SPACETERM_HANDOFF_CHILD", &replacement_report);
        command.env(
            "SPACETERM_HANDOFF_TRAP",
            "trap '' HUP; sleep 30 </dev/null >/dev/null 2>&1 & echo $! > \"$SPACETERM_HANDOFF_CHILD\"; exec /usr/bin/true",
        );
        let (mut pty, terminator) = spawn_command_in_pty(
            test_host(),
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        read_output_marker(&mut reader, b"SPACETERM_READY");
        pty.write_all(
            b"sh -c 'trap \"$SPACETERM_HANDOFF_TRAP\" HUP; sleep 30 & : > \"$SPACETERM_HANDOFF_READY\"; wait' & background=$!; while [ ! -f \"$SPACETERM_HANDOFF_READY\" ]; do sleep 0.01; done; echo SPACETERM_HANDOFF leader=$background child=$background\n",
        )
        .unwrap();
        pty.flush().unwrap();

        let (leader, _) = read_process_report(&mut reader, "SPACETERM_HANDOFF");
        std::fs::remove_file(&readiness).unwrap();
        // Observe the group leader before HUP so its exit cannot precede registration.
        let leader_exit = ProcessExitWatch::register(leader);
        // Establish the handoff before asking the PTY owner to shut down its session.
        // SAFETY: leader identifies the live background shell observed just above.
        assert_eq!(unsafe { libc::kill(leader, libc::SIGHUP) }, 0);
        let mut event = libc::pollfd {
            fd: report_reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // The handler's FIFO write completes the handoff before shutdown scans the session.
        // SAFETY: event points to one initialized pollfd and report_reader remains open.
        assert_eq!(unsafe { libc::poll(&mut event, 1, 2_000) }, 1);
        assert_ne!(event.revents & libc::POLLIN, 0);
        let mut report = String::new();
        BufReader::new(&mut report_reader)
            .read_line(&mut report)
            .unwrap();
        let replacement = report.trim().parse::<i32>().unwrap();
        drop(report_writer);
        drop(report_reader);
        std::fs::remove_file(&replacement_report).unwrap();
        assert!(
            leader_exit.wait(Duration::from_secs(2)),
            "background group leader did not exit after HUP"
        );
        terminator.terminate().unwrap();
        let drain = thread::spawn(move || {
            let mut sink = io::sink();
            io::copy(&mut reader, &mut sink)
        });

        pty.process.cleanup_child();
        assert!(pty.process.termination.forced.load(Ordering::Acquire));
        drop(pty);
        drain.join().unwrap().unwrap();

        assert_ne!(replacement, leader);
        assert_process_disappears(leader);
        assert_process_disappears(replacement);
    }

    #[test]
    fn close_after_session_leader_exit_should_terminate_surviving_group() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "platform::unix_pty::tests::close_after_session_leader_exit_should_terminate_surviving_group",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "trap '' HUP TERM; sleep 30 </dev/null >/dev/null 2>&1 & child=$!; echo SPACETERM_LATE_CLOSE leader=$$ child=$child",
        ]);
        command.cwd(&working_directory);
        let (mut pty, terminator) = spawn_command_in_pty(
            test_host(),
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        let (leader, child) = read_process_report(&mut reader, "SPACETERM_LATE_CLOSE");
        drop(reader);
        let session = pty
            .process
            .termination
            .lock_target()
            .as_ref()
            .and_then(|target| target.session)
            .unwrap();

        let exit = pty.process.wait_for_child(Duration::from_secs(1)).unwrap();
        assert_eq!(exit.shutdown, ShutdownDisposition::NotRequested);
        let child_identity = test_host().observe_process(child).unwrap().identity;

        terminator.terminate().unwrap();
        pty.process.cleanup_child();
        drop(pty);

        let deadline = Instant::now() + Duration::from_secs(1);
        let disappeared = loop {
            if !test_host()
                .observe_process(child)
                .is_some_and(|process| process.identity == child_identity)
            {
                break true;
            }
            if Instant::now() >= deadline {
                let _ = signal_owned_process_group(
                    test_host().as_ref(),
                    leader,
                    &[child_identity],
                    &session,
                    libc::SIGKILL,
                );
                break false;
            }
            thread::sleep(CHILD_EXIT_POLL_INTERVAL);
        };

        assert!(
            disappeared,
            "process {child} survived close after leader {leader} exited"
        );
    }

    #[test]
    fn responsive_process_group_should_finish_during_the_grace_window() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "platform::unix_pty::tests::responsive_process_group_should_finish_during_the_grace_window",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "sleep 30 & child=$!; echo SPACETERM_SHUTDOWN_REPORT leader=$$ child=$child; wait",
        ]);
        command.cwd(&working_directory);
        let (mut pty, terminator) = spawn_command_in_pty(
            test_host(),
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
            Arc::default(),
        )
        .unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        let mut report = String::new();
        reader.read_line(&mut report).unwrap();
        let fields: HashMap<_, _> = report
            .split_whitespace()
            .skip_while(|field| *field != "SPACETERM_SHUTDOWN_REPORT")
            .skip(1)
            .map(|field| field.split_once('=').unwrap())
            .collect();
        let leader = fields["leader"].parse::<i32>().unwrap();
        let child = fields["child"].parse::<i32>().unwrap();

        let started = Instant::now();
        terminator.terminate().unwrap();
        pty.process.cleanup_child();

        assert!(started.elapsed() < GRACEFUL_SHUTDOWN_TIMEOUT);
        assert!(!pty.process.termination.forced.load(Ordering::Acquire));
        assert_process_disappears(leader);
        assert_process_disappears(child);
    }

    fn assert_process_disappears(process: i32) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            // SAFETY: signal zero performs only an existence/permission check.
            if unsafe { libc::kill(process, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "process {process} remained after process-group shutdown"
            );
            thread::sleep(CHILD_EXIT_POLL_INTERVAL);
        }
    }

    fn read_process_report(reader: &mut impl BufRead, marker: &str) -> (i32, i32) {
        let mut report = String::new();
        loop {
            report.clear();
            let read = reader.read_line(&mut report).unwrap();
            assert_ne!(read, 0, "PTY closed before {marker} was reported");
            let fields: HashMap<_, _> = report
                .split_whitespace()
                .skip_while(|field| *field != marker)
                .skip(1)
                .filter_map(|field| field.split_once('='))
                .collect();
            let parsed = fields
                .get("leader")
                .and_then(|leader| leader.parse::<i32>().ok())
                .zip(
                    fields
                        .get("child")
                        .and_then(|child| child.parse::<i32>().ok()),
                );
            if let Some(ids) = parsed {
                return ids;
            }
        }
    }

    fn read_output_marker(reader: &mut impl Read, marker: &[u8]) {
        let mut observed = Vec::new();
        while !observed.ends_with(marker) {
            let mut byte = [0_u8; 1];
            reader.read_exact(&mut byte).unwrap();
            observed.push(byte[0]);
        }
    }

    #[derive(Default)]
    struct TestMasterPty {
        closed: Option<Arc<AtomicBool>>,
    }

    impl Drop for TestMasterPty {
        fn drop(&mut self) {
            if let Some(closed) = &self.closed {
                closed.store(true, Ordering::Release);
            }
        }
    }

    impl MasterPty for TestMasterPty {
        fn resize(&self, _size: PtySize) -> Result<(), AnyError> {
            Ok(())
        }

        fn get_size(&self) -> Result<PtySize, AnyError> {
            Ok(PtySize::default())
        }

        fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, AnyError> {
            Ok(Box::new(io::empty()))
        }

        fn take_writer(&self) -> Result<Box<dyn Write + Send>, AnyError> {
            Ok(Box::new(io::sink()))
        }

        fn process_group_leader(&self) -> Option<i32> {
            None
        }

        fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
            None
        }

        fn tty_name(&self) -> Option<std::path::PathBuf> {
            None
        }
    }

    #[derive(Clone, Default)]
    struct CleanupCounts {
        try_wait: Arc<AtomicUsize>,
        owner_kill: Arc<AtomicUsize>,
        signal: Arc<AtomicUsize>,
        wait: Arc<AtomicUsize>,
    }

    #[derive(Clone, Debug)]
    struct TestSignaller {
        signals: Arc<AtomicUsize>,
        fail: bool,
    }

    impl TestSignaller {
        fn new(cleanup: &CleanupCounts) -> Self {
            Self {
                signals: Arc::clone(&cleanup.signal),
                fail: false,
            }
        }

        fn failing(signals: Arc<AtomicUsize>) -> Self {
            Self {
                signals,
                fail: true,
            }
        }
    }

    impl ChildKiller for TestSignaller {
        fn kill(&mut self) -> io::Result<()> {
            self.signals.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "test signal failure",
                ))
            } else {
                Ok(())
            }
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    struct TestChild {
        cleanup: CleanupCounts,
    }

    struct FailingSignalChild {
        cleanup: CleanupCounts,
    }

    struct RacedExitChild {
        cleanup: CleanupCounts,
    }

    struct ExitedChild {
        cleanup: CleanupCounts,
    }

    #[derive(Clone, Default)]
    struct BlockingCallControl {
        state: Arc<(Mutex<BlockingCallState>, Condvar)>,
    }

    #[derive(Default)]
    struct BlockingCallState {
        entered: bool,
        released: bool,
    }

    impl BlockingCallControl {
        fn block(&self) {
            let (state, changed) = &*self.state;
            let mut state = state.lock().unwrap();
            state.entered = true;
            changed.notify_all();
            while !state.released {
                state = changed.wait(state).unwrap();
            }
        }

        fn wait_until_entered(&self, operation: &str) {
            let (state, changed) = &*self.state;
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut state = state.lock().unwrap();
            while !state.entered {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "timed out waiting for {operation}");
                let (next_state, timeout) = changed.wait_timeout(state, remaining).unwrap();
                state = next_state;
                assert!(
                    !timeout.timed_out() || state.entered,
                    "timed out waiting for {operation}"
                );
            }
        }

        fn release(&self) {
            let (state, changed) = &*self.state;
            state.lock().unwrap().released = true;
            changed.notify_all();
        }
    }

    struct BlockingWaitChild {
        cleanup: CleanupCounts,
        wait_control: BlockingCallControl,
    }

    struct BlockingOwnerKillChild {
        cleanup: CleanupCounts,
        kill_control: BlockingCallControl,
    }

    struct BlockingTryWaitChild {
        cleanup: CleanupCounts,
        try_wait_control: BlockingCallControl,
    }

    impl fmt::Debug for TestChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.debug_struct("TestChild").finish_non_exhaustive()
        }
    }

    impl ChildKiller for TestChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
        }
    }

    impl Child for TestChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    impl fmt::Debug for FailingSignalChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("FailingSignalChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for FailingSignalChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::failing(Arc::clone(&self.cleanup.signal)))
        }
    }

    impl Child for FailingSignalChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    impl fmt::Debug for RacedExitChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("RacedExitChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for RacedExitChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "test termination failure",
            ))
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
        }
    }

    impl Child for RacedExitChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            let attempt = self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            Ok((attempt > 0).then(|| ExitStatus::with_exit_code(0)))
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    impl fmt::Debug for ExitedChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("ExitedChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for ExitedChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
        }
    }

    impl Child for ExitedChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            Ok(Some(ExitStatus::with_exit_code(0)))
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    impl fmt::Debug for BlockingWaitChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("BlockingWaitChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for BlockingWaitChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
        }
    }

    impl Child for BlockingWaitChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            self.wait_control.block();
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    impl fmt::Debug for BlockingOwnerKillChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("BlockingOwnerKillChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for BlockingOwnerKillChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            self.kill_control.block();
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
        }
    }

    impl Child for BlockingOwnerKillChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    impl fmt::Debug for BlockingTryWaitChild {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("BlockingTryWaitChild")
                .finish_non_exhaustive()
        }
    }

    impl ChildKiller for BlockingTryWaitChild {
        fn kill(&mut self) -> io::Result<()> {
            self.cleanup.owner_kill.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(TestSignaller::new(&self.cleanup))
        }
    }

    impl Child for BlockingTryWaitChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.cleanup.try_wait.fetch_add(1, Ordering::Relaxed);
            self.try_wait_control.block();
            Ok(Some(ExitStatus::with_exit_code(0)))
        }

        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.cleanup.wait.fetch_add(1, Ordering::Relaxed);
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    fn spawned_pty(child: Box<dyn Child + Send + Sync>) -> (SpawnedPty, PtyTerminator) {
        let termination = Arc::new(ChildTermination::new(
            test_host(),
            None,
            child.clone_killer(),
        ));
        let terminator = PtyTerminator::new(Arc::clone(&termination));
        (
            SpawnedPty {
                master: Box::new(TestMasterPty::default()),
                reader: Some(Box::new(io::empty())),
                writer: Box::new(io::sink()),
                reader_stop: None,
                closing: Arc::new(AtomicBool::new(false)),
                shell_process_group: 0,
                _foreground: None,
                probe: Arc::new(ShellSession {
                    foreground: std::sync::Weak::new(),
                    activity: Arc::default(),
                    slave: None,
                    frozen: Mutex::new(Vec::new()),
                    host: test_host(),
                    session: ProcessSessionIdentity {
                        session: 0,
                        leader: ProcessIdentity {
                            process: 0,
                            start: ProcessStart { coarse: 0, fine: 0 },
                        },
                    },
                }),
                process: SpawnedChild {
                    child: Some(child),
                    termination,
                },
            },
            terminator,
        )
    }

    #[test]
    fn pty_handles_close_before_waiting_for_a_child_that_is_draining_its_terminal() {
        #[derive(Clone, Debug)]
        struct DrainingChild {
            master_closed: Arc<AtomicBool>,
            reaped: Arc<AtomicBool>,
        }
        impl ChildKiller for DrainingChild {
            fn kill(&mut self) -> io::Result<()> {
                Ok(())
            }
            fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
                Box::new(self.clone())
            }
        }
        impl Child for DrainingChild {
            fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
                if self.master_closed.load(Ordering::Acquire) {
                    self.reaped.store(true, Ordering::Release);
                    Ok(Some(ExitStatus::with_exit_code(0)))
                } else {
                    Ok(None)
                }
            }
            fn wait(&mut self) -> io::Result<ExitStatus> {
                assert!(
                    self.master_closed.load(Ordering::Acquire),
                    "child exit is waiting for the PTY master to close"
                );
                self.reaped.store(true, Ordering::Release);
                Ok(ExitStatus::with_exit_code(0))
            }
            fn process_id(&self) -> Option<u32> {
                None
            }
        }
        let master_closed = Arc::new(AtomicBool::new(false));
        let reaped = Arc::new(AtomicBool::new(false));
        let (mut pty, terminator) = spawned_pty(Box::new(DrainingChild {
            master_closed: master_closed.clone(),
            reaped: reaped.clone(),
        }));
        pty.master = Box::new(TestMasterPty {
            closed: Some(master_closed),
        });
        terminator.terminate().unwrap();
        drop(pty);
        assert!(reaped.load(Ordering::Acquire));
        assert!(terminator.cleanup_confirmed());
    }

    #[test]
    fn spawned_pty_reader_can_be_taken_only_once() {
        let cleanup = CleanupCounts::default();
        let (mut pty, _terminator) = spawned_pty(Box::new(ExitedChild { cleanup }));

        drop(pty.take_reader().unwrap());
        let error = pty.take_reader().err().unwrap();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn dropping_a_spawned_pty_kills_and_reaps_the_active_process_once() {
        let cleanup = CleanupCounts::default();
        let (pty, _terminator) = spawned_pty(Box::new(TestChild {
            cleanup: cleanup.clone(),
        }));

        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 0, 1)
        );
    }

    #[test]
    fn terminator_should_signal_without_invoking_owner_kill() {
        let cleanup = CleanupCounts::default();
        let kill_control = BlockingCallControl::default();
        let (pty, terminator) = spawned_pty(Box::new(BlockingOwnerKillChild {
            cleanup: cleanup.clone(),
            kill_control: kill_control.clone(),
        }));
        let (termination_finished, termination_completion) = mpsc::sync_channel(1);

        let termination_thread = thread::spawn(move || {
            termination_finished.send(terminator.terminate()).unwrap();
        });
        let termination = match termination_completion.recv_timeout(Duration::from_millis(250)) {
            Ok(termination) => termination,
            Err(error) => {
                kill_control.release();
                termination_thread.join().unwrap();
                panic!("termination invoked or waited for owner kill: {error}");
            }
        };

        termination.unwrap();
        termination_thread.join().unwrap();
        assert_eq!(
            (
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
            ),
            (1, 0)
        );

        kill_control.release();
        drop(pty);
        kill_control.wait_until_entered("owner child kill");
        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 1, 1)
        );
    }

    #[test]
    fn terminator_should_not_wait_for_cleanup_blocked_in_owner_kill() {
        let cleanup = CleanupCounts::default();
        let kill_control = BlockingCallControl::default();
        let (pty, terminator) = spawned_pty(Box::new(BlockingOwnerKillChild {
            cleanup: cleanup.clone(),
            kill_control: kill_control.clone(),
        }));
        let (cleanup_finished, cleanup_completion) = mpsc::sync_channel(1);
        let cleanup_thread = thread::spawn(move || {
            drop(pty);
            cleanup_finished.send(()).unwrap();
        });
        kill_control.wait_until_entered("owner child kill");

        let (termination_finished, termination_completion) = mpsc::sync_channel(1);
        let termination_thread = thread::spawn(move || {
            termination_finished.send(terminator.terminate()).unwrap();
        });
        let termination = match termination_completion.recv_timeout(Duration::from_millis(250)) {
            Ok(termination) => termination,
            Err(error) => {
                kill_control.release();
                cleanup_thread.join().unwrap();
                termination_thread.join().unwrap();
                panic!("termination waited for owner kill: {error}");
            }
        };

        termination.unwrap();
        assert!(matches!(
            cleanup_completion.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(
            (
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
            ),
            (1, 0)
        );
        termination_thread.join().unwrap();

        kill_control.release();
        cleanup_completion
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        cleanup_thread.join().unwrap();
        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 0, 1)
        );
    }

    #[test]
    fn terminator_should_not_wait_for_cleanup_that_owns_the_child() {
        let cleanup = CleanupCounts::default();
        let wait_control = BlockingCallControl::default();
        let (pty, terminator) = spawned_pty(Box::new(BlockingWaitChild {
            cleanup: cleanup.clone(),
            wait_control: wait_control.clone(),
        }));
        let (cleanup_finished, cleanup_completion) = mpsc::sync_channel(1);

        let cleanup_thread = thread::spawn(move || {
            drop(pty);
            cleanup_finished.send(()).unwrap();
        });
        wait_control.wait_until_entered("owner child wait");

        let (termination_finished, termination_completion) = mpsc::sync_channel(1);
        let termination_thread = thread::spawn(move || {
            termination_finished.send(terminator.terminate()).unwrap();
        });
        let termination = match termination_completion.recv_timeout(Duration::from_millis(250)) {
            Ok(termination) => termination,
            Err(error) => {
                wait_control.release();
                cleanup_thread.join().unwrap();
                termination_thread.join().unwrap();
                panic!("termination waited for child cleanup: {error}");
            }
        };

        termination.unwrap();
        assert!(matches!(
            cleanup_completion.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        termination_thread.join().unwrap();

        wait_control.release();
        cleanup_completion
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        cleanup_thread.join().unwrap();
        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 0, 1)
        );
    }

    #[test]
    fn waiting_for_a_child_marks_termination_complete() {
        let cleanup = CleanupCounts::default();
        let (mut pty, terminator) = spawned_pty(Box::new(ExitedChild {
            cleanup: cleanup.clone(),
        }));

        pty.process.wait_for_child(Duration::from_secs(1)).unwrap();
        drop(pty);
        terminator.terminate().unwrap();

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 0, 0, 0)
        );
    }

    #[test]
    fn waiting_for_a_live_child_should_honor_the_deadline() {
        let cleanup = CleanupCounts::default();
        let (mut pty, _terminator) = spawned_pty(Box::new(TestChild {
            cleanup: cleanup.clone(),
        }));

        let error = pty.process.wait_for_child(Duration::ZERO).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            error.to_string(),
            "timed out after 0 ms waiting for the shell process to exit"
        );
        assert_eq!(cleanup.try_wait.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn observing_an_exited_child_marks_termination_complete() {
        let cleanup = CleanupCounts::default();
        let (pty, terminator) = spawned_pty(Box::new(ExitedChild {
            cleanup: cleanup.clone(),
        }));

        drop(pty);
        terminator.terminate().unwrap();

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 0, 0, 0)
        );
    }

    #[test]
    fn repeated_termination_signals_once_and_worker_cleanup_still_kills_and_reaps() {
        let cleanup = CleanupCounts::default();
        let (pty, terminator) = spawned_pty(Box::new(TestChild {
            cleanup: cleanup.clone(),
        }));

        terminator.terminate().unwrap();
        terminator.terminate().unwrap();
        assert_eq!(
            pty.process
                .termination
                .complete_process_group(Instant::now())
                .unwrap(),
            ShutdownDisposition::Graceful,
        );
        assert!(pty.process.termination.lock_target().is_none());
        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 1, 1)
        );
    }

    #[test]
    fn failed_termination_signal_should_be_consumed_before_worker_cleanup_recovers() {
        let cleanup = CleanupCounts::default();
        let (pty, terminator) = spawned_pty(Box::new(FailingSignalChild {
            cleanup: cleanup.clone(),
        }));

        let first_error = terminator.terminate().unwrap_err();
        terminator.terminate().unwrap();
        drop(pty);

        assert_eq!(first_error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 1, 1, 1)
        );
    }

    #[test]
    fn termination_should_not_block_behind_a_concurrent_child_reap() {
        let cleanup = CleanupCounts::default();
        let try_wait_control = BlockingCallControl::default();
        let (mut pty, terminator) = spawned_pty(Box::new(BlockingTryWaitChild {
            cleanup: cleanup.clone(),
            try_wait_control: try_wait_control.clone(),
        }));
        let (wait_finished, wait_completion) = mpsc::sync_channel(1);
        let wait_thread = thread::spawn(move || {
            let result = pty.process.wait_for_child(Duration::from_secs(1));
            wait_finished.send((result, pty)).unwrap();
        });
        try_wait_control.wait_until_entered("child try_wait");

        let (termination_started, termination_start) = mpsc::sync_channel(1);
        let (termination_finished, termination_completion) = mpsc::sync_channel(1);
        let termination_thread = thread::spawn(move || {
            termination_started.send(()).unwrap();
            termination_finished.send(terminator.terminate()).unwrap();
        });
        termination_start.recv().unwrap();
        termination_completion
            .recv_timeout(Duration::from_millis(50))
            .unwrap()
            .unwrap();
        assert_eq!(cleanup.signal.load(Ordering::Relaxed), 1);

        try_wait_control.release();
        let (wait_result, pty) = wait_completion
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        wait_result.unwrap();
        wait_thread.join().unwrap();
        termination_thread.join().unwrap();
        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (1, 0, 1, 0)
        );
    }

    #[test]
    fn drop_reaps_a_child_that_exits_while_kill_fails() {
        let cleanup = CleanupCounts::default();
        let (pty, _terminator) = spawned_pty(Box::new(RacedExitChild {
            cleanup: cleanup.clone(),
        }));

        drop(pty);

        assert_eq!(
            (
                cleanup.try_wait.load(Ordering::Relaxed),
                cleanup.owner_kill.load(Ordering::Relaxed),
                cleanup.signal.load(Ordering::Relaxed),
                cleanup.wait.load(Ordering::Relaxed),
            ),
            (2, 1, 0, 0)
        );
    }
}
