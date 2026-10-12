//! Runs Repository Status programs in a private process group on the POSIX hosts.
//!
//! Stdout is read without blocking on the calling thread: the pipe is nonblocking and `poll`
//! waits for data with a short timeout, so cancellation, the deadline, and the child's exit are
//! checked between reads. A background process the child leaves behind can keep the pipe open
//! after the child exits, so reading continues past the exit only until EOF or a short drain
//! deadline. The child's own output is complete by then, because everything it wrote before
//! exiting is already in the pipe.
//!
//! The exit is observed with `waitid(WNOWAIT)`, which leaves the child unreaped. The zombie keeps
//! the process group ID reserved, so the group signal that follows cannot reach a reused ID.

use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use super::CancellationToken;
use crate::git::{ProgramError, ProgramExit, ProgramRequest, RepositoryProgramRunner};

/// The longest wait for stdout before cancellation, the deadline, and exit are checked again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// How long stdout is still read after the child exits while another process holds the pipe.
const POST_EXIT_DRAIN: Duration = Duration::from_millis(200);
const READ_CHUNK_BYTES: usize = 64 * 1024;
/// Reads per poll, so an endless writer cannot starve the cancellation and deadline checks.
const READS_PER_POLL: usize = 16;
/// Every signal number either host defines. Undefined numbers fail harmlessly.
const HIGHEST_SIGNAL: libc::c_int = 64;

/// Spawns each program directly with `fork` and `exec`; it holds no state between runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnixRepositoryProgramRunner;

impl UnixRepositoryProgramRunner {
    /// Construct the stateless native program runner.
    pub const fn new() -> Self {
        Self
    }
}

impl RepositoryProgramRunner for UnixRepositoryProgramRunner {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &CancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        if cancellation.is_cancelled() {
            return Err(ProgramError::Cancelled);
        }
        if request
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ProgramError::TimedOut);
        }
        // A missing directory and a missing program both fail `exec` with ENOENT, so the directory
        // is checked first to keep NotFound about the program.
        if !request.directory.is_dir() {
            return Err(ProgramError::Failed);
        }
        let mut program = ProgramGroup::spawn(request)?;
        let pipe = program.child.stdout.take().ok_or(ProgramError::Failed)?;
        set_nonblocking(&pipe)?;
        let mut reader = StdoutReader {
            pipe,
            open: true,
            received: 0,
            limit: request.stdout_limit,
            buffer: vec![0; READ_CHUNK_BYTES],
        };

        loop {
            program.check_abort(request.deadline, cancellation)?;
            let wait = remaining(POLL_INTERVAL, request.deadline);
            if reader.open {
                wait_readable(&reader.pipe, wait);
                program.on_failure(reader.read_available(stdout))?;
            } else {
                std::thread::sleep(wait);
            }
            if program.has_exited()? {
                break;
            }
        }

        if !request.keep_process_group_on_exit {
            program.signal_group();
        }
        let drain_deadline = Instant::now() + POST_EXIT_DRAIN;
        let drain_deadline = request
            .deadline
            .map_or(drain_deadline, |deadline| deadline.min(drain_deadline));
        while reader.open && Instant::now() < drain_deadline {
            if cancellation.is_cancelled() {
                return Err(program.abort(ProgramError::Cancelled));
            }
            wait_readable(&reader.pipe, remaining(POLL_INTERVAL, Some(drain_deadline)));
            program.on_failure(reader.read_available(stdout))?;
        }
        program.reap()
    }
}

/// A spawned program whose group is killed and whose leader is reaped if it is dropped early.
struct ProgramGroup {
    child: Child,
    group: libc::pid_t,
    reaped: bool,
}

impl ProgramGroup {
    fn spawn(request: &ProgramRequest) -> Result<Self, ProgramError> {
        let mut command = Command::new(&request.executable);
        command
            .args(&request.arguments)
            .current_dir(&request.directory)
            .env_clear()
            .envs(
                request
                    .environment
                    .iter()
                    .map(|(name, value)| (name, value)),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        // SAFETY: the callback runs after fork and performs only async-signal-safe signal
        // operations before exec. It does not access shared application state.
        unsafe {
            command.pre_exec(reset_child_signals);
        }
        let mut child = command.spawn().map_err(|error| match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => ProgramError::NotFound,
            _ => ProgramError::Failed,
        })?;
        match libc::pid_t::try_from(child.id()) {
            Ok(group) => Ok(Self {
                child,
                group,
                reaped: false,
            }),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(ProgramError::Failed)
            }
        }
    }

    fn check_abort(
        &mut self,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
    ) -> Result<(), ProgramError> {
        if cancellation.is_cancelled() {
            return Err(self.abort(ProgramError::Cancelled));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(self.abort(ProgramError::TimedOut));
        }
        Ok(())
    }

    fn on_failure(&mut self, result: Result<(), ProgramError>) -> Result<(), ProgramError> {
        result.map_err(|error| self.abort(error))
    }

    /// Ends the whole group, reaps the leader, and returns `error`.
    fn abort(&mut self, error: ProgramError) -> ProgramError {
        self.signal_group();
        let _ = self.child.wait();
        self.reaped = true;
        error
    }

    fn signal_group(&self) {
        // SAFETY: the group belongs to the unreaped child launched above with a private group, so
        // its ID cannot have been reused. An empty group reports ESRCH, which needs no handling.
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
    }

    /// Reports whether the leader exited, leaving it unreaped.
    fn has_exited(&mut self) -> Result<bool, ProgramError> {
        let mut information = MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: the information pointer is valid for writes, and WNOWAIT leaves the child for
        // the later reap.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.group as libc::id_t,
                information.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(self.abort(ProgramError::Failed));
        }
        // SAFETY: the structure was zeroed and waitid succeeded; WNOHANG leaves the process ID zero
        // when the child has not changed state.
        Ok(unsafe { information.assume_init().si_pid() } != 0)
    }

    fn reap(&mut self) -> Result<ProgramExit, ProgramError> {
        let status = self.child.wait();
        self.reaped = true;
        status
            .map(|status| ProgramExit {
                code: status.code(),
            })
            .map_err(|_| ProgramError::Failed)
    }
}

impl Drop for ProgramGroup {
    fn drop(&mut self) {
        if !self.reaped {
            self.signal_group();
            let _ = self.child.wait();
        }
    }
}

struct StdoutReader {
    pipe: ChildStdout,
    open: bool,
    received: usize,
    limit: Option<usize>,
    buffer: Vec<u8>,
}

impl StdoutReader {
    /// Delivers what the pipe holds now, up to a bounded number of reads.
    fn read_available(&mut self, stdout: &mut dyn FnMut(&[u8])) -> Result<(), ProgramError> {
        for _ in 0..READS_PER_POLL {
            match self.pipe.read(&mut self.buffer) {
                Ok(0) => {
                    self.open = false;
                    return Ok(());
                }
                Ok(count) => {
                    self.received = self.received.saturating_add(count);
                    if self.limit.is_some_and(|limit| self.received > limit) {
                        return Err(ProgramError::OutputTooLarge);
                    }
                    stdout(&self.buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(ProgramError::Failed),
            }
        }
        Ok(())
    }
}

fn remaining(interval: Duration, deadline: Option<Instant>) -> Duration {
    deadline.map_or(interval, |deadline| {
        interval.min(deadline.saturating_duration_since(Instant::now()))
    })
}

fn set_nonblocking(pipe: &ChildStdout) -> Result<(), ProgramError> {
    let descriptor = pipe.as_raw_fd();
    // SAFETY: the descriptor is the live stdout pipe owned by `pipe`.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    // SAFETY: as above; only the status flags change.
    if flags == -1
        || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(ProgramError::Failed);
    }
    Ok(())
}

/// Waits until the pipe is readable or closed, or `timeout` passes. Errors fall through to the read.
fn wait_readable(pipe: &ChildStdout, timeout: Duration) {
    let mut descriptor = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let milliseconds = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: the descriptor array has one valid entry for the live pipe.
    unsafe {
        libc::poll(&mut descriptor, 1, milliseconds);
    }
}

/// Gives the program default signal dispositions and an empty signal mask.
///
/// `exec` already resets caught signals, but ignored signals and the mask are inherited from the
/// spawning thread.
fn reset_child_signals() -> io::Result<()> {
    for signal in 1..=HIGHEST_SIGNAL {
        if signal == libc::SIGKILL || signal == libc::SIGSTOP {
            continue;
        }
        // SAFETY: an all-zero sigaction is a valid default action with an empty mask.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = libc::SIG_DFL;
        // SAFETY: the action is initialized; the previous action is not needed. Signal numbers
        // the host does not define fail with EINVAL, which is ignored.
        unsafe {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
    let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: sigemptyset initializes the provided signal set.
    if unsafe { libc::sigemptyset(empty.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: sigemptyset initialized the signal set after succeeding above.
    let empty = unsafe { empty.assume_init() };
    // SAFETY: empty is initialized and the previous mask is not needed in the child process.
    if unsafe { libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
