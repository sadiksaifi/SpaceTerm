use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use thiserror::Error;

use super::command::SshCommandSpec;
use super::startup_environment::StartupSshEnvironment;
use crate::cancellation::CancellationToken;
use crate::ssh::askpass::{AskPassBrokerLease, AskPassCapabilityCopy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Content-free result from a supervised SSH process.
pub(crate) struct ProcessExit {
    success: bool,
    code: Option<i32>,
}

impl ProcessExit {
    pub(crate) const fn new(success: bool, code: Option<i32>) -> Self {
        Self { success, code }
    }

    #[cfg(test)]
    pub(crate) const fn successful() -> Self {
        Self::new(true, Some(0))
    }

    pub(crate) const fn is_success(self) -> bool {
        self.success
    }

    pub(crate) const fn code(self) -> Option<i32> {
        self.code
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessSignal {
    #[cfg(all(test, feature = "native-tests"))]
    Terminate,
    Kill,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
/// Content-free failure from the narrow Operating-System process mechanism.
pub(crate) enum SshProcessMechanismError {
    #[error("the selected OpenSSH executable was not found")]
    NotFound,
    #[error("the SSH process could not be launched")]
    LaunchFailed,
    #[error("the SSH process pipes were unavailable")]
    PipesUnavailable,
    #[error("the SSH process status could not be collected")]
    StatusFailed,
    #[error("the SSH process group could not be signalled")]
    SignalFailed,
    #[error("the SSH process could not be reaped")]
    ReapFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SshProcessStdio {
    Null,
    Piped,
}

/// Complete portable process request consumed by an Operating-System process adapter.
///
/// The value deliberately has no `Debug` implementation because it contains command and
/// environment values that must never enter diagnostics.
pub(crate) struct SshProcessSpawnRequest {
    executable: PathBuf,
    arguments: Vec<OsString>,
    current_directory: PathBuf,
    environment: Vec<(OsString, OsString)>,
    askpass_capability: Option<AskPassCapabilityCopy>,
    stdin: SshProcessStdio,
    stdout: SshProcessStdio,
    stderr: SshProcessStdio,
}

impl SshProcessSpawnRequest {
    pub(crate) fn new(
        executable: PathBuf,
        arguments: Vec<OsString>,
        current_directory: PathBuf,
        environment: Vec<(OsString, OsString)>,
        stdin: SshProcessStdio,
        stdout: SshProcessStdio,
        stderr: SshProcessStdio,
    ) -> Self {
        Self {
            executable,
            arguments,
            current_directory,
            environment,
            askpass_capability: None,
            stdin,
            stdout,
            stderr,
        }
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    pub(crate) fn current_directory(&self) -> &Path {
        &self.current_directory
    }

    pub(crate) fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    pub(crate) fn with_askpass_capability(
        mut self,
        capability: Option<AskPassCapabilityCopy>,
    ) -> Self {
        self.askpass_capability = capability;
        self
    }

    pub(crate) fn askpass_capability_environment(&self) -> Option<(&OsStr, &[u8])> {
        self.askpass_capability
            .as_ref()
            .map(|capability| (capability.environment_name(), capability.as_bytes()))
    }

    pub(crate) const fn stdin(&self) -> SshProcessStdio {
        self.stdin
    }

    pub(crate) const fn stdout(&self) -> SshProcessStdio {
        self.stdout
    }

    pub(crate) const fn stderr(&self) -> SshProcessStdio {
        self.stderr
    }
}

pub(crate) struct SshProcessPipes {
    stdin: Option<Box<dyn Write + Send>>,
    stdout: Option<Box<dyn Read + Send>>,
    stderr: Option<Box<dyn Read + Send>>,
}

impl SshProcessPipes {
    pub(crate) fn new(
        stdin: Option<Box<dyn Write + Send>>,
        stdout: Option<Box<dyn Read + Send>>,
        stderr: Option<Box<dyn Read + Send>>,
    ) -> Self {
        Self {
            stdin,
            stdout,
            stderr,
        }
    }
}

pub(crate) struct SpawnedSshProcess<P> {
    process: P,
    pipes: SshProcessPipes,
}

impl<P> SpawnedSshProcess<P> {
    pub(crate) const fn new(process: P, pipes: SshProcessPipes) -> Self {
        Self { process, pipes }
    }

    #[cfg(all(test, feature = "native-tests"))]
    pub(crate) fn process_mut(&mut self) -> &mut P {
        &mut self.process
    }

    #[cfg(all(test, feature = "native-tests"))]
    pub(crate) fn into_process(self) -> P {
        self.process
    }
}

/// Narrow boundary for irreducible Operating-System process mechanics.
///
/// Implementations keep process identities, process-group identifiers, signals, native command
/// extensions, and raw Operating-System failures private.
pub(crate) trait SshProcessAdapter: Clone + Send + Sync + 'static {
    type Process: Send + 'static;

    fn spawn(
        &self,
        request: SshProcessSpawnRequest,
    ) -> Result<SpawnedSshProcess<Self::Process>, SshProcessMechanismError>;

    fn try_status(
        &self,
        process: &mut Self::Process,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError>;

    fn signal(
        &self,
        process: &mut Self::Process,
        signal: ProcessSignal,
    ) -> Result<(), SshProcessMechanismError>;

    fn reap(&self, process: Self::Process) -> Result<(), SshProcessMechanismError>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SshProcessCounts {
    pub spawned: usize,
    pub reaped: usize,
}
impl SshProcessCounts {
    pub(crate) fn active(self) -> usize {
        self.spawned.saturating_sub(self.reaped)
    }
}

/// Counts owned process admission and completed reaping without recording process metadata.
#[derive(Clone)]
pub(crate) struct TrackedSshProcessAdapter<A: SshProcessAdapter> {
    inner: A,
    counts: Arc<std::sync::Mutex<SshProcessCounts>>,
}
impl<A: SshProcessAdapter> TrackedSshProcessAdapter<A> {
    pub(crate) fn new(inner: A) -> Self {
        Self {
            inner,
            counts: Arc::default(),
        }
    }
    pub(crate) fn counts(&self) -> SshProcessCounts {
        *self
            .counts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}
impl<A: SshProcessAdapter> SshProcessAdapter for TrackedSshProcessAdapter<A> {
    type Process = A::Process;
    fn spawn(
        &self,
        request: SshProcessSpawnRequest,
    ) -> Result<SpawnedSshProcess<Self::Process>, SshProcessMechanismError> {
        let spawned = self.inner.spawn(request)?;
        self.counts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .spawned += 1;
        Ok(spawned)
    }
    fn try_status(
        &self,
        process: &mut Self::Process,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
        self.inner.try_status(process)
    }
    fn signal(
        &self,
        process: &mut Self::Process,
        signal: ProcessSignal,
    ) -> Result<(), SshProcessMechanismError> {
        self.inner.signal(process, signal)
    }
    fn reap(&self, process: Self::Process) -> Result<(), SshProcessMechanismError> {
        self.inner.reap(process)?;
        self.counts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .reaped += 1;
        Ok(())
    }
}

#[derive(Debug, Error)]
/// Failure to construct a safe, explicitly captured SSH process environment.
pub(crate) enum SshProcessEnvironmentError {
    #[error("the captured local HOME must be an absolute control-free path")]
    UnsafeHome,
    #[error("the captured SSH agent socket must be an absolute control-free path")]
    UnsafeAgentSocket,
}

#[derive(Clone)]
/// Captured environment for authenticated SSH commands.
///
/// Applying this value clears ambient variables, installs only fixed `HOME`, the explicitly
/// allowlisted capture-once startup environment, and the fixed AskPass transport overlay. The
/// retained AskPass lease keeps its private broker alive for the process lifetime.
pub(crate) struct SshProcessEnvironment {
    home: PathBuf,
    authentication: Option<AskPassBrokerLease>,
    startup: StartupSshEnvironment,
}

#[derive(Clone)]
/// Captured environment for the helper-free OpenSSH capability probe.
///
/// This environment clears ambient variables and deliberately excludes AskPass transport state.
pub(crate) struct SshProbeEnvironment {
    home: PathBuf,
    startup: StartupSshEnvironment,
}

impl SshProcessEnvironment {
    pub(crate) fn new(
        home: PathBuf,
        authentication: AskPassBrokerLease,
        startup: &StartupSshEnvironment,
    ) -> Result<Self, SshProcessEnvironmentError> {
        Self::validated(home, Some(authentication), startup.clone())
    }

    fn validated(
        home: PathBuf,
        authentication: Option<AskPassBrokerLease>,
        startup: StartupSshEnvironment,
    ) -> Result<Self, SshProcessEnvironmentError> {
        if !safe_absolute_path(&home) {
            return Err(SshProcessEnvironmentError::UnsafeHome);
        }
        if startup
            .agent_socket()
            .is_some_and(|socket| socket.is_empty() || !safe_absolute_path(Path::new(socket)))
        {
            return Err(SshProcessEnvironmentError::UnsafeAgentSocket);
        }
        Ok(Self {
            home,
            authentication,
            startup,
        })
    }

    pub(crate) fn finish_authentication(&self) {
        if let Some(lease) = &self.authentication {
            lease.finish();
        }
    }
    pub(crate) fn remote_authorization_command(
        &self,
    ) -> Result<SshCommandSpec, SshProcessMechanismError> {
        let lease = self
            .authentication
            .as_ref()
            .ok_or(SshProcessMechanismError::LaunchFailed)?;
        let helper = lease
            .ordinary_spawn_entries()
            .find(|(name, _)| *name == "SSH_ASKPASS")
            .map(|(_, value)| PathBuf::from(value))
            .ok_or(SshProcessMechanismError::NotFound)?;
        super::command::SshCommandContext::authorization_prompt(helper)
            .map_err(|_| SshProcessMechanismError::LaunchFailed)
    }

    #[cfg(test)]
    pub(crate) fn new_without_authentication(
        home: PathBuf,
        agent_socket: Option<OsString>,
    ) -> Result<Self, SshProcessEnvironmentError> {
        Self::validated(home, None, StartupSshEnvironment::for_test(agent_socket))
    }

    #[cfg(test)]
    pub(super) fn new_without_authentication_from_startup(
        home: PathBuf,
        startup: &StartupSshEnvironment,
    ) -> Result<Self, SshProcessEnvironmentError> {
        Self::validated(home, None, startup.clone())
    }

    fn launch_parts(
        &self,
    ) -> (
        PathBuf,
        Vec<(OsString, OsString)>,
        Option<AskPassCapabilityCopy>,
    ) {
        let mut entries = vec![(OsString::from("HOME"), self.home.as_os_str().to_owned())];
        entries.extend(
            self.startup
                .entries()
                .map(|(name, value)| (name.into(), value.to_owned())),
        );
        let askpass_capability = self.authentication.as_ref().map(|authentication| {
            entries.extend(
                authentication
                    .ordinary_spawn_entries()
                    .map(|(name, value)| (name.into(), value.to_owned())),
            );
            authentication.capability_copy_for_spawn()
        });
        (self.home.clone(), entries, askpass_capability)
    }
}

impl SshProbeEnvironment {
    pub(crate) fn new(
        home: PathBuf,
        startup: &StartupSshEnvironment,
    ) -> Result<Self, SshProcessEnvironmentError> {
        if !safe_absolute_path(&home) {
            return Err(SshProcessEnvironmentError::UnsafeHome);
        }
        if startup
            .agent_socket()
            .is_some_and(|socket| socket.is_empty() || !safe_absolute_path(Path::new(socket)))
        {
            return Err(SshProcessEnvironmentError::UnsafeAgentSocket);
        }
        Ok(Self {
            home,
            startup: startup.clone(),
        })
    }

    fn launch_parts(&self) -> (PathBuf, Vec<(OsString, OsString)>) {
        let mut entries = vec![(OsString::from("HOME"), self.home.as_os_str().to_owned())];
        entries.extend(
            self.startup
                .entries()
                .map(|(name, value)| (name.into(), value.to_owned())),
        );
        (self.home.clone(), entries)
    }
}

fn process_request(
    spec: &SshCommandSpec,
    (current_directory, environment, askpass_capability): (
        PathBuf,
        Vec<(OsString, OsString)>,
        Option<AskPassCapabilityCopy>,
    ),
    stdin: SshProcessStdio,
    stdout: SshProcessStdio,
    stderr: SshProcessStdio,
) -> SshProcessSpawnRequest {
    SshProcessSpawnRequest::new(
        spec.executable().into(),
        spec.arguments().to_vec(),
        current_directory,
        environment,
        stdin,
        stdout,
        stderr,
    )
    .with_askpass_capability(askpass_capability)
}

fn safe_absolute_path(path: &Path) -> bool {
    path.is_absolute() && !path.to_string_lossy().chars().any(char::is_control)
}

fn cleanup_now<A: SshProcessAdapter>(
    adapter: &A,
    mut process: A::Process,
    readers: Vec<JoinHandle<io::Result<Vec<u8>>>>,
) {
    let _ = adapter.signal(&mut process, ProcessSignal::Kill);
    let _ = adapter.reap(process);
    for reader in readers {
        let _ = reader.join();
    }
}

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Error)]
pub(crate) enum CapturedProcessError {
    #[error("the SSH process operation was cancelled")]
    Cancelled,
    #[error("the SSH process operation exceeded its deadline")]
    TimedOut,
    #[error("the SSH process output exceeded its safety limit")]
    OutputTooLarge,
    #[error("the SSH process operation failed")]
    Operation(#[source] SshProcessMechanismError),
}

pub(crate) struct CapturedProcessOutput {
    pub(crate) exit: ProcessExit,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

impl fmt::Debug for CapturedProcessOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CapturedProcessOutput(<redacted>)")
    }
}

pub(crate) fn run_captured_process<A: SshProcessAdapter>(
    adapter: &A,
    spec: &SshCommandSpec,
    environment: &SshProcessEnvironment,
    input: Vec<u8>,
    stdout_limit: usize,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Result<CapturedProcessOutput, CapturedProcessError> {
    run_captured_process_with_parts(
        adapter,
        spec,
        environment.launch_parts(),
        CapturedProcessPlan {
            admitted: None,
            input: Some(input),
            stdout_limit,
            stderr_limit: None,
            deadline: Some(deadline),
        },
        cancellation,
    )
}

#[derive(Debug, Error)]
pub(crate) enum CapturedEffectError {
    #[error("the SSH effect was not dispatched")]
    NotAdmitted(#[source] CapturedProcessError),
    #[error("the admitted SSH effect has an unconfirmed outcome")]
    Unconfirmed,
}

/// An effect distinguishes cancellation before dispatch from an uncertain admitted outcome.
pub(crate) fn run_captured_effect<A: SshProcessAdapter>(
    adapter: &A,
    spec: &SshCommandSpec,
    environment: &SshProcessEnvironment,
    input: Vec<u8>,
    stdout_limit: usize,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Result<CapturedProcessOutput, CapturedEffectError> {
    let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    run_captured_process_with_parts(
        adapter,
        spec,
        environment.launch_parts(),
        CapturedProcessPlan {
            admitted: Some(Arc::clone(&admitted)),
            input: Some(input),
            stdout_limit,
            stderr_limit: None,
            deadline: Some(deadline),
        },
        cancellation,
    )
    .map_err(|error| {
        if admitted.load(std::sync::atomic::Ordering::Acquire) {
            CapturedEffectError::Unconfirmed
        } else {
            CapturedEffectError::NotAdmitted(error)
        }
    })
}

/// A cancellable transfer has no arbitrary total download/authentication deadline.
/// Callers bound subsequent native observation phases independently.
pub(crate) fn run_captured_transfer<A: SshProcessAdapter>(
    adapter: &A,
    spec: &SshCommandSpec,
    environment: &SshProcessEnvironment,
    input: Vec<u8>,
    stdout_limit: usize,
    cancellation: &CancellationToken,
) -> Result<CapturedProcessOutput, CapturedProcessError> {
    run_captured_process_with_parts(
        adapter,
        spec,
        environment.launch_parts(),
        CapturedProcessPlan {
            admitted: None,
            input: Some(input),
            stdout_limit,
            stderr_limit: None,
            deadline: None,
        },
        cancellation,
    )
}

pub(crate) fn run_probe_process<A: SshProcessAdapter>(
    adapter: &A,
    spec: &SshCommandSpec,
    environment: &SshProbeEnvironment,
    stream_limit: usize,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Result<CapturedProcessOutput, CapturedProcessError> {
    let (current_directory, environment) = environment.launch_parts();
    run_captured_process_with_parts(
        adapter,
        spec,
        (current_directory, environment, None),
        CapturedProcessPlan {
            admitted: None,
            input: None,
            stdout_limit: stream_limit,
            stderr_limit: Some(stream_limit),
            deadline: Some(deadline),
        },
        cancellation,
    )
}

struct CapturedProcessPlan {
    admitted: Option<Arc<std::sync::atomic::AtomicBool>>,
    input: Option<Vec<u8>>,
    stdout_limit: usize,
    stderr_limit: Option<usize>,
    deadline: Option<Instant>,
}

fn run_captured_process_with_parts<A: SshProcessAdapter>(
    adapter: &A,
    spec: &SshCommandSpec,
    environment: (
        PathBuf,
        Vec<(OsString, OsString)>,
        Option<AskPassCapabilityCopy>,
    ),
    plan: CapturedProcessPlan,
    cancellation: &CancellationToken,
) -> Result<CapturedProcessOutput, CapturedProcessError> {
    let CapturedProcessPlan {
        admitted,
        input,
        stdout_limit,
        stderr_limit,
        deadline,
    } = plan;
    if cancellation.is_cancelled() {
        return Err(CapturedProcessError::Cancelled);
    }
    let stdin = if input.is_some() {
        SshProcessStdio::Piped
    } else {
        SshProcessStdio::Null
    };
    let stderr = if stderr_limit.is_some() {
        SshProcessStdio::Piped
    } else {
        SshProcessStdio::Null
    };
    let request = process_request(spec, environment, stdin, SshProcessStdio::Piped, stderr);
    let SpawnedSshProcess { process, mut pipes } = adapter
        .spawn(request)
        .map_err(CapturedProcessError::Operation)?;
    if let Some(admitted) = admitted {
        admitted.store(true, std::sync::atomic::Ordering::Release);
    }
    let mut ownership = CapturedProcessOwnership::new(adapter.clone(), process);
    let stdout = pipes.stdout.take().ok_or(CapturedProcessError::Operation(
        SshProcessMechanismError::PipesUnavailable,
    ))?;
    let writer = match input {
        Some(input) => {
            let input = zeroize::Zeroizing::new(input);
            let mut stdin = pipes.stdin.take().ok_or(CapturedProcessError::Operation(
                SshProcessMechanismError::PipesUnavailable,
            ))?;
            Some(
                std::thread::Builder::new()
                    .name("spaceterm-ssh-stdin".to_owned())
                    .spawn(move || {
                        stdin.write_all(&input)?;
                        stdin.flush()
                    })
                    .map_err(|_| {
                        CapturedProcessError::Operation(SshProcessMechanismError::LaunchFailed)
                    })?,
            )
        }
        None => None,
    };
    let mut stdout_reader = Some(spawn_bounded_reader(
        "spaceterm-ssh-stdout",
        stdout,
        stdout_limit,
    )?);
    let mut stderr_reader = match stderr_limit {
        Some(stderr_limit) => Some(spawn_bounded_reader(
            "spaceterm-ssh-stderr",
            pipes.stderr.take().ok_or(CapturedProcessError::Operation(
                SshProcessMechanismError::PipesUnavailable,
            ))?,
            stderr_limit,
        )?),
        None => None,
    };
    let mut captured_stdout = None;
    let mut captured_stderr = None;

    let exit = loop {
        if cancellation.is_cancelled() {
            ownership.cleanup_now();
            let _ = join_writer(writer);
            join_unfinished_reader(stdout_reader);
            join_unfinished_reader(stderr_reader);
            return Err(CapturedProcessError::Cancelled);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            ownership.cleanup_now();
            let _ = join_writer(writer);
            join_unfinished_reader(stdout_reader);
            join_unfinished_reader(stderr_reader);
            return Err(CapturedProcessError::TimedOut);
        }
        collect_finished_reader(&mut stdout_reader, &mut captured_stdout)?;
        collect_finished_reader(&mut stderr_reader, &mut captured_stderr)?;
        let process = ownership.process_mut()?;
        match adapter.try_status(process) {
            Ok(Some(exit)) => break exit,
            Ok(None) => std::thread::sleep(deadline.map_or(PROCESS_POLL_INTERVAL, |deadline| {
                PROCESS_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now()))
            })),
            Err(error) => {
                return Err(CapturedProcessError::Operation(error));
            }
        }
    };
    ownership.cleanup_now();
    if join_writer(writer).is_err() && exit.is_success() {
        return Err(CapturedProcessError::Operation(
            SshProcessMechanismError::PipesUnavailable,
        ));
    }
    let stdout = finish_reader(stdout_reader, captured_stdout)?;
    let stderr = match stderr_reader {
        Some(reader) => finish_reader(Some(reader), captured_stderr)?,
        None => captured_stderr.unwrap_or_default(),
    };
    Ok(CapturedProcessOutput {
        exit,
        stdout,
        stderr,
    })
}

struct CapturedProcessOwnership<A: SshProcessAdapter> {
    adapter: A,
    process: Option<A::Process>,
}

impl<A: SshProcessAdapter> CapturedProcessOwnership<A> {
    const fn new(adapter: A, process: A::Process) -> Self {
        Self {
            adapter,
            process: Some(process),
        }
    }

    fn process_mut(&mut self) -> Result<&mut A::Process, CapturedProcessError> {
        self.process.as_mut().ok_or(CapturedProcessError::Operation(
            SshProcessMechanismError::StatusFailed,
        ))
    }

    fn cleanup_now(&mut self) {
        if let Some(process) = self.process.take() {
            cleanup_now(&self.adapter, process, Vec::new());
        }
    }
}

impl<A: SshProcessAdapter> Drop for CapturedProcessOwnership<A> {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            cleanup_now(&self.adapter, process, Vec::new());
        }
    }
}

enum BoundedReadError {
    OutputTooLarge,
    Io,
}

type BoundedReader = JoinHandle<Result<Vec<u8>, BoundedReadError>>;

fn spawn_bounded_reader(
    name: &str,
    reader: Box<dyn Read + Send>,
    maximum_bytes: usize,
) -> Result<BoundedReader, CapturedProcessError> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || read_bounded(reader, maximum_bytes))
        .map_err(|_| CapturedProcessError::Operation(SshProcessMechanismError::LaunchFailed))
}

fn read_bounded(
    mut reader: Box<dyn Read + Send>,
    maximum_bytes: usize,
) -> Result<Vec<u8>, BoundedReadError> {
    let mut output = Vec::with_capacity(maximum_bytes.min(16 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|_| BoundedReadError::Io)?;
        if read == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(read) > maximum_bytes {
            return Err(BoundedReadError::OutputTooLarge);
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

fn collect_finished_reader(
    reader: &mut Option<BoundedReader>,
    captured: &mut Option<Vec<u8>>,
) -> Result<(), CapturedProcessError> {
    if reader.as_ref().is_some_and(JoinHandle::is_finished) {
        let finished = reader.take().ok_or(CapturedProcessError::Operation(
            SshProcessMechanismError::PipesUnavailable,
        ))?;
        *captured = Some(join_reader(finished)?);
    }
    Ok(())
}

fn finish_reader(
    reader: Option<BoundedReader>,
    captured: Option<Vec<u8>>,
) -> Result<Vec<u8>, CapturedProcessError> {
    match captured {
        Some(output) => Ok(output),
        None => join_reader(reader.ok_or(CapturedProcessError::Operation(
            SshProcessMechanismError::PipesUnavailable,
        ))?),
    }
}

fn join_reader(reader: BoundedReader) -> Result<Vec<u8>, CapturedProcessError> {
    match reader.join() {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(BoundedReadError::OutputTooLarge)) => Err(CapturedProcessError::OutputTooLarge),
        Ok(Err(BoundedReadError::Io)) | Err(_) => Err(CapturedProcessError::Operation(
            SshProcessMechanismError::PipesUnavailable,
        )),
    }
}

fn join_unfinished_reader(reader: Option<BoundedReader>) {
    if let Some(reader) = reader {
        let _ = reader.join();
    }
}

fn join_writer(writer: Option<JoinHandle<io::Result<()>>>) -> Result<(), ()> {
    match writer {
        Some(writer) => writer.join().map_err(|_| ())?.map_err(|_| ()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::ssh::askpass::AskPassLease;
    use crate::ssh::command::SshCommandSpec;
    use zeroize::Zeroizing;

    const ASKPASS_OVERLAY_NAMES: [&str; 6] = [
        "SSH_ASKPASS",
        "SSH_ASKPASS_REQUIRE",
        "DISPLAY",
        "SPACETERM_SSH_ASKPASS_MODE",
        "SPACETERM_SSH_ASKPASS_SOCKET",
        "SPACETERM_SSH_ASKPASS_CAPABILITY",
    ];

    struct TestAskPassLease {
        helper: OsString,
        endpoint: OsString,
        capability: Zeroizing<String>,
    }

    impl AskPassLease for TestAskPassLease {
        fn entries(&self) -> Vec<(&'static str, &OsStr)> {
            vec![
                ("SSH_ASKPASS", self.helper.as_os_str()),
                ("SSH_ASKPASS_REQUIRE", OsStr::new("force")),
                ("DISPLAY", OsStr::new("spaceterm-askpass")),
                ("SPACETERM_SSH_ASKPASS_MODE", OsStr::new("broker-v1")),
                ("SPACETERM_SSH_ASKPASS_SOCKET", self.endpoint.as_os_str()),
                (
                    "SPACETERM_SSH_ASKPASS_CAPABILITY",
                    OsStr::new(self.capability.as_str()),
                ),
            ]
        }

        fn cancel(&self) {}

        fn capability(&self) -> &[u8] {
            self.capability.as_bytes()
        }
    }

    fn authenticated_test_environment() -> SshProcessEnvironment {
        let authentication = AskPassBrokerLease::new(Arc::new(TestAskPassLease {
            helper: OsString::from("/test/helper"),
            endpoint: OsString::from("00000001:/test/socket"),
            capability: Zeroizing::new("test-only-capability".to_owned()),
        }));
        SshProcessEnvironment::new(
            PathBuf::from("/captured/home"),
            authentication,
            &StartupSshEnvironment::for_test(None),
        )
        .unwrap()
    }

    #[test]
    fn captured_launch_environment_uses_home_and_startup_entries() {
        let home = PathBuf::from("/captured/home");
        let agent_socket = OsString::from("/captured/agent.sock");
        let environment = SshProcessEnvironment::new_without_authentication(
            home.clone(),
            Some(agent_socket.clone()),
        )
        .unwrap();
        let (directory, entries, askpass_capability) = environment.launch_parts();
        let entries: std::collections::HashMap<_, _> = entries.into_iter().collect();

        assert_eq!(directory, home);
        assert!(askpass_capability.is_none());
        assert_eq!(
            entries.get(std::ffi::OsStr::new("HOME")),
            Some(&OsString::from("/captured/home"))
        );
        assert_eq!(
            entries.get(std::ffi::OsStr::new("PATH")),
            Some(&OsString::from("/fixture/bin"))
        );
        assert_eq!(
            entries.get(std::ffi::OsStr::new("SSH_AUTH_SOCK")),
            Some(&agent_socket)
        );
        assert!(!entries.contains_key(std::ffi::OsStr::new("SPACETERM_UNKNOWN")));
    }

    #[test]
    fn launch_environment_preserves_captured_path() {
        let home = PathBuf::from("/captured/home");
        let startup = StartupSshEnvironment::for_test_with_path(
            OsString::from("/captured/bin:/usr/bin:/bin"),
            None,
        );
        let environment =
            SshProcessEnvironment::new_without_authentication_from_startup(home.clone(), &startup)
                .unwrap();
        let (directory, entries, askpass_capability) = environment.launch_parts();

        assert_eq!(directory, home);
        assert!(askpass_capability.is_none());
        assert!(entries.contains(&(
            OsString::from("PATH"),
            OsString::from("/captured/bin:/usr/bin:/bin")
        )));
    }

    #[test]
    fn authenticated_process_launch_separates_only_the_capability_from_the_six_entry_overlay() {
        let (directory, entries, capability) = authenticated_test_environment().launch_parts();
        let capability = capability.unwrap();
        let mut overlay_names: Vec<_> = entries
            .iter()
            .map(|(name, _)| name.as_os_str())
            .filter(|name| {
                ASKPASS_OVERLAY_NAMES
                    .iter()
                    .any(|candidate| name == candidate)
            })
            .collect();
        overlay_names.push(capability.environment_name());

        assert_eq!(directory, Path::new("/captured/home"));
        assert_eq!(
            overlay_names,
            ASKPASS_OVERLAY_NAMES.map(OsStr::new).as_slice()
        );
        assert_eq!(capability.as_bytes(), b"test-only-capability");
        assert!(
            entries
                .iter()
                .all(|(name, _)| name != "SPACETERM_SSH_ASKPASS_CAPABILITY")
        );
    }

    #[test]
    fn launch_environment_should_reject_a_relative_home() {
        let error =
            SshProcessEnvironment::new_without_authentication(PathBuf::from("relative-home"), None)
                .err();

        assert!(matches!(
            error,
            Some(SshProcessEnvironmentError::UnsafeHome)
        ));
    }

    use super::testing::*;
    fn test_spec() -> SshCommandSpec {
        SshCommandSpec::for_test(PathBuf::from("/selected/ssh"), vec!["-V".into()])
    }

    fn test_environment() -> SshProcessEnvironment {
        SshProcessEnvironment::new_without_authentication(PathBuf::from("/captured/home"), None)
            .unwrap()
    }

    #[test]
    fn effect_cancellation_preserves_whether_dispatch_was_admitted() {
        let adapter = RecordingAdapter::default();
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        let result = run_captured_effect(
            &adapter,
            &test_spec(),
            &test_environment(),
            Vec::new(),
            16,
            &cancelled,
            Instant::now() + Duration::from_secs(1),
        );
        assert!(matches!(
            result,
            Err(CapturedEffectError::NotAdmitted(
                CapturedProcessError::Cancelled
            ))
        ));
        assert_eq!(adapter.state.lock().unwrap().spawns, 0);
        let cancellation = CancellationToken::default();
        adapter.state.lock().unwrap().cancel_on_status = Some(cancellation.clone());
        let result = run_captured_effect(
            &adapter,
            &test_spec(),
            &test_environment(),
            Vec::new(),
            16,
            &cancellation,
            Instant::now() + Duration::from_secs(1),
        );
        assert!(
            matches!(result, Err(CapturedEffectError::Unconfirmed)),
            "an admitted effect may have run despite cancellation"
        );
        let state = adapter.state.lock().unwrap();
        assert_eq!(state.spawns, 1);
        assert_eq!(state.reaps, 1);
    }

    #[test]
    fn recording_adapter_should_observe_success_and_exactly_once_cleanup() {
        let adapter = RecordingAdapter::default();
        {
            let mut state = adapter.state.lock().unwrap();
            state.stdout = b"ready".to_vec();
            state.statuses.push_back(Some(ProcessExit::successful()));
        }

        let output = run_captured_process(
            &adapter,
            &test_spec(),
            &test_environment(),
            Vec::new(),
            16,
            &CancellationToken::default(),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        let state = adapter.state.lock().unwrap();

        assert!(
            output.exit.is_success()
                && output.stdout == b"ready"
                && state.spawns == 1
                && state.signals == [ProcessSignal::Kill]
                && state.reaps == 1
        );
    }

    #[test]
    fn recording_adapter_should_cleanup_once_when_deadline_expires() {
        let adapter = RecordingAdapter::default();
        let error = run_captured_process(
            &adapter,
            &test_spec(),
            &test_environment(),
            Vec::new(),
            16,
            &CancellationToken::default(),
            Instant::now(),
        )
        .unwrap_err();
        let state = adapter.state.lock().unwrap();

        assert!(
            matches!(error, CapturedProcessError::TimedOut)
                && state.signals == [ProcessSignal::Kill]
                && state.reaps == 1
        );
    }

    #[test]
    fn recording_adapter_should_cleanup_once_when_cancellation_arrives_after_spawn() {
        let adapter = RecordingAdapter::default();
        let cancellation = CancellationToken::default();
        adapter.state.lock().unwrap().cancel_on_status = Some(cancellation.clone());

        let error = run_captured_process(
            &adapter,
            &test_spec(),
            &test_environment(),
            Vec::new(),
            16,
            &cancellation,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err();
        let state = adapter.state.lock().unwrap();

        assert!(
            matches!(error, CapturedProcessError::Cancelled)
                && state.spawns == 1
                && state.signals == [ProcessSignal::Kill]
                && state.reaps == 1
        );
    }

    #[test]
    fn recording_adapter_should_cleanup_oversized_output_and_delayed_status() {
        let adapter = RecordingAdapter::default();
        {
            let mut state = adapter.state.lock().unwrap();
            state.stdout = vec![b'x'; 17];
            state
                .statuses
                .extend([None, Some(ProcessExit::successful())]);
        }

        let error = run_captured_process(
            &adapter,
            &test_spec(),
            &test_environment(),
            Vec::new(),
            16,
            &CancellationToken::default(),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err();
        for _ in 0..100 {
            if adapter.state.lock().unwrap().reaps == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let state = adapter.state.lock().unwrap();

        assert!(
            matches!(error, CapturedProcessError::OutputTooLarge)
                && state.signals == [ProcessSignal::Kill]
                && state.reaps == 1
        );
    }

    #[test]
    fn recording_adapter_should_return_closed_spawn_failure_without_cleanup() {
        let adapter = RecordingAdapter::default();
        adapter.state.lock().unwrap().spawn_error = Some(SshProcessMechanismError::NotFound);

        let error = run_captured_process(
            &adapter,
            &test_spec(),
            &test_environment(),
            Vec::new(),
            16,
            &CancellationToken::default(),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err();
        let state = adapter.state.lock().unwrap();

        assert!(
            matches!(
                error,
                CapturedProcessError::Operation(SshProcessMechanismError::NotFound)
            ) && state.spawns == 1
                && state.signals.is_empty()
                && state.reaps == 0
        );
    }

    #[test]
    fn pre_cancelled_operation_should_not_spawn_or_cleanup() {
        let adapter = RecordingAdapter::default();
        let cancellation = CancellationToken::default();
        cancellation.cancel();

        let error = run_captured_process(
            &adapter,
            &test_spec(),
            &test_environment(),
            Vec::new(),
            16,
            &cancellation,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err();
        let state = adapter.state.lock().unwrap();

        assert!(
            matches!(error, CapturedProcessError::Cancelled)
                && state.spawns == 0
                && state.signals.is_empty()
                && state.reaps == 0
        );
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    pub(crate) struct RecordingAdapter {
        pub(super) state: Arc<Mutex<RecordingState>>,
    }

    #[derive(Default)]
    pub(super) struct RecordingState {
        pub(super) stdout: Vec<u8>,
        pub(super) stderr: Vec<u8>,
        pub(super) statuses: VecDeque<Option<ProcessExit>>,
        pub(super) spawn_error: Option<SshProcessMechanismError>,
        pub(super) omit_requested_stderr: bool,
        pub(super) cancel_on_status: Option<CancellationToken>,
        pub(super) spawns: usize,
        pub(super) signals: Vec<ProcessSignal>,
        pub(super) reaps: usize,
    }

    pub(crate) struct RecordingProcess;

    impl SshProcessAdapter for RecordingAdapter {
        type Process = RecordingProcess;

        fn spawn(
            &self,
            request: SshProcessSpawnRequest,
        ) -> Result<SpawnedSshProcess<Self::Process>, SshProcessMechanismError> {
            let mut state = self.state.lock().unwrap();
            state.spawns += 1;
            if let Some(error) = state.spawn_error {
                return Err(error);
            }
            let stdin = (request.stdin() == SshProcessStdio::Piped)
                .then(|| Box::new(io::sink()) as Box<dyn Write + Send>);
            let stdout = (request.stdout() == SshProcessStdio::Piped)
                .then(|| Box::new(Cursor::new(state.stdout.clone())) as Box<dyn Read + Send>);
            let stderr = (request.stderr() == SshProcessStdio::Piped
                && !state.omit_requested_stderr)
                .then(|| Box::new(Cursor::new(state.stderr.clone())) as Box<dyn Read + Send>);
            Ok(SpawnedSshProcess::new(
                RecordingProcess,
                SshProcessPipes::new(stdin, stdout, stderr),
            ))
        }

        fn try_status(
            &self,
            _process: &mut Self::Process,
        ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
            let mut state = self.state.lock().unwrap();
            if let Some(cancellation) = state.cancel_on_status.take() {
                cancellation.cancel();
            }
            Ok(state.statuses.pop_front().unwrap_or(None))
        }

        fn signal(
            &self,
            _process: &mut Self::Process,
            signal: ProcessSignal,
        ) -> Result<(), SshProcessMechanismError> {
            self.state.lock().unwrap().signals.push(signal);
            Ok(())
        }

        fn reap(&self, _process: Self::Process) -> Result<(), SshProcessMechanismError> {
            self.state.lock().unwrap().reaps += 1;
            Ok(())
        }
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "../platform/unix_adapter_tests/account_process.rs"]
mod unix_adapter_tests;
