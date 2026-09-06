use std::ffi::{OsStr, OsString};
use std::fmt;
use std::future::Future;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gpui::BackgroundExecutor;
use thiserror::Error;

use super::cancellation::SshCancellationToken;
use super::command::SshCommandSpec;
use super::startup_environment::StartupSshEnvironment;
use crate::platform::askpass::{AskPassBrokerLease, AskPassCapabilityCopy};

pub(crate) const MAXIMUM_TRANSIENT_SSH_ERROR_BYTES: usize = 8 * 1024;
const TRANSIENT_SSH_ERROR_TRUNCATION_MARKER: &str = "[earlier OpenSSH output truncated] ";

/// Bounded control-free OpenSSH diagnostics retained only for the active connection failure UI.
#[derive(Eq, PartialEq)]
pub(crate) struct TransientSshErrorOutput(String);

impl TransientSshErrorOutput {
    pub(crate) fn from_untrusted_bytes(bytes: &[u8]) -> Option<Self> {
        let untrusted_start = bytes
            .len()
            .saturating_sub(MAXIMUM_TRANSIENT_SSH_ERROR_BYTES);
        let sanitized: String = String::from_utf8_lossy(&bytes[untrusted_start..])
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        let sanitized = sanitized.trim();
        if sanitized.is_empty() {
            return None;
        }
        let truncated = untrusted_start != 0 || sanitized.len() > MAXIMUM_TRANSIENT_SSH_ERROR_BYTES;
        let retained_bytes = if truncated {
            MAXIMUM_TRANSIENT_SSH_ERROR_BYTES
                .saturating_sub(TRANSIENT_SSH_ERROR_TRUNCATION_MARKER.len())
        } else {
            MAXIMUM_TRANSIENT_SSH_ERROR_BYTES
        };
        let mut retained_start = sanitized.len().saturating_sub(retained_bytes);
        while !sanitized.is_char_boundary(retained_start) {
            retained_start = retained_start.saturating_add(1);
        }
        let retained = &sanitized[retained_start..];
        if truncated {
            Some(Self(format!(
                "{TRANSIENT_SSH_ERROR_TRUNCATION_MARKER}{retained}"
            )))
        } else {
            Some(Self(retained.to_owned()))
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TransientSshErrorOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpenSSH failure detail (<redacted>)")
    }
}

impl fmt::Debug for TransientSshErrorOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TransientSshErrorOutput(<redacted>)")
    }
}

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

    pub(crate) const fn unsuccessful(code: Option<i32>) -> Self {
        Self {
            success: false,
            code,
        }
    }

    pub(crate) const fn is_success(self) -> bool {
        self.success
    }

    pub(crate) const fn code(self) -> Option<i32> {
        self.code
    }
}

/// Owns every process operation used by an SSH control connection.
///
/// Implementations must launch children in a private process group, avoid blocking async executor
/// threads, and terminate and reap the leader and all descendants on cancellation or ownership
/// loss. Commands run with [`SshProcessEnvironment`], never the ambient process environment.
pub(crate) trait SshProcessBackend: Send + Sync + 'static {
    /// Non-clone ownership of one child process and its private process group.
    type Child: Send + 'static;

    /// Returns the captured, sanitized environment applied to all spawned SSH processes.
    fn environment(&self) -> &SshProcessEnvironment;

    fn now(&self) -> Instant;

    /// Starts one owned command without blocking the caller's async executor thread.
    fn spawn(
        &self,
        spec: SshCommandSpec,
    ) -> impl Future<Output = Result<Self::Child, SshProcessMechanismError>> + Send;

    /// Runs a short-lived command until completion, cancellation, or the supplied wall deadline.
    ///
    /// Cancellation and timeout must terminate and reap the command's entire private process
    /// group before returning.
    fn run(
        &self,
        spec: SshCommandSpec,
        cancellation: SshCancellationToken,
        deadline: Instant,
    ) -> impl Future<Output = Result<ProcessExit, ProcessRunError>> + Send;

    /// Polls and reaps the child leader when it has exited.
    fn try_wait(
        &self,
        child: &mut Self::Child,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError>;

    /// Signals the private process group owned by `child`.
    fn signal_process_group(
        &self,
        child: &mut Self::Child,
        signal: ProcessSignal,
    ) -> Result<(), SshProcessMechanismError>;

    /// Enqueues owned-process cleanup without blocking the caller.
    ///
    /// The returned completion resolves only after the process group is killed, the leader is
    /// reaped, every diagnostic reader has finished, and `after` has run. Dropping the completion
    /// detaches observation without cancelling cleanup.
    fn begin_cleanup(
        &self,
        child: Self::Child,
        after: Option<ProcessCleanupCallback>,
    ) -> SshProcessCleanup;

    /// Takes a bounded, sanitized diagnostic tail without exposing raw process output.
    fn take_error_output(&self, _child: &mut Self::Child) -> Option<TransientSshErrorOutput> {
        None
    }

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

pub(crate) type ProcessCleanupCallback = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone)]
/// Awaitable observation of an already-enqueued owned-process cleanup.
pub(crate) struct SshProcessCleanup {
    completion: async_channel::Receiver<()>,
}

impl SshProcessCleanup {
    pub(crate) fn completed() -> Self {
        let (sender, completion) = async_channel::bounded(1);
        drop(sender);
        Self { completion }
    }

    pub(crate) async fn wait(&self) {
        let _ = self.completion.recv().await;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessSignal {
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

#[derive(Debug, Error)]
pub(crate) enum ProcessRunError {
    #[error("SSH process operation was cancelled")]
    Cancelled,
    #[error("SSH process operation exceeded its deadline")]
    TimedOut,
    #[error("SSH process operation failed")]
    Operation(#[from] SshProcessMechanismError),
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

    #[cfg(test)]
    pub(crate) fn process_mut(&mut self) -> &mut P {
        &mut self.process
    }

    #[cfg(test)]
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
    authentication: SshAuthentication,
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

#[derive(Clone)]
enum SshAuthentication {
    AskPass(AskPassBrokerLease),
    #[cfg(test)]
    None,
}

impl SshProcessEnvironment {
    pub(crate) fn new(
        home: PathBuf,
        authentication: AskPassBrokerLease,
        startup: &StartupSshEnvironment,
    ) -> Result<Self, SshProcessEnvironmentError> {
        Self::validated(
            home,
            SshAuthentication::AskPass(authentication),
            startup.clone(),
        )
    }

    fn validated(
        home: PathBuf,
        authentication: SshAuthentication,
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

    #[cfg(test)]
    pub(super) fn new_without_authentication(
        home: PathBuf,
        agent_socket: Option<OsString>,
    ) -> Result<Self, SshProcessEnvironmentError> {
        Self::validated(
            home,
            SshAuthentication::None,
            StartupSshEnvironment::for_test(agent_socket),
        )
    }

    #[cfg(test)]
    pub(super) fn new_without_authentication_from_startup(
        home: PathBuf,
        startup: &StartupSshEnvironment,
    ) -> Result<Self, SshProcessEnvironmentError> {
        Self::validated(home, SshAuthentication::None, startup.clone())
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
        let askpass_capability = match &self.authentication {
            SshAuthentication::AskPass(authentication) => {
                entries.extend(
                    authentication
                        .ordinary_spawn_entries()
                        .map(|(name, value)| (name.into(), value.to_owned())),
                );
                Some(authentication.capability_copy_for_spawn())
            }
            #[cfg(test)]
            SshAuthentication::None => None,
        };
        (self.home.clone(), entries, askpass_capability)
    }

    /// Builds the environment for an already-authorized pane channel.
    ///
    /// Pane commands must use their live control connection and never receive AskPass transport
    /// state. If that connection is unavailable, OpenSSH must fail instead of re-authenticating.
    pub(crate) fn into_pane_launch_environment(self) -> (PathBuf, Vec<(OsString, OsString)>) {
        let mut entries = vec![(OsString::from("HOME"), self.home.as_os_str().to_owned())];
        entries.extend(
            self.startup
                .entries()
                .map(|(name, value)| (name.into(), value.to_owned())),
        );
        (self.home, entries)
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

#[derive(Clone)]
/// Portable SSH process owner built over one capability-specific process adapter.
pub(crate) struct SshProcessSupervisor<A: SshProcessAdapter> {
    executor: BackgroundExecutor,
    environment: SshProcessEnvironment,
    adapter: A,
}

impl<A: SshProcessAdapter> SshProcessSupervisor<A> {
    pub(crate) const fn new(
        executor: BackgroundExecutor,
        environment: SshProcessEnvironment,
        adapter: A,
    ) -> Self {
        Self {
            executor,
            environment,
            adapter,
        }
    }
}

/// Non-clone portable owner of one process and its bounded diagnostic reader.
pub(crate) struct SupervisedSshChild<A: SshProcessAdapter> {
    adapter: A,
    process: Option<A::Process>,
    stderr_reader: Option<JoinHandle<io::Result<Vec<u8>>>>,
    cleanup_sender: async_channel::Sender<CleanupOwnership<A>>,
    cleanup_receiver_guard: async_channel::Receiver<CleanupOwnership<A>>,
}

impl<A: SshProcessAdapter> SshProcessBackend for SshProcessSupervisor<A> {
    type Child = SupervisedSshChild<A>;

    fn environment(&self) -> &SshProcessEnvironment {
        &self.environment
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn spawn(
        &self,
        spec: SshCommandSpec,
    ) -> impl Future<Output = Result<Self::Child, SshProcessMechanismError>> + Send {
        let adapter = self.adapter.clone();
        let request = process_request(
            &spec,
            self.environment.launch_parts(),
            SshProcessStdio::Null,
            SshProcessStdio::Null,
            SshProcessStdio::Piped,
        );
        async move { spawn_owned_process_off_thread(adapter, request).await }
    }

    fn run(
        &self,
        spec: SshCommandSpec,
        cancellation: SshCancellationToken,
        deadline: Instant,
    ) -> impl Future<Output = Result<ProcessExit, ProcessRunError>> + Send {
        let backend = self.clone();
        async move {
            let mut child = backend.spawn(spec).await?;
            loop {
                if let Some(exit) = backend.try_wait(&mut child)? {
                    return Ok(exit);
                }
                if cancellation.is_cancelled() {
                    backend.begin_cleanup(child, None).wait().await;
                    return Err(ProcessRunError::Cancelled);
                }
                let now = backend.now();
                if now >= deadline {
                    backend.begin_cleanup(child, None).wait().await;
                    return Err(ProcessRunError::TimedOut);
                }
                backend
                    .delay(Duration::from_millis(10).min(deadline.duration_since(now)))
                    .await;
            }
        }
    }

    fn try_wait(
        &self,
        child: &mut Self::Child,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
        let Some(process) = child.process.as_mut() else {
            return Ok(Some(ProcessExit::unsuccessful(None)));
        };
        child.adapter.try_status(process)
    }

    fn signal_process_group(
        &self,
        child: &mut Self::Child,
        signal: ProcessSignal,
    ) -> Result<(), SshProcessMechanismError> {
        let process = child
            .process
            .as_mut()
            .ok_or(SshProcessMechanismError::StatusFailed)?;
        child.adapter.signal(process, signal)
    }

    fn begin_cleanup(
        &self,
        mut child: Self::Child,
        after: Option<ProcessCleanupCallback>,
    ) -> SshProcessCleanup {
        child.enqueue_cleanup(after)
    }

    fn take_error_output(&self, child: &mut Self::Child) -> Option<TransientSshErrorOutput> {
        if let Some(process) = child.process.as_mut() {
            let _ = child.adapter.signal(process, ProcessSignal::Kill);
        }
        let bytes = child.stderr_reader.take()?.join().ok()?.ok()?;
        TransientSshErrorOutput::from_untrusted_bytes(&bytes)
    }

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send {
        let executor = self.executor.clone();
        async move { executor.timer(duration).await }
    }
}

impl<A: SshProcessAdapter> Drop for SupervisedSshChild<A> {
    fn drop(&mut self) {
        let _ = self.enqueue_cleanup(None);
    }
}

impl<A: SshProcessAdapter> SupervisedSshChild<A> {
    fn enqueue_cleanup(&mut self, after: Option<ProcessCleanupCallback>) -> SshProcessCleanup {
        let Some(process) = self.process.take() else {
            return SshProcessCleanup::completed();
        };
        let (completion_sender, completion) = async_channel::bounded(1);
        let ownership = CleanupOwnership {
            adapter: self.adapter.clone(),
            process,
            readers: self.stderr_reader.take().into_iter().collect(),
            after,
            _completion_sender: completion_sender,
        };

        // This guard keeps the unbounded queue open through the non-blocking send even if the
        // dedicated reaper unexpectedly exits. Cleanup starts when the reaper receives ownership.
        let _guard = &self.cleanup_receiver_guard;
        let _ = self.cleanup_sender.send_blocking(ownership);
        SshProcessCleanup { completion }
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

fn spawn_owned_process<A: SshProcessAdapter>(
    adapter: A,
    request: SshProcessSpawnRequest,
) -> Result<SupervisedSshChild<A>, SshProcessMechanismError> {
    let SpawnedSshProcess { process, mut pipes } = adapter.spawn(request)?;
    let Some(stderr) = pipes.stderr.take() else {
        cleanup_now(&adapter, process, Vec::new());
        return Err(SshProcessMechanismError::PipesUnavailable);
    };
    let stderr_reader = match std::thread::Builder::new()
        .name("spaceterm-ssh-stderr".to_owned())
        .spawn(move || read_final_error_tail(stderr))
    {
        Ok(reader) => reader,
        Err(_) => {
            cleanup_now(&adapter, process, Vec::new());
            return Err(SshProcessMechanismError::LaunchFailed);
        }
    };
    let (cleanup_sender, cleanup_receiver) = async_channel::unbounded::<CleanupOwnership<A>>();
    let cleanup_receiver_guard = cleanup_receiver.clone();
    if std::thread::Builder::new()
        .name("spaceterm-ssh-reaper".to_owned())
        .spawn(move || {
            if let Ok(ownership) = cleanup_receiver.recv_blocking() {
                cleanup_now(&ownership.adapter, ownership.process, ownership.readers);
                if let Some(after) = ownership.after {
                    after();
                }
            }
        })
        .is_err()
    {
        cleanup_now(&adapter, process, vec![stderr_reader]);
        return Err(SshProcessMechanismError::LaunchFailed);
    }
    Ok(SupervisedSshChild {
        adapter,
        process: Some(process),
        stderr_reader: Some(stderr_reader),
        cleanup_sender,
        cleanup_receiver_guard,
    })
}

fn read_final_error_tail(mut stderr: impl Read) -> io::Result<Vec<u8>> {
    let mut tail = Vec::with_capacity(MAXIMUM_TRANSIENT_SSH_ERROR_BYTES);
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stderr.read(&mut chunk)?;
        if read == 0 {
            return Ok(tail);
        }
        if read >= MAXIMUM_TRANSIENT_SSH_ERROR_BYTES {
            tail.clear();
            tail.extend_from_slice(&chunk[read - MAXIMUM_TRANSIENT_SSH_ERROR_BYTES..read]);
            continue;
        }
        let excess = tail
            .len()
            .saturating_add(read)
            .saturating_sub(MAXIMUM_TRANSIENT_SSH_ERROR_BYTES);
        if excess != 0 {
            tail.drain(..excess);
        }
        tail.extend_from_slice(&chunk[..read]);
    }
}

async fn spawn_owned_process_off_thread<A: SshProcessAdapter>(
    adapter: A,
    request: SshProcessSpawnRequest,
) -> Result<SupervisedSshChild<A>, SshProcessMechanismError> {
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("spaceterm-ssh-spawn".to_owned())
        .spawn(move || {
            let result = spawn_owned_process(adapter, request);
            let _ = sender.send_blocking(result);
        })
        .map_err(|_| SshProcessMechanismError::LaunchFailed)?;
    receiver
        .recv()
        .await
        .map_err(|_| SshProcessMechanismError::LaunchFailed)?
}

struct CleanupOwnership<A: SshProcessAdapter> {
    adapter: A,
    process: A::Process,
    readers: Vec<JoinHandle<io::Result<Vec<u8>>>>,
    after: Option<ProcessCleanupCallback>,
    _completion_sender: async_channel::Sender<()>,
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
    cancellation: &SshCancellationToken,
    deadline: Instant,
) -> Result<CapturedProcessOutput, CapturedProcessError> {
    run_captured_process_with_parts(
        adapter,
        spec,
        environment.launch_parts(),
        CapturedProcessPlan {
            input: Some(input),
            stdout_limit,
            stderr_limit: None,
            deadline,
        },
        cancellation,
    )
}

pub(crate) fn run_probe_process<A: SshProcessAdapter>(
    adapter: &A,
    spec: &SshCommandSpec,
    environment: &SshProbeEnvironment,
    stream_limit: usize,
    cancellation: &SshCancellationToken,
    deadline: Instant,
) -> Result<CapturedProcessOutput, CapturedProcessError> {
    let (current_directory, environment) = environment.launch_parts();
    run_captured_process_with_parts(
        adapter,
        spec,
        (current_directory, environment, None),
        CapturedProcessPlan {
            input: None,
            stdout_limit: stream_limit,
            stderr_limit: Some(stream_limit),
            deadline,
        },
        cancellation,
    )
}

struct CapturedProcessPlan {
    input: Option<Vec<u8>>,
    stdout_limit: usize,
    stderr_limit: Option<usize>,
    deadline: Instant,
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
    cancellation: &SshCancellationToken,
) -> Result<CapturedProcessOutput, CapturedProcessError> {
    let CapturedProcessPlan {
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
    let mut ownership = CapturedProcessOwnership::new(adapter.clone(), process);
    let stdout = pipes.stdout.take().ok_or(CapturedProcessError::Operation(
        SshProcessMechanismError::PipesUnavailable,
    ))?;
    let writer = match input {
        Some(input) => {
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
        if Instant::now() >= deadline {
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
            Ok(None) => std::thread::sleep(
                PROCESS_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            ),
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

pub(crate) struct CancelOnDrop {
    cancellation: SshCancellationToken,
    armed: bool,
}

impl CancelOnDrop {
    pub(crate) fn new(cancellation: SshCancellationToken) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::platform::askpass::AskPassLease;
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
    fn transient_error_output_should_keep_only_the_sanitized_final_eight_kibibytes() {
        let mut input = vec![b'x'; MAXIMUM_TRANSIENT_SSH_ERROR_BYTES + 32];
        input.extend_from_slice(b"\nfinal\x1b[31m message\0");

        let output = TransientSshErrorOutput::from_untrusted_bytes(&input).unwrap();

        assert!(
            output.as_str().len() <= MAXIMUM_TRANSIENT_SSH_ERROR_BYTES
                && output.as_str().ends_with("final [31m message")
                && output
                    .as_str()
                    .starts_with(TRANSIENT_SSH_ERROR_TRUNCATION_MARKER)
                && !output.as_str().chars().any(char::is_control)
                && !format!("{output:?}").contains("final")
                && !output.to_string().contains("final")
        );
    }

    #[test]
    fn transient_error_output_should_lossily_sanitize_invalid_utf8_without_exposing_formatting() {
        let output =
            TransientSshErrorOutput::from_untrusted_bytes(b"bad\xff\xfe\nmessage").unwrap();

        assert_eq!(output.as_str(), "bad\u{fffd}\u{fffd} message");
        assert_eq!(format!("{output:?}"), "TransientSshErrorOutput(<redacted>)");
        assert_eq!(output.to_string(), "OpenSSH failure detail (<redacted>)");
    }

    #[test]
    fn launch_environment_should_clear_unknown_variables_and_use_captured_home_as_cwd() {
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
            Some(&OsString::from("/usr/bin:/bin"))
        );
        assert_eq!(
            entries.get(std::ffi::OsStr::new("SSH_AUTH_SOCK")),
            Some(&agent_socket)
        );
        assert!(!entries.contains_key(std::ffi::OsStr::new("SPACETERM_UNKNOWN")));
    }

    #[test]
    fn launch_environment_should_preserve_captured_path_for_openssh_proxy_commands_only() {
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

    #[test]
    fn pane_environment_should_clear_ambient_values_and_use_captured_home() {
        let home = PathBuf::from("/private/tmp");
        let environment = SshProcessEnvironment::new_without_authentication(
            home.clone(),
            Some(OsString::from("/private/tmp/agent.sock")),
        )
        .unwrap();
        let (directory, entries) = environment.into_pane_launch_environment();
        let entries: std::collections::HashMap<_, _> = entries.into_iter().collect();
        assert_eq!(directory, home);
        assert_eq!(
            entries.get(std::ffi::OsStr::new("HOME")),
            Some(&OsString::from("/private/tmp"))
        );
        assert_eq!(
            entries.get(std::ffi::OsStr::new("PATH")),
            Some(&OsString::from("/usr/bin:/bin"))
        );
        assert_eq!(
            entries.get(std::ffi::OsStr::new("SSH_AUTH_SOCK")),
            Some(&OsString::from("/private/tmp/agent.sock"))
        );
        assert!(!entries.contains_key(std::ffi::OsStr::new("SPACETERM_UNKNOWN")));
    }

    #[test]
    fn authenticated_pane_environment_should_exclude_askpass_transport() {
        let (directory, entries) = authenticated_test_environment().into_pane_launch_environment();

        assert_eq!(directory, Path::new("/captured/home"));
        assert!(entries.iter().all(|(name, _)| {
            !ASKPASS_OVERLAY_NAMES
                .iter()
                .any(|candidate| name == candidate)
        }));
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
            &SshCancellationToken::default(),
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
            &SshCancellationToken::default(),
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
        let cancellation = SshCancellationToken::default();
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
            &SshCancellationToken::default(),
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
            &SshCancellationToken::default(),
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
    fn recording_adapter_should_cleanup_when_a_requested_pipe_is_missing() {
        let adapter = RecordingAdapter::default();
        adapter.state.lock().unwrap().omit_requested_stderr = true;
        let request = process_request(
            &test_spec(),
            test_environment().launch_parts(),
            SshProcessStdio::Null,
            SshProcessStdio::Null,
            SshProcessStdio::Piped,
        );

        let error = match spawn_owned_process(adapter.clone(), request) {
            Ok(_) => panic!("missing requested stderr should fail the spawn"),
            Err(error) => error,
        };
        let state = adapter.state.lock().unwrap();

        assert_eq!(error, SshProcessMechanismError::PipesUnavailable);
        assert_eq!(state.signals, [ProcessSignal::Kill]);
        assert_eq!(state.reaps, 1);
    }

    #[test]
    fn cleanup_completion_should_follow_process_reap_readers_and_callback() {
        let adapter = RecordingAdapter::default();
        let request = process_request(
            &test_spec(),
            test_environment().launch_parts(),
            SshProcessStdio::Null,
            SshProcessStdio::Null,
            SshProcessStdio::Piped,
        );
        let mut child = spawn_owned_process(adapter.clone(), request).unwrap();
        let callback_ran = Arc::new(AtomicBool::new(false));
        let callback_observer = Arc::clone(&callback_ran);

        let cleanup = child.enqueue_cleanup(Some(Box::new(move || {
            callback_observer.store(true, Ordering::Release);
        })));
        let _ = cleanup.completion.recv_blocking();
        let state = adapter.state.lock().unwrap();

        assert!(
            state.signals == [ProcessSignal::Kill]
                && state.reaps == 1
                && callback_ran.load(Ordering::Acquire)
        );
    }

    #[test]
    fn pre_cancelled_operation_should_not_spawn_or_cleanup() {
        let adapter = RecordingAdapter::default();
        let cancellation = SshCancellationToken::default();
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
        pub(super) cancel_on_status: Option<SshCancellationToken>,
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
