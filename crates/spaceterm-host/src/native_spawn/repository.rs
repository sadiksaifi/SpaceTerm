use std::{
    fs::File,
    io::{self, Read},
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

use super::{
    client::{self, CredentialFreeSpawner, POLL_INTERVAL, RemoteProcess, RequestBudget},
    protocol::{Failure, Program, ProgramKind, Retirement},
};
use crate::{
    git::{CancellationToken, ProgramError, ProgramExit, ProgramRequest, RepositoryProgramRunner},
    startup_exchange::unix::anonymous_pipe,
};

const DRAIN: Duration = Duration::from_millis(200);
const READ_CHUNK: usize = 64 * 1024;
const READS_PER_POLL: usize = 16;

/// Runs repository programs through the server's existing credential-free peer. Cancellation,
/// deadlines, output limits, process groups and post-exit drainage match the ordinary runner.
#[derive(Clone)]
pub struct CredentialFreeRepositoryProgramRunner {
    spawner: CredentialFreeSpawner,
}
impl CredentialFreeRepositoryProgramRunner {
    /// Reuse the same pre-secret connection selected for PTYs.
    pub fn new(spawner: CredentialFreeSpawner) -> Self {
        Self { spawner }
    }
}

impl RepositoryProgramRunner for CredentialFreeRepositoryProgramRunner {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &CancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        check_abort(request.deadline, cancellation)?;
        let program = Program {
            kind: ProgramKind::Tool,
            executable: request.executable.as_os_str().to_owned(),
            arguments: request.arguments.clone(),
            environment: request.environment.clone(),
        };
        let directory = client::directory(&request.directory).map_err(program_error)?;
        let (read, write) = anonymous_pipe().map_err(|_| ProgramError::Failed)?;
        let mut reader = File::from(read);
        client::set_nonblocking(&reader).map_err(|_| ProgramError::Failed)?;
        let budget = RequestBudget {
            deadline: request.deadline,
            cancellation: Some(cancellation),
        };
        check_abort(request.deadline, cancellation)?;
        let process = self
            .spawner
            .spawn(program, directory, write, budget)
            .map_err(program_error)?;
        let retirement = if request.keep_process_group_on_exit {
            Retirement::KeepGroup
        } else {
            Retirement::Normal
        };
        let result = read_program(
            &process,
            &mut reader,
            request,
            stdout,
            cancellation,
            retirement,
        );
        match result {
            Ok(exit) => Ok(exit),
            Err(error) => match process.retire(Retirement::Kill, RequestBudget::default()) {
                Ok(_) => Err(error),
                Err(_) => Err(ProgramError::Failed),
            },
        }
    }
}

fn program_error(failure: Failure) -> ProgramError {
    match failure {
        Failure::NotFound => ProgramError::NotFound,
        Failure::TimedOut => ProgramError::TimedOut,
        Failure::Cancelled => ProgramError::Cancelled,
        _ => ProgramError::Failed,
    }
}
fn check_abort(
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<(), ProgramError> {
    if cancellation.is_cancelled() {
        return Err(ProgramError::Cancelled);
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(ProgramError::TimedOut);
    }
    Ok(())
}

fn read_program(
    process: &RemoteProcess,
    pipe: &mut File,
    request: &ProgramRequest,
    stdout: &mut dyn FnMut(&[u8]),
    cancellation: &CancellationToken,
    retirement: Retirement,
) -> Result<ProgramExit, ProgramError> {
    let budget = RequestBudget {
        deadline: request.deadline,
        cancellation: Some(cancellation),
    };
    let mut open = true;
    let mut received = 0usize;
    let mut buffer = vec![0u8; READ_CHUNK];
    loop {
        check_abort(request.deadline, cancellation)?;
        let wait = remaining(request.deadline);
        if open {
            wait_readable(pipe, wait);
            read_available(
                pipe,
                &mut open,
                &mut received,
                &mut buffer,
                request.stdout_limit,
                stdout,
            )?;
        } else {
            // POLLHUP stays readable after EOF. Waiting on that pipe would busy-loop while
            // the program continues, flooding the shared peer with status requests.
            std::thread::sleep(wait);
        }
        let exited = process.has_exited(budget).map_err(program_error)?;
        // A reply is an observation, not permission to ignore an expired caller budget.
        check_abort(request.deadline, cancellation)?;
        if exited {
            break;
        }
    }
    if !request.keep_process_group_on_exit {
        process.signal_kill(budget).map_err(program_error)?;
    }
    let drain_deadline = Instant::now() + DRAIN;
    let drain_deadline = request
        .deadline
        .map_or(drain_deadline, |deadline| deadline.min(drain_deadline));
    while open && Instant::now() < drain_deadline {
        check_abort(request.deadline, cancellation)?;
        wait_readable(pipe, remaining(Some(drain_deadline)));
        read_available(
            pipe,
            &mut open,
            &mut received,
            &mut buffer,
            request.stdout_limit,
            stdout,
        )?;
    }
    check_abort(request.deadline, cancellation)?;
    let exit = process.retire(retirement, budget).map_err(program_error)?;
    if !request.keep_process_group_on_exit {
        check_abort(request.deadline, cancellation)?;
    }
    // Kept-group completion is selected at ReleaseGroup admission, after a final abort check.
    // After that decision succeeds, later cancellation cannot turn a released daemon into an
    // aborted run whose group this caller no longer owns.
    Ok(ProgramExit {
        code: exit.status.code(),
    })
}

fn read_available(
    pipe: &mut File,
    open: &mut bool,
    received: &mut usize,
    buffer: &mut [u8],
    limit: Option<usize>,
    stdout: &mut dyn FnMut(&[u8]),
) -> Result<(), ProgramError> {
    if !*open {
        return Ok(());
    }
    for _ in 0..READS_PER_POLL {
        match pipe.read(buffer) {
            Ok(0) => {
                *open = false;
                break;
            }
            Ok(count) => {
                *received = received.saturating_add(count);
                if limit.is_some_and(|limit| *received > limit) {
                    return Err(ProgramError::OutputTooLarge);
                }
                stdout(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(ProgramError::Failed),
        }
    }
    Ok(())
}
fn remaining(deadline: Option<Instant>) -> Duration {
    deadline.map_or(POLL_INTERVAL, |deadline| {
        deadline
            .saturating_duration_since(Instant::now())
            .min(POLL_INTERVAL)
    })
}
fn wait_readable(pipe: &File, timeout: Duration) {
    let mut descriptor = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe {
        libc::poll(
            &mut descriptor,
            1,
            timeout.as_millis().min(i32::MAX as u128) as i32,
        );
    }
}
