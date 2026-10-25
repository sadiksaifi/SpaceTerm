use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;

use super::process::{
    CapturedProcessError, SshProbeEnvironment, SshProcessAdapter, SshProcessEnvironmentError,
    SshProcessMechanismError, run_probe_process,
};
use super::startup_environment::StartupSshEnvironment;
use crate::cancellation::CancellationToken;
use crate::domain::SshDestination;

const MINIMUM_OPENSSH_VERSION: OpenSshVersion = OpenSshVersion::new(8, 2);
const MAX_PROBE_STREAM_BYTES: usize = 4 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const SETUP_READY_PREAMBLE: &[u8] = b"spaceterm-setup-ready-v1\n";

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

    pub(crate) fn probe(&self, cancellation: &CancellationToken) -> SshCapability {
        classify_supervised_probe_result(run_probe_process(
            &self.adapter,
            &self.command(),
            &self.environment,
            MAX_PROBE_STREAM_BYTES + 1,
            cancellation,
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

pub(crate) struct SshCommandContext;

impl SshCommandContext {
    /// Ask OpenSSH to evaluate the same configuration used by this setup attempt.
    pub(crate) fn resolve_setup(
        executable: OpenSshExecutable,
        configuration: &Path,
        destination: &SshDestination,
    ) -> Result<SshCommandSpec, RemoteShellCommandError> {
        validate_setup_configuration_path(configuration)?;
        validate_setup_configuration_path(executable.as_path())?;
        Ok(SshCommandSpec::new(
            executable,
            vec![
                "-G".into(),
                "-F".into(),
                configuration.as_os_str().to_owned(),
                "--".into(),
                destination.as_str().into(),
            ],
        ))
    }

    /// A foreground master belongs only to this one setup, including consent waits.
    pub(crate) fn setup_master(
        executable: OpenSshExecutable,
        configuration: &Path,
        socket: &Path,
        destination: &SshDestination,
    ) -> Result<SshCommandSpec, RemoteShellCommandError> {
        validate_setup_configuration_path(socket)?;
        let mut spec = Self::setup(
            executable,
            configuration,
            destination,
            ValidatedRemoteShellCommand::server_script("exec /bin/cat >/dev/null")?,
        )?;
        spec.arguments.splice(
            0..0,
            [
                "-o".into(),
                "ControlMaster=yes".into(),
                "-o".into(),
                format!("ControlPath={}", socket.display()).into(),
            ],
        );
        Ok(spec)
    }

    /// Refuse a fresh connection if the exact attempt-owned master is unavailable.
    pub(crate) fn setup_channel(
        executable: OpenSshExecutable,
        configuration: &Path,
        socket: &Path,
        destination: &SshDestination,
        command: ValidatedRemoteShellCommand,
    ) -> Result<SshCommandSpec, RemoteShellCommandError> {
        validate_setup_configuration_path(socket)?;
        let mut spec = Self::setup(executable, configuration, destination, command)?;
        spec.arguments.splice(
            0..0,
            [
                "-o".into(),
                format!("ControlPath={}", socket.display()).into(),
                "-o".into(),
                "ProxyCommand=false".into(),
                "-o".into(),
                "ProxyJump=none".into(),
                "-o".into(),
                "BatchMode=yes".into(),
            ],
        );
        Ok(spec)
    }

    /// One setup command, using normal OpenSSH configuration without sharing a master.
    pub(crate) fn setup(
        executable: OpenSshExecutable,
        configuration: &Path,
        destination: &SshDestination,
        command: ValidatedRemoteShellCommand,
    ) -> Result<SshCommandSpec, RemoteShellCommandError> {
        validate_setup_configuration_path(configuration)?;
        validate_setup_configuration_path(executable.as_path())?;
        let mut arguments = vec![
            "-F".into(),
            configuration.as_os_str().to_owned(),
            "-T".into(),
        ];
        for option in [
            "ControlMaster=no",
            "ControlPath=none",
            "ControlPersist=no",
            "IgnoreUnknown=ForkAfterAuthentication,SessionType",
            "ForkAfterAuthentication=no",
            "ClearAllForwardings=yes",
            "RemoteCommand=none",
            "RequestTTY=no",
            "SessionType=default",
            "PermitLocalCommand=no",
        ] {
            push_option(&mut arguments, option.into());
        }
        arguments.extend([
            "--".into(),
            destination.as_str().into(),
            format!(
                "/bin/sh -c {}",
                quote_for_posix_shell(&format!(
                    "printf '%s\\n' spaceterm-setup-ready-v1; exec {}",
                    command.argument
                ))
            )
            .into(),
        ]);
        let mut command = SshCommandSpec::new(executable, arguments);
        command.setup_preamble = true;
        Ok(command)
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
}

pub(crate) fn validate_setup_configuration_path(
    path: &Path,
) -> Result<(), RemoteShellCommandError> {
    if !path.is_absolute()
        || !path.to_str().is_some_and(|path| {
            path.chars().all(|character| {
                character.is_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
            })
        })
    {
        return Err(RemoteShellCommandError::UnsafeConfiguration);
    }
    Ok(())
}

pub(crate) fn setup_configuration(home: &Path) -> Result<Vec<u8>, RemoteShellCommandError> {
    let user = home.join(".ssh/config");
    let user = user
        .to_str()
        .filter(|_| is_safe_absolute_path(&user))
        .ok_or(RemoteShellCommandError::UnsafeConfiguration)?;
    let user = user.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("Host *\n    ControlMaster no\n    ControlPath none\n    ControlPersist no\n    IgnoreUnknown ForkAfterAuthentication,SessionType\n    ForkAfterAuthentication no\n    ClearAllForwardings yes\n    PermitLocalCommand no\nInclude \"{user}\"\nHost *\nInclude /etc/ssh/ssh_config\n").into_bytes())
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
    setup_preamble: bool,
}

impl SshCommandSpec {
    fn new(executable: OpenSshExecutable, arguments: Vec<OsString>) -> Self {
        Self {
            executable,
            arguments,
            setup_preamble: false,
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
    pub(super) fn has_setup_preamble(&self) -> bool {
        self.setup_preamble
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum RemoteShellCommandError {
    #[error("the remote server command is too long or contains NUL")]
    CommandTooLong,
    #[error("the temporary SSH configuration path is unsafe")]
    UnsafeConfiguration,
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
    use std::ffi::OsString;
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
        .probe(&CancellationToken::default())
    }

    fn executable() -> OpenSshExecutable {
        OpenSshExecutable::new(PathBuf::from("/selected/openssh")).unwrap()
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
    fn setup_should_use_user_configuration_without_reusing_or_creating_a_master() {
        let destination = SshDestination::new("jump-alias".into()).unwrap();
        let command = ValidatedRemoteShellCommand::server_script("exec server").unwrap();
        let spec = SshCommandContext::setup(
            executable(),
            Path::new("/fixture/config"),
            &destination,
            command,
        )
        .unwrap();
        let arguments = arguments(&spec);
        for option in [
            "ControlMaster=no",
            "ControlPath=none",
            "ControlPersist=no",
            "ForkAfterAuthentication=no",
            "ClearAllForwardings=yes",
            "PermitLocalCommand=no",
        ] {
            assert!(arguments.windows(2).any(|pair| pair == ["-o", option]));
        }
        assert!(
            !arguments
                .iter()
                .any(|arg| matches!(arg.as_str(), "-S" | "-N"))
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["-F", "/fixture/config"])
        );
        assert_eq!(arguments[arguments.len() - 2], "jump-alias");
        assert!(
            arguments
                .last()
                .unwrap()
                .contains("spaceterm-setup-ready-v1")
        );
        assert!(spec.has_setup_preamble());
    }

    #[test]
    fn setup_refuses_paths_that_openssh_cannot_safely_forward_to_generated_jumps() {
        for path in [
            "/fixture/config root",
            "/fixture/config;command",
            "/fixture/config%h",
        ] {
            let command = ValidatedRemoteShellCommand::server_script("exec server").unwrap();
            assert!(
                SshCommandContext::setup(
                    executable(),
                    Path::new(path),
                    &SshDestination::new("jump".into()).unwrap(),
                    command
                )
                .is_err()
            );
        }
        let command = ValidatedRemoteShellCommand::server_script("exec server").unwrap();
        assert!(
            SshCommandContext::setup(
                OpenSshExecutable::new("/fixture/ssh executable".into()).unwrap(),
                Path::new("/fixture/config"),
                &SshDestination::new("jump".into()).unwrap(),
                command
            )
            .is_err()
        );
    }
}
