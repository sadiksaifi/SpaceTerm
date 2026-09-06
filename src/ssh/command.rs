use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;

use super::cancellation::SshCancellationToken;
use super::live_connection::LiveConnectionCapability;
use super::process::{
    CapturedProcessError, SshProbeEnvironment, SshProcessAdapter, SshProcessEnvironment,
    SshProcessEnvironmentError, SshProcessMechanismError, run_probe_process,
};
use super::startup_environment::StartupSshEnvironment;
use crate::domain::{RemoteWorkspaceDirectory, SshDestination};

const MINIMUM_OPENSSH_VERSION: OpenSshVersion = OpenSshVersion::new(8, 2);
const MAX_PROBE_STREAM_BYTES: usize = 4 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
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
    #[error("the selected OpenSSH client is unavailable")]
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

#[derive(Clone, Eq, PartialEq)]
/// Capture-once validated host selection for the OpenSSH executable.
pub(crate) struct OpenSshExecutable(PathBuf);

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum OpenSshExecutableError {
    #[error("the selected OpenSSH executable is invalid")]
    UnsafePath,
}

impl OpenSshExecutable {
    pub(crate) fn new(path: PathBuf) -> Result<Self, OpenSshExecutableError> {
        if !is_safe_absolute_path(&path) {
            return Err(OpenSshExecutableError::UnsafePath);
        }
        Ok(Self(path))
    }

    pub(super) fn as_path(&self) -> &Path {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self(PathBuf::from("/test/ssh"))
    }

    fn into_path(self) -> PathBuf {
        self.0
    }
}

impl fmt::Debug for OpenSshExecutable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpenSshExecutable(<redacted>)")
    }
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

#[cfg(test)]
pub(crate) trait SshProbeRunner {
    fn run(
        &self,
        executable: &OpenSshExecutable,
        arguments: &[OsString],
    ) -> Result<SshProbeOutput, SshProcessMechanismError>;
}

#[cfg(test)]
pub(crate) fn probe_ssh_capability(
    executable: &OpenSshExecutable,
    runner: &impl SshProbeRunner,
) -> SshCapability {
    let output = match runner.run(executable, &[OsString::from("-V")]) {
        Ok(output) => output,
        Err(SshProcessMechanismError::NotFound) => {
            return SshCapability::Unavailable(SshUnavailableReason::NotFound);
        }
        Err(_) => return SshCapability::Unavailable(SshUnavailableReason::ProbeFailed),
    };
    classify_probe_output(output)
}

#[derive(Clone)]
pub(crate) struct SshCapabilityProbe<A: SshProcessAdapter> {
    executable: OpenSshExecutable,
    environment: SshProbeEnvironment,
    adapter: A,
    timeout: Duration,
}

impl<A: SshProcessAdapter> SshCapabilityProbe<A> {
    pub(crate) fn from_startup(
        executable: OpenSshExecutable,
        home: PathBuf,
        startup: &StartupSshEnvironment,
        adapter: A,
    ) -> Result<Self, SshProcessEnvironmentError> {
        Ok(Self {
            executable,
            environment: SshProbeEnvironment::new(home, startup)?,
            adapter,
            timeout: PROBE_TIMEOUT,
        })
    }

    pub(crate) fn probe_blocking(&self) -> SshCapability {
        let cancellation = SshCancellationToken::default();
        classify_supervised_probe_result(run_probe_process(
            &self.adapter,
            &self.command(),
            &self.environment,
            MAX_PROBE_STREAM_BYTES + 1,
            &cancellation,
            Instant::now()
                .checked_add(self.timeout)
                .unwrap_or_else(Instant::now),
        ))
    }

    fn command(&self) -> SshCommandSpec {
        SshCommandSpec::new(self.executable.clone(), vec![OsString::from("-V")])
    }
}

fn classify_supervised_probe_result(
    result: Result<super::process::CapturedProcessOutput, CapturedProcessError>,
) -> SshCapability {
    match result {
        Ok(output) => classify_probe_output(SshProbeOutput::new(
            output.exit.is_success(),
            output.stdout,
            output.stderr,
        )),
        Err(CapturedProcessError::Operation(SshProcessMechanismError::NotFound)) => {
            SshCapability::Unavailable(SshUnavailableReason::NotFound)
        }
        Err(CapturedProcessError::OutputTooLarge) => {
            SshCapability::Unavailable(SshUnavailableReason::Unrecognized)
        }
        Err(
            CapturedProcessError::Cancelled
            | CapturedProcessError::TimedOut
            | CapturedProcessError::Operation(_),
        ) => SshCapability::Unavailable(SshUnavailableReason::ProbeFailed),
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
    executable: OpenSshExecutable,
    managed_config: PathBuf,
    destination: SshDestination,
    control_path: PathBuf,
}

impl SshCommandContext {
    pub(crate) fn new(
        executable: OpenSshExecutable,
        managed_config: PathBuf,
        destination: SshDestination,
        control_path: PathBuf,
    ) -> Result<Self, SshCommandContextError> {
        if !is_safe_absolute_path(&managed_config) || !is_safe_absolute_path(&control_path) {
            return Err(SshCommandContextError::UnsafePath);
        }
        Ok(Self {
            executable,
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
        arguments.push(OsString::from("-N"));
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
    path.is_absolute() && !path.to_string_lossy().chars().any(char::is_control)
}

fn push_option(arguments: &mut Vec<OsString>, option: OsString) {
    arguments.push(OsString::from("-o"));
    arguments.push(option);
}

pub(crate) struct SshCommandSpec {
    executable: OpenSshExecutable,
    arguments: Vec<OsString>,
    pane_execution: Option<SshPaneExecution>,
}

struct SshPaneExecution {
    capability: LiveConnectionCapability,
    environment: SshProcessEnvironment,
}

impl SshCommandSpec {
    fn new(executable: OpenSshExecutable, arguments: Vec<OsString>) -> Self {
        Self {
            executable,
            arguments,
            pane_execution: None,
        }
    }

    #[cfg(test)]
    pub(super) fn for_test(executable: PathBuf, arguments: Vec<OsString>) -> Self {
        Self::new(OpenSshExecutable::new(executable).unwrap(), arguments)
    }

    pub(crate) fn executable(&self) -> &OsStr {
        self.executable.as_path().as_os_str()
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
        Ok((self.executable.into_path(), self.arguments, environment))
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
    #[error("the POSIX sh login shell requires a verified login option")]
    PosixShLoginCapabilityRequired,
    #[error("the remote login-shell capability does not match the configured shell")]
    InvalidLoginShellCapability,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
            Self::PosixSh => &["-l"],
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

    const fn launch_prefix(self) -> &'static str {
        match self {
            Self::PosixSh | Self::Bash | Self::Zsh => "SPACETERM='1' COLORTERM='truecolor' exec",
            Self::Fish => "set -lx SPACETERM '1' && set -lx COLORTERM 'truecolor' && exec",
            Self::Nushell => "$env.SPACETERM = \"1\"; $env.COLORTERM = \"truecolor\"; exec",
            Self::Elvish => "set E:SPACETERM = '1'; set E:COLORTERM = 'truecolor'; exec",
        }
    }
}

/// Capability result produced by remote account discovery for a configured POSIX `sh`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PosixShLoginCapability {
    /// The configured login shell is not POSIX `sh`.
    NotApplicable,
    /// The configured `sh` accepted its implementation's login option under supervision.
    LoginOptionSupported,
}

/// Validated remote account metadata for one supported absolute login-shell path.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ValidatedRemoteLoginShell {
    path: String,
    kind: SupportedRemoteLoginShell,
}

impl ValidatedRemoteLoginShell {
    /// Validates a supported shell that does not require a separately discovered capability.
    ///
    /// POSIX does not standardize `sh -l`, so callers must use [`Self::from_discovery`] for `sh`.
    pub(crate) fn new(path: String) -> Result<Self, RemoteShellCommandError> {
        let shell = Self::parse(path)?;
        if shell.kind == SupportedRemoteLoginShell::PosixSh {
            return Err(RemoteShellCommandError::PosixShLoginCapabilityRequired);
        }
        Ok(shell)
    }

    /// Binds the remote capability observation to the exact configured login-shell path.
    pub(crate) fn from_discovery(
        path: String,
        posix_sh_capability: PosixShLoginCapability,
    ) -> Result<Self, RemoteShellCommandError> {
        let shell = Self::parse(path)?;
        match (shell.kind, posix_sh_capability) {
            (SupportedRemoteLoginShell::PosixSh, PosixShLoginCapability::LoginOptionSupported) => {
                Ok(shell)
            }
            (SupportedRemoteLoginShell::PosixSh, PosixShLoginCapability::NotApplicable) => {
                Err(RemoteShellCommandError::PosixShLoginCapabilityRequired)
            }
            (_, PosixShLoginCapability::NotApplicable) => Ok(shell),
            (_, PosixShLoginCapability::LoginOptionSupported) => {
                Err(RemoteShellCommandError::InvalidLoginShellCapability)
            }
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.path
    }

    fn parse(path: String) -> Result<Self, RemoteShellCommandError> {
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

impl fmt::Debug for ValidatedRemoteLoginShell {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedRemoteLoginShell")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
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
        let launch_prefix = kind.launch_prefix();
        let mut command = format!("cd {directory} {separator} {launch_prefix} {login_shell}");
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
    use std::path::PathBuf;

    use crate::domain::{RemoteWorkspaceDirectory, SshDestination};

    use super::*;

    enum FakeProbeResult {
        Output(SshProbeOutput),
        Error(SshProcessMechanismError),
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

        fn error(error: SshProcessMechanismError) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                result: FakeProbeResult::Error(error),
            }
        }
    }

    impl SshProbeRunner for FakeProbeRunner {
        fn run(
            &self,
            executable: &OpenSshExecutable,
            arguments: &[OsString],
        ) -> Result<SshProbeOutput, SshProcessMechanismError> {
            self.calls
                .borrow_mut()
                .push((executable.as_path().to_path_buf(), arguments.to_vec()));
            match &self.result {
                FakeProbeResult::Output(output) => Ok(output.clone()),
                FakeProbeResult::Error(error) => Err(*error),
            }
        }
    }

    fn executable() -> OpenSshExecutable {
        OpenSshExecutable::new(PathBuf::from("/selected/openssh")).unwrap()
    }

    fn context() -> SshCommandContext {
        SshCommandContext::new(
            executable(),
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

    pub(super) fn pane_command(
        directory: &str,
        login_shell: &str,
    ) -> Result<ValidatedRemoteShellCommand, RemoteShellCommandError> {
        let directory = RemoteWorkspaceDirectory::new(directory.to_owned()).unwrap();
        let login_shell = if login_shell.rsplit('/').next() == Some("sh") {
            ValidatedRemoteLoginShell::from_discovery(
                login_shell.to_owned(),
                PosixShLoginCapability::LoginOptionSupported,
            )?
        } else {
            ValidatedRemoteLoginShell::new(login_shell.to_owned())?
        };
        RemotePaneShellCommandBuilder::new(&directory, &login_shell).build()
    }

    #[test]
    fn capability_probe_should_invoke_only_the_selected_ssh_version_command() {
        let runner = FakeProbeRunner::output(true, b"", b"OpenSSH_9.9p2\n");

        let _ = probe_ssh_capability(&executable(), &runner);

        assert_eq!(
            runner.calls.into_inner(),
            vec![(
                PathBuf::from("/selected/openssh"),
                vec![OsString::from("-V")]
            )]
        );
    }

    #[test]
    fn capability_probe_should_accept_an_apple_version_from_stderr() {
        let runner = FakeProbeRunner::output(true, b"", b"OpenSSH_9.9p2 Apple-1, LibreSSL 3.3.6\n");

        let capability = probe_ssh_capability(&executable(), &runner);

        assert_eq!(
            capability,
            SshCapability::Available(OpenSshVersion::new(9, 9))
        );
    }

    #[test]
    fn capability_probe_should_accept_the_minimum_version_from_stdout() {
        let runner = FakeProbeRunner::output(true, b"OpenSSH_8.2\n", b"");

        let capability = probe_ssh_capability(&executable(), &runner);

        assert_eq!(
            capability,
            SshCapability::Available(OpenSshVersion::new(8, 2))
        );
    }

    #[test]
    fn capability_probe_should_report_a_too_old_version() {
        let runner = FakeProbeRunner::output(true, b"", b"OpenSSH_8.1p1\n");

        let capability = probe_ssh_capability(&executable(), &runner);

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
        let runner = FakeProbeRunner::error(SshProcessMechanismError::NotFound);

        let capability = probe_ssh_capability(&executable(), &runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::NotFound)
        );
    }

    #[test]
    fn capability_probe_should_report_unrecognized_control_output() {
        let runner = FakeProbeRunner::output(true, b"", b"OpenSSH_9.9\0secret");

        let capability = probe_ssh_capability(&executable(), &runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::Unrecognized)
        );
    }

    #[test]
    fn capability_probe_should_report_unrecognized_oversized_output() {
        let runner = FakeProbeRunner::output(true, &vec![b'x'; MAX_PROBE_STREAM_BYTES + 1], b"");

        let capability = probe_ssh_capability(&executable(), &runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::Unrecognized)
        );
    }

    #[test]
    fn capability_probe_should_report_probe_failed_for_a_failed_exit() {
        let runner = FakeProbeRunner::output(false, b"", b"OpenSSH_9.9p2\n");

        let capability = probe_ssh_capability(&executable(), &runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::ProbeFailed)
        );
    }

    #[test]
    fn capability_probe_should_report_probe_failed_for_an_io_error() {
        let runner = FakeProbeRunner::error(SshProcessMechanismError::LaunchFailed);

        let capability = probe_ssh_capability(&executable(), &runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::ProbeFailed)
        );
    }

    #[test]
    fn openssh_executable_should_require_a_safe_absolute_path_and_redact_debug() {
        let executable = executable();

        assert_eq!(format!("{executable:?}"), "OpenSshExecutable(<redacted>)");
        assert_eq!(
            OpenSshExecutable::new(PathBuf::from("relative/ssh")).unwrap_err(),
            OpenSshExecutableError::UnsafePath
        );
        assert!(!format!("{executable:?}").contains("selected"));
    }

    #[test]
    fn master_spec_should_pin_config_socket_and_enable_master_exactly_once() {
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
                "--",
                "root@fedora@orb",
            ]
        );
    }

    #[test]
    fn master_spec_should_not_request_confirmation_for_mux_clients() {
        let arguments = arguments(&context().master());
        let explicit_master_enables = arguments
            .iter()
            .filter(|argument| argument.as_str() == "ControlMaster=yes")
            .count();
        let short_master_enables = arguments
            .iter()
            .filter(|argument| argument.as_str() == "-M")
            .count();

        assert_eq!(explicit_master_enables, 1);
        assert_eq!(
            short_master_enables, 0,
            "a second master enable makes OpenSSH request confirmation for mux clients"
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
                "cd '/srv/project' && SPACETERM='1' COLORTERM='truecolor' exec '/bin/zsh' -l",
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
            "cd '/srv/-project dir/it'\"'\"'s $(touch nope); ü' && SPACETERM='1' COLORTERM='truecolor' exec '/opt/shell dir/it'\"'\"'s/zsh' -l"
        );
    }

    #[test]
    fn pane_command_should_expand_only_the_remote_home_prefix() {
        let command = pane_command("~/project dir/it's", "/bin/bash").unwrap();

        assert_eq!(
            command.argument,
            "cd \"${HOME}\"'/project dir/it'\"'\"'s' && SPACETERM='1' COLORTERM='truecolor' exec '/bin/bash' -l"
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
            "cd ($nu.home-dir | path join \"project \\\"quoted\\\" it's $(touch nope); ü\") ; $env.SPACETERM = \"1\"; $env.COLORTERM = \"truecolor\"; exec \"/opt/shell \\\"quoted\\\"/nu\" -l"
        );
    }

    #[test]
    fn pane_command_should_use_explicit_arguments_for_every_supported_shell() {
        let cases = [
            (
                "/bin/sh",
                "cd '/srv/project' && SPACETERM='1' COLORTERM='truecolor' exec '/bin/sh' -l",
            ),
            (
                "/usr/local/bin/bash",
                "cd '/srv/project' && SPACETERM='1' COLORTERM='truecolor' exec '/usr/local/bin/bash' -l",
            ),
            (
                "/opt/bin/zsh",
                "cd '/srv/project' && SPACETERM='1' COLORTERM='truecolor' exec '/opt/bin/zsh' -l",
            ),
            (
                "/opt/bin/fish",
                "cd '/srv/project' && set -lx SPACETERM '1' && set -lx COLORTERM 'truecolor' && exec '/opt/bin/fish' -l",
            ),
            (
                "/opt/bin/nu",
                "cd \"/srv/project\" ; $env.SPACETERM = \"1\"; $env.COLORTERM = \"truecolor\"; exec \"/opt/bin/nu\" -l",
            ),
            (
                "/opt/bin/nushell",
                "cd \"/srv/project\" ; $env.SPACETERM = \"1\"; $env.COLORTERM = \"truecolor\"; exec \"/opt/bin/nushell\" -l",
            ),
            (
                "/opt/bin/elvish",
                "cd '/srv/project' ; set E:SPACETERM = '1'; set E:COLORTERM = 'truecolor'; exec '/opt/bin/elvish'",
            ),
        ];

        for (shell, expected) in cases {
            let command = pane_command("/srv/project", shell).unwrap();
            assert_eq!(command.argument, expected);
        }
    }

    #[test]
    fn posix_sh_should_require_a_matching_discovered_login_capability() {
        assert_eq!(
            ValidatedRemoteLoginShell::new("/bin/sh".to_owned()).unwrap_err(),
            RemoteShellCommandError::PosixShLoginCapabilityRequired
        );
        assert_eq!(
            ValidatedRemoteLoginShell::from_discovery(
                "/bin/sh".to_owned(),
                PosixShLoginCapability::NotApplicable,
            )
            .unwrap_err(),
            RemoteShellCommandError::PosixShLoginCapabilityRequired
        );
        assert_eq!(
            ValidatedRemoteLoginShell::from_discovery(
                "/bin/zsh".to_owned(),
                PosixShLoginCapability::LoginOptionSupported,
            )
            .unwrap_err(),
            RemoteShellCommandError::InvalidLoginShellCapability
        );
        assert_eq!(
            ValidatedRemoteLoginShell::from_discovery(
                "/bin/sh".to_owned(),
                PosixShLoginCapability::LoginOptionSupported,
            )
            .unwrap()
            .as_str(),
            "/bin/sh"
        );
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

        assert_eq!(spec.executable(), OsStr::new("/selected/openssh"));
        assert_eq!(
            arguments(&spec).last().map(String::as_str),
            Some("cd '/srv/project' && SPACETERM='1' COLORTERM='truecolor' exec '/bin/zsh' -l")
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
    fn all_specs_should_use_the_exact_selected_executable() {
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
                .all(|spec| spec.executable() == OsStr::new("/selected/openssh"))
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
            executable(),
            PathBuf::from("relative/config"),
            destination,
            PathBuf::from("relative/control"),
        )
        .err();

        assert_eq!(error, Some(SshCommandContextError::UnsafePath));
    }
}

#[cfg(test)]
#[path = "../platform/macos_adapter_tests/command.rs"]
mod macos_adapter_tests;
