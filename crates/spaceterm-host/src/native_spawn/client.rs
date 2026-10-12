use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::{fs::OpenOptionsExt, net::UnixStream, process::ExitStatusExt},
    },
    path::Path,
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    time::{Duration, Instant},
};

use super::protocol::{self, Failure, Program, ProgramKind, Reply, Request, Result, Retirement};
use crate::{
    git::CancellationToken,
    native_deployment::contracts::NativeFailure,
    shell_launch::PreparedShellLaunch,
    startup_exchange::{ExchangeDeadline, unix::ExactChild},
};

#[path = "transport.rs"]
mod transport;

const CHANNEL_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Bounds caller admission and status waits. Admitted child-owning transactions finish under
/// a separate native I/O deadline so their ownership is never lost.
#[derive(Clone, Copy, Default)]
pub(super) struct RequestBudget<'a> {
    pub deadline: Option<Instant>,
    pub cancellation: Option<&'a CancellationToken>,
}
impl RequestBudget<'_> {
    fn for_request(self) -> Self {
        let deadline = Instant::now() + CHANNEL_TIMEOUT;
        Self {
            deadline: Some(
                self.deadline
                    .map_or(deadline, |caller| caller.min(deadline)),
            ),
            ..self
        }
    }
    pub(super) fn check(self) -> Result<()> {
        if self
            .cancellation
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(Failure::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(Failure::TimedOut);
        }
        Ok(())
    }
    fn remaining(self) -> Duration {
        self.deadline.map_or(POLL_INTERVAL, |deadline| {
            deadline.saturating_duration_since(Instant::now())
        })
    }
    fn pause(self) -> Result<()> {
        self.check()?;
        std::thread::sleep(POLL_INTERVAL.min(self.remaining()));
        self.check()
    }
    fn lock<T>(self, mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
        loop {
            self.check()?;
            match mutex.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::Poisoned(_)) => return Err(Failure::Unavailable),
                Err(TryLockError::WouldBlock) => self.pause()?,
            }
        }
    }
}

struct CheckedChannel<'a, 'b> {
    channel: &'a mut UnixStream,
    budget: RequestBudget<'b>,
}
impl AsRawFd for CheckedChannel<'_, '_> {
    fn as_raw_fd(&self) -> i32 {
        self.channel.as_raw_fd()
    }
}
impl Write for CheckedChannel<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            self.budget.check().map_err(Failure::io)?;
            match self.channel.write(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.channel.flush()
    }
}

/// One connection shared by every server PTY and tool. It never forks or executes a program.
#[derive(Clone)]
pub struct CredentialFreeSpawner {
    inner: Arc<Connection>,
}
struct Connection {
    transport: transport::Transport,
    pid: u32,
    child: Mutex<SpawnerChild>,
    // Capture ordinary launch defaults at startup, not from the secret server on each launch.
    environment: BTreeMap<OsString, OsString>,
}

enum SpawnerChild {
    Owned(ExactChild),
    Reaped(std::result::Result<(), NativeFailure>),
}

impl CredentialFreeSpawner {
    /// Claim the launcher's anonymous channel and exclusive child owner before receiving
    /// credentials. The launcher authenticates the executable, drops privileges and execs the
    /// spawner before secrets arrive. This connection cannot recreate that native authority.
    pub fn connect(
        mut channel: UnixStream,
        child: ExactChild,
    ) -> std::result::Result<Self, NativeFailure> {
        if !child.has_wait_reservation() {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let pid = child.identity().pid;
        let connection = Self {
            inner: Arc::new(Connection {
                transport: transport::Transport::empty(),
                pid,
                child: Mutex::new(SpawnerChild::Owned(child)),
                environment: std::env::vars_os().collect(),
            }),
        };
        let result = (|| {
            protocol::configure(&channel).map_err(|_| NativeFailure::NativeUnavailable)?;
            channel
                .set_read_timeout(Some(CHANNEL_TIMEOUT))
                .and_then(|()| channel.set_write_timeout(Some(CHANNEL_TIMEOUT)))
                .map_err(|_| NativeFailure::NativeUnavailable)?;
            let mut hello = [0u8; 16];
            channel
                .read_exact(&mut hello)
                .map_err(|_| NativeFailure::NativeUnavailable)?;
            let observed_pid = u32::from_be_bytes(
                hello[4..8]
                    .try_into()
                    .map_err(|_| NativeFailure::InvalidRecord)?,
            );
            let uid = u32::from_be_bytes(
                hello[8..12]
                    .try_into()
                    .map_err(|_| NativeFailure::InvalidRecord)?,
            );
            let gid = u32::from_be_bytes(
                hello[12..16]
                    .try_into()
                    .map_err(|_| NativeFailure::InvalidRecord)?,
            );
            // This handshake is peer routing, never retained execution or credential authority.
            if &hello[..4] != protocol::HELLO_MAGIC
                || observed_pid != pid
                || uid == 0
                || gid == 0
                || uid != unsafe { libc::getuid() }
                || gid != unsafe { libc::getgid() }
            {
                return Err(NativeFailure::ExecutionBindingUnproven);
            }
            Ok(())
        })();
        if let Err(failure) = result {
            drop(channel);
            return match connection.shutdown() {
                Ok(()) => Err(failure),
                Err(cleanup) => Err(cleanup),
            };
        }
        if connection.inner.transport.start(channel).is_err() {
            return match connection.shutdown() {
                Ok(()) => Err(NativeFailure::NativeUnavailable),
                Err(cleanup) => Err(cleanup),
            };
        }
        Ok(connection)
    }

    /// Close admission to every clone, retire the peer's owned account programs and reap the exact
    /// spawner. Server shutdown calls this after draining Git writes and Terminal owners.
    pub fn shutdown(&self) -> std::result::Result<(), NativeFailure> {
        let transport = self.inner.transport.shutdown();
        let mut child = self
            .inner
            .child
            .lock()
            .map_err(|_| NativeFailure::CleanupUnresolved)?;
        child.reap()?;
        transport.map_err(|_| NativeFailure::CleanupUnresolved)
    }

    /// Native peer identifier for bounded lifecycle observations. It grants no signal authority.
    pub fn process_id(&self) -> u32 {
        self.inner.pid
    }

    pub(super) fn shell_program(&self, launch: &PreparedShellLaunch) -> Program {
        let defaults = portable_pty::CommandBuilder::new(launch.executable());
        let mut environment = BTreeMap::new();
        if launch.inherit_environment() {
            for (name, value) in &self.inner.environment {
                if !launch.denies_inherited_environment(name) {
                    environment.insert(name.clone(), value.clone());
                }
            }
        }
        for (name, value) in launch.environment() {
            environment.insert(name.clone(), value.clone());
        }
        environment
            .entry("SHELL".into())
            .or_insert_with(|| defaults.get_shell().into());
        Program {
            kind: ProgramKind::Pty,
            executable: launch.executable().to_owned(),
            arguments: launch.arguments().to_vec(),
            environment: environment.into_iter().collect(),
        }
    }

    pub(super) fn spawn(
        &self,
        program: Program,
        directory: OwnedFd,
        stream: OwnedFd,
        budget: RequestBudget<'_>,
    ) -> Result<RemoteProcess> {
        match self.request(Request::Spawn(program), &[directory, stream], budget)? {
            Reply::Spawned { handle, pid } if pid != 0 => Ok(RemoteProcess {
                inner: Arc::new(Process {
                    connection: self.clone(),
                    handle,
                    result: Mutex::new(None),
                }),
            }),
            _ => Err(Failure::Invalid),
        }
    }

    fn request(
        &self,
        request: Request,
        descriptors: &[OwnedFd],
        budget: RequestBudget<'_>,
    ) -> Result<Reply> {
        self.inner.transport.request(request, descriptors, budget)
    }
}

impl SpawnerChild {
    fn reap(&mut self) -> std::result::Result<(), NativeFailure> {
        match self {
            Self::Reaped(result) => *result,
            Self::Owned(child) => {
                let result = child.reap(ExchangeDeadline::start(Duration::from_secs(2))?);
                match result {
                    Ok(success) => {
                        let outcome = if success {
                            Ok(())
                        } else {
                            Err(NativeFailure::CleanupUnresolved)
                        };
                        *self = Self::Reaped(outcome);
                        outcome
                    }
                    Err(_) => Err(NativeFailure::CleanupUnresolved),
                }
            }
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.transport.shutdown();
        if let Ok(child) = self.child.get_mut() {
            let _ = child.reap();
        }
    }
}

pub(super) fn directory(path: &Path) -> Result<OwnedFd> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(path)
        .map(OwnedFd::from)
        .map_err(|_| Failure::Unavailable)
}

#[derive(Clone, Copy)]
pub(super) struct ProcessExit {
    pub status: std::process::ExitStatus,
    pub forced: bool,
}

#[derive(Clone)]
pub(super) struct RemoteProcess {
    inner: Arc<Process>,
}
struct Process {
    connection: CredentialFreeSpawner,
    handle: u64,
    result: Mutex<Option<ProcessExit>>,
}
impl RemoteProcess {
    pub(super) fn has_exited(&self, budget: RequestBudget<'_>) -> Result<bool> {
        let result = budget.lock(&self.inner.result)?;
        if result.is_some() {
            return Ok(true);
        }
        match self
            .inner
            .connection
            .request(Request::Poll(self.inner.handle), &[], budget)?
        {
            Reply::Poll(exited) => Ok(exited),
            _ => Err(Failure::Invalid),
        }
    }
    pub(super) fn signal_kill(&self, budget: RequestBudget<'_>) -> Result<()> {
        let result = budget.lock(&self.inner.result)?;
        if result.is_some() {
            return Ok(());
        }
        match self
            .inner
            .connection
            .request(Request::KillGroup(self.inner.handle), &[], budget)?
        {
            Reply::Poll(_) => Ok(()),
            _ => Err(Failure::Invalid),
        }
    }
    fn poll(&self, budget: RequestBudget<'_>) -> Result<Option<ProcessExit>> {
        let mut result = budget.lock(&self.inner.result)?;
        if result.is_some() {
            return Ok(*result);
        }
        match self
            .inner
            .connection
            .request(Request::Poll(self.inner.handle), &[], budget)?
        {
            Reply::Poll(false) => Ok(None),
            Reply::Poll(true) => {
                let status = self.inner.retire_step(Retirement::Normal, budget)?;
                *result = status;
                Ok(status)
            }
            _ => Err(Failure::Invalid),
        }
    }
    pub(super) fn retire(
        &self,
        retirement: Retirement,
        budget: RequestBudget<'_>,
    ) -> Result<ProcessExit> {
        let budget = budget.for_request();
        loop {
            {
                let mut result = budget.lock(&self.inner.result)?;
                if let Some(status) = *result {
                    return Ok(status);
                }
                if let Some(status) = self.inner.retire_step(retirement, budget)? {
                    *result = Some(status);
                    return Ok(status);
                }
            }
            // Neither the connection nor this process's result lock spans the grace period.
            budget.pause()?;
        }
    }
    pub(super) fn wait(&self, timeout: Duration) -> io::Result<ProcessExit> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Failure::Invalid.io())?;
        let budget = RequestBudget {
            deadline: Some(deadline),
            cancellation: None,
        };
        loop {
            let status = self.poll(budget).map_err(Failure::io)?;
            budget.check().map_err(Failure::io)?;
            if let Some(status) = status {
                return Ok(status);
            }
            budget.pause().map_err(Failure::io)?;
        }
    }
}
impl Process {
    fn retire_step(
        &self,
        mode: Retirement,
        budget: RequestBudget<'_>,
    ) -> Result<Option<ProcessExit>> {
        let mut reply = self
            .connection
            .request(Request::Retire(self.handle, mode), &[], budget)?;
        if matches!(reply, Reply::GroupRetained) {
            // The peer still owns the unreaped leader and group. Selecting successful completion
            // is the ReleaseGroup admission check; an abort before that point still retires Kill.
            reply = self
                .connection
                .request(Request::ReleaseGroup(self.handle), &[], budget)?;
        }
        match reply {
            Reply::Retired { status, forced } => Ok(Some(ProcessExit {
                status: std::process::ExitStatus::from_raw(status),
                forced,
            })),
            Reply::Retiring => Ok(None),
            _ => Err(Failure::Invalid),
        }
    }
    fn retire_on_drop(&self) -> Result<()> {
        let budget = RequestBudget::default().for_request();
        while self.retire_step(Retirement::Kill, budget)?.is_none() {
            budget.pause()?;
        }
        Ok(())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.result.get_mut().is_ok_and(|result| result.is_none()) {
            let _ = self.retire_on_drop();
        }
    }
}

pub(super) fn set_nonblocking(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::other("native stream configuration failed"));
    }
    Ok(())
}
