use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;

use super::cancellation::SshCancellationToken;
use super::process::{
    CapturedProcessError, SshProbeEnvironment, SshProcessAdapter, SshProcessEnvironmentError,
    SshProcessMechanismError, run_probe_process,
};
use super::startup_environment::StartupSshEnvironment;
use crate::domain::SshDestination;

const MINIMUM_OPENSSH_VERSION: OpenSshVersion = OpenSshVersion::new(8, 2);
const MAX_PROBE_STREAM_BYTES: usize = 4 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

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
    quiet: bool,
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
            quiet: false,
        })
    }

    pub(crate) fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }
    pub(crate) fn authorization_prompt(
        helper: PathBuf,
    ) -> Result<SshCommandSpec, OpenSshExecutableError> {
        Ok(SshCommandSpec::new(
            OpenSshExecutable::new(helper)?,
            // The presenter recognizes exactly this prompt and explains the remote sudo request.
            vec![super::askpass::prompt::REMOTE_LINGER_PROMPT.into()],
        ))
    }
    pub(crate) fn key_public(
        executable: PathBuf,
        identity: &Path,
    ) -> Result<SshCommandSpec, OpenSshExecutableError> {
        Ok(SshCommandSpec::new(
            OpenSshExecutable::new(executable)?,
            vec![
                "-y".into(),
                "-P".into(),
                "".into(),
                "-f".into(),
                identity.as_os_str().to_owned(),
            ],
        ))
    }
    pub(crate) fn master(&self) -> SshCommandSpec {
        let mut arguments = self.base_arguments();
        if self.quiet {
            push_option(&mut arguments, "BatchMode=yes".into());
            push_option(&mut arguments, "StrictHostKeyChecking=yes".into());
        }
        push_option(&mut arguments, OsString::from("ControlMaster=yes"));
        push_option(&mut arguments, self.control_path_option());
        push_option(&mut arguments, OsString::from("ControlPersist=no"));
        // Clients predating this key cannot enable it in their configuration.
        push_option(
            &mut arguments,
            OsString::from("IgnoreUnknown=ForkAfterAuthentication"),
        );
        push_option(&mut arguments, OsString::from("ForkAfterAuthentication=no"));
        arguments.push(OsString::from("-N"));
        self.finish(arguments)
    }

    pub(crate) fn readiness_check(&self) -> SshCommandSpec {
        self.control_operation("check")
    }

    pub(crate) fn server_channel(&self, command: ValidatedRemoteShellCommand) -> SshCommandSpec {
        let mut arguments = self.child_arguments();
        for option in [
            "ClearAllForwardings=yes",
            "RemoteCommand=none",
            "RequestTTY=no",
            "SessionType=default",
            "BatchMode=yes",
        ] {
            push_option(&mut arguments, option.into());
        }
        arguments.push("-T".into());
        self.push_destination(&mut arguments);
        arguments.push(command.argument.into());
        self.spec(arguments)
    }

    pub(crate) fn configuration(&self) -> SshCommandSpec {
        let arguments = vec![
            "-F".into(),
            self.managed_config.as_os_str().to_owned(),
            "-G".into(),
            "--".into(),
            self.destination.as_str().into(),
        ];
        self.spec(arguments)
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
        // OpenSSH prefixes ProxyCommand with `exec `. Terminate that empty exec before
        // using the shell builtin, so even a hostile PATH cannot supply an `exit` executable.
        push_option(&mut arguments, OsString::from("ProxyCommand=; exit 1"));
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
    path.is_absolute() && !path.to_string_lossy().chars().any(char::is_control)
}

fn push_option(arguments: &mut Vec<OsString>, option: OsString) {
    arguments.push(OsString::from("-o"));
    arguments.push(option);
}

pub(crate) struct SshCommandSpec {
    executable: OpenSshExecutable,
    arguments: Vec<OsString>,
}

impl SshCommandSpec {
    fn new(executable: OpenSshExecutable, arguments: Vec<OsString>) -> Self {
        Self {
            executable,
            arguments,
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
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum RemoteShellCommandError {
    #[error("the remote server command is too long or contains NUL")]
    CommandTooLong,
}

pub(crate) struct ValidatedRemoteShellCommand {
    argument: String,
}

impl ValidatedRemoteShellCommand {
    pub(crate) fn server_script(script: &str) -> Result<Self, RemoteShellCommandError> {
        if script.len() > 64 * 1024 || script.contains('\0') {
            return Err(RemoteShellCommandError::CommandTooLong);
        }
        Ok(Self {
            argument: format!("/bin/sh -c {}", quote_for_posix_shell(script)),
        })
    }
}

fn quote_for_posix_shell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::io::Cursor;
    use std::path::PathBuf;

    use crate::domain::SshDestination;
    use crate::ssh::process::{
        ProcessExit, ProcessSignal, SpawnedSshProcess, SshProcessPipes, SshProcessSpawnRequest,
    };

    use super::*;

    #[derive(Clone)]
    enum FakeProbeResult {
        Output(SshProbeOutput),
        Error(SshProcessMechanismError),
    }

    type ProbeInvocation = (PathBuf, Vec<OsString>);

    #[derive(Clone)]
    struct FakeProbeAdapter {
        calls: Arc<Mutex<Vec<ProbeInvocation>>>,
        result: FakeProbeResult,
    }

    impl FakeProbeAdapter {
        fn output(success: bool, stdout: &[u8], stderr: &[u8]) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                result: FakeProbeResult::Output(SshProbeOutput::new(
                    success,
                    stdout.to_vec(),
                    stderr.to_vec(),
                )),
            }
        }

        fn error(error: SshProcessMechanismError) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                result: FakeProbeResult::Error(error),
            }
        }
    }

    impl SshProcessAdapter for FakeProbeAdapter {
        type Process = ProcessExit;

        fn spawn(
            &self,
            request: SshProcessSpawnRequest,
        ) -> Result<SpawnedSshProcess<Self::Process>, SshProcessMechanismError> {
            self.calls.lock().unwrap().push((
                request.executable().to_path_buf(),
                request.arguments().to_vec(),
            ));
            match &self.result {
                FakeProbeResult::Output(output) => Ok(SpawnedSshProcess::new(
                    ProcessExit::new(output.success, Some(if output.success { 0 } else { 1 })),
                    SshProcessPipes::new(
                        None,
                        Some(Box::new(Cursor::new(output.stdout.clone()))),
                        Some(Box::new(Cursor::new(output.stderr.clone()))),
                    ),
                )),
                FakeProbeResult::Error(error) => Err(*error),
            }
        }

        fn try_status(
            &self,
            process: &mut Self::Process,
        ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
            Ok(Some(*process))
        }

        fn signal(
            &self,
            _process: &mut Self::Process,
            _signal: ProcessSignal,
        ) -> Result<(), SshProcessMechanismError> {
            Ok(())
        }

        fn reap(&self, _process: Self::Process) -> Result<(), SshProcessMechanismError> {
            Ok(())
        }
    }

    fn probe(adapter: &FakeProbeAdapter) -> SshCapability {
        SshCapabilityProbe::from_startup(
            executable(),
            PathBuf::from("/captured/home"),
            &StartupSshEnvironment::for_test(None),
            adapter.clone(),
        )
        .unwrap()
        .probe_blocking()
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

    #[test]
    fn capability_probe_should_invoke_only_the_selected_ssh_version_command() {
        let runner = FakeProbeAdapter::output(true, b"", b"OpenSSH_9.9p2\n");

        let _ = probe(&runner);

        assert_eq!(
            *runner.calls.lock().unwrap(),
            vec![(
                PathBuf::from("/selected/openssh"),
                vec![OsString::from("-V")]
            )]
        );
    }

    #[test]
    fn capability_probe_should_accept_an_apple_version_from_stderr() {
        let runner =
            FakeProbeAdapter::output(true, b"", b"OpenSSH_9.9p2 Apple-1, LibreSSL 3.3.6\n");

        let capability = probe(&runner);

        assert_eq!(
            capability,
            SshCapability::Available(OpenSshVersion::new(9, 9))
        );
    }

    #[test]
    fn capability_probe_should_accept_the_minimum_version_from_stdout() {
        let runner = FakeProbeAdapter::output(true, b"OpenSSH_8.2\n", b"");

        let capability = probe(&runner);

        assert_eq!(
            capability,
            SshCapability::Available(OpenSshVersion::new(8, 2))
        );
    }

    #[test]
    fn capability_probe_should_report_a_too_old_version() {
        let runner = FakeProbeAdapter::output(true, b"", b"OpenSSH_8.1p1\n");

        let capability = probe(&runner);

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
        let runner = FakeProbeAdapter::error(SshProcessMechanismError::NotFound);

        let capability = probe(&runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::NotFound)
        );
    }

    #[test]
    fn capability_probe_should_report_unrecognized_control_output() {
        let runner = FakeProbeAdapter::output(true, b"", b"OpenSSH_9.9\0secret");

        let capability = probe(&runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::Unrecognized)
        );
    }

    #[test]
    fn capability_probe_should_report_unrecognized_oversized_output() {
        let runner = FakeProbeAdapter::output(true, &vec![b'x'; MAX_PROBE_STREAM_BYTES + 1], b"");

        let capability = probe(&runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::Unrecognized)
        );
    }

    #[test]
    fn capability_probe_should_report_probe_failed_for_a_failed_exit() {
        let runner = FakeProbeAdapter::output(false, b"", b"OpenSSH_9.9p2\n");

        let capability = probe(&runner);

        assert_eq!(
            capability,
            SshCapability::Unavailable(SshUnavailableReason::ProbeFailed)
        );
    }

    #[test]
    fn capability_probe_should_report_probe_failed_for_an_io_error() {
        let runner = FakeProbeAdapter::error(SshProcessMechanismError::LaunchFailed);

        let capability = probe(&runner);

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
                "-o",
                "IgnoreUnknown=ForkAfterAuthentication",
                "-o",
                "ForkAfterAuthentication=no",
                "-N",
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
                "ProxyCommand=; exit 1",
                "-O",
                "check",
                "--",
                "root@fedora@orb",
            ]
        );
    }

    #[test]
    fn server_spec_should_disable_external_master_forwarding_and_tty_behavior() {
        let spec = context()
            .server_channel(ValidatedRemoteShellCommand::server_script("exec server").unwrap());

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
                "ProxyCommand=; exit 1",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "RemoteCommand=none",
                "-o",
                "RequestTTY=no",
                "-o",
                "SessionType=default",
                "-o",
                "BatchMode=yes",
                "-T",
                "--",
                "root@fedora@orb",
                "/bin/sh -c 'exec server'",
            ]
        );
    }

    #[test]
    fn all_specs_should_use_the_exact_selected_executable() {
        let context = context();
        let command = ValidatedRemoteShellCommand::server_script("exec server").unwrap();
        let specs = [
            context.master(),
            context.readiness_check(),
            context.server_channel(command),
        ];

        assert!(
            specs
                .iter()
                .all(|spec| spec.executable() == OsStr::new("/selected/openssh"))
        );
    }

    #[test]
    fn command_context_should_reject_relative_config_or_control_paths() {
        for (config, control) in [
            ("relative/config", "/private/runtime/control"),
            ("/private/config/ssh_config", "relative/control"),
        ] {
            let error = SshCommandContext::new(
                executable(),
                PathBuf::from(config),
                SshDestination::new("host".to_owned()).unwrap(),
                PathBuf::from(control),
            )
            .err();

            assert_eq!(error, Some(SshCommandContextError::UnsafePath));
        }
    }
}
