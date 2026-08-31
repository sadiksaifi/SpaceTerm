use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::domain::SshDestination;

const SSH_EXECUTABLE: &str = "/usr/bin/ssh";
const MINIMUM_OPENSSH_VERSION: OpenSshVersion = OpenSshVersion::new(8, 2);
const MAX_PROBE_STREAM_BYTES: usize = 4 * 1024;

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

    pub(crate) fn prepare_pane_channel(
        &self,
        command: ValidatedRemoteShellCommand,
    ) -> PreparedSshPaneChannelCommand {
        PreparedSshPaneChannelCommand::new(self.pane_channel(command))
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

#[derive(Debug)]
pub(crate) struct SshCommandSpec {
    executable: PathBuf,
    arguments: Vec<OsString>,
}

impl SshCommandSpec {
    pub(crate) fn executable(&self) -> &OsStr {
        self.executable.as_os_str()
    }

    pub(crate) fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    pub(crate) fn into_parts(self) -> (PathBuf, Vec<OsString>) {
        (self.executable, self.arguments)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedSshPaneChannelCommand {
    command: Arc<Mutex<Option<SshCommandSpec>>>,
}

impl PreparedSshPaneChannelCommand {
    fn new(command: SshCommandSpec) -> Self {
        Self {
            command: Arc::new(Mutex::new(Some(command))),
        }
    }

    pub(crate) fn take(&self) -> Result<SshCommandSpec, PreparedSshPaneChannelError> {
        let mut command = self
            .command
            .lock()
            .map_err(|_| PreparedSshPaneChannelError::Unavailable)?;
        command
            .take()
            .ok_or(PreparedSshPaneChannelError::AlreadyConsumed)
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
    #[error("remote shell command must not be empty")]
    Empty,
    #[error("remote shell command must not contain control characters")]
    Control,
}

pub(crate) struct ValidatedRemoteShellCommand {
    argument: String,
}

impl ValidatedRemoteShellCommand {
    pub(crate) fn new(argument: String) -> Result<Self, RemoteShellCommandError> {
        if argument.is_empty() {
            return Err(RemoteShellCommandError::Empty);
        }
        if argument.chars().any(char::is_control) {
            return Err(RemoteShellCommandError::Control);
        }
        Ok(Self { argument })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::path::{Path, PathBuf};

    use crate::domain::SshDestination;

    use super::*;

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
        let command = ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap();

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
                "exec /bin/zsh -l",
            ]
        );
    }

    #[test]
    fn pane_command_should_reject_control_characters() {
        assert_eq!(
            ValidatedRemoteShellCommand::new("exec shell\nsecond command".to_owned()).err(),
            Some(RemoteShellCommandError::Control)
        );
    }

    #[test]
    fn prepared_pane_command_should_preserve_exact_argv_and_be_single_use() {
        let prepared = context().prepare_pane_channel(
            ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
        );
        let duplicate_owner = prepared.clone();

        let spec = prepared.take().unwrap();

        assert_eq!(spec.executable(), OsStr::new("/usr/bin/ssh"));
        assert_eq!(
            arguments(&spec).last().map(String::as_str),
            Some("exec /bin/zsh -l")
        );
        assert_eq!(
            duplicate_owner.take().err(),
            Some(PreparedSshPaneChannelError::AlreadyConsumed)
        );
    }

    #[test]
    fn all_specs_should_use_the_exact_system_executable() {
        let context = context();
        let command = ValidatedRemoteShellCommand::new("exec shell".to_owned()).unwrap();
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
        let command = ValidatedRemoteShellCommand::new("exec shell".to_owned()).unwrap();
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
