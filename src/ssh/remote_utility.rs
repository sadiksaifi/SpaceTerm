use std::future::Future;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::str;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use super::cancellation::SshCancellationToken;
use super::command::{PosixShLoginCapability, SshCommandSpec};
use super::live_connection::LiveConnectionCapability;
use super::process::{ProcessExit, SshProcessEnvironment};
use crate::domain::RemoteWorkspaceDirectory;

pub(crate) const MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES: usize = 384 * 1024;
const MAXIMUM_REMOTE_UTILITY_REQUEST_BYTES: usize = 32 * 1024;
const MAXIMUM_REMOTE_FIELD_BYTES: usize = 16 * 1024;
const MAXIMUM_REMOTE_DIRECTORY_NAMES: usize = 1024;
const MAXIMUM_REMOTE_DIRECTORY_ENTRIES_EXAMINED: usize = 1024;
const MAXIMUM_REMOTE_PATH_BYTES: usize = 4096;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const NATIVE_UTILITY_TIMEOUT: Duration = Duration::from_secs(60);
const PROTOCOL_HEADER: &str = "SPACETERM-REMOTE/1";

#[derive(Debug)]
/// Content-free exit status plus bounded untrusted stdout from one utility process.
pub(crate) struct RemoteUtilityProcessOutput {
    exit: ProcessExit,
    stdout: Vec<u8>,
}

impl RemoteUtilityProcessOutput {
    pub(crate) const fn new(exit: ProcessExit, stdout: Vec<u8>) -> Self {
        Self { exit, stdout }
    }
}

#[derive(Debug, Error)]
/// Transport-level utility failure that never retains remote output.
pub(crate) enum RemoteUtilityRunError {
    #[error("remote utility command was cancelled")]
    Cancelled,
    #[error("remote utility output exceeded its safety limit")]
    OutputTooLarge,
    #[error("remote utility process exceeded its deadline")]
    TimedOut,
    #[error("remote utility process failed")]
    Io(#[source] io::Error),
}

/// Process boundary for fixed `/bin/sh -s` remote utility requests.
///
/// Implementations must enforce the supplied output bound, link cancellation to process-group
/// termination and reaping, and never log, persist, or interpret untrusted remote bytes.
pub(crate) trait SshRemoteUtilityRunner: Send + Sync + 'static {
    /// Runs one owned script with no TTY and a request-scoped cancellation token.
    fn run(
        &self,
        command: Arc<SshCommandSpec>,
        script: Vec<u8>,
        maximum_output_bytes: usize,
        cancellation: SshCancellationToken,
    ) -> impl Future<Output = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>> + Send;
}

/// A reusable utility channel command created only by the centralized SSH command policy.
///
/// A live command carries revocable authority for one exact control instance and generation, so
/// it cannot fall back to a direct connection or outlive its control socket.
pub(crate) struct PreparedSshRemoteUtilityCommand {
    command: Arc<SshCommandSpec>,
    capability: Option<LiveConnectionCapability>,
}

impl PreparedSshRemoteUtilityCommand {
    #[cfg(test)]
    pub(super) fn new(command: SshCommandSpec) -> Self {
        Self {
            command: Arc::new(command),
            capability: None,
        }
    }

    pub(super) fn new_live(command: SshCommandSpec, capability: LiveConnectionCapability) -> Self {
        Self {
            command: Arc::new(command),
            capability: Some(capability),
        }
    }

    #[cfg(test)]
    pub(super) fn connection_cancellation(&self) -> Option<SshCancellationToken> {
        self.capability
            .as_ref()
            .map(LiveConnectionCapability::cancellation)
    }
}

#[derive(Clone)]
/// Native bounded runner that owns a private utility process group per request.
///
/// Work executes off async executor threads. Timeout, cancellation, or future drop kills and
/// reaps the process group before ownership is released.
pub(crate) struct NativeSshRemoteUtilityRunner {
    environment: SshProcessEnvironment,
    timeout: Duration,
}

impl NativeSshRemoteUtilityRunner {
    pub(crate) const fn new(environment: SshProcessEnvironment) -> Self {
        Self {
            environment,
            timeout: NATIVE_UTILITY_TIMEOUT,
        }
    }

    #[cfg(test)]
    const fn with_timeout(environment: SshProcessEnvironment, timeout: Duration) -> Self {
        Self {
            environment,
            timeout,
        }
    }
}

impl SshRemoteUtilityRunner for NativeSshRemoteUtilityRunner {
    fn run(
        &self,
        command: Arc<SshCommandSpec>,
        script: Vec<u8>,
        maximum_output_bytes: usize,
        cancellation: SshCancellationToken,
    ) -> impl Future<Output = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>> + Send
    {
        let environment = self.environment.clone();
        let timeout = self.timeout;
        async move {
            let mut cancel_on_drop = CancelOnDrop::new(cancellation.clone());
            let (sender, receiver) = async_channel::bounded(1);
            thread::Builder::new()
                .name("spaceterm-ssh-utility".to_owned())
                .spawn(move || {
                    let result = run_native_command(
                        command,
                        script,
                        maximum_output_bytes,
                        &cancellation,
                        &environment,
                        Instant::now()
                            .checked_add(timeout)
                            .unwrap_or_else(Instant::now),
                    );
                    let _ = sender.send_blocking(result);
                })
                .map_err(RemoteUtilityRunError::Io)?;
            let result = receiver.recv().await.map_err(|_| {
                RemoteUtilityRunError::Io(io::Error::other(
                    "SSH utility worker ended without returning process ownership",
                ))
            })?;
            cancel_on_drop.disarm();
            result
        }
    }
}

struct CancelOnDrop {
    cancellation: SshCancellationToken,
    armed: bool,
}

impl CancelOnDrop {
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

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

fn run_native_command(
    spec: Arc<SshCommandSpec>,
    script: Vec<u8>,
    maximum_output_bytes: usize,
    cancellation: &SshCancellationToken,
    environment: &SshProcessEnvironment,
    deadline: Instant,
) -> Result<RemoteUtilityProcessOutput, RemoteUtilityRunError> {
    if cancellation.is_cancelled() {
        return Err(RemoteUtilityRunError::Cancelled);
    }
    let mut command = Command::new(spec.executable());
    command
        .args(spec.arguments())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    environment.apply(&mut command);
    let mut child = command.spawn().map_err(RemoteUtilityRunError::Io)?;
    let process_group = match child.id().try_into() {
        Ok(process_group) => process_group,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RemoteUtilityRunError::Io(io::Error::other(
                "SSH utility process identifier did not fit the platform process type",
            )));
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        terminate_and_reap(&mut child, process_group);
        return Err(RemoteUtilityRunError::Io(io::Error::other(
            "SSH stdin pipe was unavailable",
        )));
    };
    let Some(mut stdout) = child.stdout.take() else {
        terminate_and_reap(&mut child, process_group);
        return Err(RemoteUtilityRunError::Io(io::Error::other(
            "SSH stdout pipe was unavailable",
        )));
    };
    let writer = thread::spawn(move || -> io::Result<()> {
        stdin.write_all(&script)?;
        stdin.flush()
    });
    let mut reader = Some(thread::spawn(move || {
        read_bounded(&mut stdout, maximum_output_bytes)
    }));
    let mut captured_stdout = None;

    let exit = loop {
        if cancellation.is_cancelled() {
            terminate_and_reap(&mut child, process_group);
            let _ = writer.join();
            if let Some(reader) = reader.take() {
                let _ = reader.join();
            }
            return Err(RemoteUtilityRunError::Cancelled);
        }
        if Instant::now() >= deadline {
            terminate_and_reap(&mut child, process_group);
            let _ = writer.join();
            if let Some(reader) = reader.take() {
                let _ = reader.join();
            }
            return Err(RemoteUtilityRunError::TimedOut);
        }
        if reader.as_ref().is_some_and(|reader| reader.is_finished()) {
            let Some(finished_reader) = reader.take() else {
                terminate_and_reap(&mut child, process_group);
                let _ = writer.join();
                return Err(RemoteUtilityRunError::Io(io::Error::other(
                    "SSH stdout reader ownership was lost",
                )));
            };
            match finished_reader.join() {
                Ok(Err(ReadBoundedError::OutputTooLarge)) => {
                    terminate_and_reap(&mut child, process_group);
                    let _ = writer.join();
                    return Err(RemoteUtilityRunError::OutputTooLarge);
                }
                Ok(Err(ReadBoundedError::Io(error))) => {
                    terminate_and_reap(&mut child, process_group);
                    let _ = writer.join();
                    return Err(RemoteUtilityRunError::Io(error));
                }
                Ok(Ok(stdout)) => {
                    captured_stdout = Some(stdout);
                }
                Err(_) => {
                    terminate_and_reap(&mut child, process_group);
                    let _ = writer.join();
                    return Err(RemoteUtilityRunError::Io(io::Error::other(
                        "SSH stdout reader failed",
                    )));
                }
            }
        }
        match child.try_wait() {
            Err(error) => {
                terminate_and_reap(&mut child, process_group);
                let _ = writer.join();
                if let Some(reader) = reader.take() {
                    let _ = reader.join();
                }
                return Err(RemoteUtilityRunError::Io(error));
            }
            Ok(Some(status)) => {
                terminate_process_group(process_group);
                break ProcessExit::from(status);
            }
            Ok(None) => thread::sleep(
                PROCESS_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            ),
        }
    };
    if let Ok(Err(error)) = writer.join()
        && exit.is_success()
    {
        return Err(RemoteUtilityRunError::Io(error));
    }
    let stdout = match captured_stdout {
        Some(stdout) => stdout,
        None => match reader
            .ok_or_else(|| {
                RemoteUtilityRunError::Io(io::Error::other("SSH stdout reader ownership was lost"))
            })?
            .join()
        {
            Ok(Ok(stdout)) => stdout,
            Ok(Err(ReadBoundedError::OutputTooLarge)) => {
                return Err(RemoteUtilityRunError::OutputTooLarge);
            }
            Ok(Err(ReadBoundedError::Io(error))) => return Err(RemoteUtilityRunError::Io(error)),
            Err(_) => {
                return Err(RemoteUtilityRunError::Io(io::Error::other(
                    "SSH stdout reader failed",
                )));
            }
        },
    };
    Ok(RemoteUtilityProcessOutput::new(exit, stdout))
}

fn terminate_and_reap(child: &mut std::process::Child, process_group: libc::pid_t) {
    terminate_process_group(process_group);
    let _ = child.wait();
}

fn terminate_process_group(process_group: libc::pid_t) {
    // SAFETY: the positive process group is the PID of a child launched with a private group.
    let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
}

enum ReadBoundedError {
    OutputTooLarge,
    Io(io::Error),
}

fn read_bounded(reader: &mut impl Read, maximum_bytes: usize) -> Result<Vec<u8>, ReadBoundedError> {
    let mut output = Vec::with_capacity(maximum_bytes.min(16 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(ReadBoundedError::Io)?;
        if read == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(read) > maximum_bytes {
            return Err(ReadBoundedError::OutputTooLarge);
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
/// Typed utility failure with raw protocol and shell output excluded.
pub(crate) enum RemoteUtilityError {
    #[error("remote utility request was cancelled")]
    Cancelled,
    #[error("remote utility request was too large")]
    RequestTooLarge,
    #[error("remote utility output exceeded its safety limit")]
    OutputTooLarge,
    #[error("remote utility command failed with status {0:?}")]
    CommandFailed(Option<i32>),
    #[error("remote utility transport failed")]
    Transport,
    #[error("remote utility returned an invalid response")]
    InvalidResponse,
    #[error("the configured remote login shell cannot start in login mode")]
    UnsupportedLoginShell,
    #[error("remote path does not exist")]
    Missing,
    #[error("remote path is not a directory")]
    NotDirectory,
    #[error("remote path permission was denied")]
    PermissionDenied,
    #[error("remote utility operation failed")]
    RemoteFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Strictly decoded account metadata returned by protocol version 1.
pub(crate) struct RemoteAccountMetadata {
    user: String,
    uid: u64,
    home: String,
    login_shell: String,
    physical_home: String,
    posix_sh_login_capability: PosixShLoginCapability,
}

impl RemoteAccountMetadata {
    pub(crate) fn user(&self) -> &str {
        &self.user
    }

    #[cfg(test)]
    pub(crate) const fn uid(&self) -> u64 {
        self.uid
    }

    #[cfg(test)]
    pub(crate) fn home(&self) -> &str {
        &self.home
    }

    pub(crate) fn login_shell(&self) -> &str {
        &self.login_shell
    }

    pub(crate) fn physical_home(&self) -> &str {
        &self.physical_home
    }

    pub(crate) const fn posix_sh_login_capability(&self) -> PosixShLoginCapability {
        self.posix_sh_login_capability
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Bounded safe directory names plus an explicit partial-listing marker.
pub(crate) struct RemoteUtilityDirectoryListing {
    names: Vec<String>,
    truncated: bool,
}

impl RemoteUtilityDirectoryListing {
    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }

    pub(crate) const fn is_truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Exact-path probe outcome after distinct type and access checks.
pub(crate) enum RemoteDirectoryProbe {
    ReadableDirectory,
    Missing,
}

/// Typed client for SpaceTerm's bounded, versioned remote utility protocol.
///
/// Remote paths remain validated remote-domain strings and never reach local filesystem APIs.
/// Each operation validates live control authority, request size, output size, UTF-8, frame
/// version, field lengths, row counts, and operation kind. Raw output is never retained in errors.
/// Session and request cancellation are linked before the runner receives process ownership.
pub(crate) struct SshRemoteUtilityClient<R: SshRemoteUtilityRunner> {
    command: PreparedSshRemoteUtilityCommand,
    runner: Arc<R>,
    cancellation: SshCancellationToken,
}

impl<R: SshRemoteUtilityRunner> SshRemoteUtilityClient<R> {
    /// Creates a client bound to one prepared command and session cancellation scope.
    pub(crate) fn new(
        command: PreparedSshRemoteUtilityCommand,
        runner: Arc<R>,
        cancellation: SshCancellationToken,
    ) -> Self {
        Self {
            command,
            runner,
            cancellation,
        }
    }

    #[cfg(test)]
    pub(crate) async fn discover_account(
        &self,
    ) -> Result<RemoteAccountMetadata, RemoteUtilityError> {
        self.discover_account_with_cancellation(SshCancellationToken::default())
            .await
    }

    pub(crate) async fn discover_account_with_cancellation(
        &self,
        cancellation: SshCancellationToken,
    ) -> Result<RemoteAccountMetadata, RemoteUtilityError> {
        let output = self.execute(build_account_script(), cancellation).await?;
        parse_account(&output)
    }

    #[cfg(test)]
    pub(crate) async fn list_directories(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Result<RemoteUtilityDirectoryListing, RemoteUtilityError> {
        self.list_directories_with_cancellation(directory, SshCancellationToken::default())
            .await
    }

    pub(crate) async fn list_directories_with_cancellation(
        &self,
        directory: RemoteWorkspaceDirectory,
        cancellation: SshCancellationToken,
    ) -> Result<RemoteUtilityDirectoryListing, RemoteUtilityError> {
        let output = self
            .execute(build_path_script("list", directory.as_str())?, cancellation)
            .await?;
        parse_listing(&output)
    }

    #[cfg(test)]
    pub(crate) async fn probe_exact_path(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Result<RemoteDirectoryProbe, RemoteUtilityError> {
        self.probe_exact_path_with_cancellation(directory, SshCancellationToken::default())
            .await
    }

    pub(crate) async fn probe_exact_path_with_cancellation(
        &self,
        directory: RemoteWorkspaceDirectory,
        cancellation: SshCancellationToken,
    ) -> Result<RemoteDirectoryProbe, RemoteUtilityError> {
        let output = self
            .execute(
                build_path_script("probe", directory.as_str())?,
                cancellation,
            )
            .await?;
        parse_probe(&output)
    }

    #[cfg(test)]
    pub(crate) async fn create_directory_recursively(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Result<(), RemoteUtilityError> {
        self.create_directory_recursively_with_cancellation(
            directory,
            SshCancellationToken::default(),
        )
        .await
    }

    pub(crate) async fn create_directory_recursively_with_cancellation(
        &self,
        directory: RemoteWorkspaceDirectory,
        cancellation: SshCancellationToken,
    ) -> Result<(), RemoteUtilityError> {
        let output = self
            .execute(
                build_path_script("mkdir", directory.as_str())?,
                cancellation,
            )
            .await?;
        parse_empty_success(&output, "mkdir")
    }

    #[cfg(test)]
    pub(crate) async fn resolve_physical_directory(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Result<String, RemoteUtilityError> {
        self.resolve_physical_directory_with_cancellation(
            directory,
            SshCancellationToken::default(),
        )
        .await
    }

    pub(crate) async fn resolve_physical_directory_with_cancellation(
        &self,
        directory: RemoteWorkspaceDirectory,
        cancellation: SshCancellationToken,
    ) -> Result<String, RemoteUtilityError> {
        let output = self
            .execute(
                build_path_script("physical", directory.as_str())?,
                cancellation,
            )
            .await?;
        parse_physical(&output)
    }

    async fn execute(
        &self,
        script: Vec<u8>,
        request_cancellation: SshCancellationToken,
    ) -> Result<Vec<u8>, RemoteUtilityError> {
        if self.cancellation.is_cancelled() || request_cancellation.is_cancelled() {
            return Err(RemoteUtilityError::Cancelled);
        }
        if script.len() > MAXIMUM_REMOTE_UTILITY_REQUEST_BYTES {
            return Err(RemoteUtilityError::RequestTooLarge);
        }
        let request_cancellation =
            SshCancellationToken::linked(&self.cancellation, &request_cancellation);
        let operation_cancellation = match &self.command.capability {
            Some(capability) => {
                capability
                    .authorize()
                    .map_err(|_| RemoteUtilityError::Transport)?;
                SshCancellationToken::linked(&request_cancellation, &capability.cancellation())
            }
            None => request_cancellation,
        };
        let output = self
            .runner
            .run(
                Arc::clone(&self.command.command),
                script,
                MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES,
                operation_cancellation,
            )
            .await
            .map_err(|error| match error {
                RemoteUtilityRunError::Cancelled => RemoteUtilityError::Cancelled,
                RemoteUtilityRunError::OutputTooLarge => RemoteUtilityError::OutputTooLarge,
                RemoteUtilityRunError::TimedOut | RemoteUtilityRunError::Io(_) => {
                    RemoteUtilityError::Transport
                }
            })?;
        if !output.exit.is_success() {
            return Err(RemoteUtilityError::CommandFailed(output.exit.code()));
        }
        if output.stdout.len() > MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES {
            return Err(RemoteUtilityError::OutputTooLarge);
        }
        Ok(output.stdout)
    }
}

fn build_account_script() -> Vec<u8> {
    format!("{COMMON_SCRIPT}\n{ACCOUNT_SCRIPT}").into_bytes()
}

fn build_path_script(operation: &str, path: &str) -> Result<Vec<u8>, RemoteUtilityError> {
    if path.len() > MAXIMUM_REMOTE_PATH_BYTES {
        return Err(RemoteUtilityError::RequestTooLarge);
    }
    let operation_script = match operation {
        "list" => LIST_SCRIPT_TEMPLATE
            .replace(
                "__MAXIMUM_ENTRIES_EXAMINED__",
                &MAXIMUM_REMOTE_DIRECTORY_ENTRIES_EXAMINED.to_string(),
            )
            .replace(
                "__MAXIMUM_DIRECTORY_NAMES__",
                &MAXIMUM_REMOTE_DIRECTORY_NAMES.to_string(),
            ),
        "probe" => PROBE_SCRIPT.to_owned(),
        "mkdir" => MKDIR_SCRIPT.to_owned(),
        "physical" => PHYSICAL_SCRIPT.to_owned(),
        _ => unreachable!("remote utility operation is fixed by the caller"),
    };
    let script = format!(
        "{COMMON_SCRIPT}\ninput_path={}\n{PATH_EXPANSION_SCRIPT}\n{operation_script}",
        quote_for_posix_shell(path)
    )
    .into_bytes();
    if script.len() > MAXIMUM_REMOTE_UTILITY_REQUEST_BYTES {
        return Err(RemoteUtilityError::RequestTooLarge);
    }
    Ok(script)
}

fn quote_for_posix_shell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

const COMMON_SCRIPT: &str = r#"LC_ALL=C
export LC_ALL
set -f
emit_header() {
    printf 'SPACETERM-REMOTE/1\n%s\n%s\n' "$1" "$2"
}
emit_field() {
    field_length=$(LC_ALL=C printf '%s' "$1" | LC_ALL=C wc -c | tr -d '[:space:]') || exit 70
    printf '%s:' "$field_length"
    printf '%s' "$1"
    printf ','
}
emit_empty() {
    emit_header "$1" "$2"
    printf '.\n'
}
"#;

const PATH_EXPANSION_SCRIPT: &str = r#"case "$input_path" in
    '~') remote_path=${HOME-} ;;
    '~/'*) remote_path=${HOME-}/${input_path#\~/} ;;
    /*) remote_path=$input_path ;;
    *) emit_empty protocol invalid-path; exit 0 ;;
esac
[ -n "$remote_path" ] || { emit_empty protocol invalid-path; exit 0; }
classify_remote_path() {
    path_status=missing
    candidate=$remote_path
    while :; do
        if [ -e "$candidate" ]; then
            if [ "$candidate" != "$remote_path" ]; then
                [ -d "$candidate" ] || { path_status=not-directory; return; }
                [ -x "$candidate" ] || { path_status=permission-denied; return; }
            else
                path_status=exists
            fi
            return
        fi
        [ "$candidate" != / ] || return
        candidate=${candidate%/*}
        [ -n "$candidate" ] || candidate=/
    done
}
classify_remote_path
"#;

const ACCOUNT_SCRIPT: &str = r#"user=$(id -un 2>/dev/null) || { emit_empty account failed; exit 0; }
uid=$(id -u 2>/dev/null) || { emit_empty account failed; exit 0; }
home=${HOME-}
login_shell=${SHELL-}
[ -n "$user" ] && [ -n "$uid" ] && [ -n "$home" ] && [ -n "$login_shell" ] || { emit_empty account failed; exit 0; }
case "$login_shell" in
    /*) ;;
    *) emit_empty account unsupported-login-shell; exit 0 ;;
esac
login_shell_name=${login_shell##*/}
posix_sh_login_capability=not-applicable
if [ "$login_shell_name" = sh ]; then
    if "$login_shell" -l -c ':' </dev/null >/dev/null 2>&1; then
        posix_sh_login_capability=login-option-supported
    else
        emit_empty account unsupported-login-shell
        exit 0
    fi
fi
physical_home_output=$(cd "$home" 2>/dev/null && { pwd -P && printf .; }) || { emit_empty account failed; exit 0; }
physical_home_with_separator=${physical_home_output%?}
physical_home=${physical_home_with_separator%?}
emit_header account ok
emit_field "$user"
emit_field "$uid"
emit_field "$home"
emit_field "$login_shell"
emit_field "$physical_home"
emit_field "$posix_sh_login_capability"
printf '.\n'
"#;

const PROBE_SCRIPT: &str = r#"if [ "$path_status" != exists ]; then
    emit_empty probe "$path_status"
elif [ ! -d "$remote_path" ]; then
    emit_empty probe not-directory
elif [ ! -r "$remote_path" ] || [ ! -x "$remote_path" ]; then
    emit_empty probe permission-denied
else
    emit_empty probe ok
fi
"#;

// POSIX has no dependency-free streaming directory API. The supervised `find` child is bounded by
// examined rows here and by the transport's wall-clock and output limits on every request.
const LIST_SCRIPT_TEMPLATE: &str = r#"if [ "$path_status" != exists ]; then
    emit_empty list "$path_status"
    exit 0
fi
if [ ! -d "$remote_path" ]; then
    emit_empty list not-directory
    exit 0
fi
if [ ! -r "$remote_path" ] || [ ! -x "$remote_path" ]; then
    emit_empty list permission-denied
    exit 0
fi
state_directory=$(umask 077; mktemp -d "${TMPDIR:-/tmp}/spaceterm-list.XXXXXXXXXX") || {
    emit_empty list failed
    exit 0
}
state_file=$state_directory/state
result_file=$state_directory/result
enumerator_pid=
cleanup_listing_state() {
    if [ -n "$enumerator_pid" ]; then
        kill -TERM "$enumerator_pid" 2>/dev/null
        wait "$enumerator_pid" 2>/dev/null
        enumerator_pid=
    fi
    rm -f "$state_file" "$result_file"
    rmdir "$state_directory"
}
cancel_listing() {
    cleanup_listing_state
    trap - EXIT HUP INT TERM
    exit 129
}
trap cleanup_listing_state EXIT
trap cancel_listing HUP INT TERM
printf '0 0 0\n' > "$state_file" || { emit_empty list failed; exit 0; }
: > "$result_file" || { emit_empty list failed; exit 0; }
find "$remote_path"/. ! -name . -prune -exec /bin/sh -c '
    state_file=$1
    result_file=$2
    child=$3
    IFS=" " read -r examined emitted truncated < "$state_file" || exit 70
    examined=$((examined + 1))
    if [ "$examined" -gt __MAXIMUM_ENTRIES_EXAMINED__ ]; then
        truncated=1
        printf "%s %s %s\n" "$examined" "$emitted" "$truncated" > "$state_file" || exit 70
        kill -TERM "$PPID"
        exit 0
    fi
    if [ -d "$child" ]; then
        emitted=$((emitted + 1))
        if [ "$emitted" -gt __MAXIMUM_DIRECTORY_NAMES__ ]; then
            truncated=1
            printf "%s %s %s\n" "$examined" "$emitted" "$truncated" > "$state_file" || exit 70
            kill -TERM "$PPID"
            exit 0
        fi
        child_name=${child##*/}
        field_length=$(LC_ALL=C printf "%s" "$child_name" | LC_ALL=C wc -c | tr -d "[:space:]") || exit 70
        {
            printf "%s:" "$field_length"
            printf "%s" "$child_name"
            printf ","
        } >> "$result_file" || exit 70
    fi
    printf "%s %s %s\n" "$examined" "$emitted" "$truncated" > "$state_file" || exit 70
' spaceterm-enumerate "$state_file" "$result_file" {} \; 2>/dev/null &
enumerator_pid=$!
if wait "$enumerator_pid"; then
    enumerator_status=0
else
    enumerator_status=$?
fi
enumerator_pid=
IFS=' ' read -r examined emitted listing_truncated < "$state_file" || {
    emit_empty list failed
    exit 0
}
if [ "$enumerator_status" -ne 0 ] && [ "$listing_truncated" -ne 1 ]; then
    emit_empty list failed
    exit 0
fi
emit_header list ok
cat "$result_file" || exit 70
printf '.\n%s\n' "$listing_truncated"
"#;

const MKDIR_SCRIPT: &str = r#"if [ "$path_status" = not-directory ]; then
    emit_empty mkdir not-directory
elif [ "$path_status" = permission-denied ]; then
    emit_empty mkdir permission-denied
elif [ -e "$remote_path" ] && [ ! -d "$remote_path" ]; then
    emit_empty mkdir not-directory
elif mkdir -p "$remote_path" 2>/dev/null && [ -d "$remote_path" ]; then
    emit_empty mkdir ok
elif [ -e "$remote_path" ] && [ -d "$remote_path" ] && { [ ! -r "$remote_path" ] || [ ! -x "$remote_path" ]; }; then
    emit_empty mkdir permission-denied
else
    emit_empty mkdir failed
fi
"#;

const PHYSICAL_SCRIPT: &str = r#"if [ "$path_status" != exists ]; then
    emit_empty physical "$path_status"
    exit 0
fi
if [ ! -d "$remote_path" ]; then
    emit_empty physical not-directory
    exit 0
fi
physical_path_output=$(cd "$remote_path" 2>/dev/null && { pwd -P && printf .; }) || {
    if [ -e "$remote_path" ] && [ -d "$remote_path" ] && { [ ! -r "$remote_path" ] || [ ! -x "$remote_path" ]; }; then
        emit_empty physical permission-denied
    else
        emit_empty physical failed
    fi
    exit 0
}
physical_path_with_separator=${physical_path_output%?}
physical_path=${physical_path_with_separator%?}
emit_header physical ok
emit_field "$physical_path"
printf '.\n'
"#;

fn parse_account(output: &[u8]) -> Result<RemoteAccountMetadata, RemoteUtilityError> {
    let mut response = ResponseParser::new(output, "account")?;
    response.require_ok()?;
    let fields = response.finish_fields(6)?;
    if fields[1].is_empty()
        || !fields[1].bytes().all(|byte| byte.is_ascii_digit())
        || (fields[1].len() > 1 && fields[1].starts_with('0'))
    {
        return Err(RemoteUtilityError::InvalidResponse);
    }
    let uid = fields[1]
        .parse::<u64>()
        .map_err(|_| RemoteUtilityError::InvalidResponse)?;
    if fields[0].is_empty()
        || fields[0]
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        || !fields[2].starts_with('/')
        || !fields[3].starts_with('/')
        || !fields[4].starts_with('/')
        || fields[2..5]
            .iter()
            .any(|field| field.chars().any(char::is_control))
    {
        return Err(RemoteUtilityError::InvalidResponse);
    }
    let posix_sh_login_capability = match fields[5].as_str() {
        "not-applicable" => PosixShLoginCapability::NotApplicable,
        "login-option-supported" => PosixShLoginCapability::LoginOptionSupported,
        _ => return Err(RemoteUtilityError::InvalidResponse),
    };
    Ok(RemoteAccountMetadata {
        user: fields[0].clone(),
        uid,
        home: fields[2].clone(),
        login_shell: fields[3].clone(),
        physical_home: fields[4].clone(),
        posix_sh_login_capability,
    })
}

fn parse_listing(output: &[u8]) -> Result<RemoteUtilityDirectoryListing, RemoteUtilityError> {
    let mut response = ResponseParser::new(output, "list")?;
    response.require_ok()?;
    let raw_names = response.read_raw_fields(MAXIMUM_REMOTE_DIRECTORY_NAMES)?;
    let mut truncated = match response.read_line()? {
        b"0" => false,
        b"1" => true,
        _ => return Err(RemoteUtilityError::InvalidResponse),
    };
    response.require_end()?;
    let names = raw_names
        .into_iter()
        .filter_map(|raw_name| match str::from_utf8(raw_name) {
            Ok(name)
                if !name.is_empty()
                    && name != "."
                    && name != ".."
                    && !name.contains('/')
                    && !name.chars().any(char::is_control) =>
            {
                Some(name.to_owned())
            }
            _ => {
                truncated = true;
                None
            }
        })
        .collect();
    Ok(RemoteUtilityDirectoryListing { names, truncated })
}

fn parse_probe(output: &[u8]) -> Result<RemoteDirectoryProbe, RemoteUtilityError> {
    let mut response = ResponseParser::new(output, "probe")?;
    match response.status {
        b"ok" => {
            response.finish_fields(0)?;
            Ok(RemoteDirectoryProbe::ReadableDirectory)
        }
        b"missing" => {
            response.finish_fields(0)?;
            Ok(RemoteDirectoryProbe::Missing)
        }
        _ => {
            response.finish_fields(0)?;
            Err(response.status_error())
        }
    }
}

fn parse_empty_success(output: &[u8], kind: &str) -> Result<(), RemoteUtilityError> {
    let mut response = ResponseParser::new(output, kind)?;
    response.require_ok()?;
    response.finish_fields(0)?;
    Ok(())
}

fn parse_physical(output: &[u8]) -> Result<String, RemoteUtilityError> {
    let mut response = ResponseParser::new(output, "physical")?;
    response.require_ok()?;
    let fields = response.finish_fields(1)?;
    fields
        .into_iter()
        .next()
        .ok_or(RemoteUtilityError::InvalidResponse)
}

struct ResponseParser<'a> {
    input: &'a [u8],
    cursor: usize,
    status: &'a [u8],
}

impl<'a> ResponseParser<'a> {
    fn new(output: &'a [u8], expected_kind: &str) -> Result<Self, RemoteUtilityError> {
        if output.len() > MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES {
            return Err(RemoteUtilityError::OutputTooLarge);
        }
        let mut response = Self {
            input: output,
            cursor: 0,
            status: b"",
        };
        if response.read_line()? != PROTOCOL_HEADER.as_bytes()
            || response.read_line()? != expected_kind.as_bytes()
        {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        response.status = response.read_line()?;
        Ok(response)
    }

    fn require_ok(&mut self) -> Result<(), RemoteUtilityError> {
        if self.status == b"ok" {
            Ok(())
        } else {
            self.finish_fields(0)?;
            Err(self.status_error())
        }
    }

    fn status_error(&self) -> RemoteUtilityError {
        match self.status {
            b"missing" => RemoteUtilityError::Missing,
            b"not-directory" => RemoteUtilityError::NotDirectory,
            b"permission-denied" => RemoteUtilityError::PermissionDenied,
            b"unsupported-login-shell" => RemoteUtilityError::UnsupportedLoginShell,
            b"failed" => RemoteUtilityError::RemoteFailed,
            _ => RemoteUtilityError::InvalidResponse,
        }
    }

    fn finish_fields(&mut self, expected_count: usize) -> Result<Vec<String>, RemoteUtilityError> {
        let fields = self.read_fields(expected_count)?;
        if fields.len() != expected_count {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        self.require_end()?;
        Ok(fields)
    }

    fn read_fields(&mut self, maximum_count: usize) -> Result<Vec<String>, RemoteUtilityError> {
        self.read_raw_fields(maximum_count)?
            .into_iter()
            .map(|field| {
                str::from_utf8(field)
                    .map(str::to_owned)
                    .map_err(|_| RemoteUtilityError::InvalidResponse)
            })
            .collect()
    }

    fn read_raw_fields(
        &mut self,
        maximum_count: usize,
    ) -> Result<Vec<&'a [u8]>, RemoteUtilityError> {
        let mut fields = Vec::new();
        loop {
            if self.remaining().starts_with(b".\n") {
                self.cursor += 2;
                return Ok(fields);
            }
            if fields.len() >= maximum_count {
                return Err(RemoteUtilityError::InvalidResponse);
            }
            fields.push(self.read_netstring()?);
        }
    }

    fn read_netstring(&mut self) -> Result<&'a [u8], RemoteUtilityError> {
        let remaining = self.remaining();
        let colon = remaining
            .iter()
            .position(|byte| *byte == b':')
            .ok_or(RemoteUtilityError::InvalidResponse)?;
        let length_spelling = &remaining[..colon];
        if length_spelling.is_empty()
            || !length_spelling.iter().all(u8::is_ascii_digit)
            || (length_spelling.len() > 1 && length_spelling.starts_with(b"0"))
        {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        let length = str::from_utf8(length_spelling)
            .map_err(|_| RemoteUtilityError::InvalidResponse)?
            .parse::<usize>()
            .map_err(|_| RemoteUtilityError::InvalidResponse)?;
        if length > MAXIMUM_REMOTE_FIELD_BYTES {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        let field_start = self.cursor + colon + 1;
        let field_end = field_start
            .checked_add(length)
            .ok_or(RemoteUtilityError::InvalidResponse)?;
        if self.input.get(field_end) != Some(&b',') {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        let field = &self.input[field_start..field_end];
        self.cursor = field_end + 1;
        Ok(field)
    }

    fn read_line(&mut self) -> Result<&'a [u8], RemoteUtilityError> {
        let remaining = self.remaining();
        let newline = remaining
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or(RemoteUtilityError::InvalidResponse)?;
        let line = &remaining[..newline];
        self.cursor += newline + 1;
        Ok(line)
    }

    fn require_end(&self) -> Result<(), RemoteUtilityError> {
        if self.cursor == self.input.len() {
            Ok(())
        } else {
            Err(RemoteUtilityError::InvalidResponse)
        }
    }

    fn remaining(&self) -> &'a [u8] {
        &self.input[self.cursor..]
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::future::Future;
    use std::io;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Wake, Waker};

    use gpui::TestAppContext;

    use super::*;
    use crate::domain::{RemoteWorkspaceDirectory, SshDestination};
    use crate::ssh::command::{SshCommandContext, SshCommandSpec};
    use crate::ssh::control_connection::SshCancellationToken;
    use crate::ssh::process::{ProcessExit, SshProcessEnvironment};

    #[derive(Default)]
    struct FakeRunnerState {
        responses: VecDeque<Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>>,
        scripts: Vec<Vec<u8>>,
    }

    #[derive(Default)]
    struct FakeRunner {
        state: Mutex<FakeRunnerState>,
    }

    impl FakeRunner {
        fn with_responses(
            responses: impl IntoIterator<
                Item = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>,
            >,
        ) -> Self {
            Self {
                state: Mutex::new(FakeRunnerState {
                    responses: responses.into_iter().collect(),
                    scripts: Vec::new(),
                }),
            }
        }
    }

    impl SshRemoteUtilityRunner for FakeRunner {
        fn run(
            &self,
            _command: Arc<SshCommandSpec>,
            script: Vec<u8>,
            _maximum_output_bytes: usize,
            _cancellation: SshCancellationToken,
        ) -> impl Future<Output = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>> + Send
        {
            let result = {
                let mut state = self.state.lock().unwrap();
                state.scripts.push(script);
                state.responses.pop_front().unwrap_or_else(|| {
                    Err(RemoteUtilityRunError::Io(io::Error::other(
                        "missing fake response",
                    )))
                })
            };
            async move { result }
        }
    }

    #[test]
    fn dropping_native_utility_future_should_cancel_and_reap_the_private_group() {
        let pid_file = PathBuf::from(format!(
            "/private/tmp/spaceterm-utility-drop-{}.pid",
            std::process::id()
        ));
        let script = format!("echo $$ > '{}'; sleep 30", pid_file.display());
        let command = Arc::new(SshCommandSpec::for_test(
            PathBuf::from("/bin/sh"),
            vec!["-c".into(), script.into()],
        ));
        let environment =
            SshProcessEnvironment::new_without_authentication(PathBuf::from("/private/tmp"), None)
                .unwrap();
        let runner = NativeSshRemoteUtilityRunner::new(environment);
        let mut future = Box::pin(runner.run(
            command,
            Vec::new(),
            MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES,
            SshCancellationToken::default(),
        ));
        let waker = Waker::from(Arc::new(NoopWake));
        let mut context = Context::from_waker(&waker);

        assert!(matches!(
            Pin::as_mut(&mut future).poll(&mut context),
            Poll::Pending
        ));
        for _ in 0..100 {
            if pid_file.exists() {
                break;
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
        let process: libc::pid_t = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        drop(future);

        let terminated = (0..100).any(|_| {
            // SAFETY: signal zero checks process existence and dereferences no pointers.
            let missing = unsafe { libc::kill(process, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if !missing {
                thread::sleep(PROCESS_POLL_INTERVAL);
            }
            missing
        });
        let _ = fs::remove_file(pid_file);
        assert!(terminated);
    }

    #[test]
    fn completed_native_utility_should_not_cancel_the_reusable_client_token() {
        let command = Arc::new(SshCommandSpec::for_test(
            PathBuf::from("/bin/sh"),
            vec!["-s".into()],
        ));
        let environment =
            SshProcessEnvironment::new_without_authentication(PathBuf::from("/private/tmp"), None)
                .unwrap();
        let runner = NativeSshRemoteUtilityRunner::new(environment);
        let cancellation = SshCancellationToken::default();

        let output = block_on_external(runner.run(
            command,
            b"printf ok\n".to_vec(),
            MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES,
            cancellation.clone(),
        ))
        .unwrap();

        assert!(output.exit.is_success() && output.stdout == b"ok" && !cancellation.is_cancelled());
    }

    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
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

    fn client(
        responses: impl IntoIterator<Item = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>>,
    ) -> (SshRemoteUtilityClient<FakeRunner>, Arc<FakeRunner>) {
        let command = SshCommandContext::new(
            PathBuf::from("/private/config/spaceterm/ssh_config"),
            SshDestination::new("remote".to_owned()).unwrap(),
            PathBuf::from("/private/runtime/spaceterm/master.sock"),
        )
        .unwrap()
        .remote_utility();
        let runner = Arc::new(FakeRunner::with_responses(responses));
        (
            SshRemoteUtilityClient::new(
                PreparedSshRemoteUtilityCommand::new(command),
                Arc::clone(&runner),
                SshCancellationToken::default(),
            ),
            runner,
        )
    }

    fn response(kind: &str, status: &str, fields: &[&str], tail: &str) -> Vec<u8> {
        let mut response = format!("SPACETERM-REMOTE/1\n{kind}\n{status}\n").into_bytes();
        for field in fields {
            response.extend_from_slice(format!("{}:", field.len()).as_bytes());
            response.extend_from_slice(field.as_bytes());
            response.push(b',');
        }
        response.extend_from_slice(b".\n");
        response.extend_from_slice(tail.as_bytes());
        response
    }

    fn success(stdout: Vec<u8>) -> Result<RemoteUtilityProcessOutput, RemoteUtilityRunError> {
        Ok(RemoteUtilityProcessOutput::new(
            ProcessExit::successful(),
            stdout,
        ))
    }

    fn remote_directory(value: &str) -> RemoteWorkspaceDirectory {
        RemoteWorkspaceDirectory::new(value.to_owned()).unwrap()
    }

    #[test]
    fn generated_remote_scripts_should_be_valid_posix_shell_syntax() {
        for script in [
            build_account_script(),
            build_path_script("list", "/tmp/space ' with quote").unwrap(),
            build_path_script("probe", "~/project").unwrap(),
            build_path_script("mkdir", "/tmp/-leading").unwrap(),
            build_path_script("physical", "/tmp/project").unwrap(),
        ] {
            let mut child = Command::new("/bin/sh")
                .arg("-n")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&script).unwrap();
            let output = child.wait_with_output().unwrap();

            assert!(
                output.status.success(),
                "generated script failed syntax validation: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[gpui::test]
    fn account_metadata_should_validate_all_versioned_fields(cx: &mut TestAppContext) {
        let (client, _) = client([success(response(
            "account",
            "ok",
            &[
                "tester",
                "501",
                "/Users/tester",
                "/bin/zsh",
                "/Users/tester",
                "not-applicable",
            ],
            "",
        ))]);

        let metadata = cx.executor().block(client.discover_account()).unwrap();

        assert_eq!(metadata.user(), "tester");
        assert_eq!(metadata.uid(), 501);
        assert_eq!(metadata.home(), "/Users/tester");
        assert_eq!(metadata.login_shell(), "/bin/zsh");
        assert_eq!(metadata.physical_home(), "/Users/tester");
        assert_eq!(
            metadata.posix_sh_login_capability(),
            PosixShLoginCapability::NotApplicable
        );
    }

    #[gpui::test]
    fn account_metadata_should_preserve_verified_posix_sh_login_capability(
        cx: &mut TestAppContext,
    ) {
        let (client, _) = client([success(response(
            "account",
            "ok",
            &[
                "tester",
                "501",
                "/Users/tester",
                "/bin/sh",
                "/Users/tester",
                "login-option-supported",
            ],
            "",
        ))]);

        let metadata = cx.executor().block(client.discover_account()).unwrap();

        assert_eq!(
            metadata.posix_sh_login_capability(),
            PosixShLoginCapability::LoginOptionSupported
        );
    }

    #[gpui::test]
    fn account_metadata_should_map_unsupported_posix_sh_login_mode(cx: &mut TestAppContext) {
        let (client, _) = client([success(response(
            "account",
            "unsupported-login-shell",
            &[],
            "",
        ))]);

        assert_eq!(
            cx.executor().block(client.discover_account()).unwrap_err(),
            RemoteUtilityError::UnsupportedLoginShell
        );
    }

    #[test]
    fn account_script_should_reject_a_conforming_sh_without_a_login_option() {
        let test_root = PathBuf::from(format!(
            "/private/tmp/spaceterm-account-sh-reject-{}",
            std::process::id()
        ));
        let home = test_root.join("home");
        let fake_bin = test_root.join("bin");
        let fake_shell = fake_bin.join("sh");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        fs::write(&fake_shell, b"#!/bin/sh\nexit 64\n").unwrap();
        fs::set_permissions(&fake_shell, fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = Command::new("/bin/sh")
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("SHELL", &fake_shell)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&build_account_script())
            .unwrap();
        let output = child.wait_with_output().unwrap();

        assert!(output.status.success());
        assert_eq!(
            parse_account(&output.stdout).unwrap_err(),
            RemoteUtilityError::UnsupportedLoginShell
        );
        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn account_script_should_reject_relative_sh_without_executing_it() {
        let test_root = PathBuf::from(format!(
            "/private/tmp/spaceterm-account-relative-sh-{}",
            std::process::id()
        ));
        let home = test_root.join("home");
        let fake_bin = test_root.join("bin");
        let fake_shell = fake_bin.join("sh");
        let execution_marker = test_root.join("executed");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        fs::write(
            &fake_shell,
            format!("#!/bin/sh\ntouch '{}'\n", execution_marker.display()),
        )
        .unwrap();
        fs::set_permissions(&fake_shell, fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = Command::new("/bin/sh")
            .env_clear()
            .env("HOME", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
            .env("SHELL", "sh")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&build_account_script())
            .unwrap();
        let output = child.wait_with_output().unwrap();

        assert_eq!(
            parse_account(&output.stdout).unwrap_err(),
            RemoteUtilityError::UnsupportedLoginShell
        );
        assert!(!execution_marker.exists());
        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn account_script_should_record_a_supported_posix_sh_login_option() {
        let test_root = PathBuf::from(format!(
            "/private/tmp/spaceterm-account-sh-accept-{}",
            std::process::id()
        ));
        let home = test_root.join("home");
        let fake_bin = test_root.join("bin");
        let fake_shell = fake_bin.join("sh");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        fs::write(
            &fake_shell,
            b"#!/bin/sh\n[ \"$#\" -eq 3 ] && [ \"$1\" = -l ] && [ \"$2\" = -c ]\n",
        )
        .unwrap();
        fs::set_permissions(&fake_shell, fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = Command::new("/bin/sh")
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("SHELL", &fake_shell)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&build_account_script())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let metadata = parse_account(&output.stdout).unwrap();

        assert!(output.status.success());
        assert_eq!(
            metadata.posix_sh_login_capability(),
            PosixShLoginCapability::LoginOptionSupported
        );
        fs::remove_dir_all(test_root).unwrap();
    }

    #[gpui::test]
    fn listing_should_skip_hostile_names_without_losing_safe_siblings(cx: &mut TestAppContext) {
        let mut listing_response = response(
            "list",
            "ok",
            &["Space Term", "line\nbreak", "after hostile"],
            "1\n",
        );
        let fields_start = b"SPACETERM-REMOTE/1\nlist\nok\n".len();
        listing_response.splice(fields_start..fields_start, [b'1', b':', 0xff, b',']);
        let (client, _) = client([success(listing_response)]);

        let listing = cx
            .executor()
            .block(client.list_directories(remote_directory("/srv/projects")))
            .unwrap();

        assert_eq!(listing.names(), ["Space Term", "after hostile"]);
        assert!(listing.is_truncated());
    }

    #[gpui::test]
    fn path_requests_should_be_single_quoted_without_shell_interpolation(cx: &mut TestAppContext) {
        let (client, runner) = client([success(response("probe", "missing", &[], ""))]);
        let path = "/tmp/space ' $(touch should-not-run) `false`";

        let state = cx
            .executor()
            .block(client.probe_exact_path(remote_directory(path)))
            .unwrap();

        assert_eq!(state, RemoteDirectoryProbe::Missing);
        let scripts = &runner.state.lock().unwrap().scripts;
        let script = std::str::from_utf8(&scripts[0]).unwrap();
        assert!(script.contains("input_path='/tmp/space '\"'\"' $(touch should-not-run) `false`'"));
    }

    #[gpui::test]
    fn path_request_limit_should_be_checked_before_quote_expansion(cx: &mut TestAppContext) {
        let (client, runner) = client([success(response("probe", "missing", &[], ""))]);
        let accepted = format!("/{}", "'".repeat(MAXIMUM_REMOTE_PATH_BYTES - 1));

        assert_eq!(
            cx.executor()
                .block(client.probe_exact_path(remote_directory(&accepted)))
                .unwrap(),
            RemoteDirectoryProbe::Missing
        );
        let accepted_script_length = runner.state.lock().unwrap().scripts[0].len();
        assert!(accepted_script_length <= MAXIMUM_REMOTE_UTILITY_REQUEST_BYTES);

        let rejected = format!("/{}", "'".repeat(MAXIMUM_REMOTE_PATH_BYTES));
        assert_eq!(
            cx.executor()
                .block(client.probe_exact_path(remote_directory(&rejected)))
                .unwrap_err(),
            RemoteUtilityError::RequestTooLarge
        );
        assert_eq!(runner.state.lock().unwrap().scripts.len(), 1);
    }

    #[test]
    fn listing_script_should_bound_examined_entries_without_shell_globs() {
        let script =
            String::from_utf8(build_path_script("list", "/srv/-'projects").unwrap()).unwrap();

        assert!(!script.contains("\"$remote_path\"/*"));
        assert!(script.contains(&MAXIMUM_REMOTE_DIRECTORY_ENTRIES_EXAMINED.to_string()));
        assert!(script.contains(&MAXIMUM_REMOTE_DIRECTORY_NAMES.to_string()));
        assert!(script.contains("find \"$remote_path\"/."));
        assert!(script.contains("2>/dev/null &"));
        assert!(script.contains("kill -TERM \"$enumerator_pid\""));
        assert!(script.contains("wait \"$enumerator_pid\""));
    }

    #[test]
    fn injected_million_entry_enumerator_should_stop_at_the_examination_bound() {
        let test_root = PathBuf::from(format!(
            "/private/tmp/spaceterm-list-bound-{}",
            std::process::id()
        ));
        let fake_bin = test_root.join("bin");
        let fake_find = fake_bin.join("find");
        let count_file = test_root.join("count");
        let child_file = test_root.join("ordinary-file");
        fs::create_dir_all(&fake_bin).unwrap();
        fs::write(&child_file, b"not a directory").unwrap();
        fs::write(
            &fake_find,
            br#"#!/bin/sh
shift 6
index=0
while [ "$index" -lt 1000000 ]; do
    index=$((index + 1))
    printf '%s\n' "$index" > "$SPACETERM_FAKE_FIND_COUNT"
    /bin/sh -c "$3" "$4" "$5" "$6" "$SPACETERM_FAKE_CHILD"
done
"#,
        )
        .unwrap();
        fs::set_permissions(&fake_find, fs::Permissions::from_mode(0o700)).unwrap();
        let script = build_path_script("list", test_root.to_str().unwrap()).unwrap();
        let mut child = Command::new("/bin/sh")
            .env_clear()
            .env("HOME", "/private/tmp")
            .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
            .env("TMPDIR", &test_root)
            .env("SPACETERM_FAKE_FIND_COUNT", &count_file)
            .env("SPACETERM_FAKE_CHILD", &child_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&script).unwrap();
        let output = child.wait_with_output().unwrap();

        assert!(
            output.status.success(),
            "bounded list script failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let listing = parse_listing(&output.stdout).unwrap();
        assert!(listing.names().is_empty());
        assert!(listing.is_truncated());
        assert_eq!(
            fs::read_to_string(&count_file).unwrap().trim(),
            (MAXIMUM_REMOTE_DIRECTORY_ENTRIES_EXAMINED + 1).to_string()
        );
        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn listing_enumerator_should_be_terminated_and_waited_when_remote_shell_is_cancelled() {
        let test_root = PathBuf::from(format!(
            "/private/tmp/spaceterm-list-cancel-{}",
            std::process::id()
        ));
        let fake_bin = test_root.join("bin");
        let fake_find = fake_bin.join("find");
        let pid_file = test_root.join("enumerator-pid");
        let ready_file = test_root.join("enumerator-ready");
        let stopped_file = test_root.join("enumerator-stopped");
        let _ = fs::remove_dir_all(&test_root);
        fs::create_dir_all(&fake_bin).unwrap();
        fs::write(
            &fake_find,
            br#"#!/bin/sh
printf '%s\n' "$$" > "$SPACETERM_ENUMERATOR_PID"
trap 'printf stopped > "$SPACETERM_ENUMERATOR_STOPPED"; exit 0' TERM
printf ready > "$SPACETERM_ENUMERATOR_READY"
while :; do /bin/sleep 1; done
"#,
        )
        .unwrap();
        fs::set_permissions(&fake_find, fs::Permissions::from_mode(0o700)).unwrap();
        let script = build_path_script("list", test_root.to_str().unwrap()).unwrap();
        let mut child = Command::new("/bin/sh")
            .env_clear()
            .env("HOME", "/private/tmp")
            .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
            .env("TMPDIR", &test_root)
            .env("SPACETERM_ENUMERATOR_PID", &pid_file)
            .env("SPACETERM_ENUMERATOR_READY", &ready_file)
            .env("SPACETERM_ENUMERATOR_STOPPED", &stopped_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&script).unwrap();
        let readiness_deadline = Instant::now() + Duration::from_secs(10);
        while !ready_file.exists() && Instant::now() < readiness_deadline {
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
        assert!(ready_file.exists(), "fake enumerator did not become ready");
        let enumerator: libc::pid_t = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        // SAFETY: this signals only the child shell created by this test.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        let status = child.wait().unwrap();

        assert!(!status.success());
        assert!(stopped_file.exists());
        // SAFETY: signal zero checks process existence and dereferences no pointers.
        assert_eq!(unsafe { libc::kill(enumerator, 0) }, -1);
        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn generated_listing_should_preserve_argv_safe_directory_names() {
        let test_root = PathBuf::from(format!(
            "/private/tmp/spaceterm-list-names-{}",
            std::process::id()
        ));
        let expected = ["Space Term", "-'quoted", ".hidden"];
        fs::create_dir_all(&test_root).unwrap();
        for name in expected {
            fs::create_dir(test_root.join(name)).unwrap();
        }
        fs::create_dir(test_root.join("line\nbreak")).unwrap();
        fs::write(test_root.join("ordinary-file"), b"ignored").unwrap();
        let script = build_path_script("list", test_root.to_str().unwrap()).unwrap();
        let mut child = Command::new("/bin/sh")
            .env_clear()
            .env("HOME", "/private/tmp")
            .env("PATH", "/usr/bin:/bin")
            .env("TMPDIR", "/private/tmp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&script).unwrap();
        let output = child.wait_with_output().unwrap();
        fs::remove_dir_all(&test_root).unwrap();

        assert!(
            output.status.success(),
            "listing script failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let listing = parse_listing(&output.stdout).unwrap();
        for name in expected {
            assert!(listing.names().iter().any(|candidate| candidate == name));
        }
        assert_eq!(listing.names().len(), expected.len());
        assert!(listing.is_truncated());
    }

    #[test]
    fn ambiguous_mkdir_failure_should_not_claim_permission_denied() {
        let test_root = PathBuf::from(format!(
            "/private/tmp/spaceterm-mkdir-failed-{}",
            std::process::id()
        ));
        let fake_bin = test_root.join("bin");
        fs::create_dir_all(&fake_bin).unwrap();
        let fake_mkdir = fake_bin.join("mkdir");
        fs::write(&fake_mkdir, b"#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&fake_mkdir, fs::Permissions::from_mode(0o700)).unwrap();
        let target = test_root.join("missing/child");
        let script = build_path_script("mkdir", target.to_str().unwrap()).unwrap();
        let mut child = Command::new("/bin/sh")
            .env_clear()
            .env("HOME", "/private/tmp")
            .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&script).unwrap();
        let output = child.wait_with_output().unwrap();
        fs::remove_dir_all(test_root).unwrap();

        assert_eq!(
            parse_empty_success(&output.stdout, "mkdir").unwrap_err(),
            RemoteUtilityError::RemoteFailed
        );
    }

    #[test]
    fn ambiguous_physical_path_failure_should_not_claim_permission_denied() {
        assert!(PHYSICAL_SCRIPT.contains("emit_empty physical failed"));
        assert_eq!(
            parse_physical(&response("physical", "failed", &[], "")).unwrap_err(),
            RemoteUtilityError::RemoteFailed
        );
    }

    #[gpui::test]
    fn probe_create_and_physical_identity_should_decode_typed_results(cx: &mut TestAppContext) {
        let (client, _) = client([
            success(response("probe", "ok", &[], "")),
            success(response("mkdir", "ok", &[], "")),
            success(response("physical", "ok", &["/srv/real project"], "")),
        ]);
        let directory = remote_directory("/srv/project");

        assert_eq!(
            cx.executor()
                .block(client.probe_exact_path(directory.clone()))
                .unwrap(),
            RemoteDirectoryProbe::ReadableDirectory
        );
        cx.executor()
            .block(client.create_directory_recursively(directory.clone()))
            .unwrap();
        assert_eq!(
            cx.executor()
                .block(client.resolve_physical_directory(directory))
                .unwrap(),
            "/srv/real project"
        );
    }

    #[gpui::test]
    fn probe_should_distinguish_missing_type_and_access_errors(cx: &mut TestAppContext) {
        let (client, _) = client([
            success(response("probe", "missing", &[], "")),
            success(response("probe", "not-directory", &[], "")),
            success(response("probe", "permission-denied", &[], "")),
        ]);

        assert_eq!(
            cx.executor()
                .block(client.probe_exact_path(remote_directory("/srv/missing")))
                .unwrap(),
            RemoteDirectoryProbe::Missing
        );
        assert_eq!(
            cx.executor()
                .block(client.probe_exact_path(remote_directory("/srv/file/child")))
                .unwrap_err(),
            RemoteUtilityError::NotDirectory
        );
        assert_eq!(
            cx.executor()
                .block(client.probe_exact_path(remote_directory("/srv/private/child")))
                .unwrap_err(),
            RemoteUtilityError::PermissionDenied
        );
    }

    #[test]
    fn generated_probe_should_report_an_inaccessible_ancestor_without_claiming_missing() {
        let test_root = PathBuf::from(format!(
            "/private/tmp/spaceterm-probe-status-{}",
            std::process::id()
        ));
        let private = test_root.join("private");
        let ordinary_file = test_root.join("ordinary-file");
        fs::create_dir_all(&private).unwrap();
        fs::write(&ordinary_file, b"not a directory").unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o000)).unwrap();

        let run_probe = |path: &std::path::Path| {
            let script = build_path_script("probe", path.to_str().unwrap()).unwrap();
            let mut child = Command::new("/bin/sh")
                .env_clear()
                .env("HOME", "/private/tmp")
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&script).unwrap();
            child.wait_with_output().unwrap()
        };

        let inaccessible = run_probe(&private.join("child"));
        let not_directory = run_probe(&ordinary_file.join("child"));
        let missing = run_probe(&test_root.join("missing/child"));
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(&test_root).unwrap();

        assert_eq!(
            parse_probe(&inaccessible.stdout).unwrap_err(),
            RemoteUtilityError::PermissionDenied
        );
        assert_eq!(
            parse_probe(&not_directory.stdout).unwrap_err(),
            RemoteUtilityError::NotDirectory
        );
        assert_eq!(
            parse_probe(&missing.stdout).unwrap(),
            RemoteDirectoryProbe::Missing
        );
    }

    #[test]
    fn native_utility_should_force_cleanup_at_its_wall_clock_deadline() {
        let command = Arc::new(SshCommandSpec::for_test(
            PathBuf::from("/bin/sh"),
            vec!["-c".into(), "sleep 30".into()],
        ));
        let environment =
            SshProcessEnvironment::new_without_authentication(PathBuf::from("/private/tmp"), None)
                .unwrap();
        let runner =
            NativeSshRemoteUtilityRunner::with_timeout(environment, Duration::from_millis(20));

        let error = block_on_external(runner.run(
            command,
            Vec::new(),
            MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES,
            SshCancellationToken::default(),
        ))
        .unwrap_err();

        assert!(matches!(error, RemoteUtilityRunError::TimedOut));
    }

    #[gpui::test]
    fn malformed_truncated_and_oversized_output_should_be_rejected(cx: &mut TestAppContext) {
        let (client, _) = client([
            success(b"SPACETERM-REMOTE/2\nprobe\nok\n.\n".to_vec()),
            success(b"SPACETERM-REMOTE/1\nphysical\nok\n12:/short".to_vec()),
            success(vec![b'x'; MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES + 1]),
        ]);

        assert_eq!(
            cx.executor()
                .block(client.probe_exact_path(remote_directory("/one")))
                .unwrap_err(),
            RemoteUtilityError::InvalidResponse
        );
        assert_eq!(
            cx.executor()
                .block(client.resolve_physical_directory(remote_directory("/two")))
                .unwrap_err(),
            RemoteUtilityError::InvalidResponse
        );
        assert_eq!(
            cx.executor()
                .block(client.probe_exact_path(remote_directory("/three")))
                .unwrap_err(),
            RemoteUtilityError::OutputTooLarge
        );
    }

    #[gpui::test]
    fn remote_command_failure_and_cancellation_should_remain_typed(cx: &mut TestAppContext) {
        let (failed, _) = client([Ok(RemoteUtilityProcessOutput::new(
            ProcessExit::unsuccessful(Some(255)),
            Vec::new(),
        ))]);
        assert_eq!(
            cx.executor()
                .block(failed.probe_exact_path(remote_directory("/srv")))
                .unwrap_err(),
            RemoteUtilityError::CommandFailed(Some(255))
        );

        let cancellation = SshCancellationToken::default();
        cancellation.cancel();
        let command = SshCommandContext::new(
            PathBuf::from("/private/config/spaceterm/ssh_config"),
            SshDestination::new("remote".to_owned()).unwrap(),
            PathBuf::from("/private/runtime/spaceterm/master.sock"),
        )
        .unwrap()
        .remote_utility();
        let runner = Arc::new(FakeRunner::default());
        let cancelled = SshRemoteUtilityClient::new(
            PreparedSshRemoteUtilityCommand::new(command),
            Arc::clone(&runner),
            cancellation,
        );

        assert_eq!(
            cx.executor()
                .block(cancelled.discover_account())
                .unwrap_err(),
            RemoteUtilityError::Cancelled
        );
        assert!(runner.state.lock().unwrap().scripts.is_empty());

        let (cancelled_by_runner, _) = client([Err(RemoteUtilityRunError::Cancelled)]);
        assert_eq!(
            cx.executor()
                .block(cancelled_by_runner.discover_account())
                .unwrap_err(),
            RemoteUtilityError::Cancelled
        );
    }
}
