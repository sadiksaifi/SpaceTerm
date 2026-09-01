use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use super::cancellation::SshCancellationToken;
use super::live_connection::LiveConnectionCapability;
use super::process::{SshProbeEnvironment, SshProcessEnvironment, SshProcessEnvironmentError};
use super::startup_environment::StartupSshEnvironment;
use crate::domain::{RemoteWorkspaceDirectory, SshDestination};

const SSH_EXECUTABLE: &str = "/usr/bin/ssh";
const MINIMUM_OPENSSH_VERSION: OpenSshVersion = OpenSshVersion::new(8, 2);
const MAX_PROBE_STREAM_BYTES: usize = 4 * 1024;
const NATIVE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const NATIVE_PROBE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAXIMUM_REMOTE_SHELL_VALUE_BYTES: usize = 4 * 1024;
const MAXIMUM_REMOTE_PANE_COMMAND_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct OpenSshVersion {
    major: u16,
    minor: u16,
}

impl OpenSshVersion {
    pub(crate) const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

impl fmt::Display for OpenSshVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SshCapability {
    Available(OpenSshVersion),
    Unavailable(SshUnavailableReason),
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum SshUnavailableReason {
    #[error("OpenSSH was not found at /usr/bin/ssh")]
    NotFound,
    #[error("OpenSSH {minimum} or newer is required; found {found}")]
    TooOld {
        found: OpenSshVersion,
        minimum: OpenSshVersion,
    },
    #[error("the installed SSH client did not report a recognized OpenSSH version")]
    Unrecognized,
    #[error("the installed SSH client could not be checked")]
    ProbeFailed,
}

#[derive(Clone)]
pub(crate) struct SshProbeOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl SshProbeOutput {
    pub(crate) const fn new(success: bool, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            success,
            stdout,
            stderr,
        }
    }
}

pub(crate) trait SshProbeRunner {
    fn run(&self, executable: &Path, arguments: &[OsString]) -> io::Result<SshProbeOutput>;
}

pub(crate) fn probe_ssh_capability(runner: &impl SshProbeRunner) -> SshCapability {
    let output = match runner.run(Path::new(SSH_EXECUTABLE), &[OsString::from("-V")]) {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return SshCapability::Unavailable(SshUnavailableReason::NotFound);
        }
        Err(_) => return SshCapability::Unavailable(SshUnavailableReason::ProbeFailed),
    };
    classify_probe_output(output)
}

#[derive(Clone)]
pub(crate) struct NativeSshProbeRunner {
    environment: SshProbeEnvironment,
    timeout: Duration,
    #[cfg(test)]
    executable: PathBuf,
}

impl NativeSshProbeRunner {
    pub(crate) fn new(environment: SshProcessEnvironment) -> Self {
        Self {
            environment: environment.probe_environment(),
            timeout: NATIVE_PROBE_TIMEOUT,
            #[cfg(test)]
            executable: PathBuf::new(),
        }
    }

    pub(crate) fn from_startup(
        home: PathBuf,
        startup: &StartupSshEnvironment,
    ) -> Result<Self, SshProcessEnvironmentError> {
        Ok(Self {
            environment: SshProbeEnvironment::new(home, startup)?,
            timeout: NATIVE_PROBE_TIMEOUT,
            #[cfg(test)]
            executable: PathBuf::new(),
        })
    }

    #[cfg(test)]
    fn for_test(
        environment: SshProcessEnvironment,
        executable: PathBuf,
        timeout: Duration,
    ) -> Self {
        Self {
            environment: environment.probe_environment(),
            timeout,
            executable,
        }
    }

    pub(crate) fn probe_blocking(&self) -> SshCapability {
        let cancellation = SshCancellationToken::default();
        classify_native_probe_result(run_native_probe(
            self.executable(),
            self.environment.clone(),
            &cancellation,
            Instant::now()
                .checked_add(self.timeout)
                .unwrap_or_else(Instant::now),
        ))
    }

    fn executable(&self) -> PathBuf {
        #[cfg(not(test))]
        {
            PathBuf::from(SSH_EXECUTABLE)
        }
        #[cfg(test)]
        {
            if self.executable.as_os_str().is_empty() {
                PathBuf::from(SSH_EXECUTABLE)
            } else {
                self.executable.clone()
            }
        }
    }

    pub(crate) async fn probe(&self, cancellation: SshCancellationToken) -> SshCapability {
        if cancellation.is_cancelled() {
            return SshCapability::Unavailable(SshUnavailableReason::ProbeFailed);
        }
        let environment = self.environment.clone();
        let timeout = self.timeout;
        let executable = self.executable();
        let mut cancel_on_drop = ProbeCancelOnDrop::new(cancellation.clone());
        let (sender, receiver) = async_channel::bounded(1);
        let worker = thread::Builder::new()
            .name("spaceterm-ssh-version".to_owned())
            .spawn(move || {
                let result = run_native_probe(
                    executable,
                    environment,
                    &cancellation,
                    Instant::now()
                        .checked_add(timeout)
                        .unwrap_or_else(Instant::now),
                );
                let _ = sender.send_blocking(result);
            });
        if worker.is_err() {
            cancel_on_drop.disarm();
            return SshCapability::Unavailable(SshUnavailableReason::ProbeFailed);
        }
        let result = receiver.recv().await;
        cancel_on_drop.disarm();
        result.map_or_else(
            |_| SshCapability::Unavailable(SshUnavailableReason::ProbeFailed),
            classify_native_probe_result,
        )
    }
}

fn classify_native_probe_result(result: Result<SshProbeOutput, NativeProbeError>) -> SshCapability {
    match result {
        Ok(output) => classify_probe_output(output),
        Err(NativeProbeError::NotFound) => {
            SshCapability::Unavailable(SshUnavailableReason::NotFound)
        }
        Err(NativeProbeError::Cancelled | NativeProbeError::TimedOut | NativeProbeError::Io) => {
            SshCapability::Unavailable(SshUnavailableReason::ProbeFailed)
        }
    }
}

fn classify_probe_output(output: SshProbeOutput) -> SshCapability {
    if !output.success {
        return SshCapability::Unavailable(SshUnavailableReason::ProbeFailed);
    }
    if output.stdout.len() > MAX_PROBE_STREAM_BYTES || output.stderr.len() > MAX_PROBE_STREAM_BYTES
    {
        return SshCapability::Unavailable(SshUnavailableReason::Unrecognized);
    }
    let version =
        parse_version_stream(&output.stderr).or_else(|| parse_version_stream(&output.stdout));
    let Some(version) = version else {
        return SshCapability::Unavailable(SshUnavailableReason::Unrecognized);
    };
    if version < MINIMUM_OPENSSH_VERSION {
        return SshCapability::Unavailable(SshUnavailableReason::TooOld {
            found: version,
            minimum: MINIMUM_OPENSSH_VERSION,
        });
    }
    SshCapability::Available(version)
}

#[derive(Debug)]
enum NativeProbeError {
    NotFound,
    Cancelled,
    TimedOut,
    Io,
}

struct ProbeCancelOnDrop {
    cancellation: SshCancellationToken,
    armed: bool,
}

impl ProbeCancelOnDrop {
    fn new(cancellation: SshCancellationToken) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProbeCancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

fn run_native_probe(
    executable: PathBuf,
    environment: SshProbeEnvironment,
    cancellation: &SshCancellationToken,
    deadline: Instant,
) -> Result<SshProbeOutput, NativeProbeError> {
    let mut command = Command::new(executable);
    command
        .arg("-V")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    environment.apply(&mut command);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            NativeProbeError::NotFound
        } else {
            NativeProbeError::Io
        }
    })?;
    let process_group = match libc::pid_t::try_from(child.id()) {
        Ok(process_group) => process_group,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(NativeProbeError::Io);
        }
    };
    let stdout = child.stdout.take().ok_or_else(|| {
        terminate_probe(&mut child, process_group);
        NativeProbeError::Io
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        terminate_probe(&mut child, process_group);
        NativeProbeError::Io
    })?;
    let stdout_reader = thread::spawn(move || read_probe_stream(stdout));
    let stderr_reader = thread::spawn(move || read_probe_stream(stderr));

    let status = loop {
        if cancellation.is_cancelled() {
            terminate_probe(&mut child, process_group);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(NativeProbeError::Cancelled);
        }
        if Instant::now() >= deadline {
            terminate_probe(&mut child, process_group);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(NativeProbeError::TimedOut);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                signal_probe_group(process_group);
                break status;
            }
            Ok(None) => thread::sleep(
                NATIVE_PROBE_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            ),
            Err(_) => {
                terminate_probe(&mut child, process_group);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(NativeProbeError::Io);
            }
        }
    };
    let stdout = join_probe_reader(stdout_reader)?;
    let stderr = join_probe_reader(stderr_reader)?;
    Ok(SshProbeOutput::new(status.success(), stdout, stderr))
}

fn read_probe_stream(mut stream: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(256);
    stream
        .by_ref()
        .take(u64::try_from(MAX_PROBE_STREAM_BYTES + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut output)?;
    Ok(output)
}

fn join_probe_reader(
    reader: thread::JoinHandle<io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, NativeProbeError> {
    reader
        .join()
        .map_err(|_| NativeProbeError::Io)?
        .map_err(|_| NativeProbeError::Io)
}

fn terminate_probe(child: &mut Child, process_group: libc::pid_t) {
    signal_probe_group(process_group);
    let _ = child.wait();
}

fn signal_probe_group(process_group: libc::pid_t) {
    // SAFETY: this is the positive process group of the child launched above with a private group.
    let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
}

fn parse_version_stream(stream: &[u8]) -> Option<OpenSshVersion> {
    if stream.is_empty() || stream.len() > MAX_PROBE_STREAM_BYTES {
        return None;
    }
    let output = std::str::from_utf8(stream).ok()?;
    let output = output.trim_end_matches(['\r', '\n']);
    if output.is_empty() || output.chars().any(char::is_control) {
        return None;
    }
    let version = output.strip_prefix("OpenSSH_")?;
    let major_end = version.find('.')?;
    let major = parse_version_component(&version[..major_end])?;
    let minor_and_suffix = &version[major_end + 1..];
    let minor_end = minor_and_suffix
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(minor_and_suffix.len());
    let minor = parse_version_component(&minor_and_suffix[..minor_end])?;
    Some(OpenSshVersion::new(major, minor))
}

fn parse_version_component(component: &str) -> Option<u16> {
    (!component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| component.parse().ok())?
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum SshCommandContextError {
    #[error("SSH command paths must be absolute and control-free")]
    UnsafePath,
}

pub(crate) struct SshCommandContext {
    executable: PathBuf,
    managed_config: PathBuf,
    destination: SshDestination,
    control_path: PathBuf,
}

impl SshCommandContext {
    pub(crate) fn new(
        managed_config: PathBuf,
        destination: SshDestination,
        control_path: PathBuf,
    ) -> Result<Self, SshCommandContextError> {
        if !is_safe_absolute_path(&managed_config) || !is_safe_absolute_path(&control_path) {
            return Err(SshCommandContextError::UnsafePath);
        }
        Ok(Self {
            executable: PathBuf::from(SSH_EXECUTABLE),
            managed_config,
            destination,
            control_path,
        })
    }

    pub(crate) fn master(&self) -> SshCommandSpec {
        let mut arguments = self.base_arguments();
        push_option(&mut arguments, OsString::from("ControlMaster=yes"));
        push_option(&mut arguments, self.control_path_option());
        push_option(&mut arguments, OsString::from("ControlPersist=no"));
        arguments.extend([OsString::from("-N"), OsString::from("-M")]);
        self.finish(arguments)
    }

    pub(crate) fn readiness_check(&self) -> SshCommandSpec {
        self.control_operation("check")
    }

    pub(crate) fn graceful_exit(&self) -> SshCommandSpec {
        self.control_operation("exit")
    }

    pub(crate) fn remote_utility(&self) -> SshCommandSpec {
        let mut arguments = self.child_arguments();
        push_option(&mut arguments, OsString::from("ClearAllForwardings=yes"));
        arguments.push(OsString::from("-T"));
        push_option(&mut arguments, OsString::from("RemoteCommand=none"));
        push_option(&mut arguments, OsString::from("RequestTTY=no"));
        push_option(&mut arguments, OsString::from("SessionType=default"));
        self.push_destination(&mut arguments);
        arguments.extend([OsString::from("/bin/sh"), OsString::from("-s")]);
        self.spec(arguments)
    }

    pub(crate) fn pane_channel(&self, command: ValidatedRemoteShellCommand) -> SshCommandSpec {
        let mut arguments = self.child_arguments();
        push_option(&mut arguments, OsString::from("ClearAllForwardings=yes"));
        arguments.push(OsString::from("-tt"));
        push_option(&mut arguments, OsString::from("RemoteCommand=none"));
        push_option(&mut arguments, OsString::from("RequestTTY=force"));
        push_option(&mut arguments, OsString::from("SessionType=default"));
        self.push_destination(&mut arguments);
        arguments.push(OsString::from(command.argument));
        self.spec(arguments)
    }

    #[cfg(test)]
    pub(crate) fn prepare_pane_channel(
        &self,
        command: ValidatedRemoteShellCommand,
    ) -> PreparedSshPaneChannelCommand {
        PreparedSshPaneChannelCommand::new(self.pane_channel(command), None, None)
    }

    fn control_operation(&self, operation: &str) -> SshCommandSpec {
        let mut arguments = self.child_arguments();
        arguments.extend([OsString::from("-O"), OsString::from(operation)]);
        self.finish(arguments)
    }

    fn base_arguments(&self) -> Vec<OsString> {
        vec![
            OsString::from("-F"),
            self.managed_config.as_os_str().to_owned(),
            OsString::from("-S"),
            self.control_path.as_os_str().to_owned(),
        ]
    }

    fn child_arguments(&self) -> Vec<OsString> {
        let mut arguments = self.base_arguments();
        push_option(&mut arguments, OsString::from("ControlMaster=no"));
        push_option(&mut arguments, OsString::from("ControlPersist=no"));
        push_option(
            &mut arguments,
            OsString::from("ProxyCommand=/usr/bin/false"),
        );
        arguments
    }

    fn finish(&self, mut arguments: Vec<OsString>) -> SshCommandSpec {
        self.push_destination(&mut arguments);
        self.spec(arguments)
    }

    fn push_destination(&self, arguments: &mut Vec<OsString>) {
        arguments.push(OsString::from("--"));
        arguments.push(OsString::from(self.destination.as_str()));
    }

    fn spec(&self, arguments: Vec<OsString>) -> SshCommandSpec {
        SshCommandSpec {
            executable: self.executable.clone(),
            arguments,
            pane_execution: None,
        }
    }

    fn control_path_option(&self) -> OsString {
        let mut option = OsString::from("ControlPath=");
        option.push(&self.control_path);
        option
    }
}

fn is_safe_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .as_os_str()
            .as_bytes()
            .iter()
            .any(|byte| byte.is_ascii_control())
}

fn push_option(arguments: &mut Vec<OsString>, option: OsString) {
    arguments.push(OsString::from("-o"));
    arguments.push(option);
}

pub(crate) struct SshCommandSpec {
    executable: PathBuf,
    arguments: Vec<OsString>,
    pane_execution: Option<SshPaneExecution>,
}

struct SshPaneExecution {
    capability: LiveConnectionCapability,
    environment: SshProcessEnvironment,
}

impl SshCommandSpec {
    #[cfg(test)]
    pub(super) fn for_test(executable: PathBuf, arguments: Vec<OsString>) -> Self {
        Self {
            executable,
            arguments,
            pane_execution: None,
        }
    }

    pub(crate) fn executable(&self) -> &OsStr {
        self.executable.as_os_str()
    }

    pub(crate) fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    pub(crate) fn into_pane_launch_parts(
        self,
    ) -> Result<(PathBuf, Vec<OsString>, Option<SshProcessEnvironment>), PreparedSshPaneChannelError>
    {
        let environment = match self.pane_execution {
            Some(execution) => {
                execution
                    .capability
                    .authorize()
                    .map_err(|_| PreparedSshPaneChannelError::Unavailable)?;
                Some(execution.environment)
            }
            None => {
                #[cfg(test)]
                {
                    None
                }
                #[cfg(not(test))]
                {
                    return Err(PreparedSshPaneChannelError::Unavailable);
                }
            }
        };
        Ok((self.executable, self.arguments, environment))
    }
}

#[derive(Clone)]
pub(crate) struct PreparedSshPaneChannelCommand {
    command: Arc<Mutex<Option<SshCommandSpec>>>,
    capability: Option<LiveConnectionCapability>,
    environment: Option<SshProcessEnvironment>,
}

impl PreparedSshPaneChannelCommand {
    pub(super) fn new(
        command: SshCommandSpec,
        capability: Option<LiveConnectionCapability>,
        environment: Option<SshProcessEnvironment>,
    ) -> Self {
        Self {
            command: Arc::new(Mutex::new(Some(command))),
            capability,
            environment,
        }
    }

    pub(crate) fn take(&self) -> Result<SshCommandSpec, PreparedSshPaneChannelError> {
        if self
            .capability
            .as_ref()
            .is_some_and(|capability| capability.authorize().is_err())
        {
            return Err(PreparedSshPaneChannelError::Unavailable);
        }
        let mut command = self
            .command
            .lock()
            .map_err(|_| PreparedSshPaneChannelError::Unavailable)?;
        let mut command = command
            .take()
            .ok_or(PreparedSshPaneChannelError::AlreadyConsumed)?;
        command.pane_execution = match (&self.capability, &self.environment) {
            (Some(capability), Some(environment)) => Some(SshPaneExecution {
                capability: capability.clone(),
                environment: environment.clone(),
            }),
            (None, None) => None,
            _ => return Err(PreparedSshPaneChannelError::Unavailable),
        };
        Ok(command)
    }
}

impl fmt::Debug for PreparedSshPaneChannelCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSshPaneChannelCommand")
            .finish_non_exhaustive()
    }
}

impl PartialEq for PreparedSshPaneChannelCommand {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.command, &other.command)
    }
}

impl Eq for PreparedSshPaneChannelCommand {}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum PreparedSshPaneChannelError {
    #[error("the prepared SSH Pane channel command has already been consumed")]
    AlreadyConsumed,
    #[error("the prepared SSH Pane channel command is unavailable")]
    Unavailable,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum RemoteShellCommandError {
    #[error("the remote login shell is missing")]
    MissingLoginShell,
    #[error("the remote login shell must be an absolute path")]
    RelativeLoginShell,
    #[error("the remote login shell must not contain control characters")]
    LoginShellControl,
    #[error("the remote login shell path is invalid")]
    InvalidLoginShellPath,
    #[error("the remote login shell path is too long")]
    LoginShellTooLong,
    #[error("the remote login shell is not supported")]
    UnsupportedLoginShell,
    #[error("the Remote Workspace Directory is too long to launch")]
    WorkspaceDirectoryTooLong,
    #[error("the remote Pane launch command is too long")]
    CommandTooLong,
}

pub(crate) struct ValidatedRemoteShellCommand {
    argument: String,
}

impl ValidatedRemoteShellCommand {
    #[cfg(test)]
    pub(crate) fn new(argument: String) -> Result<Self, RemoteShellCommandError> {
        if argument.is_empty() {
            return Err(RemoteShellCommandError::MissingLoginShell);
        }
        if argument.chars().any(char::is_control) {
            return Err(RemoteShellCommandError::LoginShellControl);
        }
        Ok(Self { argument })
    }
}

#[derive(Clone, Copy)]
enum SupportedRemoteLoginShell {
    PosixSh,
    Bash,
    Zsh,
    Fish,
    Nushell,
    Elvish,
}

impl SupportedRemoteLoginShell {
    fn from_basename(basename: &str) -> Result<Self, RemoteShellCommandError> {
        match basename {
            "sh" => Ok(Self::PosixSh),
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            "fish" => Ok(Self::Fish),
            "nu" | "nushell" => Ok(Self::Nushell),
            "elvish" => Ok(Self::Elvish),
            _ => Err(RemoteShellCommandError::UnsupportedLoginShell),
        }
    }

    const fn login_arguments(self) -> &'static [&'static str] {
        match self {
            // POSIX does not define `sh -l`. The channel already owns a PTY, so a conforming
            // `sh` starts interactively without a non-portable login option.
            Self::PosixSh => &[],
            Self::Bash => &["-l"],
            Self::Zsh => &["-l"],
            Self::Fish => &["-l"],
            Self::Nushell => &["-l"],
            Self::Elvish => &[],
        }
    }

    fn quote_directory(self, directory: &RemoteWorkspaceDirectory) -> String {
        match self {
            Self::Nushell => quote_remote_workspace_directory_for_nushell(directory),
            Self::Elvish => quote_remote_workspace_directory_for_elvish(directory),
            Self::PosixSh | Self::Bash | Self::Zsh | Self::Fish => {
                quote_remote_workspace_directory_for_posix(directory)
            }
        }
    }

    fn quote_login_shell(self, login_shell: &str) -> String {
        match self {
            Self::Nushell => quote_for_nushell(login_shell),
            Self::PosixSh | Self::Bash | Self::Zsh | Self::Fish | Self::Elvish => {
                quote_for_posix_shell(login_shell)
            }
        }
    }

    const fn success_separator(self) -> &'static str {
        match self {
            Self::Nushell | Self::Elvish => ";",
            Self::PosixSh | Self::Bash | Self::Zsh | Self::Fish => "&&",
        }
    }
}

/// Validated remote account metadata for one supported absolute login-shell path.
pub(crate) struct ValidatedRemoteLoginShell {
    path: String,
    kind: SupportedRemoteLoginShell,
}

impl ValidatedRemoteLoginShell {
    pub(crate) fn new(path: String) -> Result<Self, RemoteShellCommandError> {
        if path.is_empty() {
            return Err(RemoteShellCommandError::MissingLoginShell);
        }
        if path.len() > MAXIMUM_REMOTE_SHELL_VALUE_BYTES {
            return Err(RemoteShellCommandError::LoginShellTooLong);
        }
        if path.chars().any(char::is_control) {
            return Err(RemoteShellCommandError::LoginShellControl);
        }
        let Some(relative) = path.strip_prefix('/') else {
            return Err(RemoteShellCommandError::RelativeLoginShell);
        };
        if relative.is_empty()
            || relative
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err(RemoteShellCommandError::InvalidLoginShellPath);
        }
        let basename = relative
            .rsplit('/')
            .next()
            .ok_or(RemoteShellCommandError::InvalidLoginShellPath)?;
        let kind = SupportedRemoteLoginShell::from_basename(basename)?;
        Ok(Self { path, kind })
    }
}

/// Builds the sole supported remote Pane startup command.
///
/// The caller must separately revalidate the selected directory's physical identity before
/// creating a child. This builder preserves remote-string authority and performs no local path
/// conversion or filesystem access.
pub(crate) struct RemotePaneShellCommandBuilder<'a> {
    directory: &'a RemoteWorkspaceDirectory,
    login_shell: &'a ValidatedRemoteLoginShell,
}

impl<'a> RemotePaneShellCommandBuilder<'a> {
    pub(crate) const fn new(
        directory: &'a RemoteWorkspaceDirectory,
        login_shell: &'a ValidatedRemoteLoginShell,
    ) -> Self {
        Self {
            directory,
            login_shell,
        }
    }

    pub(crate) fn build(self) -> Result<ValidatedRemoteShellCommand, RemoteShellCommandError> {
        if self.directory.as_str().len() > MAXIMUM_REMOTE_SHELL_VALUE_BYTES {
            return Err(RemoteShellCommandError::WorkspaceDirectoryTooLong);
        }
        let kind = self.login_shell.kind;
        let directory = kind.quote_directory(self.directory);
        let login_shell = kind.quote_login_shell(&self.login_shell.path);
        let arguments = kind.login_arguments();
        let separator = kind.success_separator();
        let mut command = format!("cd {directory} {separator} exec {login_shell}");
        for argument in arguments {
            command.push(' ');
            command.push_str(argument);
        }
        if command.len() > MAXIMUM_REMOTE_PANE_COMMAND_BYTES {
            return Err(RemoteShellCommandError::CommandTooLong);
        }
        Ok(ValidatedRemoteShellCommand { argument: command })
    }
}

fn quote_remote_workspace_directory_for_posix(directory: &RemoteWorkspaceDirectory) -> String {
    match directory.as_str() {
        "~" => "\"${HOME}\"".to_owned(),
        value if value.starts_with("~/") => {
            format!("\"${{HOME}}\"{}", quote_for_posix_shell(&value[1..]))
        }
        value => quote_for_posix_shell(value),
    }
}

fn quote_remote_workspace_directory_for_elvish(directory: &RemoteWorkspaceDirectory) -> String {
    match directory.as_str() {
        "~" => "$E:HOME".to_owned(),
        value if value.starts_with("~/") => {
            format!("$E:HOME{}", quote_for_posix_shell(&value[1..]))
        }
        value => quote_for_posix_shell(value),
    }
}

fn quote_remote_workspace_directory_for_nushell(directory: &RemoteWorkspaceDirectory) -> String {
    match directory.as_str() {
        "~" | "~/" => "$nu.home-dir".to_owned(),
        value if value.starts_with("~/") => format!(
            "($nu.home-dir | path join {})",
            quote_for_nushell(&value[2..])
        ),
        value => quote_for_nushell(value),
    }
}

fn quote_for_posix_shell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn quote_for_nushell(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::future::Future;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    use crate::domain::{RemoteWorkspaceDirectory, SshDestination};

    use super::*;

    static NEXT_PROBE_SCRIPT: AtomicU64 = AtomicU64::new(0);

    struct ProbeScript(PathBuf);

    impl ProbeScript {
        fn new(body: &str) -> Self {
            let sequence = NEXT_PROBE_SCRIPT.fetch_add(1, Ordering::Relaxed);
            let path = PathBuf::from(format!(
                "/private/tmp/spaceterm-probe-{}-{sequence}",
                std::process::id()
            ));
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }

    impl Drop for ProbeScript {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    struct ThreadWake(std::thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    fn block_on_external<T>(future: impl Future<Output = T>) -> T {
        let mut future = Box::pin(future);
        let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
        let mut context = Context::from_waker(&waker);
        loop {
            match Pin::as_mut(&mut future).poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => thread::park(),
            }
        }
    }

    fn native_probe(executable: PathBuf, timeout: Duration) -> NativeSshProbeRunner {
        NativeSshProbeRunner::for_test(
            SshProcessEnvironment::new_without_authentication(PathBuf::from("/private/tmp"), None)
                .unwrap(),
            executable,
            timeout,
        )
    }

    fn startup_probe(
        executable: PathBuf,
        home: PathBuf,
        agent_socket: Option<OsString>,
    ) -> Result<NativeSshProbeRunner, SshProcessEnvironmentError> {
        let startup = StartupSshEnvironment::for_test(agent_socket);
        let mut runner = NativeSshProbeRunner::from_startup(home, &startup)?;
        runner.executable = executable;
        Ok(runner)
    }

    enum FakeProbeResult {
        Output(SshProbeOutput),
        Error(io::ErrorKind),
    }

    struct FakeProbeRunner {
        calls: RefCell<Vec<(PathBuf, Vec<OsString>)>>,
        result: FakeProbeResult,
    }

    impl FakeProbeRunner {
        fn output(success: bool, stdout: &[u8], stderr: &[u8]) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                result: FakeProbeResult::Output(SshProbeOutput::new(
                    success,
                    stdout.to_vec(),
                    stderr.to_vec(),
                )),
            }
        }

        fn error(kind: io::ErrorKind) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                result: FakeProbeResult::Error(kind),
            }
        }
    }

    impl SshProbeRunner for FakeProbeRunner {
        fn run(&self, executable: &Path, arguments: &[OsString]) -> io::Result<SshProbeOutput> {
            self.calls
                .borrow_mut()
                .push((executable.to_path_buf(), arguments.to_vec()));
            match &self.result {
                FakeProbeResult::Output(output) => Ok(output.clone()),
                FakeProbeResult::Error(kind) => Err(io::Error::from(*kind)),
            }
        }
    }

    fn context() -> SshCommandContext {
        SshCommandContext::new(
            PathBuf::from("/private/config/spaceterm/ssh_config"),
            SshDestination::new("root@fedora@orb".to_owned()).unwrap(),
            PathBuf::from("/private/runtime/spaceterm/ssh/control.sock"),
        )
        .unwrap()
    }

    fn arguments(spec: &SshCommandSpec) -> Vec<String> {
        spec.arguments()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    fn pane_command(
        directory: &str,
        login_shell: &str,
    ) -> Result<ValidatedRemoteShellCommand, RemoteShellCommandError> {
        let directory = RemoteWorkspaceDirectory::new(directory.to_owned()).unwrap();
        let login_shell = ValidatedRemoteLoginShell::new(login_shell.to_owned())?;
        RemotePaneShellCommandBuilder::new(&directory, &login_shell).build()
    }

    #[test]
    fn capability_probe_should_invoke_only_the_system_ssh_version_command() {
        let runner = FakeProbeRunner::output(true, b"", b"OpenSSH_9.9p2\n");

        let _ = probe_ssh_capability(&runner);

        assert_eq!(
            runner.calls.into_inner(),
            vec![(PathBuf::from("/usr/bin/ssh"), vec![OsString::from("-V")])]
        );
    }

    #[test]
    fn capability_probe_should_accept_an_apple_version_from_stderr() {
        let runner = FakeProbeRunner::output(true, b"", b"OpenSSH_9.9p2 Apple-1, LibreSSL 3.3.6\n");

        let capability = probe_ssh_capability(&runner);

        assert_eq!(
            capability,
            SshCapability::Available(OpenSshVersion::new(9, 9))
        );
    }

    #[test]
    fn capability_probe_should_accept_the_minimum_version_from_stdout() {
        let runner = FakeProbeRunner::output(true, b"OpenSSH_8.2\n", b"");

        let capability = probe_ssh_capability(&runner);

        assert_eq!(
            capability,
            SshCapability::Available(OpenSshVersion::new(8, 2))
        );
    }

    #[test]
    fn capability_probe_should_report_a_too_old_version() {
        let runner = FakeProbeRunner::output(true, b"", b"OpenSSH_8.1p1\n");

        let capability = probe_ssh_capability(&runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::TooOld {
                found: OpenSshVersion::new(8, 1),
                minimum: OpenSshVersion::new(8, 2),
            })
        );
    }

    #[test]
    fn capability_probe_should_report_not_found_without_an_io_message() {
        let runner = FakeProbeRunner::error(io::ErrorKind::NotFound);

        let capability = probe_ssh_capability(&runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::NotFound)
        );
    }

    #[test]
    fn capability_probe_should_report_unrecognized_control_output() {
        let runner = FakeProbeRunner::output(true, b"", b"OpenSSH_9.9\0secret");

        let capability = probe_ssh_capability(&runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::Unrecognized)
        );
    }

    #[test]
    fn capability_probe_should_report_unrecognized_oversized_output() {
        let runner = FakeProbeRunner::output(true, &vec![b'x'; MAX_PROBE_STREAM_BYTES + 1], b"");

        let capability = probe_ssh_capability(&runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::Unrecognized)
        );
    }

    #[test]
    fn capability_probe_should_report_probe_failed_for_a_failed_exit() {
        let runner = FakeProbeRunner::output(false, b"", b"OpenSSH_9.9p2\n");

        let capability = probe_ssh_capability(&runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::ProbeFailed)
        );
    }

    #[test]
    fn capability_probe_should_report_probe_failed_for_an_io_error() {
        let runner = FakeProbeRunner::error(io::ErrorKind::PermissionDenied);

        let capability = probe_ssh_capability(&runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::ProbeFailed)
        );
    }

    #[test]
    fn native_probe_should_report_not_found_without_exposing_an_io_message() {
        let runner = native_probe(
            PathBuf::from("/private/tmp/spaceterm-missing-ssh"),
            Duration::from_secs(1),
        );

        assert_eq!(
            block_on_external(runner.probe(SshCancellationToken::default())),
            SshCapability::Unavailable(SshUnavailableReason::NotFound)
        );
    }

    #[test]
    fn native_probe_should_force_cleanup_at_its_deadline() {
        let script = ProbeScript::new("sleep 30");
        let runner = native_probe(script.0.clone(), Duration::from_millis(20));

        assert_eq!(
            block_on_external(runner.probe(SshCancellationToken::default())),
            SshCapability::Unavailable(SshUnavailableReason::ProbeFailed)
        );
    }

    #[test]
    fn native_probe_should_reject_malformed_and_too_old_versions() {
        let malformed = ProbeScript::new("printf 'not-openssh\\n' >&2");
        let old = ProbeScript::new("printf 'OpenSSH_8.1p1\\n' >&2");

        assert_eq!(
            block_on_external(
                native_probe(malformed.0.clone(), Duration::from_secs(1))
                    .probe(SshCancellationToken::default())
            ),
            SshCapability::Unavailable(SshUnavailableReason::Unrecognized)
        );
        assert_eq!(
            block_on_external(
                native_probe(old.0.clone(), Duration::from_secs(1))
                    .probe(SshCancellationToken::default())
            ),
            SshCapability::Unavailable(SshUnavailableReason::TooOld {
                found: OpenSshVersion::new(8, 1),
                minimum: OpenSshVersion::new(8, 2),
            })
        );
    }

    #[test]
    fn native_probe_should_use_only_the_sanitized_captured_environment() {
        let script = ProbeScript::new(
            r#"[ "$HOME" = /private/tmp ] || exit 4
[ "$PATH" = /usr/bin:/bin ] || exit 5
[ -z "${SPACETERM_AMBIENT_PROBE+x}" ] || exit 6
printf 'OpenSSH_9.9p2 Apple-1, LibreSSL 3.3.6\n' >&2"#,
        );
        let runner = native_probe(script.0.clone(), Duration::from_secs(1));

        assert_eq!(
            block_on_external(runner.probe(SshCancellationToken::default())),
            SshCapability::Available(OpenSshVersion::new(9, 9))
        );
    }

    #[test]
    fn startup_probe_should_use_no_askpass_environment_and_keep_the_captured_agent() {
        let script = ProbeScript::new(
            r#"[ "$HOME" = /private/tmp ] || exit 4
[ "$PATH" = /usr/bin:/bin ] || exit 5
[ "$SSH_AUTH_SOCK" = /private/tmp/ssh-agent.sock ] || exit 6
[ -z "${SSH_ASKPASS+x}" ] || exit 7
[ -z "${SSH_ASKPASS_REQUIRE+x}" ] || exit 8
printf 'OpenSSH_9.9p2 Apple-1, LibreSSL 3.3.6\n' >&2"#,
        );
        let runner = startup_probe(
            script.0.clone(),
            PathBuf::from("/private/tmp"),
            Some(OsString::from("/private/tmp/ssh-agent.sock")),
        )
        .unwrap();

        assert_eq!(
            runner.probe_blocking(),
            SshCapability::Available(OpenSshVersion::new(9, 9))
        );
    }

    #[test]
    fn startup_probe_should_reject_unsafe_captured_paths_before_launch() {
        assert!(matches!(
            startup_probe(
                PathBuf::from("/usr/bin/ssh"),
                PathBuf::from("relative-home"),
                None,
            ),
            Err(SshProcessEnvironmentError::UnsafeHome)
        ));
        assert!(matches!(
            startup_probe(
                PathBuf::from("/usr/bin/ssh"),
                PathBuf::from("/private/tmp"),
                Some(OsString::from("relative-agent")),
            ),
            Err(SshProcessEnvironmentError::UnsafeAgentSocket)
        ));
    }

    #[test]
    fn native_probe_cancellation_should_terminate_the_private_process_group() {
        let pid_file = PathBuf::from(format!(
            "/private/tmp/spaceterm-probe-cancel-{}.pid",
            std::process::id()
        ));
        let script = ProbeScript::new(&format!("echo $$ > '{}'; sleep 30", pid_file.display()));
        let runner = native_probe(script.0.clone(), Duration::from_secs(5));
        let cancellation = SshCancellationToken::default();
        let canceller = cancellation.clone();
        let cancel_thread = thread::spawn(move || {
            for _ in 0..100 {
                if pid_file.exists() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            canceller.cancel();
            pid_file
        });

        assert_eq!(
            block_on_external(runner.probe(cancellation)),
            SshCapability::Unavailable(SshUnavailableReason::ProbeFailed)
        );
        let pid_file = cancel_thread.join().unwrap();
        let process: libc::pid_t = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let missing = (0..100).any(|_| {
            // SAFETY: signal zero checks process existence and dereferences no pointers.
            let missing = unsafe { libc::kill(process, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if !missing {
                thread::sleep(Duration::from_millis(10));
            }
            missing
        });
        let _ = fs::remove_file(pid_file);
        assert!(missing);
    }

    #[test]
    fn master_spec_should_pin_config_socket_and_master_policy() {
        let spec = context().master();

        assert_eq!(
            arguments(&spec),
            [
                "-F",
                "/private/config/spaceterm/ssh_config",
                "-S",
                "/private/runtime/spaceterm/ssh/control.sock",
                "-o",
                "ControlMaster=yes",
                "-o",
                "ControlPath=/private/runtime/spaceterm/ssh/control.sock",
                "-o",
                "ControlPersist=no",
                "-N",
                "-M",
                "--",
                "root@fedora@orb",
            ]
        );
    }

    #[test]
    fn readiness_spec_should_target_only_the_private_master() {
        let spec = context().readiness_check();

        assert_eq!(
            arguments(&spec),
            [
                "-F",
                "/private/config/spaceterm/ssh_config",
                "-S",
                "/private/runtime/spaceterm/ssh/control.sock",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPersist=no",
                "-o",
                "ProxyCommand=/usr/bin/false",
                "-O",
                "check",
                "--",
                "root@fedora@orb",
            ]
        );
    }

    #[test]
    fn graceful_exit_spec_should_target_only_the_private_master() {
        let spec = context().graceful_exit();

        assert_eq!(
            arguments(&spec),
            [
                "-F",
                "/private/config/spaceterm/ssh_config",
                "-S",
                "/private/runtime/spaceterm/ssh/control.sock",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPersist=no",
                "-o",
                "ProxyCommand=/usr/bin/false",
                "-O",
                "exit",
                "--",
                "root@fedora@orb",
            ]
        );
    }

    #[test]
    fn utility_spec_should_disable_external_master_forwarding_and_tty_behavior() {
        let spec = context().remote_utility();

        assert_eq!(
            arguments(&spec),
            [
                "-F",
                "/private/config/spaceterm/ssh_config",
                "-S",
                "/private/runtime/spaceterm/ssh/control.sock",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPersist=no",
                "-o",
                "ProxyCommand=/usr/bin/false",
                "-o",
                "ClearAllForwardings=yes",
                "-T",
                "-o",
                "RemoteCommand=none",
                "-o",
                "RequestTTY=no",
                "-o",
                "SessionType=default",
                "--",
                "root@fedora@orb",
                "/bin/sh",
                "-s",
            ]
        );
    }

    #[test]
    fn pane_spec_should_force_a_tty_and_append_one_validated_remote_argument() {
        let command = pane_command("/srv/project", "/bin/zsh").unwrap();

        let spec = context().pane_channel(command);

        assert_eq!(
            arguments(&spec),
            [
                "-F",
                "/private/config/spaceterm/ssh_config",
                "-S",
                "/private/runtime/spaceterm/ssh/control.sock",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPersist=no",
                "-o",
                "ProxyCommand=/usr/bin/false",
                "-o",
                "ClearAllForwardings=yes",
                "-tt",
                "-o",
                "RemoteCommand=none",
                "-o",
                "RequestTTY=force",
                "-o",
                "SessionType=default",
                "--",
                "root@fedora@orb",
                "cd '/srv/project' && exec '/bin/zsh' -l",
            ]
        );
    }

    #[test]
    fn pane_command_should_quote_hostile_remote_values_as_one_posix_command() {
        let command = pane_command(
            "/srv/-project dir/it's $(touch nope); ü",
            "/opt/shell dir/it's/zsh",
        )
        .unwrap();

        assert_eq!(
            command.argument,
            "cd '/srv/-project dir/it'\"'\"'s $(touch nope); ü' && exec '/opt/shell dir/it'\"'\"'s/zsh' -l"
        );
    }

    #[test]
    fn pane_command_should_expand_only_the_remote_home_prefix() {
        let command = pane_command("~/project dir/it's", "/bin/bash").unwrap();

        assert_eq!(
            command.argument,
            "cd \"${HOME}\"'/project dir/it'\"'\"'s' && exec '/bin/bash' -l"
        );
    }

    #[test]
    fn nushell_pane_command_should_quote_values_and_expand_remote_home() {
        let command = pane_command(
            "~/project \"quoted\" it's $(touch nope); ü",
            "/opt/shell \"quoted\"/nu",
        )
        .unwrap();

        assert_eq!(
            command.argument,
            "cd ($nu.home-dir | path join \"project \\\"quoted\\\" it's $(touch nope); ü\") ; exec \"/opt/shell \\\"quoted\\\"/nu\" -l"
        );
    }

    #[test]
    fn pane_command_should_use_explicit_arguments_for_every_supported_shell() {
        let cases = [
            ("/bin/sh", "cd '/srv/project' && exec '/bin/sh'"),
            (
                "/usr/local/bin/bash",
                "cd '/srv/project' && exec '/usr/local/bin/bash' -l",
            ),
            (
                "/opt/bin/zsh",
                "cd '/srv/project' && exec '/opt/bin/zsh' -l",
            ),
            (
                "/opt/bin/fish",
                "cd '/srv/project' && exec '/opt/bin/fish' -l",
            ),
            (
                "/opt/bin/nu",
                "cd \"/srv/project\" ; exec \"/opt/bin/nu\" -l",
            ),
            (
                "/opt/bin/nushell",
                "cd \"/srv/project\" ; exec \"/opt/bin/nushell\" -l",
            ),
            (
                "/opt/bin/elvish",
                "cd '/srv/project' ; exec '/opt/bin/elvish'",
            ),
        ];

        for (shell, expected) in cases {
            let command = pane_command("/srv/project", shell).unwrap();
            assert_eq!(command.argument, expected);
        }
    }

    #[test]
    fn posix_sh_pane_command_should_not_require_a_nonstandard_login_option() {
        let sequence = NEXT_PROBE_SCRIPT.fetch_add(1, Ordering::Relaxed);
        let test_root = PathBuf::from(format!(
            "/private/tmp/spaceterm-posix-sh-{}-{sequence}",
            std::process::id()
        ));
        let workspace = test_root.join("workspace with spaces");
        let fake_bin = test_root.join("bin");
        let fake_shell = fake_bin.join("sh");
        let argument_count = test_root.join("argument-count");
        let working_directory = test_root.join("working-directory");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        fs::write(
            &fake_shell,
            br#"#!/bin/sh
if [ "$#" -ne 0 ]; then
    exit 64
fi
printf '%s\n' "$#" > "$SPACETERM_ARGUMENT_COUNT"
pwd -P > "$SPACETERM_WORKING_DIRECTORY"
"#,
        )
        .unwrap();
        fs::set_permissions(&fake_shell, fs::Permissions::from_mode(0o700)).unwrap();
        let command =
            pane_command(workspace.to_str().unwrap(), fake_shell.to_str().unwrap()).unwrap();

        let status = Command::new("/bin/sh")
            .args(["-c", &command.argument])
            .env_clear()
            .env("SPACETERM_ARGUMENT_COUNT", &argument_count)
            .env("SPACETERM_WORKING_DIRECTORY", &working_directory)
            .status()
            .unwrap();

        assert!(status.success());
        assert_eq!(fs::read_to_string(argument_count).unwrap(), "0\n");
        assert_eq!(
            fs::read_to_string(working_directory).unwrap(),
            format!("{}\n", workspace.display())
        );
        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn login_shell_should_reject_missing_relative_control_and_unknown_metadata() {
        let cases = [
            ("", RemoteShellCommandError::MissingLoginShell),
            ("bin/zsh", RemoteShellCommandError::RelativeLoginShell),
            (
                "/bin/zsh\nforged",
                RemoteShellCommandError::LoginShellControl,
            ),
            (
                "/bin/unknown",
                RemoteShellCommandError::UnsupportedLoginShell,
            ),
        ];

        for (shell, expected) in cases {
            assert_eq!(
                ValidatedRemoteLoginShell::new(shell.to_owned()).err(),
                Some(expected)
            );
        }
    }

    #[test]
    fn login_shell_should_reject_non_normal_absolute_paths() {
        for shell in ["/bin/../bin/zsh", "/bin//zsh", "/bin/zsh/"] {
            assert_eq!(
                ValidatedRemoteLoginShell::new(shell.to_owned()).err(),
                Some(RemoteShellCommandError::InvalidLoginShellPath)
            );
        }
    }

    #[test]
    fn pane_command_should_reject_oversized_directory_and_shell_values() {
        let oversized_directory = RemoteWorkspaceDirectory::new(format!(
            "/{}",
            "d".repeat(MAXIMUM_REMOTE_SHELL_VALUE_BYTES)
        ))
        .unwrap();
        let shell = ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap();
        assert_eq!(
            RemotePaneShellCommandBuilder::new(&oversized_directory, &shell)
                .build()
                .err(),
            Some(RemoteShellCommandError::WorkspaceDirectoryTooLong)
        );

        let oversized_shell = format!("/{}/zsh", "s".repeat(MAXIMUM_REMOTE_SHELL_VALUE_BYTES));
        assert_eq!(
            ValidatedRemoteLoginShell::new(oversized_shell).err(),
            Some(RemoteShellCommandError::LoginShellTooLong)
        );
    }

    #[test]
    fn pane_command_should_reject_oversized_quoted_output() {
        let directory = RemoteWorkspaceDirectory::new(format!(
            "/{}",
            "'".repeat(MAXIMUM_REMOTE_SHELL_VALUE_BYTES - 1)
        ))
        .unwrap();
        let shell = ValidatedRemoteLoginShell::new(format!(
            "/{}/zsh",
            "'".repeat(MAXIMUM_REMOTE_SHELL_VALUE_BYTES - 5)
        ))
        .unwrap();

        assert_eq!(
            RemotePaneShellCommandBuilder::new(&directory, &shell)
                .build()
                .err(),
            Some(RemoteShellCommandError::CommandTooLong)
        );
    }

    #[test]
    fn prepared_pane_command_should_preserve_exact_argv_and_be_single_use() {
        let prepared =
            context().prepare_pane_channel(pane_command("/srv/project", "/bin/zsh").unwrap());
        let duplicate_owner = prepared.clone();

        let spec = prepared.take().unwrap();

        assert_eq!(spec.executable(), OsStr::new("/usr/bin/ssh"));
        assert_eq!(
            arguments(&spec).last().map(String::as_str),
            Some("cd '/srv/project' && exec '/bin/zsh' -l")
        );
        assert_eq!(
            duplicate_owner.take().err(),
            Some(PreparedSshPaneChannelError::AlreadyConsumed)
        );
    }

    #[test]
    fn prepared_pane_command_debug_should_redact_command_context() {
        let prepared = context().prepare_pane_channel(
            pane_command("/srv/sensitive-project", "/sensitive/shell/zsh").unwrap(),
        );

        let debug = format!("{prepared:?}");

        assert_eq!(debug, "PreparedSshPaneChannelCommand { .. }");
        assert!(!debug.contains("root@fedora@orb"));
        assert!(!debug.contains("sensitive"));
        assert!(!debug.contains("control.sock"));
    }

    #[test]
    fn all_specs_should_use_the_exact_system_executable() {
        let context = context();
        let command = pane_command("/srv/project", "/bin/fish").unwrap();
        let specs = [
            context.master(),
            context.readiness_check(),
            context.graceful_exit(),
            context.remote_utility(),
            context.pane_channel(command),
        ];

        assert!(
            specs
                .iter()
                .all(|spec| spec.executable() == OsStr::new("/usr/bin/ssh"))
        );
    }

    #[test]
    fn channel_specs_should_make_direct_connection_fallback_impossible() {
        let context = context();
        let command = pane_command("/srv/project", "/bin/fish").unwrap();
        let specs = [context.remote_utility(), context.pane_channel(command)];

        assert!(specs.iter().all(|spec| {
            spec.arguments()
                .windows(2)
                .any(|pair| pair[0] == "-o" && pair[1] == "ProxyCommand=/usr/bin/false")
        }));
    }

    #[test]
    fn command_context_should_reject_relative_config_or_control_paths() {
        let destination = SshDestination::new("host".to_owned()).unwrap();

        let error = SshCommandContext::new(
            PathBuf::from("relative/config"),
            destination,
            PathBuf::from("relative/control"),
        )
        .err();

        assert_eq!(error, Some(SshCommandContextError::UnsafePath));
    }
}
