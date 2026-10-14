//! POSIX PTY construction and process-session identity-checked retirement.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
#[cfg(all(test, feature = "native-tests"))]
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Error as AnyError;
use portable_pty::{
    Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system,
};
use thiserror::Error;

use crate::pty::native_pty::{
    NativePtyAdapter, NativePtyAdapterConstructionFailure, NativePtyAdapterFactory,
    NativePtyAdapterParts, NativePtyExit, NativePtyOperationFailure, NativePtySettlement,
    NativePtySize, NativePtyTermination, NativePtyWaitFailure,
};
use crate::shell_launch::PreparedShellLaunch;
#[cfg(all(test, feature = "native-tests"))]
use crate::shell_launch::ShellLaunchPlanner;

const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const TERM_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const FORCED_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
// Fence children from concurrent macOS proofs would invalidate each other's fork-free interval.
// Retain this gate for the complete attempt, including foreground adapter cleanup.
#[cfg(target_os = "macos")]
static SETTLEMENT_ATTEMPT_LOCK: Mutex<()> = Mutex::new(());
#[cfg(target_os = "macos")]
const SETTLEMENT_ATTEMPT_TIMEOUT: Duration = GRACEFUL_SHUTDOWN_TIMEOUT
    .saturating_add(TERM_SHUTDOWN_TIMEOUT)
    .saturating_add(FORCED_SHUTDOWN_TIMEOUT);

fn lock_for_settlement<T>(
    lock: &Mutex<T>,
    deadline: Option<Instant>,
) -> Result<MutexGuard<'_, T>, io::ErrorKind> {
    let Some(deadline) = deadline else {
        return lock.lock().map_err(|_| io::ErrorKind::Other);
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut);
        }
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(_)) => return Err(io::ErrorKind::Other),
            Err(TryLockError::WouldBlock) => {
                thread::sleep(CHILD_EXIT_POLL_INTERVAL.min(remaining));
            }
        }
    }
}
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
pub trait UnixPtyHost: Send + Sync + 'static {
    /// Observe one live process. Returns `None` for a missing, exited, or unstable process.
    fn observe_process(&self, process: i32) -> Option<ProcessObservation>;

    /// Inspect a process for settlement. Missing processes are not inspection failures.
    /// Unlike live observations, exited but unreaped members must remain visible.
    fn inspect_process(&self, _process: i32) -> io::Result<Option<ProcessObservation>> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Capture an optional monotonic birth ordinal without changing native start identity.
    fn process_birth(&self, _process: i32) -> io::Result<Option<u64>> {
        Ok(None)
    }

    /// Inspect membership in the isolated session created by this leader.
    fn inspect_session_member(
        &self,
        process: i32,
        leader: ProcessIdentity,
        _leader_birth: Option<u64>,
    ) -> io::Result<Option<ProcessObservation>> {
        self.inspect_process(process).map(|observation| {
            observation.filter(|observation| observation.session == leader.process)
        })
    }

    /// Capture the identity of a child this owner just spawned and has not reaped. A child may
    /// already have exited before the parent runs again, while its process identity is retained.
    fn observe_spawned_child(&self, process: i32) -> Option<ProcessObservation> {
        self.observe_process(process)
    }

    /// List every process identifier currently visible to this user.
    fn process_ids(&self) -> io::Result<Vec<i32>>;

    /// Fence a settlement census with a global, monotonic process-creation observation.
    /// Providers that cannot prove a fork-free census leave settlement unavailable.
    fn process_creation_epoch(&self) -> io::Result<ProcessCreationEpoch> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Enumerate processes for an absence proof, including a check for unpublished births.
    fn settlement_process_ids(&self) -> io::Result<Vec<i32>> {
        self.process_ids()
    }

    /// Classify an output read failure as hangup or failure.
    fn master_read_error(&self, error: &io::Error) -> MasterReadError;
}

/// Native evidence that no process was created during a complete settlement census.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessCreationEpoch {
    /// The kernel's cumulative count of published forks, including threads.
    ForkCount(u64),
    /// The permanent birth ordinal of a newly created and reaped fence process.
    FenceBirth(u64),
}

impl ProcessCreationEpoch {
    pub(crate) fn no_intervening_fork(self, after: Self) -> bool {
        match (self, after) {
            (Self::ForkCount(before), Self::ForkCount(after)) => before == after,
            (Self::FenceBirth(before), Self::FenceBirth(after)) => {
                before.checked_add(1) == Some(after)
            }
            _ => false,
        }
    }
}

/// How the shared PTY reader treats an Operating-System read failure on the PTY master.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MasterReadError {
    /// Every slave descriptor closed; the terminal output ended.
    Hangup,
    /// The read failed for another reason.
    Failure,
}

/// A process start instant in host-defined units; equal starts identify the same process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessStart {
    /// Host-defined coarse start time.
    pub coarse: u64,
    /// Host-defined subunit of the start time.
    pub fine: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A process identifier paired with its native start time.
pub struct ProcessIdentity {
    /// Native process identifier.
    pub process: i32,
    /// Start time used to reject identifier reuse.
    pub start: ProcessStart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessSessionIdentity {
    session: i32,
    leader: ProcessIdentity,
    birth: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// An atomic or stability-checked observation of native process ownership.
pub struct ProcessObservation {
    /// Stable process identity.
    pub identity: ProcessIdentity,
    /// Current process group.
    pub process_group: i32,
    /// Current process session.
    pub session: i32,
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
    // Select session settlement without waiting on authority held by an in-flight proof.
    session_settlement: bool,
    target: Mutex<Option<TerminationTarget>>,
    requested: AtomicBool,
    forced: AtomicBool,
    child: Arc<Mutex<ChildState>>,
    settlement: Mutex<Option<NativePtySettlement>>,
}

struct ChildState {
    child: Option<Box<dyn Child + Send + Sync>>,
    exit: Option<ExitStatus>,
}

impl ChildState {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = &self.exit {
            return Ok(Some(status.clone()));
        }
        let child = self.child.as_mut().ok_or_else(child_already_reaped_error)?;
        let status = child.try_wait()?;
        if let Some(status) = &status {
            self.exit = Some(status.clone());
            self.child.take();
        }
        Ok(status)
    }
}

impl ChildTermination {
    #[cfg(all(test, feature = "native-tests"))]
    fn new(
        host: Arc<dyn UnixPtyHost>,
        process_group: Option<i32>,
        fallback: Box<dyn ChildKiller + Send + Sync>,
        child: Arc<Mutex<ChildState>>,
    ) -> Self {
        Self {
            session_settlement: false,
            target: Mutex::new(Some(TerminationTarget {
                host,
                process_group,
                session: None,
                fallback,
            })),
            requested: AtomicBool::new(false),
            forced: AtomicBool::new(false),
            child,
            settlement: Mutex::new(None),
        }
    }

    fn new_with_session(
        host: Arc<dyn UnixPtyHost>,
        process_group: i32,
        session: ProcessSessionIdentity,
        fallback: Box<dyn ChildKiller + Send + Sync>,
        child: Arc<Mutex<ChildState>>,
    ) -> Self {
        Self {
            session_settlement: true,
            target: Mutex::new(Some(TerminationTarget {
                host,
                process_group: Some(process_group),
                session: Some(session),
                fallback,
            })),
            requested: AtomicBool::new(false),
            forced: AtomicBool::new(false),
            child,
            settlement: Mutex::new(None),
        }
    }

    fn signal(&self) -> io::Result<()> {
        if self.requested.load(Ordering::Acquire) {
            return Ok(());
        }
        self.signal_locked(&mut self.lock_target())
    }

    fn signal_locked(&self, target: &mut Option<TerminationTarget>) -> io::Result<()> {
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
        Ok(ShutdownDisposition::Forced)
    }

    fn settle(&self) -> NativePtySettlement {
        #[cfg(target_os = "macos")]
        let deadline = Some(Instant::now() + SETTLEMENT_ATTEMPT_TIMEOUT);
        #[cfg(not(target_os = "macos"))]
        let deadline = None;

        #[cfg(target_os = "macos")]
        {
            // A positive receipt remains valid even while an unrelated Session is proving absence.
            let Ok(receipt) = lock_for_settlement(&self.settlement, deadline) else {
                return NativePtySettlement::Unavailable;
            };
            if let Some(receipt) = *receipt {
                return receipt;
            }
        }
        #[cfg(target_os = "macos")]
        let Ok(_attempt) = lock_for_settlement(&SETTLEMENT_ATTEMPT_LOCK, deadline) else {
            self.force_after_proof_contention();
            return NativePtySettlement::Unavailable;
        };
        let Ok(mut receipt) = lock_for_settlement(&self.settlement, deadline) else {
            return NativePtySettlement::Unavailable;
        };
        if let Some(receipt) = *receipt {
            return receipt;
        }
        let result = self.settle_once(deadline);
        // Inconclusive observations keep the original Session authority for a fresh proof.
        if result == NativePtySettlement::Settled {
            *receipt = Some(result);
        }
        result
    }

    #[cfg(target_os = "macos")]
    fn force_after_proof_contention(&self) {
        // The foreground budget expired while another Session owned the proof gate. Escalation
        // spawns no fence children and may still retire a HUP-resistant Session. Never wait on
        // authority held by another caller, clear that authority, or claim an absence certificate.
        let Ok(mut target_slot) = self.target.try_lock() else {
            return;
        };
        let Some(target) = target_slot.as_mut() else {
            return;
        };
        self.requested.store(true, Ordering::Release);
        self.forced.store(true, Ordering::Release);
        // Failure leaves the same unresolved authority available to the later re-proof service.
        let _ = target.force();
    }

    fn settle_once(&self, attempt_deadline: Option<Instant>) -> NativePtySettlement {
        let mut unavailable = if self.requested.load(Ordering::Acquire) {
            false
        } else {
            let Ok(mut target) = lock_for_settlement(&self.target, attempt_deadline) else {
                return NativePtySettlement::Unavailable;
            };
            self.signal_locked(&mut target).is_err()
        };
        let mut retained_groups = BTreeMap::<i32, Vec<ProcessIdentity>>::new();
        // One caller owns the complete attempt. A later caller may re-prove an inconclusive result.
        for (signal, timeout) in [
            (libc::SIGHUP, GRACEFUL_SHUTDOWN_TIMEOUT),
            (libc::SIGTERM, TERM_SHUTDOWN_TIMEOUT),
            (libc::SIGKILL, FORCED_SHUTDOWN_TIMEOUT),
        ] {
            if attempt_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return NativePtySettlement::Unavailable;
            }
            if signal != libc::SIGHUP {
                let Ok(mut target) = lock_for_settlement(&self.target, attempt_deadline) else {
                    return NativePtySettlement::Unavailable;
                };
                let Some(target) = target.as_mut() else {
                    return NativePtySettlement::Unavailable;
                };
                unavailable |= target.signal_owned_process_groups(signal).is_err();
                self.forced.store(true, Ordering::Release);
            }
            let phase_deadline = Instant::now() + timeout;
            let deadline =
                attempt_deadline.map_or(phase_deadline, |deadline| deadline.min(phase_deadline));
            loop {
                if attempt_deadline.is_some() && Instant::now() >= deadline {
                    break;
                }
                let reaped = match lock_for_settlement(&self.child, attempt_deadline) {
                    Ok(mut child) => match child.try_wait() {
                        Ok(status) => status.is_some(),
                        Err(_) => {
                            unavailable = true;
                            false
                        }
                    },
                    Err(_) => return NativePtySettlement::Unavailable,
                };
                let Ok(mut target_slot) = lock_for_settlement(&self.target, attempt_deadline)
                else {
                    return NativePtySettlement::Unavailable;
                };
                let Some(target) = target_slot.as_ref() else {
                    return NativePtySettlement::Unavailable;
                };
                let Some(session) = target.session else {
                    return NativePtySettlement::Unavailable;
                };
                match inspected_process_groups_in_session(target.host.as_ref(), &session) {
                    Ok(groups) => {
                        let census_empty = groups.is_empty();
                        // Keep secondary groups across parent/child handoffs. A vanished
                        // member is not proof that its group, or the session, is empty.
                        retained_groups.extend(groups);
                        let mut groups_absent = true;
                        for (&group, members) in &retained_groups {
                            match process_group_is_alive(group) {
                                Ok(false) => {}
                                Ok(true) => {
                                    groups_absent = false;
                                    unavailable |= signal_owned_process_group(
                                        target.host.as_ref(),
                                        group,
                                        members,
                                        &session,
                                        signal,
                                    )
                                    .is_err();
                                }
                                Err(_) => {
                                    groups_absent = false;
                                    unavailable = true;
                                }
                            }
                        }
                        if reaped && census_empty && groups_absent {
                            if attempt_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                                return NativePtySettlement::Unavailable;
                            }
                            match target.process_group.map(process_group_is_alive).transpose() {
                                Ok(Some(false)) => {
                                    match confirmed_session_absence(target.host.as_ref(), &session)
                                    {
                                        Ok(true) => {
                                            if attempt_deadline
                                                .is_some_and(|deadline| Instant::now() >= deadline)
                                            {
                                                return NativePtySettlement::Unavailable;
                                            }
                                            // Revoke authority only after the fork-fenced absence proof.
                                            target_slot.take();
                                            return NativePtySettlement::Settled;
                                        }
                                        Ok(false) => {}
                                        Err(_) => unavailable = true,
                                    }
                                }
                                Ok(_) => {}
                                Err(_) => unavailable = true,
                            }
                        }
                    }
                    Err(_) => unavailable = true,
                }
                drop(target_slot);
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                thread::sleep(CHILD_EXIT_POLL_INTERVAL.min(remaining));
            }
        }
        if unavailable {
            NativePtySettlement::Unavailable
        } else {
            NativePtySettlement::StillRunning
        }
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

fn process_groups_in_session(
    host: &dyn UnixPtyHost,
    session: &ProcessSessionIdentity,
) -> io::Result<BTreeMap<i32, Vec<ProcessIdentity>>> {
    if let Some(birth) = session.birth
        && host
            .process_birth(session.session)?
            .is_some_and(|current| current != birth)
    {
        return Ok(BTreeMap::new());
    }
    let leader = host.observe_process(session.session);
    if leader.is_some_and(|leader| {
        leader.identity != session.leader || leader.session != session.session
    }) {
        return Ok(BTreeMap::new());
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

fn inspected_process_groups_in_session(
    host: &dyn UnixPtyHost,
    session: &ProcessSessionIdentity,
) -> io::Result<BTreeMap<i32, Vec<ProcessIdentity>>> {
    if let Some(birth) = session.birth
        && host
            .process_birth(session.session)?
            .is_some_and(|current| current != birth)
    {
        return Ok(BTreeMap::new());
    }
    if host
        .inspect_process(session.session)?
        .is_some_and(|leader| {
            leader.identity != session.leader || leader.session != session.session
        })
    {
        // A session identifier cannot be reused while the old session has members.
        return Ok(BTreeMap::new());
    }
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for process in host
        .process_ids()?
        .into_iter()
        .filter(|process| *process > 0)
    {
        let Some(observation) =
            host.inspect_session_member(process, session.leader, session.birth)?
        else {
            continue;
        };
        if observation.session == session.session {
            groups
                .entry(observation.process_group)
                .or_default()
                .push(observation.identity);
        }
    }
    Ok(groups)
}

fn confirmed_session_absence(
    host: &dyn UnixPtyHost,
    session: &ProcessSessionIdentity,
) -> io::Result<bool> {
    let before = host.process_creation_epoch()?;
    for process in host
        .settlement_process_ids()?
        .into_iter()
        .filter(|pid| *pid > 0)
    {
        if host
            .inspect_session_member(process, session.leader, session.birth)?
            .is_some()
        {
            return Ok(false);
        }
    }
    let after = host.process_creation_epoch()?;
    // Existing processes cannot join an existing session. With a complete census
    // and no intervening fork, an unseen replacement cannot inherit this session.
    if before.no_intervening_fork(after) {
        Ok(true)
    } else {
        Err(io::ErrorKind::WouldBlock.into())
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
    child: Arc<Mutex<ChildState>>,
    termination: Arc<ChildTermination>,
}

struct PtyReader {
    reader: MasterReader,
    readiness: OwnedFd,
}

impl Read for PtyReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.reader.read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let mut poll = libc::pollfd {
                        fd: self.readiness.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: readiness owns a live descriptor and poll points to one initialized entry.
                    if unsafe { libc::poll(&mut poll, 1, -1) } == -1 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                }
                result => return result,
            }
        }
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

    fn hidden_input(&self) -> io::Result<bool> {
        let descriptor = self
            .master
            .as_raw_fd()
            .ok_or_else(|| io::Error::other("PTY master descriptor is unavailable"))?;
        read_termios(descriptor).map(|termios| termios_hidden_input(&termios))
    }

    fn wait_for_child(&mut self, timeout: Duration) -> io::Result<ShellExit> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait_for_child()? {
                let shutdown = if self.termination.session_settlement {
                    if self.termination.requested() {
                        let _ = self.termination.settle();
                    }
                    if !self.termination.requested() {
                        ShutdownDisposition::NotRequested
                    } else if self.termination.forced.load(Ordering::Acquire) {
                        ShutdownDisposition::Forced
                    } else {
                        ShutdownDisposition::Graceful
                    }
                } else {
                    self.termination
                        .complete_process_group(Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT)?
                };
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

    fn poll_child_exit(&self) -> io::Result<ShellExit> {
        // Mechanical retries must not wait for a proof's child lock or start another proof.
        let mut child = self.child.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::ErrorKind::TimedOut,
            TryLockError::Poisoned(_) => io::ErrorKind::Other,
        })?;
        let status = child.try_wait()?.ok_or(io::ErrorKind::TimedOut)?;
        let shutdown = if !self.termination.requested() {
            ShutdownDisposition::NotRequested
        } else if self.termination.forced.load(Ordering::Acquire) {
            ShutdownDisposition::Forced
        } else {
            ShutdownDisposition::Graceful
        };
        Ok(ShellExit { status, shutdown })
    }

    fn try_wait_for_child(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child
            .lock()
            .map_err(|_| io::ErrorKind::Other)?
            .try_wait()
    }

    fn cleanup_child(&mut self) {
        if self.termination.session_settlement {
            let _ = self.termination.settle();
            return;
        }
        let child = match self.child.lock() {
            Ok(mut child) => child.child.take(),
            Err(_) => return,
        };
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
            if let Err(kill_error) = child.kill() {
                eprintln!(
                    "failed to terminate shell process after process-group shutdown failed: {:?}",
                    kill_error.kind()
                );
            }
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

    fn hidden_input(&self) -> Result<bool, NativePtyOperationFailure> {
        Self::hidden_input(self).map_err(|error| NativePtyOperationFailure::new(error.kind()))
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Result<NativePtyExit, NativePtyWaitFailure> {
        let exit = if timeout.is_zero() {
            self.poll_child_exit()
        } else {
            self.wait_for_child(timeout)
        }
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
    fn request_termination(&self) -> io::Result<()> {
        self.terminate()
    }

    fn settle(&self) -> NativePtySettlement {
        self.termination.settle()
    }
}

impl Drop for SpawnedPty {
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
pub struct UnixNativePtyAdapterFactory {
    host: Arc<dyn UnixPtyHost>,
}

impl UnixNativePtyAdapterFactory {
    /// Select the operating system's process and hangup observation adapter.
    pub fn new(host: Arc<dyn UnixPtyHost>) -> Self {
        Self { host }
    }
}

impl NativePtyAdapterFactory for UnixNativePtyAdapterFactory {
    fn create(
        &self,
        launch: PreparedShellLaunch,
        size: NativePtySize,
    ) -> Result<NativePtyAdapterParts, NativePtyAdapterConstructionFailure> {
        spawn_native_pty(Arc::clone(&self.host), launch, size)
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
) -> Result<NativePtyAdapterParts, PtyError> {
    let command = command_from_launch(&launch);
    let (pty, termination) = spawn_command_in_pty(host, portable_size(size), command)?;
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
    mut command: Command,
) -> Result<(SpawnedPty, PtyTerminator), PtyError> {
    let pair = {
        // openpty applies CLOEXEC separately; exclude spawn until both sides are ready.
        let _guard =
            crate::process::descriptor_guard().map_err(|error| PtyError::Open(error.into()))?;
        native_pty_system().openpty(size).map_err(PtyError::Open)?
    };
    initialize_pty(pair.master.as_ref(), size)?;
    let readiness = pair.master.as_raw_fd().ok_or(PtyError::MissingDescriptor)?;
    // SAFETY: pair.master owns this live descriptor throughout cloning.
    let readiness = unsafe { BorrowedFd::borrow_raw(readiness) }
        .try_clone_to_owned()
        .map_err(|error| PtyError::CloneReader(error.into()))?;

    let slave_path = pair.master.tty_name().ok_or(PtyError::MissingDescriptor)?;
    let mut child: Box<dyn Child + Send + Sync> =
        Box::new(super::spawn::spawn(&mut command, &slave_path).map_err(|_| PtyError::SpawnShell)?);

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
    let reader: Box<dyn Read + Send> = Box::new(PtyReader { reader, readiness });
    let writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(source) => {
            terminate_after_startup_failure(child.as_mut());
            return Err(PtyError::TakeWriter(source));
        }
    };

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
        birth: host.process_birth(process_group).ok().flatten(),
    };
    let fallback = child.clone_killer();
    let child = Arc::new(Mutex::new(ChildState {
        child: Some(child),
        exit: None,
    }));
    let termination = Arc::new(ChildTermination::new_with_session(
        host,
        process_group,
        session,
        fallback,
        Arc::clone(&child),
    ));
    let terminator = PtyTerminator::new(Arc::clone(&termination));

    Ok((
        SpawnedPty {
            master: pair.master,
            reader: Some(Box::new(reader)),
            writer,
            child,
            termination,
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

const fn termios_hidden_input(termios: &libc::termios) -> bool {
    termios.c_lflag & libc::ICANON != 0 && termios.c_lflag & libc::ECHO == 0
}

/// Translate already-decided launch values without giving the process library spawn authority.
fn command_from_launch(launch: &PreparedShellLaunch) -> Command {
    let defaults = CommandBuilder::new(launch.executable());
    let fallback_shell = defaults.get_env("SHELL").map(std::ffi::OsStr::to_owned);
    let mut command = Command::new(launch.executable());
    command.args(launch.arguments());
    // Build one filtered snapshot instead of removing names from another ambient snapshot.
    // Prefix matching uses native bytes, including non-Unicode names and values.
    command.env_clear();
    if launch.inherit_environment() {
        for (name, value) in std::env::vars_os() {
            if !launch.denies_inherited_environment(&name) {
                command.env(name, value);
            }
        }
        // Preserve portable-pty's account-shell default when the parent exports no SHELL.
        if !command.get_envs().any(|(name, _)| name == "SHELL")
            && !launch.denies_inherited_environment(std::ffi::OsStr::new("SHELL"))
            && let Some(shell) = fallback_shell
        {
            command.env("SHELL", shell);
        }
    }
    for (name, value) in launch.environment() {
        command.env(name, value);
    }
    // portable-pty also supplied the account shell for an explicitly cleared remote environment.
    if !command.get_envs().any(|(name, _)| name == "SHELL") {
        let mut account = defaults;
        account.env_clear();
        command.env("SHELL", account.get_shell());
    }
    command.current_dir(launch.working_directory());
    command
}

#[cfg(all(test, feature = "native-tests"))]
pub(crate) fn test_factory() -> UnixNativePtyAdapterFactory {
    UnixNativePtyAdapterFactory::new(test_host())
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

    #[test]
    fn launch_command_preserves_arguments_directory_and_environment() {
        let launch = ShellLaunchPlanner::for_test(
            "/bin/zsh".into(),
            "/spaceterm-conformance-missing-resources".into(),
        )
        .local(Path::new("/tmp"))
        .unwrap();
        let command = command_from_launch(&launch);
        assert_eq!(
            (
                command.get_program(),
                command.get_args().collect::<Vec<_>>()
            ),
            (
                std::ffi::OsStr::new("/bin/zsh"),
                vec![std::ffi::OsStr::new("-l")]
            )
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/tmp")));
        for (name, value) in [
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "ghostty"),
            ("SPACETERM", "1"),
        ] {
            assert_eq!(
                command
                    .get_envs()
                    .find_map(|(key, value)| (key == name).then_some(value).flatten()),
                Some(std::ffi::OsStr::new(value)),
                "{name}"
            );
        }
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
    fn hidden_input_requires_canonical_mode_without_echo() {
        // SAFETY: termios is a plain C data structure whose zero value is valid for flag tests.
        let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
        termios.c_lflag = libc::ICANON | libc::ECHO;
        assert!(!termios_hidden_input(&termios));

        termios.c_lflag = libc::ICANON;
        assert!(termios_hidden_input(&termios));

        termios.c_lflag = 0;
        assert!(!termios_hidden_input(&termios));
    }

    const CONTROLLED_CHILD_ENV: &str = "SPACETERM_CONTROLLED_PTY_CHILD";
    const CONTROLLED_CHILD_ACK: &str = "SPACETERM_ACK\n";

    #[test]
    fn unread_native_pty_input_returns_backpressure_without_blocking() {
        const NAME: &str =
            "pty::unix_pty::tests::unread_native_pty_input_returns_backpressure_without_blocking";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _lock = lock_real_pty_test();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "stty raw -echo; printf 'READY'; exec sleep 30"]);
        let (mut pty, terminator) =
            spawn_command_in_pty(test_host(), PtySize::default(), command).unwrap();
        let mut reader = pty.take_reader().unwrap();
        read_output_marker(&mut reader, b"READY");
        // Bound the write, not shell startup under a loaded native test runner.
        let (cancel, rescue) = mpsc::channel();
        let watchdog = thread::spawn(move || {
            if rescue.recv_timeout(Duration::from_secs(1)).is_err() {
                terminator.terminate().unwrap();
            }
        });
        let result = pty.write_all(&vec![b'x'; 1024 * 1024]);
        let _ = cancel.send(());
        watchdog.join().unwrap();
        drop(reader);
        drop(pty);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
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

    fn controlled_child_command(working_directory: &Path, marker: &str) -> Command {
        let mut command = Command::new(env::current_exe().unwrap());
        command.args([
            "--ignored",
            "--exact",
            "pty::unix_pty::tests::controlled_pty_child_reports_runtime_configuration",
            "--nocapture",
        ]);
        command.env(CONTROLLED_CHILD_ENV, "1");
        command.env("SPACETERM_PTY_MARKER", marker);
        command.current_dir(working_directory);
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
        let status = pty.wait_for_child(Duration::from_secs(2)).unwrap();
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
            "pty::unix_pty::tests::controlled_child_should_observe_initialized_terminal_state_before_interaction",
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

        let (mut pty, _terminator) = spawn_command_in_pty(test_host(), size, command).unwrap();
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
            "pty::unix_pty::tests::concurrent_terminal_sessions_should_own_distinct_process_groups",
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
        )
        .unwrap();
        let (mut second, _second_terminator) = spawn_command_in_pty(
            test_host(),
            size,
            controlled_child_command(&working_directory, "second"),
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

    struct CensusHandoffHost {
        native: Arc<dyn UnixPtyHost>,
        inspecting: AtomicBool,
        handoff: Mutex<Option<(i32, std::path::PathBuf, std::path::PathBuf)>>,
    }

    impl UnixPtyHost for CensusHandoffHost {
        fn observe_process(&self, process: i32) -> Option<ProcessObservation> {
            self.native.observe_process(process)
        }
        fn observe_spawned_child(&self, process: i32) -> Option<ProcessObservation> {
            self.native.observe_spawned_child(process)
        }
        fn inspect_process(&self, process: i32) -> io::Result<Option<ProcessObservation>> {
            self.inspecting.store(true, Ordering::Release);
            self.native.inspect_process(process)
        }
        fn process_birth(&self, process: i32) -> io::Result<Option<u64>> {
            self.native.process_birth(process)
        }
        fn inspect_session_member(
            &self,
            process: i32,
            leader: ProcessIdentity,
            birth: Option<u64>,
        ) -> io::Result<Option<ProcessObservation>> {
            self.native.inspect_session_member(process, leader, birth)
        }
        fn process_ids(&self) -> io::Result<Vec<i32>> {
            let ids = self.native.process_ids()?;
            if self.inspecting.load(Ordering::Acquire)
                && let Some((parent, barrier, replacement)) = self.handoff.lock().unwrap().take()
            {
                assert!(ids.contains(&parent));
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(barrier)
                    .unwrap()
                    .write_all(b"fork\n")
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                while self.native.inspect_process(parent)?.is_some() || !replacement.exists() {
                    assert!(Instant::now() < deadline, "handoff did not finish");
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Ok(ids)
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

    #[test]
    fn session_census_fork_after_enumeration_cannot_confirm_a_live_replacement() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "pty::unix_pty::tests::session_census_fork_after_enumeration_cannot_confirm_a_live_replacement",
        ) {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let barrier = directory.path().join("barrier");
        let ready = directory.path().join("ready");
        let parent_file = directory.path().join("parent");
        let replacement = directory.path().join("replacement");
        let fifo = CString::new(barrier.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: fifo is a NUL-terminated path in this test's private directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let secondary = directory.path().join("secondary");
        std::fs::write(
            &secondary,
            format!(
                "trap '' HUP TERM\nexec </dev/null >/dev/null 2>&1\necho ready > '{}'\nread token < '{}'\n/bin/sh -c 'trap \"\" HUP TERM; echo $$ > \"{}\"; exec sleep 30' &\nexit 0\n",
                ready.display(), barrier.display(), replacement.display()
            ),
        )
        .unwrap();
        let native = test_host();
        let host = Arc::new(CensusHandoffHost {
            native: Arc::clone(&native),
            inspecting: AtomicBool::new(false),
            handoff: Mutex::new(None),
        });
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!(
                "trap '' HUP\nset -m\n/bin/sh '{}' &\necho $! > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\necho ready\nexit 0\n",
                secondary.display(), parent_file.display(), ready.display()
            ),
        ]);
        let (mut pty, terminator) =
            spawn_command_in_pty(host.clone(), PtySize::default(), command).unwrap();
        let leader = pty
            .termination
            .lock_target()
            .as_ref()
            .unwrap()
            .session
            .unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "ready");
        // Reap through the shared child state before admitting the census handoff.
        let deadline = Instant::now() + Duration::from_secs(5);
        while pty.child.lock().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "leader did not exit");
            thread::sleep(Duration::from_millis(1));
        }
        let parent = std::fs::read_to_string(parent_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let member = native.observe_process(parent).unwrap();
        assert_eq!(member.session, leader.session);
        assert_ne!(
            member.process_group, leader.session,
            "the handoff must use a secondary group"
        );
        *host.handoff.lock().unwrap() = Some((parent, barrier, replacement.clone()));

        let receipt = terminator.settle();
        let replacement: i32 = std::fs::read_to_string(replacement)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let survivor = native.observe_process(replacement);
        if let Some(survivor) = survivor {
            assert_eq!(survivor.session, leader.session);
            // Clean up this exact observed fixture before asserting, including on RED.
            if native.observe_process(replacement) == Some(survivor) {
                // SAFETY: the stable observed member belongs to this test's owned session.
                unsafe { libc::kill(replacement, libc::SIGKILL) };
            }
        }
        assert!(
            receipt != NativePtySettlement::Settled || survivor.is_none(),
            "a replacement born after enumeration survived a Settled receipt"
        );
    }

    struct SettlementHost {
        outcome: NativePtySettlement,
        leader: ProcessIdentity,
    }

    impl UnixPtyHost for SettlementHost {
        fn observe_process(&self, _process: i32) -> Option<ProcessObservation> {
            None
        }
        fn inspect_process(&self, process: i32) -> io::Result<Option<ProcessObservation>> {
            match self.outcome {
                NativePtySettlement::Unavailable => Err(io::ErrorKind::PermissionDenied.into()),
                NativePtySettlement::Settled => Ok(None),
                NativePtySettlement::StillRunning => Ok(Some(ProcessObservation {
                    identity: ProcessIdentity {
                        process,
                        start: self.leader.start,
                    },
                    process_group: self.leader.process,
                    session: self.leader.process,
                })),
            }
        }
        fn process_ids(&self) -> io::Result<Vec<i32>> {
            Ok(vec![self.leader.process])
        }
        fn process_creation_epoch(&self) -> io::Result<ProcessCreationEpoch> {
            Ok(ProcessCreationEpoch::ForkCount(0))
        }
        fn master_read_error(&self, _error: &io::Error) -> MasterReadError {
            MasterReadError::Failure
        }
    }

    struct UnstableCensusHost {
        epochs: [ProcessCreationEpoch; 2],
        queries: AtomicUsize,
        incomplete: bool,
    }

    impl UnixPtyHost for UnstableCensusHost {
        fn observe_process(&self, _process: i32) -> Option<ProcessObservation> {
            None
        }
        fn inspect_process(&self, _process: i32) -> io::Result<Option<ProcessObservation>> {
            Ok(None)
        }
        fn process_ids(&self) -> io::Result<Vec<i32>> {
            Ok(Vec::new())
        }
        fn settlement_process_ids(&self) -> io::Result<Vec<i32>> {
            if self.incomplete {
                Err(io::ErrorKind::WouldBlock.into())
            } else {
                self.process_ids()
            }
        }
        fn process_creation_epoch(&self) -> io::Result<ProcessCreationEpoch> {
            Ok(self.epochs[self.queries.fetch_add(1, Ordering::Relaxed) % 2])
        }
        fn master_read_error(&self, _error: &io::Error) -> MasterReadError {
            MasterReadError::Failure
        }
    }

    #[test]
    fn settlement_rejects_empty_censuses_with_forks_or_unpublished_members() {
        let _isolation = lock_real_pty_test();
        for (epochs, incomplete) in [
            (
                [
                    ProcessCreationEpoch::ForkCount(1),
                    ProcessCreationEpoch::ForkCount(2),
                ],
                false,
            ),
            (
                [
                    ProcessCreationEpoch::FenceBirth(1),
                    ProcessCreationEpoch::FenceBirth(3),
                ],
                false,
            ),
            (
                [
                    ProcessCreationEpoch::FenceBirth(1),
                    ProcessCreationEpoch::FenceBirth(2),
                ],
                true,
            ),
        ] {
            let leader = ProcessIdentity {
                process: i32::MAX,
                start: ProcessStart { coarse: 1, fine: 0 },
            };
            let child: Box<dyn Child + Send + Sync> = Box::new(ExitedChild {
                cleanup: CleanupCounts::default(),
            });
            let fallback = child.clone_killer();
            let termination = ChildTermination::new_with_session(
                Arc::new(UnstableCensusHost {
                    epochs,
                    queries: AtomicUsize::new(0),
                    incomplete,
                }),
                leader.process,
                ProcessSessionIdentity {
                    session: leader.process,
                    leader,
                    birth: None,
                },
                fallback,
                Arc::new(Mutex::new(ChildState {
                    child: Some(child),
                    exit: None,
                })),
            );
            assert_eq!(
                PtyTerminator::new(Arc::new(termination)).settle(),
                NativePtySettlement::Unavailable
            );
        }
    }

    struct ReprovableSettlementHost {
        conclusive: AtomicBool,
        inconclusive_members: bool,
        censuses: AtomicUsize,
        queries: AtomicUsize,
        member_checks: Mutex<Vec<(ProcessIdentity, Option<u64>)>>,
        blocked_census: Option<BlockingCallControl>,
        census_entered: Option<mpsc::Sender<()>>,
    }

    impl ReprovableSettlementHost {
        fn new(conclusive: bool) -> Self {
            Self {
                conclusive: AtomicBool::new(conclusive),
                inconclusive_members: false,
                censuses: AtomicUsize::new(0),
                queries: AtomicUsize::new(0),
                member_checks: Mutex::new(Vec::new()),
                blocked_census: None,
                census_entered: None,
            }
        }
    }

    impl UnixPtyHost for ReprovableSettlementHost {
        fn observe_process(&self, _process: i32) -> Option<ProcessObservation> {
            None
        }
        fn inspect_process(&self, _process: i32) -> io::Result<Option<ProcessObservation>> {
            Ok(None)
        }
        fn inspect_session_member(
            &self,
            _process: i32,
            leader: ProcessIdentity,
            birth: Option<u64>,
        ) -> io::Result<Option<ProcessObservation>> {
            self.member_checks.lock().unwrap().push((leader, birth));
            Ok(
                (self.inconclusive_members && !self.conclusive.load(Ordering::Acquire)).then_some(
                    ProcessObservation {
                        identity: leader,
                        process_group: leader.process,
                        session: leader.process,
                    },
                ),
            )
        }
        fn process_ids(&self) -> io::Result<Vec<i32>> {
            self.censuses.fetch_add(1, Ordering::Relaxed);
            if let Some(entered) = &self.census_entered {
                entered.send(()).unwrap();
            }
            if let Some(blocked) = &self.blocked_census {
                blocked.block();
            }
            Ok(vec![1])
        }
        fn settlement_process_ids(&self) -> io::Result<Vec<i32>> {
            self.queries.fetch_add(1, Ordering::Relaxed);
            if self.conclusive.load(Ordering::Acquire) {
                Ok(vec![1])
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
        fn process_creation_epoch(&self) -> io::Result<ProcessCreationEpoch> {
            Ok(ProcessCreationEpoch::ForkCount(0))
        }
        fn master_read_error(&self, _error: &io::Error) -> MasterReadError {
            MasterReadError::Failure
        }
    }

    fn reprovable_pty(
        host: Arc<dyn UnixPtyHost>,
        process: i32,
    ) -> (SpawnedPty, Arc<dyn NativePtyTermination>, CleanupCounts) {
        let cleanup = CleanupCounts::default();
        let child: Box<dyn Child + Send + Sync> = Box::new(ExitedChild {
            cleanup: cleanup.clone(),
        });
        let fallback = child.clone_killer();
        let child = Arc::new(Mutex::new(ChildState {
            child: Some(child),
            exit: None,
        }));
        let leader = ProcessIdentity {
            process,
            start: ProcessStart { coarse: 1, fine: 2 },
        };
        let termination = Arc::new(ChildTermination::new_with_session(
            host,
            process,
            ProcessSessionIdentity {
                session: process,
                leader,
                birth: Some(7),
            },
            fallback,
            Arc::clone(&child),
        ));
        let pty = SpawnedPty {
            master: Box::new(TestMasterPty),
            reader: None,
            writer: Box::new(io::sink()),
            child,
            termination: Arc::clone(&termination),
        };
        (pty, Arc::new(PtyTerminator::new(termination)), cleanup)
    }

    #[test]
    fn zero_duration_native_wait_does_not_block_on_child_coordination() {
        let _isolation = lock_real_pty_test();
        let host = Arc::new(ReprovableSettlementHost::new(true));
        let (mut pty, _termination, _) = reprovable_pty(host.clone(), i32::MAX);
        let child = Arc::clone(&pty.child);
        let guard = child.lock().unwrap();
        let (started, starting) = mpsc::channel();
        let (result, receipt) = mpsc::channel();
        let poll = thread::spawn(move || {
            started.send(()).unwrap();
            result
                .send(NativePtyAdapter::wait_for_exit(&mut pty, Duration::ZERO))
                .unwrap();
            pty
        });
        starting.recv_timeout(Duration::from_secs(1)).unwrap();
        let bounded_result = receipt.recv_timeout(Duration::from_secs(1));
        let censuses = host.censuses.load(Ordering::Relaxed);
        drop(guard);
        let pty = poll.join().unwrap();
        drop(pty);

        assert_eq!(
            bounded_result
                .expect("mechanical exit poll blocked on child coordination")
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(censuses, 0);
    }

    #[test]
    fn zero_duration_native_wait_reaps_without_attempting_session_settlement() {
        let _isolation = lock_real_pty_test();
        let host = Arc::new(ReprovableSettlementHost::new(true));
        let (mut pty, termination, cleanup) = reprovable_pty(host.clone(), i32::MAX);
        let original_session = pty.termination.lock_target().as_ref().unwrap().session;
        termination.request_termination().unwrap();
        let initial_censuses = host.censuses.load(Ordering::Relaxed);
        let exit = NativePtyAdapter::wait_for_exit(&mut pty, Duration::ZERO);
        let censuses_after_exit = host.censuses.load(Ordering::Relaxed);
        let proofs_after_exit = host.queries.load(Ordering::Relaxed);
        let retained_session = pty
            .termination
            .lock_target()
            .as_ref()
            .and_then(|target| target.session);
        pty.termination.forced.store(true, Ordering::Release);
        let retained_exit = NativePtyAdapter::wait_for_exit(&mut pty, Duration::ZERO);
        let reaps = cleanup.try_wait.load(Ordering::Relaxed);
        drop(pty);

        assert_eq!(exit, Ok(NativePtyExit::GracefulShutdown));
        assert_eq!(retained_exit, Ok(NativePtyExit::ForcedShutdown));
        assert_eq!(censuses_after_exit, initial_censuses);
        assert_eq!(proofs_after_exit, 0);
        assert_eq!(retained_session, original_session);
        assert_eq!(reaps, 1);
    }

    #[test]
    fn settlement_reproves_inconclusive_cleanup_with_original_session_after_adapter_drop() {
        let _isolation = lock_real_pty_test();
        for outcome in [
            NativePtySettlement::Unavailable,
            NativePtySettlement::StillRunning,
        ] {
            let mut host = ReprovableSettlementHost::new(false);
            host.inconclusive_members = outcome == NativePtySettlement::StillRunning;
            let host = Arc::new(host);
            let (pty, termination, cleanup) = reprovable_pty(host.clone(), i32::MAX);
            // Adapter cleanup makes the initial inconclusive attempt, then leaves only the Arc.
            drop(pty);
            assert_eq!(termination.settle(), outcome);
            let inconclusive_queries = host.queries.load(Ordering::Relaxed);
            host.conclusive.store(true, Ordering::Release);

            assert_eq!(termination.settle(), NativePtySettlement::Settled);
            let conclusive_queries = host.queries.load(Ordering::Relaxed);
            assert!(conclusive_queries > inconclusive_queries);
            assert!(host.member_checks.lock().unwrap().iter().all(|check| {
                *check
                    == (
                        ProcessIdentity {
                            process: i32::MAX,
                            start: ProcessStart { coarse: 1, fine: 2 },
                        },
                        Some(7),
                    )
            }));
            host.conclusive.store(false, Ordering::Release);
            assert_eq!(termination.settle(), NativePtySettlement::Settled);
            assert_eq!(host.queries.load(Ordering::Relaxed), conclusive_queries);
            assert_eq!(cleanup.try_wait.load(Ordering::Relaxed), 1);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn settlement_serializes_foreground_cleanup_and_later_reproof_across_sessions() {
        let _isolation = lock_real_pty_test();
        let (entered, observed) = mpsc::channel();
        let mut retry_host = ReprovableSettlementHost::new(false);
        retry_host.census_entered = Some(entered.clone());
        let retry_host = Arc::new(retry_host);
        let (retry_pty, retry, _) = reprovable_pty(retry_host.clone(), i32::MAX - 2);
        drop(retry_pty);
        retry_host.conclusive.store(true, Ordering::Release);
        while observed.try_recv().is_ok() {}
        let control = BlockingCallControl::default();
        let mut first_host = ReprovableSettlementHost::new(true);
        first_host.blocked_census = Some(control.clone());
        let (first_pty, first, _) = reprovable_pty(Arc::new(first_host), i32::MAX);
        let first_cleanup = thread::spawn(move || drop(first_pty));
        control.wait_until_entered("foreground cleanup census");
        let mut second_host = ReprovableSettlementHost::new(true);
        second_host.census_entered = Some(entered.clone());
        let (second_pty, second, _) = reprovable_pty(Arc::new(second_host), i32::MAX - 1);
        let (started, starting) = mpsc::channel();
        let second_started = started.clone();
        let second_cleanup = thread::spawn(move || {
            second_started.send(()).unwrap();
            drop(second_pty);
        });
        let retry = Arc::clone(&retry);
        let retry_cleanup = thread::spawn(move || {
            started.send(()).unwrap();
            retry.settle()
        });
        starting.recv_timeout(Duration::from_secs(1)).unwrap();
        starting.recv_timeout(Duration::from_secs(1)).unwrap();
        let overlapped = observed.recv_timeout(Duration::from_millis(100)).is_ok();
        control.release();
        first_cleanup.join().unwrap();
        second_cleanup.join().unwrap();
        let retry_result = retry_cleanup.join().unwrap();

        assert!(
            !overlapped,
            "another Session entered while foreground cleanup held its attempt"
        );
        assert_eq!(first.settle(), NativePtySettlement::Settled);
        assert_eq!(second.settle(), NativePtySettlement::Settled);
        assert_eq!(retry_result, NativePtySettlement::Settled);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn settlement_adapter_cleanup_does_not_wait_indefinitely_for_same_authority() {
        let _isolation = lock_real_pty_test();
        let control = BlockingCallControl::default();
        let mut host = ReprovableSettlementHost::new(true);
        host.blocked_census = Some(control.clone());
        let (pty, termination, _) = reprovable_pty(Arc::new(host), i32::MAX);
        let proving = Arc::clone(&termination);
        let proof = thread::spawn(move || proving.settle());
        control.wait_until_entered("same-authority proof census");
        let (finished, completion) = mpsc::channel();
        let cleanup = thread::spawn(move || {
            drop(pty);
            finished.send(()).unwrap();
        });
        let bounded_completion = completion.recv_timeout(Duration::from_secs(2));
        control.release();
        proof.join().unwrap();
        cleanup.join().unwrap();

        assert!(
            bounded_completion.is_ok(),
            "adapter cleanup waited outside the settlement deadline"
        );
        assert_eq!(termination.settle(), NativePtySettlement::Settled);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn settlement_gate_contention_still_forces_a_hup_resistant_live_session() {
        const NAME: &str = "pty::unix_pty::tests::settlement_gate_contention_still_forces_a_hup_resistant_live_session";
        if isolate_real_pty_test(NAME) {
            return;
        }
        let _isolation = lock_real_pty_test();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "trap '' HUP TERM; printf READY; exec sleep 30"]);
        let (mut pty, terminator) =
            spawn_command_in_pty(test_host(), PtySize::default(), command).unwrap();
        let mut reader = pty.take_reader().unwrap();
        read_output_marker(&mut reader, b"READY");
        let original_session = pty.termination.lock_target().as_ref().unwrap().session;
        terminator.terminate().unwrap();

        let control = BlockingCallControl::default();
        let mut gate_host = ReprovableSettlementHost::new(true);
        gate_host.blocked_census = Some(control.clone());
        let (gate_pty, gate_termination, _) = reprovable_pty(Arc::new(gate_host), i32::MAX);
        let gate_holder = thread::spawn(move || gate_termination.settle());
        control.wait_until_entered("cross-session proof gate");
        let started = Instant::now();
        let receipt = terminator.settle();
        let elapsed = started.elapsed();
        let retained_session = pty
            .termination
            .lock_target()
            .as_ref()
            .and_then(|target| target.session);
        let forced = pty.termination.forced.load(Ordering::Acquire);
        let deadline = Instant::now() + Duration::from_secs(1);
        let exited_while_contended = loop {
            if pty.try_wait_for_child().unwrap().is_some() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(CHILD_EXIT_POLL_INTERVAL);
        };
        control.release();
        gate_holder.join().unwrap();
        drop(gate_pty);
        // Clean up the exact retained fixture even on RED, before asserting the observed exit.
        if !exited_while_contended {
            pty.termination
                .lock_target()
                .as_mut()
                .unwrap()
                .force()
                .unwrap();
        }
        drop(reader);
        drop(pty);

        assert_eq!(receipt, NativePtySettlement::Unavailable);
        assert_eq!(retained_session, original_session);
        assert!(forced);
        assert!(elapsed < Duration::from_secs(2));
        assert!(
            exited_while_contended,
            "proof-gate contention prevented forced native retirement"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn settlement_contention_is_bounded_and_does_not_cache_unavailable() {
        let _isolation = lock_real_pty_test();
        let control = BlockingCallControl::default();
        let mut first_host = ReprovableSettlementHost::new(true);
        first_host.blocked_census = Some(control.clone());
        let (first_pty, first, _) = reprovable_pty(Arc::new(first_host), i32::MAX);
        let first_cleanup = thread::spawn(move || drop(first_pty));
        control.wait_until_entered("held foreground cleanup attempt");
        let second_host = Arc::new(ReprovableSettlementHost::new(true));
        let (second_pty, second, _) = reprovable_pty(second_host.clone(), i32::MAX - 1);
        let joining = Arc::clone(&second);
        let (result, receipt) = mpsc::channel();
        let started = Instant::now();
        let second_cleanup = thread::spawn(move || result.send(joining.settle()).unwrap());
        let bounded_result = receipt.recv_timeout(Duration::from_secs(2));
        control.release();
        first_cleanup.join().unwrap();
        second_cleanup.join().unwrap();

        assert_eq!(bounded_result.unwrap(), NativePtySettlement::Unavailable);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(second_host.queries.load(Ordering::Relaxed), 0);
        assert_eq!(second.settle(), NativePtySettlement::Settled);
        assert_eq!(first.settle(), NativePtySettlement::Settled);
        drop(second_pty);
    }

    #[test]
    fn settlement_receipts_cache_conclusive_outcomes_and_reap_the_leader_once() {
        let _isolation = lock_real_pty_test();
        {
            let outcome = NativePtySettlement::Settled;
            let leader = ProcessIdentity {
                process: i32::MAX,
                start: ProcessStart { coarse: 1, fine: 0 },
            };
            let cleanup = CleanupCounts::default();
            let child: Box<dyn Child + Send + Sync> = Box::new(ExitedChild {
                cleanup: cleanup.clone(),
            });
            let fallback = child.clone_killer();
            let child = Arc::new(Mutex::new(ChildState {
                child: Some(child),
                exit: None,
            }));
            let termination = Arc::new(PtyTerminator::new(Arc::new(
                ChildTermination::new_with_session(
                    Arc::new(SettlementHost { outcome, leader }),
                    leader.process,
                    ProcessSessionIdentity {
                        session: leader.process,
                        leader,
                        birth: None,
                    },
                    fallback,
                    child,
                ),
            )));
            let joining = Arc::clone(&termination);
            let join = thread::spawn(move || joining.settle());
            assert_eq!(termination.settle(), outcome);
            assert_eq!(join.join().unwrap(), outcome);
            assert_eq!(termination.settle(), outcome);
            assert_eq!(cleanup.try_wait.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn native_process_session_receipt_reaps_and_settles() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "pty::unix_pty::tests::native_process_session_receipt_reaps_and_settles",
        ) {
            return;
        }
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo ready; exec sleep 30"]);
        let (mut pty, terminator) =
            spawn_command_in_pty(test_host(), PtySize::default(), command).unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        let mut ready = String::new();
        reader.read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), "ready");

        assert_eq!(terminator.settle(), NativePtySettlement::Settled);
        assert_eq!(terminator.settle(), NativePtySettlement::Settled);
        assert!(pty.wait_for_child(Duration::ZERO).is_ok());
    }

    #[test]
    fn stubborn_process_group_should_receive_bounded_forced_shutdown() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "pty::unix_pty::tests::stubborn_process_group_should_receive_bounded_forced_shutdown",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "trap '' HUP; (trap '' HUP; exec </dev/null >/dev/null 2>&1; while :; do sleep 1; done) & child=$!; echo SPACETERM_SHUTDOWN_REPORT leader=$$ child=$child; exec </dev/null >/dev/null 2>&1; while :; do sleep 1; done",
        ]);
        command.current_dir(&working_directory);
        let (mut pty, terminator) = spawn_command_in_pty(
            test_host(),
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
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
        pty.cleanup_child();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= GRACEFUL_SHUTDOWN_TIMEOUT,
            "forced shutdown skipped the grace window: {elapsed:?}"
        );
        assert!(
            elapsed <= GRACEFUL_SHUTDOWN_TIMEOUT + FORCED_SHUTDOWN_TIMEOUT + Duration::from_secs(1),
            "forced shutdown exceeded its bound: {elapsed:?}"
        );
        assert!(pty.termination.forced.load(Ordering::Acquire));
        assert_process_disappears(leader);
        assert_process_disappears(child);
    }

    #[test]
    fn interactive_foreground_process_group_should_be_forced_down_with_its_shell() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "pty::unix_pty::tests::interactive_foreground_process_group_should_be_forced_down_with_its_shell",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let mut command = Command::new("/bin/zsh");
        command.args(["-f", "-i"]);
        command.current_dir(&working_directory);
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
        pty.cleanup_child();
        assert!(pty.termination.forced.load(Ordering::Acquire));
        drop(pty);
        drain.join().unwrap().unwrap();

        assert_process_disappears(leader);
        assert_process_disappears(child);
    }

    #[test]
    fn interactive_background_process_group_should_be_forced_down_with_its_shell() {
        let _isolation = lock_real_pty_test();
        if isolate_real_pty_test(
            "pty::unix_pty::tests::interactive_background_process_group_should_be_forced_down_with_its_shell",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let readiness = env::temp_dir().join(format!(
            "spaceterm-background-pty-ready-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&readiness);
        let mut command = Command::new("/bin/zsh");
        command.args(["-f", "-i"]);
        command.current_dir(&working_directory);
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
            .termination
            .lock_target()
            .as_ref()
            .and_then(|target| target.process_group)
            .unwrap();
        assert_ne!(leader, shell_process_group);
        let session = pty
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
        pty.cleanup_child();
        assert!(pty.termination.forced.load(Ordering::Acquire));
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
            "pty::unix_pty::tests::hup_handler_process_handoff_should_be_forced_down_with_its_shell",
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
        let mut command = Command::new("/bin/zsh");
        command.args(["-f", "-i"]);
        command.current_dir(&working_directory);
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

        pty.cleanup_child();
        assert!(pty.termination.forced.load(Ordering::Acquire));
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
            "pty::unix_pty::tests::close_after_session_leader_exit_should_terminate_surviving_group",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "trap '' HUP TERM; sleep 30 </dev/null >/dev/null 2>&1 & child=$!; echo SPACETERM_LATE_CLOSE leader=$$ child=$child",
        ]);
        command.current_dir(&working_directory);
        let (mut pty, terminator) = spawn_command_in_pty(
            test_host(),
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
        )
        .unwrap();
        let mut reader = BufReader::new(pty.take_reader().unwrap());
        let (leader, child) = read_process_report(&mut reader, "SPACETERM_LATE_CLOSE");
        drop(reader);
        let session = pty
            .termination
            .lock_target()
            .as_ref()
            .and_then(|target| target.session)
            .unwrap();

        let exit = pty.wait_for_child(Duration::from_secs(1)).unwrap();
        assert_eq!(exit.shutdown, ShutdownDisposition::NotRequested);
        let child_identity = test_host().observe_process(child).unwrap().identity;

        terminator.terminate().unwrap();
        pty.cleanup_child();
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
            "pty::unix_pty::tests::responsive_process_group_should_finish_during_the_grace_window",
        ) {
            return;
        }
        let working_directory = env::current_dir().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "sleep 30 & child=$!; echo SPACETERM_SHUTDOWN_REPORT leader=$$ child=$child; wait",
        ]);
        command.current_dir(&working_directory);
        let (mut pty, terminator) = spawn_command_in_pty(
            test_host(),
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 640,
                pixel_height: 480,
            },
            command,
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
        pty.cleanup_child();

        assert!(started.elapsed() < GRACEFUL_SHUTDOWN_TIMEOUT);
        assert!(!pty.termination.forced.load(Ordering::Acquire));
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
    struct TestMasterPty;

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
        let fallback = child.clone_killer();
        let child = Arc::new(Mutex::new(ChildState {
            child: Some(child),
            exit: None,
        }));
        let termination = Arc::new(ChildTermination::new(
            test_host(),
            None,
            fallback,
            Arc::clone(&child),
        ));
        let terminator = PtyTerminator::new(Arc::clone(&termination));
        (
            SpawnedPty {
                master: Box::new(TestMasterPty),
                reader: Some(Box::new(io::empty())),
                writer: Box::new(io::sink()),
                child,
                termination,
            },
            terminator,
        )
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

        pty.wait_for_child(Duration::from_secs(1)).unwrap();
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

        let error = pty.wait_for_child(Duration::ZERO).unwrap_err();

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
            pty.termination
                .complete_process_group(Instant::now())
                .unwrap(),
            ShutdownDisposition::Graceful,
        );
        assert!(pty.termination.lock_target().is_none());
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
            let result = pty.wait_for_child(Duration::from_secs(1));
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
