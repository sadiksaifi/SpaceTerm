//! Private broker exchange. Native custody owns executable identity through exec and reaping.

use super::{SudoBoundary, SudoInvocation, protected_execution::ManagementBroker, reply};
use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::{
    controller::ManagementProgress,
    request::{ManagementRequest, REQUEST_BYTES},
};
use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

// This bounds the unprivileged broker wait, including native authentication and execution.
const BROKER_WAIT: Duration = Duration::from_secs(300);

/// Native retention and final exec binding for the fixed system sudo and registered manager.
/// The implementation retains dependencies, policy configuration, protected ancestors,
/// descriptor-bound mount evidence, and publication authority excluding writable aliases.
/// It authenticates the destination before exec and keeps approved copies bound through exec.
/// A path/hash comparison followed by an unbound path spawn cannot implement this port.
pub trait RetainedSudoExecution: Send + Sync {
    /// Native proof/lock/descriptor scope retained until the exact broker is reaped.
    type Helper: Send + Sync + 'static;
    /// Resolves only the registered protected manager and fixed system broker.
    fn retain(&self, request: &ManagementRequest) -> Result<Self::Helper, NativeFailure>;
    /// Revalidates the exact retained closure and publication policy, not decoded facts.
    fn recheck(&self, helper: &mut Self::Helper) -> Result<(), NativeFailure>;
    /// Constructs only the fixed sudo command with the closed invocation's flags and
    /// admitted manager. It installs the native descriptor/exec binding before returning.
    /// No application passwords, AskPass, caller argv, or client-selected path enter this port.
    fn prepare(
        &self,
        helper: &mut Self::Helper,
        invocation: SudoInvocation,
    ) -> Result<Command, NativeFailure>;
}

/// Concrete sudo exchange over native retained execution. It owns transport and cleanup.
pub struct NativeSudoBoundary<C> {
    custody: C,
}
impl<C: RetainedSudoExecution> NativeSudoBoundary<C> {
    /// Injects native retained execution, not a command, path, or readiness assertion.
    pub fn new(custody: C) -> Self {
        Self { custody }
    }
}
impl<C: RetainedSudoExecution> SudoBoundary for NativeSudoBoundary<C> {
    type RetainedHelper = C::Helper;
    fn retain_fixed_helper(
        &self,
        request: &ManagementRequest,
    ) -> Result<Self::RetainedHelper, NativeFailure> {
        self.custody.retain(request)
    }
    fn recheck_execution_binding(
        &self,
        helper: &mut Self::RetainedHelper,
    ) -> Result<(), NativeFailure> {
        self.custody.recheck(helper)
    }
    fn invoke(
        &self,
        mut helper: Self::RetainedHelper,
        invocation: SudoInvocation,
        bytes: &[u8; REQUEST_BYTES],
    ) -> Result<ManagementProgress, NativeFailure> {
        let command = self.custody.prepare(&mut helper, invocation)?;
        invoke_broker(command, helper, bytes, ManagementBroker::Sudo, |helper| {
            self.custody.recheck(helper)
        })
    }
}

pub(crate) fn invoke_broker<H: Send + Sync + 'static>(
    mut command: Command,
    mut helper: H,
    bytes: &[u8; REQUEST_BYTES],
    broker: ManagementBroker,
    recheck: impl FnOnce(&mut H) -> Result<(), NativeFailure>,
) -> Result<ManagementProgress, NativeFailure> {
    // No inherited or prepared injection, AskPass, agent, signing, or credential value survives.
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C");
    let (mut channel, stdin) = crate::native_management::stdin::broker_request_channel()?;
    command
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    recheck(&mut helper)?;
    // The native exec binding installed by prepare remains active during this spawn.
    let child = command
        .spawn()
        .map_err(|_| NativeFailure::AuthorizationUnavailable)?;
    // Command owns its configured stdin even after spawn. Close that unpublished
    // receiver copy now, or a helper refusal cannot deliver EOF on the native channel.
    drop(command);
    let mut running = RunningBroker::new(child, helper);
    let mut observed = ExchangeObservation::default();
    let result = exchange(running.child_mut()?, &mut channel, bytes, &mut observed);
    // No result, including malformed output, releases custody while the broker remains live.
    match running.reap() {
        Ok(status) => {
            if broker == ManagementBroker::Pkexec
                && status.code() == Some(126)
                && !observed.manager_started
                && !observed.response_seen
            {
                // The retained fixed pkexec documents 126 for dismissed authentication,
                // before exec. It cannot supply successful authority or hide a helper reply.
                return Err(NativeFailure::AuthorizationCancelled);
            }
            if !observed.manager_started
                && !observed.response_seen
                && !status.success()
                && matches!(
                    result,
                    Err(NativeFailure::InvalidRecord | NativeFailure::IoUnavailable)
                )
            {
                // A broker can close with an unread request, producing EOF or ECONNRESET.
                // Denial, a missing agent and exec failure can share an exit status. Neither
                // an empty reply nor that pre-manager reset is a malformed manager request.
                return Err(NativeFailure::AuthorizationUnavailable);
            }
            let progress = result?;
            match progress {
                Ok(progress) if status.success() => Ok(progress),
                Ok(_) => Err(NativeFailure::AuthorizationUnavailable),
                Err(failure) if status.code() == Some(failure as i32) => Err(failure),
                Err(_) => Err(NativeFailure::AuthorizationUnavailable),
            }
        }
        Err(failure) => Err(failure),
    }
}

struct RunningBroker<H: Send + Sync + 'static> {
    child: Option<Child>,
    custody: Option<H>,
    reaped: bool,
}
impl<H: Send + Sync + 'static> RunningBroker<H> {
    fn new(child: Child, helper: H) -> Self {
        Self {
            child: Some(child),
            custody: Some(helper),
            reaped: false,
        }
    }
    fn child_mut(&mut self) -> Result<&mut Child, NativeFailure> {
        self.child.as_mut().ok_or(NativeFailure::CleanupUnresolved)
    }
    fn reap(&mut self) -> Result<std::process::ExitStatus, NativeFailure> {
        let expires = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = self
                .child_mut()?
                .try_wait()
                .map_err(|_| NativeFailure::CleanupUnresolved)?
            {
                self.reaped = true;
                return Ok(status);
            }
            if Instant::now() >= expires {
                return Err(NativeFailure::CleanupUnresolved);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl<H: Send + Sync + 'static> Drop for RunningBroker<H> {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        // A live unreaped child cannot have a recycled PID. This targets only our broker,
        // never a server/runtime, and has no SIGKILL or process-name fallback.
        // SAFETY: kill receives the PID of this retained unreaped child and no pointer.
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
        // The exact Child and custody move together. No reopened PID supplies cleanup proof.
        if let Some(custody) = self.custody.take() {
            let retained = std::sync::Arc::new(custody);
            let worker_custody = std::sync::Arc::clone(&retained);
            let reaper = std::thread::Builder::new()
                .name("native-broker-reap".into())
                .spawn(move || {
                    if child.wait().is_err() {
                        // Exact reaping is unproven. Keep custody until process exit.
                        std::mem::forget(worker_custody);
                    }
                });
            if reaper.is_err() {
                // A bounded CleanupUnresolved cannot release a still-live execution scope.
                std::mem::forget(retained);
            }
        }
    }
}

#[derive(Default)]
struct ExchangeObservation {
    manager_started: bool,
    response_seen: bool,
}

fn exchange(
    child: &mut Child,
    channel: &mut std::os::unix::net::UnixStream,
    bytes: &[u8; REQUEST_BYTES],
    observed: &mut ExchangeObservation,
) -> Result<Result<ManagementProgress, NativeFailure>, NativeFailure> {
    let expires = Instant::now() + BROKER_WAIT;
    channel
        .set_write_timeout(Some(BROKER_WAIT))
        .map_err(|_| NativeFailure::IoUnavailable)?;
    channel
        .write_all(bytes)
        .map_err(|_| NativeFailure::IoUnavailable)?;
    let mut challenge = zeroize::Zeroizing::new([0u8; 32]);
    let mut used = 0;
    while used < challenge.len() {
        let remaining = expires
            .checked_duration_since(Instant::now())
            .ok_or(NativeFailure::TimedOut)?;
        channel
            .set_read_timeout(Some(remaining))
            .map_err(|_| NativeFailure::IoUnavailable)?;
        match channel.read(&mut challenge[used..]) {
            Ok(0) if used == 0 => break, // A source-gated helper may refuse before the handshake.
            Ok(0) => return Err(NativeFailure::InvalidRecord),
            Ok(count) => {
                observed.manager_started = true;
                used += count;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(NativeFailure::TimedOut);
            }
            Err(_) => return Err(NativeFailure::IoUnavailable),
        }
    }
    if used == challenge.len() {
        let remaining = expires
            .checked_duration_since(Instant::now())
            .ok_or(NativeFailure::TimedOut)?;
        channel
            .set_write_timeout(Some(remaining))
            .map_err(|_| NativeFailure::IoUnavailable)?;
        channel
            .write_all(&challenge[..])
            .map_err(|_| NativeFailure::IoUnavailable)?;
        channel
            .shutdown(std::net::Shutdown::Write)
            .map_err(|_| NativeFailure::IoUnavailable)?;
    }
    let caller_bound = used == challenge.len();
    let mut stdout = child.stdout.take().ok_or(NativeFailure::IoUnavailable)?;
    let fd = stdout.as_raw_fd();
    // SAFETY: this owned pipe is exclusively read here; fcntl retains no pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    let mut reply = [0; reply::REPLY_LIMIT + 1];
    let mut used = 0;
    loop {
        match stdout.read(&mut reply[used..]) {
            Ok(0) => {
                let result = reply::decode(&reply[..used])?;
                if result.is_ok() && !caller_bound {
                    return Err(NativeFailure::AuthorizationUnavailable);
                }
                return Ok(result);
            }
            Ok(count) => {
                observed.response_seen = true;
                used += count;
                if used > reply::REPLY_LIMIT {
                    return Err(NativeFailure::LimitExceeded);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let remaining = expires
                    .checked_duration_since(Instant::now())
                    .ok_or(NativeFailure::TimedOut)?;
                let mut descriptor = libc::pollfd {
                    fd,
                    events: libc::POLLIN | libc::POLLHUP,
                    revents: 0,
                };
                let timeout = remaining.as_millis().min(100) as i32;
                // SAFETY: poll receives one initialized live pipe descriptor and no escaping pointers.
                let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
                if result < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    return Err(NativeFailure::IoUnavailable);
                }
            }
            Err(_) => return Err(NativeFailure::IoUnavailable),
        }
        if Instant::now() >= expires {
            return Err(NativeFailure::TimedOut);
        }
    }
}
