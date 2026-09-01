use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gpui::BackgroundExecutor;
use thiserror::Error;

use super::cancellation::SshCancellationToken;
use super::command::SshCommandSpec;
use crate::platform::macos_askpass_transport::AskPassBrokerLease;

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
    pub(crate) const fn successful() -> Self {
        Self {
            success: true,
            code: Some(0),
        }
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

impl From<ExitStatus> for ProcessExit {
    fn from(status: ExitStatus) -> Self {
        Self {
            success: status.success(),
            code: status.code(),
        }
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
    fn spawn(&self, spec: SshCommandSpec) -> impl Future<Output = io::Result<Self::Child>> + Send;

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
    fn try_wait(&self, child: &mut Self::Child) -> io::Result<Option<ProcessExit>>;

    /// Signals the private process group owned by `child`.
    fn signal_process_group(
        &self,
        child: &mut Self::Child,
        signal: ProcessSignal,
    ) -> io::Result<()>;

    /// Consumes ownership and synchronously terminates and reaps the process group.
    fn force_cleanup(&self, child: Self::Child);

    /// Takes a bounded, sanitized diagnostic tail without exposing raw process output.
    fn take_error_output(&self, _child: &mut Self::Child) -> Option<TransientSshErrorOutput> {
        None
    }

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessSignal {
    Terminate,
    Kill,
}

impl ProcessSignal {
    const fn raw(self) -> libc::c_int {
        match self {
            Self::Terminate => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ProcessRunError {
    #[error("SSH process operation was cancelled")]
    Cancelled,
    #[error("SSH process operation exceeded its deadline")]
    TimedOut,
    #[error("SSH process operation failed: {0}")]
    Io(#[from] io::Error),
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
/// Applying this value clears ambient variables, installs only fixed `HOME` and `PATH`, an
/// optional validated agent socket, and the fixed AskPass transport overlay. The retained AskPass
/// lease keeps its private broker alive for the process lifetime.
pub(crate) struct SshProcessEnvironment {
    home: PathBuf,
    authentication: SshAuthentication,
    agent_socket: Option<OsString>,
}

#[derive(Clone)]
/// Captured environment for the helper-free OpenSSH capability probe.
///
/// This environment clears ambient variables and deliberately excludes AskPass transport state.
pub(crate) struct SshProbeEnvironment {
    home: PathBuf,
    agent_socket: Option<OsString>,
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
        agent_socket: Option<OsString>,
    ) -> Result<Self, SshProcessEnvironmentError> {
        Self::validated(
            home,
            SshAuthentication::AskPass(authentication),
            agent_socket,
        )
    }

    fn validated(
        home: PathBuf,
        authentication: SshAuthentication,
        agent_socket: Option<OsString>,
    ) -> Result<Self, SshProcessEnvironmentError> {
        if !safe_absolute_path(&home) {
            return Err(SshProcessEnvironmentError::UnsafeHome);
        }
        if agent_socket.as_ref().is_some_and(|socket| {
            socket.as_bytes().is_empty() || !safe_absolute_path(Path::new(socket))
        }) {
            return Err(SshProcessEnvironmentError::UnsafeAgentSocket);
        }
        Ok(Self {
            home,
            authentication,
            agent_socket,
        })
    }

    #[cfg(test)]
    pub(super) fn new_without_authentication(
        home: PathBuf,
        agent_socket: Option<OsString>,
    ) -> Result<Self, SshProcessEnvironmentError> {
        Self::validated(home, SshAuthentication::None, agent_socket)
    }

    pub(super) fn apply(&self, command: &mut Command) {
        command
            .env_clear()
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin");
        if let Some(agent_socket) = &self.agent_socket {
            command.env("SSH_AUTH_SOCK", agent_socket);
        }
        match &self.authentication {
            SshAuthentication::AskPass(authentication) => {
                for (name, value) in authentication.entries() {
                    command.env(name, value);
                }
            }
            #[cfg(test)]
            SshAuthentication::None => {}
        }
    }

    pub(crate) fn apply_to_pty(&self, command: &mut portable_pty::CommandBuilder) {
        command.env_clear();
        command.cwd(&self.home);
        command.env("HOME", &self.home);
        command.env("PATH", "/usr/bin:/bin");
        if let Some(agent_socket) = &self.agent_socket {
            command.env("SSH_AUTH_SOCK", agent_socket);
        }
        match &self.authentication {
            SshAuthentication::AskPass(authentication) => {
                for (name, value) in authentication.entries() {
                    command.env(name, value);
                }
            }
            #[cfg(test)]
            SshAuthentication::None => {}
        }
    }

    pub(super) fn probe_environment(&self) -> SshProbeEnvironment {
        SshProbeEnvironment {
            home: self.home.clone(),
            agent_socket: self.agent_socket.clone(),
        }
    }
}

impl SshProbeEnvironment {
    pub(crate) fn new(
        home: PathBuf,
        agent_socket: Option<OsString>,
    ) -> Result<Self, SshProcessEnvironmentError> {
        if !safe_absolute_path(&home) {
            return Err(SshProcessEnvironmentError::UnsafeHome);
        }
        if agent_socket.as_ref().is_some_and(|socket| {
            socket.as_bytes().is_empty() || !safe_absolute_path(Path::new(socket))
        }) {
            return Err(SshProcessEnvironmentError::UnsafeAgentSocket);
        }
        Ok(Self { home, agent_socket })
    }

    pub(super) fn apply(&self, command: &mut Command) {
        command
            .env_clear()
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin");
        if let Some(agent_socket) = &self.agent_socket {
            command.env("SSH_AUTH_SOCK", agent_socket);
        }
    }
}

#[derive(Clone)]
/// Native backend that supervises SSH commands outside GPUI executor threads.
pub(crate) struct NativeSshProcessBackend {
    executor: BackgroundExecutor,
    environment: SshProcessEnvironment,
}

impl NativeSshProcessBackend {
    pub(crate) const fn new(
        executor: BackgroundExecutor,
        environment: SshProcessEnvironment,
    ) -> Self {
        Self {
            executor,
            environment,
        }
    }
}

/// Non-clone owner of a child, its private process group, and bounded stderr reader.
///
/// Dropping this value terminates and reaps the group even when the leader has already exited.
pub(crate) struct NativeSshChild {
    child: Option<Child>,
    process_group: libc::pid_t,
    stderr_reader: Option<JoinHandle<io::Result<Vec<u8>>>>,
}

impl SshProcessBackend for NativeSshProcessBackend {
    type Child = NativeSshChild;

    fn environment(&self) -> &SshProcessEnvironment {
        &self.environment
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn spawn(&self, spec: SshCommandSpec) -> impl Future<Output = io::Result<Self::Child>> + Send {
        let command = command(spec, &self.environment);
        async move { spawn_owned_command_off_thread(command).await }
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
                    backend.force_cleanup(child);
                    return Err(ProcessRunError::Cancelled);
                }
                let now = backend.now();
                if now >= deadline {
                    backend.force_cleanup(child);
                    return Err(ProcessRunError::TimedOut);
                }
                backend
                    .delay(Duration::from_millis(10).min(deadline.duration_since(now)))
                    .await;
            }
        }
    }

    fn try_wait(&self, child: &mut Self::Child) -> io::Result<Option<ProcessExit>> {
        let Some(process) = child.child.as_mut() else {
            return Ok(Some(ProcessExit::unsuccessful(None)));
        };
        process
            .try_wait()
            .map(|status| status.map(ProcessExit::from))
    }

    fn signal_process_group(
        &self,
        child: &mut Self::Child,
        signal: ProcessSignal,
    ) -> io::Result<()> {
        signal_process_group(child.process_group, signal.raw())
    }

    fn force_cleanup(&self, child: Self::Child) {
        drop(child);
    }

    fn take_error_output(&self, child: &mut Self::Child) -> Option<TransientSshErrorOutput> {
        let _ = signal_process_group(child.process_group, libc::SIGKILL);
        let bytes = child.stderr_reader.take()?.join().ok()?.ok()?;
        TransientSshErrorOutput::from_untrusted_bytes(&bytes)
    }

    fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send {
        let executor = self.executor.clone();
        async move { executor.timer(duration).await }
    }
}

fn command(spec: SshCommandSpec, environment: &SshProcessEnvironment) -> Command {
    let mut command = Command::new(spec.executable());
    command
        .args(spec.arguments())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    environment.apply(&mut command);
    command
}

fn safe_absolute_path(path: &Path) -> bool {
    path.is_absolute() && !path.as_os_str().as_bytes().iter().any(u8::is_ascii_control)
}

fn spawn_owned_command(mut command: Command) -> io::Result<NativeSshChild> {
    command.process_group(0);
    let mut child = command.spawn()?;
    let process_group = match child.id().try_into() {
        Ok(process_group) => process_group,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(
                "SSH child process identifier did not fit the platform process type",
            ));
        }
    };
    let stderr_reader = if let Some(stderr) = child.stderr.take() {
        match std::thread::Builder::new()
            .name("spaceterm-ssh-stderr".to_owned())
            .spawn(move || read_final_error_tail(stderr))
        {
            Ok(reader) => Some(reader),
            Err(error) => {
                let _ = signal_process_group(process_group, libc::SIGKILL);
                let _ = child.wait();
                return Err(error);
            }
        }
    } else {
        None
    };
    Ok(NativeSshChild {
        child: Some(child),
        process_group,
        stderr_reader,
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

async fn spawn_owned_command_off_thread(command: Command) -> io::Result<NativeSshChild> {
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("spaceterm-ssh-spawn".to_owned())
        .spawn(move || {
            let result = spawn_owned_command(command);
            let _ = sender.send_blocking(result);
        })?;
    receiver.recv().await.map_err(|_| {
        io::Error::other("SSH process launch worker ended before returning child ownership")
    })?
}

fn signal_process_group(process_group: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: the process group is the positive PID returned by a child we launched with a new
    // process group; kill does not dereference pointers.
    let result = unsafe { libc::kill(-process_group, signal) };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

impl Drop for NativeSshChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = signal_process_group(self.process_group, libc::SIGKILL);
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        reap_child(child);
    }
}

fn reap_child(child: Child) {
    reap_child_with(child, |receiver| {
        std::thread::Builder::new()
            .name("spaceterm-ssh-reaper".to_owned())
            .spawn(move || {
                if let Ok(mut child) = receiver.recv() {
                    let _ = child.wait();
                }
            })
            .map(|_| ())
    });
}

fn reap_child_with(mut child: Child, spawn: impl FnOnce(mpsc::Receiver<Child>) -> io::Result<()>) {
    let (sender, receiver) = mpsc::sync_channel(1);
    if spawn(receiver).is_err() {
        let _ = child.wait();
        return;
    }
    if let Err(mpsc::SendError(mut returned_child)) = sender.send(child) {
        let _ = returned_child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

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
        let home = PathBuf::from(format!(
            "/private/tmp/spaceterm-process-environment-{}",
            std::process::id()
        ));
        fs::create_dir_all(&home).unwrap();
        let agent_socket = OsString::from("/private/tmp/spaceterm-test-agent.sock");
        let environment = SshProcessEnvironment::new_without_authentication(
            home.clone(),
            Some(agent_socket.clone()),
        )
        .unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "pwd; /usr/bin/env"])
            .env("SPACETERM_UNKNOWN", "secret")
            .env("HOME", "/attacker/home")
            .env("PATH", "/attacker/bin")
            .env("SSH_AUTH_SOCK", "/attacker/agent.sock")
            .stdout(Stdio::piped());

        environment.apply(&mut command);
        let output = command.output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        assert!(
            stdout.starts_with(&format!("{}\n", home.display()))
                && stdout.contains(&format!("HOME={}", home.display()))
                && stdout.contains("PATH=/usr/bin:/bin")
                && stdout.contains(&format!("SSH_AUTH_SOCK={}", agent_socket.to_string_lossy()))
                && !stdout.contains("SPACETERM_UNKNOWN")
                && !stdout.contains("/attacker/")
        );
        fs::remove_dir(home).unwrap();
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
        let mut command = portable_pty::CommandBuilder::new("/usr/bin/ssh");
        command.env("SPACETERM_UNKNOWN", "secret");
        command.env("HOME", "/attacker/home");
        command.env("PATH", "/attacker/bin");

        environment.apply_to_pty(&mut command);

        assert_eq!(command.get_cwd(), Some(&home.into_os_string()));
        assert_eq!(
            command.get_env("HOME"),
            Some(std::ffi::OsStr::new("/private/tmp"))
        );
        assert_eq!(
            command.get_env("PATH"),
            Some(std::ffi::OsStr::new("/usr/bin:/bin"))
        );
        assert_eq!(
            command.get_env("SSH_AUTH_SOCK"),
            Some(std::ffi::OsStr::new("/private/tmp/agent.sock"))
        );
        assert_eq!(command.get_env("SPACETERM_UNKNOWN"), None);
    }

    #[test]
    fn native_child_drop_should_terminate_its_private_process_group_descendants() {
        let pid_file = PathBuf::from(format!(
            "/private/tmp/spaceterm-process-descendant-{}.pid",
            std::process::id()
        ));
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);

        let child = spawn_owned_command(command).unwrap();
        for _ in 0..100 {
            if pid_file.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let descendant: i32 = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        drop(child);

        let terminated = (0..100).any(|_| {
            // SAFETY: signal zero performs a process-existence check and dereferences no pointers.
            let missing = unsafe { libc::kill(descendant, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if !missing {
                std::thread::sleep(Duration::from_millis(10));
            }
            missing
        });
        let _ = fs::remove_file(pid_file);

        assert!(terminated);
    }

    #[test]
    fn native_child_drop_should_terminate_descendants_after_the_leader_exits() {
        let pid_file = PathBuf::from(format!(
            "/private/tmp/spaceterm-process-orphan-descendant-{}.pid",
            std::process::id()
        ));
        let script = format!("sleep 30 & echo $! > '{}'; exit 0", pid_file.display());
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);

        let mut child = spawn_owned_command(command).unwrap();
        for _ in 0..100 {
            if pid_file.exists() && child.child.as_mut().unwrap().try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let descendant: i32 = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        drop(child);

        let terminated = wait_for_missing_process(descendant);
        let _ = fs::remove_file(pid_file);
        assert!(terminated);
    }

    #[test]
    fn reaper_spawn_failure_should_reap_on_the_calling_thread() {
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let process = i32::try_from(child.id()).unwrap();

        reap_child_with(child, |_| Err(io::Error::other("injected spawn failure")));

        // SAFETY: signal zero performs a process-existence check and dereferences no pointers.
        assert!(
            unsafe { libc::kill(process, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        );
    }

    fn wait_for_missing_process(process: libc::pid_t) -> bool {
        (0..100).any(|_| {
            // SAFETY: signal zero performs a process-existence check and dereferences no pointers.
            let missing = unsafe { libc::kill(process, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if !missing {
                std::thread::sleep(Duration::from_millis(10));
            }
            missing
        })
    }
}
