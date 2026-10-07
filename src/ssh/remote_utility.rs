use std::fmt;
use std::future::Future;
use std::str;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use super::cancellation::SshCancellationToken;
use super::command::{PosixShLoginCapability, SshCommandSpec};
use super::live_connection::LiveConnectionCapability;
use super::process::{
    CancelOnDrop, CapturedProcessError, ProcessExit, SshProcessAdapter, SshProcessEnvironment,
    SshProcessMechanismError, run_captured_process,
};
use crate::domain::RemoteDirectory;
use crate::repository_status::{
    ApplyMarkers, FsmonitorPolicy, MAXIMUM_MARKER_BYTES, OperationMarkers, RemoteProbeOutcome,
    RemoteRepositoryCount, RemoteRepositoryProbe, StepMarkers,
};

pub(crate) const MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES: usize = 384 * 1024;
const MAXIMUM_REMOTE_UTILITY_REQUEST_BYTES: usize = 32 * 1024;
const MAXIMUM_REMOTE_FIELD_BYTES: usize = 16 * 1024;
const MAXIMUM_REMOTE_DIRECTORY_NAMES: usize = 1024;
const MAXIMUM_REMOTE_DIRECTORY_ENTRIES_EXAMINED: usize = 1024;
const MAXIMUM_REMOTE_PATH_BYTES: usize = 4096;
/// Repository count status bytes kept from one remote read. The rest of the response stays well
/// inside [`MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES`].
pub(crate) const MAXIMUM_REMOTE_REPOSITORY_STATUS_BYTES: usize = 320 * 1024;
/// Repository configuration bytes kept from one remote probe; the field limit for the whole record
/// set. Exact keys come first so a cut drops only remote URLs.
const MAXIMUM_REMOTE_REPOSITORY_CONFIG_BYTES: usize = MAXIMUM_REMOTE_FIELD_BYTES;
const REPOSITORY_PROBE_KIND: &str = "repository-probe";
const REPOSITORY_COUNT_KIND: &str = "repository-count";
/// Operation marker presence flags, in the order the probe script emits them.
const REPOSITORY_MARKER_FLAGS: usize = 12;
const UTILITY_TIMEOUT: Duration = Duration::from_secs(60);
/// Concurrent utility sessions allowed on one Control Connection.
///
/// OpenSSH servers refuse sessions beyond `MaxSessions`, 10 by default, and Terminal Session
/// Channels share that limit. Two sessions let a listing and an exact-path probe run together.
pub(crate) const MAXIMUM_REMOTE_UTILITY_SESSIONS: usize = 2;
const PROTOCOL_HEADER: &str = "SPACETERM-REMOTE/1";
/// The status `ssh` exits with for its own failures, including a refused session.
const SSH_FAILURE_STATUS: i32 = 255;

/// Content-free exit status plus bounded untrusted stdout from one utility process.
pub(crate) struct RemoteUtilityProcessOutput {
    exit: ProcessExit,
    stdout: Vec<u8>,
}

impl fmt::Debug for RemoteUtilityProcessOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteUtilityProcessOutput(<redacted>)")
    }
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
    Process(#[source] SshProcessMechanismError),
    #[error("remote utility process worker was unavailable")]
    WorkerUnavailable,
}

/// Process boundary for fixed `/bin/sh -s` remote utility requests.
///
/// Implementations must enforce the supplied output bound, link cancellation to process-group
/// termination and reaping, and never log, persist, or interpret untrusted remote bytes.
pub(crate) trait SshRemoteUtilityRunner: Send + Sync + 'static {
    /// Runs one owned script with no TTY and a request-scoped cancellation token.
    ///
    /// The runner holds `session` until the process has exited and been reaped, including after
    /// the returned future is dropped.
    fn run(
        &self,
        command: Arc<SshCommandSpec>,
        script: Vec<u8>,
        maximum_output_bytes: usize,
        cancellation: SshCancellationToken,
        session: RemoteUtilitySession,
    ) -> impl Future<Output = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>> + Send;
}

/// Admits at most [`MAXIMUM_REMOTE_UTILITY_SESSIONS`] utility processes on one Control Connection.
///
/// Waiting requests are admitted in arrival order. A dropped waiting request gives up its place.
struct RemoteUtilitySessionLimit {
    available: async_channel::Receiver<()>,
    release: async_channel::Sender<()>,
}

impl RemoteUtilitySessionLimit {
    fn new(maximum_sessions: usize) -> Self {
        let (release, available) = async_channel::bounded(maximum_sessions);
        for _ in 0..maximum_sessions {
            let _ = release.try_send(());
        }
        Self { available, release }
    }

    async fn acquire(&self) -> RemoteUtilitySession {
        // The limit owns a sender, so the channel stays open while anyone can wait on it.
        let _ = self.available.recv().await;
        RemoteUtilitySession {
            release: self.release.clone(),
        }
    }
}

/// One admitted utility session. Dropping it frees the session for the next request.
pub(crate) struct RemoteUtilitySession {
    release: async_channel::Sender<()>,
}

impl Drop for RemoteUtilitySession {
    fn drop(&mut self) {
        let _ = self.release.try_send(());
    }
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
/// Portable bounded runner that owns one supervised utility process per request.
///
/// Work executes off async executor threads. Timeout, cancellation, or future drop kills and
/// reaps the process group before ownership is released.
pub(crate) struct SshRemoteUtilityProcessRunner<A: SshProcessAdapter> {
    adapter: A,
    environment: SshProcessEnvironment,
    timeout: Duration,
}

impl<A: SshProcessAdapter> SshRemoteUtilityProcessRunner<A> {
    pub(crate) const fn new(adapter: A, environment: SshProcessEnvironment) -> Self {
        Self {
            adapter,
            environment,
            timeout: UTILITY_TIMEOUT,
        }
    }

    #[cfg(all(test, feature = "native-tests"))]
    const fn with_timeout(
        adapter: A,
        environment: SshProcessEnvironment,
        timeout: Duration,
    ) -> Self {
        Self {
            adapter,
            environment,
            timeout,
        }
    }
}

impl<A: SshProcessAdapter> SshRemoteUtilityRunner for SshRemoteUtilityProcessRunner<A> {
    fn run(
        &self,
        command: Arc<SshCommandSpec>,
        script: Vec<u8>,
        maximum_output_bytes: usize,
        cancellation: SshCancellationToken,
        session: RemoteUtilitySession,
    ) -> impl Future<Output = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>> + Send
    {
        let environment = self.environment.clone();
        let adapter = self.adapter.clone();
        let timeout = self.timeout;
        async move {
            let mut cancel_on_drop = CancelOnDrop::new(cancellation.clone());
            let (sender, receiver) = async_channel::bounded(1);
            thread::Builder::new()
                .name("spaceterm-ssh-utility".to_owned())
                .spawn(move || {
                    let result = run_captured_process(
                        &adapter,
                        &command,
                        &environment,
                        script,
                        maximum_output_bytes,
                        &cancellation,
                        Instant::now()
                            .checked_add(timeout)
                            .unwrap_or_else(Instant::now),
                    )
                    .map(|output| RemoteUtilityProcessOutput::new(output.exit, output.stdout))
                    .map_err(map_captured_process_error);
                    drop(session);
                    let _ = sender.send_blocking(result);
                })
                .map_err(|_| RemoteUtilityRunError::WorkerUnavailable)?;
            let result = receiver
                .recv()
                .await
                .map_err(|_| RemoteUtilityRunError::WorkerUnavailable)?;
            cancel_on_drop.disarm();
            result
        }
    }
}

fn map_captured_process_error(error: CapturedProcessError) -> RemoteUtilityRunError {
    match error {
        CapturedProcessError::Cancelled => RemoteUtilityRunError::Cancelled,
        CapturedProcessError::TimedOut => RemoteUtilityRunError::TimedOut,
        CapturedProcessError::OutputTooLarge => RemoteUtilityRunError::OutputTooLarge,
        CapturedProcessError::Operation(error) => RemoteUtilityRunError::Process(error),
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
    #[error("remote utility process exceeded its deadline")]
    TimedOut,
    /// `ssh` failed while its Control Connection stayed live, usually because the server refused
    /// a session above its limit.
    #[error("remote utility session was unavailable")]
    SessionUnavailable,
    #[error("remote utility returned an invalid response")]
    InvalidResponse,
    #[error("the configured remote login shell cannot start in login mode")]
    UnsupportedLoginShell,
    #[error("remote path does not exist")]
    Missing,
    #[error("a remote program the operation needs is missing")]
    ToolMissing,
    #[error("remote path is not a directory")]
    NotDirectory,
    #[error("remote path permission was denied")]
    PermissionDenied,
    #[error("remote utility operation failed")]
    RemoteFailed,
}

#[derive(Clone, Eq, PartialEq)]
/// Strictly decoded account metadata returned by protocol version 1.
pub(crate) struct RemoteAccountMetadata {
    user: String,
    uid: u64,
    home: String,
    login_shell: String,
    physical_home: String,
    posix_sh_login_capability: PosixShLoginCapability,
}

impl fmt::Debug for RemoteAccountMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteAccountMetadata(<redacted>)")
    }
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

#[derive(Clone, Eq, PartialEq)]
/// Bounded safe directory names plus an explicit partial-listing marker.
pub(crate) struct RemoteUtilityDirectoryListing {
    names: Vec<String>,
    truncated: bool,
}

impl fmt::Debug for RemoteUtilityDirectoryListing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteUtilityDirectoryListing(<redacted>)")
    }
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
/// Control Connection and request cancellation are linked before the runner owns the process.
/// Requests beyond [`MAXIMUM_REMOTE_UTILITY_SESSIONS`] wait for an earlier process to exit.
pub(crate) struct SshRemoteUtilityClient<R: SshRemoteUtilityRunner> {
    command: PreparedSshRemoteUtilityCommand,
    runner: Arc<R>,
    cancellation: SshCancellationToken,
    sessions: RemoteUtilitySessionLimit,
}

impl<R: SshRemoteUtilityRunner> SshRemoteUtilityClient<R> {
    pub(crate) fn new(
        command: PreparedSshRemoteUtilityCommand,
        runner: Arc<R>,
        cancellation: SshCancellationToken,
    ) -> Self {
        Self {
            command,
            runner,
            cancellation,
            sessions: RemoteUtilitySessionLimit::new(MAXIMUM_REMOTE_UTILITY_SESSIONS),
        }
    }

    pub(crate) async fn discover_account_with_cancellation(
        &self,
        cancellation: SshCancellationToken,
    ) -> Result<RemoteAccountMetadata, RemoteUtilityError> {
        let output = self.execute(build_account_script(), cancellation).await?;
        parse_account(&output)
    }

    pub(crate) async fn list_directories_with_cancellation(
        &self,
        directory: RemoteDirectory,
        cancellation: SshCancellationToken,
    ) -> Result<RemoteUtilityDirectoryListing, RemoteUtilityError> {
        let output = self
            .execute(build_path_script("list", directory.as_str())?, cancellation)
            .await?;
        parse_listing(&output)
    }

    pub(crate) async fn probe_exact_path_with_cancellation(
        &self,
        directory: RemoteDirectory,
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

    pub(crate) async fn create_directory_recursively_with_cancellation(
        &self,
        directory: RemoteDirectory,
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

    pub(crate) async fn resolve_physical_directory_with_cancellation(
        &self,
        directory: RemoteDirectory,
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

    /// Reads the cheap facts about the repository containing `directory`.
    ///
    /// Raw git output is returned for the portable parsers. The probe never walks the work tree.
    #[allow(
        dead_code,
        reason = "Repository Status composition reads remote repositories in a later commit"
    )]
    pub(crate) async fn probe_repository_with_cancellation(
        &self,
        directory: RemoteDirectory,
        cancellation: SshCancellationToken,
    ) -> Result<RemoteRepositoryProbe, RemoteUtilityError> {
        let output = self
            .execute(
                build_repository_script(
                    REPOSITORY_PROBE_KIND,
                    directory.as_str(),
                    FsmonitorPolicy::Disabled,
                )?,
                cancellation,
            )
            .await?;
        parse_repository_probe(&output)
    }

    /// Reads the full status of the work tree rooted at `root`, cut at
    /// [`MAXIMUM_REMOTE_REPOSITORY_STATUS_BYTES`].
    #[allow(
        dead_code,
        reason = "Repository Status composition reads remote repositories in a later commit"
    )]
    pub(crate) async fn count_repository_with_cancellation(
        &self,
        root: &str,
        fsmonitor: FsmonitorPolicy,
        cancellation: SshCancellationToken,
    ) -> Result<RemoteRepositoryCount, RemoteUtilityError> {
        if !root.starts_with('/') || root.contains('\0') {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        let output = self
            .execute(
                build_repository_script(REPOSITORY_COUNT_KIND, root, fsmonitor)?,
                cancellation,
            )
            .await?;
        parse_repository_count(&output)
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
        let session = self.sessions.acquire().await;
        if self.cancellation.is_cancelled() || request_cancellation.is_cancelled() {
            return Err(RemoteUtilityError::Cancelled);
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
        let output =
            self.runner
                .run(
                    Arc::clone(&self.command.command),
                    script,
                    MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES,
                    operation_cancellation,
                    session,
                )
                .await
                .map_err(|error| match error {
                    RemoteUtilityRunError::Cancelled => RemoteUtilityError::Cancelled,
                    RemoteUtilityRunError::OutputTooLarge => RemoteUtilityError::OutputTooLarge,
                    RemoteUtilityRunError::TimedOut => RemoteUtilityError::TimedOut,
                    RemoteUtilityRunError::Process(_)
                    | RemoteUtilityRunError::WorkerUnavailable => RemoteUtilityError::Transport,
                })?;
        if output.exit.code() == Some(SSH_FAILURE_STATUS) {
            return Err(if self.connection_is_live() {
                RemoteUtilityError::SessionUnavailable
            } else {
                RemoteUtilityError::Transport
            });
        }
        if !output.exit.is_success() {
            return Err(RemoteUtilityError::CommandFailed(output.exit.code()));
        }
        if output.stdout.len() > MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES {
            return Err(RemoteUtilityError::OutputTooLarge);
        }
        Ok(output.stdout)
    }

    fn connection_is_live(&self) -> bool {
        !self.cancellation.is_cancelled()
            && self
                .command
                .capability
                .as_ref()
                .is_none_or(|capability| capability.authorize().is_ok())
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
        "{COMMON_SCRIPT}\ninput_path={}\n{PATH_EXPANSION_SCRIPT}{PATH_CLASSIFICATION_SCRIPT}\n{operation_script}",
        quote_for_posix_shell(path)
    )
    .into_bytes();
    if script.len() > MAXIMUM_REMOTE_UTILITY_REQUEST_BYTES {
        return Err(RemoteUtilityError::RequestTooLarge);
    }
    Ok(script)
}

/// Builds a repository script. Only `path` is caller data; it reaches the script as one
/// single-quoted assignment, as every other path operation does.
fn build_repository_script(
    kind: &str,
    path: &str,
    fsmonitor: FsmonitorPolicy,
) -> Result<Vec<u8>, RemoteUtilityError> {
    if path.len() > MAXIMUM_REMOTE_PATH_BYTES {
        return Err(RemoteUtilityError::RequestTooLarge);
    }
    let operation_script = match kind {
        REPOSITORY_PROBE_KIND => REPOSITORY_PROBE_SCRIPT
            .replace(
                "__MAXIMUM_CONFIG_BYTES__",
                &MAXIMUM_REMOTE_REPOSITORY_CONFIG_BYTES.to_string(),
            )
            .replace(
                "__MARKER_READ_BYTES__",
                &(MAXIMUM_MARKER_BYTES + 1).to_string(),
            )
            .replace(
                "__MAXIMUM_MARKER_BYTES__",
                &MAXIMUM_MARKER_BYTES.to_string(),
            ),
        REPOSITORY_COUNT_KIND => REPOSITORY_COUNT_SCRIPT
            .replace(
                "__MAXIMUM_STATUS_BYTES__",
                &MAXIMUM_REMOTE_REPOSITORY_STATUS_BYTES.to_string(),
            )
            .replace(
                "__STATUS_READ_BYTES__",
                &(MAXIMUM_REMOTE_REPOSITORY_STATUS_BYTES + 1).to_string(),
            ),
        _ => unreachable!("remote utility operation is fixed by the caller"),
    };
    let git_fsmonitor = match fsmonitor {
        FsmonitorPolicy::Disabled => "disabled",
        FsmonitorPolicy::Builtin => "builtin",
    };
    let script = format!(
        "{COMMON_SCRIPT}\ninput_path={}\n{PATH_EXPANSION_SCRIPT}repository_kind={kind}\ngit_fsmonitor={git_fsmonitor}\n{REPOSITORY_SCRIPT}{operation_script}",
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
"#;

const PATH_CLASSIFICATION_SCRIPT: &str = r#"classify_remote_path() {
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
enumerator_settled=0
cleanup_listing_state() {
    # A signal can arrive after find starts but before the PID assignment runs.
    if [ -z "$enumerator_pid" ] && [ "$enumerator_settled" -eq 0 ]; then
        enumerator_pid=$!
    fi
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
enumerator_settled=1
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

// Shared by both repository operations. Git output can hold NUL bytes, which shell variables
// cannot, so it is staged in a private directory and copied into netstrings byte for byte.
const REPOSITORY_SCRIPT: &str = r#"cd "$remote_path" 2>/dev/null || {
    emit_empty "$repository_kind" directory-unavailable
    exit 0
}
git_program=$(command -v git 2>/dev/null) || git_program=
case "$git_program" in
    /*) ;;
    *) emit_empty "$repository_kind" git-missing; exit 0 ;;
esac
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT \
    GIT_CEILING_DIRECTORIES GIT_NAMESPACE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES
GIT_TERMINAL_PROMPT=0
GIT_OPTIONAL_LOCKS=0
GIT_PAGER=cat
PAGER=cat
GIT_NO_LAZY_FETCH=1
export GIT_TERMINAL_PROMPT GIT_OPTIONAL_LOCKS GIT_PAGER PAGER GIT_NO_LAZY_FETCH
# Git reads no input, because the script itself arrives on standard input.
run_git() {
    if [ "$git_fsmonitor" = builtin ]; then
        "$git_program" --no-optional-locks --no-pager -c core.fsmonitor=true \
            -c core.hooksPath=/dev/null -c protocol.allow=never -c color.ui=false \
            -c core.quotePath=false -c status.relativePaths=false -c advice.statusHints=false \
            -C . "$@" </dev/null 2>/dev/null
    else
        "$git_program" --no-optional-locks --no-pager -c core.fsmonitor=false \
            -c core.hooksPath=/dev/null -c protocol.allow=never -c color.ui=false \
            -c core.quotePath=false -c status.relativePaths=false -c advice.statusHints=false \
            -C . "$@" </dev/null 2>/dev/null
    fi
}
state_directory=$(umask 077; mktemp -d "${TMPDIR:-/tmp}/spaceterm-repository.XXXXXXXXXX" 2>/dev/null) || {
    emit_empty "$repository_kind" failed
    exit 0
}
cleanup_repository_state() {
    rm -rf "$state_directory"
}
cancel_repository_read() {
    cleanup_repository_state
    trap - EXIT HUP INT TERM
    exit 129
}
trap cleanup_repository_state EXIT
trap cancel_repository_read HUP INT TERM
emit_file_field() {
    field_length=$(LC_ALL=C wc -c < "$1" | tr -d '[:space:]') || exit 70
    printf '%s:' "$field_length"
    cat "$1" || exit 70
    printf ','
}
"#;

// The headers pathspec excludes every entry from the top of the work tree; a plain `:(exclude)*`
// is relative to the probed directory and lets entries outside it through.
// `git config` prints command-line `-c` values among its results, so configuration reads omit the
// hardening overrides. Reading configuration runs no hook and no fsmonitor. Exact keys come first
// so a cut at the field limit drops only remote URLs.
const REPOSITORY_PROBE_SCRIPT: &str = r#"read_git_config() {
    "$git_program" --no-optional-locks --no-pager -C . config -z "$@" </dev/null 2>/dev/null
}
append_config_value() {
    if read_git_config --get "$1" > "$state_directory/config-value"; then
        printf '%s\n' "$1" >> "$state_directory/config-records" || exit 70
        cat "$state_directory/config-value" >> "$state_directory/config-records" || exit 70
    fi
}
# Copies a regular file that is not a symbolic link. A file beyond the marker limit reads as
# absent, as it does locally.
read_marker_step() {
    if [ -f "$1" ] && [ ! -L "$1" ] &&
        head -c __MARKER_READ_BYTES__ "$1" > "$2" 2>/dev/null </dev/null &&
        marker_length=$(LC_ALL=C wc -c < "$2" | tr -d '[:space:]') &&
        [ "$marker_length" -le __MAXIMUM_MARKER_BYTES__ ]; then
        marker_flags=${marker_flags}1
    else
        : > "$2" || exit 70
        marker_flags=${marker_flags}0
    fi
}
read_marker_presence() {
    if [ -e "$1" ] && [ ! -L "$1" ]; then
        marker_flags=${marker_flags}1
    else
        marker_flags=${marker_flags}0
    fi
}
# Files below a marker directory are read only when the directory itself is not a link.
read_operation_markers() {
    marker_flags=
    if [ -d "$1/rebase-merge" ] && [ ! -L "$1/rebase-merge" ]; then
        marker_flags=1
        read_marker_step "$1/rebase-merge/msgnum" "$state_directory/merge-current"
        read_marker_step "$1/rebase-merge/end" "$state_directory/merge-total"
    else
        marker_flags=000
    fi
    if [ -d "$1/rebase-apply" ] && [ ! -L "$1/rebase-apply" ]; then
        marker_flags=${marker_flags}1
        read_marker_step "$1/rebase-apply/next" "$state_directory/apply-current"
        read_marker_step "$1/rebase-apply/last" "$state_directory/apply-total"
        read_marker_presence "$1/rebase-apply/rebasing"
        read_marker_presence "$1/rebase-apply/applying"
    else
        marker_flags=${marker_flags}00000
    fi
    read_marker_presence "$1/MERGE_HEAD"
    read_marker_presence "$1/REVERT_HEAD"
    read_marker_presence "$1/CHERRY_PICK_HEAD"
    read_marker_presence "$1/BISECT_LOG"
}
for state_file in version discovery headers config-records config merge-current merge-total \
    apply-current apply-total; do
    : > "$state_directory/$state_file" || exit 70
done
marker_flags=000000000000
run_git --version > "$state_directory/version"
run_git rev-parse --is-inside-git-dir --is-bare-repository --absolute-git-dir --git-common-dir \
    --show-toplevel --show-prefix > "$state_directory/discovery"
discovery_status=$?
physical_home_output=$([ -n "${HOME-}" ] && cd "$HOME" 2>/dev/null && { pwd -P && printf .; }) ||
    physical_home_output=
physical_home_with_separator=${physical_home_output%?}
physical_home=${physical_home_with_separator%?}
if [ "$discovery_status" -eq 0 ]; then
    run_git status --porcelain=v2 --branch -z --untracked-files=no --ignore-submodules=all \
        -- ':(top,exclude)*' > "$state_directory/headers"
    branch_name=$(LC_ALL=C tr '\000' '\n' < "$state_directory/headers" |
        LC_ALL=C sed -n 's/^# branch\.head //p')
    append_config_value core.fsmonitor
    append_config_value push.default
    append_config_value remote.pushdefault
    if [ -n "$branch_name" ] && [ "$branch_name" != '(detached)' ]; then
        append_config_value "branch.$branch_name.remote"
        append_config_value "branch.$branch_name.merge"
        append_config_value "branch.$branch_name.pushremote"
    fi
    read_git_config --get-regexp '^remote\..*\.url$' >> "$state_directory/config-records"
    head -c __MAXIMUM_CONFIG_BYTES__ "$state_directory/config-records" > "$state_directory/config" ||
        exit 70
    git_directory=$(LC_ALL=C sed -n 3p "$state_directory/discovery")
    case "$git_directory" in
        /*) read_operation_markers "$git_directory" ;;
    esac
fi
emit_header repository-probe ok
emit_file_field "$state_directory/version"
emit_field "$discovery_status"
emit_file_field "$state_directory/discovery"
emit_field "$physical_home"
emit_file_field "$state_directory/headers"
emit_file_field "$state_directory/config"
emit_field "$marker_flags"
emit_file_field "$state_directory/merge-current"
emit_file_field "$state_directory/merge-total"
emit_file_field "$state_directory/apply-current"
emit_file_field "$state_directory/apply-total"
printf '.\n'
"#;

// The pipeline stops git once one byte beyond the limit arrives, so a huge work tree costs at most
// the limit in transfer and staging.
const REPOSITORY_COUNT_SCRIPT: &str = r#"{
    run_git status --porcelain=v2 --branch -z
    printf '%s\n' "$?" > "$state_directory/status-exit"
} | head -c __STATUS_READ_BYTES__ > "$state_directory/status-read"
IFS= read -r status_exit < "$state_directory/status-exit" || status_exit=1
status_length=$(LC_ALL=C wc -c < "$state_directory/status-read" | tr -d '[:space:]') || exit 70
if [ "$status_length" -gt __MAXIMUM_STATUS_BYTES__ ]; then
    head -c __MAXIMUM_STATUS_BYTES__ "$state_directory/status-read" > "$state_directory/status" ||
        exit 70
    status_truncated=1
elif [ "$status_exit" = 0 ]; then
    cat "$state_directory/status-read" > "$state_directory/status" || exit 70
    status_truncated=0
else
    emit_empty repository-count failed
    exit 0
fi
emit_header repository-count ok
emit_file_field "$state_directory/status"
printf '.\n%s\n' "$status_truncated"
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

fn parse_repository_probe(output: &[u8]) -> Result<RemoteRepositoryProbe, RemoteUtilityError> {
    let mut response = ResponseParser::new(output, REPOSITORY_PROBE_KIND)?;
    let outcome = match response.status {
        b"ok" => None,
        b"directory-unavailable" => Some(RemoteProbeOutcome::DirectoryUnavailable),
        b"git-missing" => Some(RemoteProbeOutcome::GitMissing),
        b"failed" => {
            response.finish_fields(0)?;
            return Err(RemoteUtilityError::RemoteFailed);
        }
        _ => return Err(RemoteUtilityError::InvalidResponse),
    };
    if let Some(outcome) = outcome {
        response.finish_fields(0)?;
        return Ok(RemoteRepositoryProbe {
            outcome,
            ..RemoteRepositoryProbe::default()
        });
    }
    let fields = response.read_raw_fields(11)?;
    response.require_end()?;
    let [
        git_version,
        discovery_status,
        discovery,
        physical_home,
        status_headers,
        config,
        marker_flags,
        merge_current,
        merge_total,
        apply_current,
        apply_total,
    ] = fields[..]
    else {
        return Err(RemoteUtilityError::InvalidResponse);
    };
    let discovery_succeeded = parse_exit_status(discovery_status)? == 0;
    let markers = parse_operation_markers(
        marker_flags,
        [merge_current, merge_total, apply_current, apply_total],
    )?;
    Ok(RemoteRepositoryProbe {
        outcome: if discovery_succeeded || !discovery.is_empty() {
            RemoteProbeOutcome::Repository
        } else {
            RemoteProbeOutcome::NotRepository
        },
        git_version: git_version.to_vec(),
        discovery_succeeded,
        discovery: discovery.to_vec(),
        physical_home: physical_home.to_vec(),
        status_headers: status_headers.to_vec(),
        config: complete_config_records(config).to_vec(),
        markers,
    })
}

fn parse_exit_status(field: &[u8]) -> Result<u8, RemoteUtilityError> {
    if field.is_empty()
        || !field.iter().all(u8::is_ascii_digit)
        || (field.len() > 1 && field.starts_with(b"0"))
    {
        return Err(RemoteUtilityError::InvalidResponse);
    }
    str::from_utf8(field)
        .map_err(|_| RemoteUtilityError::InvalidResponse)?
        .parse::<u8>()
        .map_err(|_| RemoteUtilityError::InvalidResponse)
}

/// Drops a record the field limit cut short; every whole `git config -z` record ends with NUL.
fn complete_config_records(config: &[u8]) -> &[u8] {
    match config.iter().rposition(|byte| *byte == 0) {
        Some(last) => &config[..=last],
        None => &[],
    }
}

/// Decodes the marker flags `rebase-merge/`, `msgnum`, `end`, `rebase-apply/`, `next`, `last`,
/// `rebasing`, `applying`, `MERGE_HEAD`, `REVERT_HEAD`, `CHERRY_PICK_HEAD`, `BISECT_LOG` and the
/// four step file contents.
fn parse_operation_markers(
    flags: &[u8],
    steps: [&[u8]; 4],
) -> Result<OperationMarkers, RemoteUtilityError> {
    if flags.len() != REPOSITORY_MARKER_FLAGS {
        return Err(RemoteUtilityError::InvalidResponse);
    }
    let flags = flags
        .iter()
        .map(|flag| match flag {
            b'0' => Ok(false),
            b'1' => Ok(true),
            _ => Err(RemoteUtilityError::InvalidResponse),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let [merge_current, merge_total, apply_current, apply_total] = steps;
    let step = |present: bool, content: &[u8]| -> Result<Option<Vec<u8>>, RemoteUtilityError> {
        if content.len() > MAXIMUM_MARKER_BYTES || (!present && !content.is_empty()) {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        Ok(present.then(|| content.to_vec()))
    };
    let within = |directory: bool, members: &[bool]| directory || members.iter().all(|flag| !flag);
    if !within(flags[0], &flags[1..3]) || !within(flags[3], &flags[4..8]) {
        return Err(RemoteUtilityError::InvalidResponse);
    }
    Ok(OperationMarkers {
        rebase_merge: flags[0]
            .then(|| -> Result<_, RemoteUtilityError> {
                Ok(StepMarkers {
                    current: step(flags[1], merge_current)?,
                    total: step(flags[2], merge_total)?,
                })
            })
            .transpose()?,
        rebase_apply: flags[3]
            .then(|| -> Result<_, RemoteUtilityError> {
                Ok(ApplyMarkers {
                    step: StepMarkers {
                        current: step(flags[4], apply_current)?,
                        total: step(flags[5], apply_total)?,
                    },
                    rebasing: flags[6],
                    applying: flags[7],
                })
            })
            .transpose()?,
        merge_head: flags[8],
        revert_head: flags[9],
        cherry_pick_head: flags[10],
        bisect_log: flags[11],
    })
}

fn parse_repository_count(output: &[u8]) -> Result<RemoteRepositoryCount, RemoteUtilityError> {
    let mut response = ResponseParser::new(output, REPOSITORY_COUNT_KIND)?;
    match response.status {
        b"ok" => {}
        status => {
            let error = match status {
                b"directory-unavailable" => RemoteUtilityError::Missing,
                b"git-missing" => RemoteUtilityError::ToolMissing,
                b"failed" => RemoteUtilityError::RemoteFailed,
                _ => return Err(RemoteUtilityError::InvalidResponse),
            };
            response.finish_fields(0)?;
            return Err(error);
        }
    }
    let fields = response.read_raw_fields_within(1, MAXIMUM_REMOTE_REPOSITORY_STATUS_BYTES)?;
    let [status] = fields[..] else {
        return Err(RemoteUtilityError::InvalidResponse);
    };
    let truncated = match response.read_line()? {
        b"0" => false,
        b"1" => true,
        _ => return Err(RemoteUtilityError::InvalidResponse),
    };
    response.require_end()?;
    Ok(RemoteRepositoryCount {
        status: status.to_vec(),
        truncated,
    })
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
        self.read_raw_fields_within(maximum_count, MAXIMUM_REMOTE_FIELD_BYTES)
    }

    fn read_raw_fields_within(
        &mut self,
        maximum_count: usize,
        maximum_field_bytes: usize,
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
            fields.push(self.read_netstring(maximum_field_bytes)?);
        }
    }

    fn read_netstring(
        &mut self,
        maximum_field_bytes: usize,
    ) -> Result<&'a [u8], RemoteUtilityError> {
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
        if length > maximum_field_bytes {
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
    use std::future::Future;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use gpui::TestAppContext;

    use super::*;
    use crate::domain::{RemoteDirectory, SshDestination};
    use crate::ssh::cancellation::SshCancellationToken;
    use crate::ssh::command::{OpenSshExecutable, SshCommandContext, SshCommandSpec};
    use crate::ssh::process::ProcessExit;

    #[test]
    fn process_output_debug_should_redact_remote_stdout() {
        let output = RemoteUtilityProcessOutput::new(
            ProcessExit::successful(),
            b"sensitive-remote-output".to_vec(),
        );

        let debug = format!("{output:?}");
        assert_eq!(debug, "RemoteUtilityProcessOutput(<redacted>)");
        assert!(!debug.contains("sensitive"));
    }

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
            _session: RemoteUtilitySession,
        ) -> impl Future<Output = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>> + Send
        {
            let result = {
                let mut state = self.state.lock().unwrap();
                state.scripts.push(script);
                state
                    .responses
                    .pop_front()
                    .unwrap_or(Err(RemoteUtilityRunError::WorkerUnavailable))
            };
            async move { result }
        }
    }

    fn client(
        responses: impl IntoIterator<Item = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>>,
    ) -> (SshRemoteUtilityClient<FakeRunner>, Arc<FakeRunner>) {
        let command = SshCommandContext::new(
            OpenSshExecutable::new(PathBuf::from("/selected/openssh")).unwrap(),
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

    pub(super) fn remote_directory(value: &str) -> RemoteDirectory {
        RemoteDirectory::new(value.to_owned()).unwrap()
    }

    #[gpui::test]
    fn account_metadata_should_decode_versioned_fields(cx: &mut TestAppContext) {
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

        let metadata = cx
            .foreground_executor()
            .block_test(client.discover_account_with_cancellation(SshCancellationToken::default()))
            .unwrap();

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

        let metadata = cx
            .foreground_executor()
            .block_test(client.discover_account_with_cancellation(SshCancellationToken::default()))
            .unwrap();

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
            cx.foreground_executor()
                .block_test(
                    client.discover_account_with_cancellation(SshCancellationToken::default())
                )
                .unwrap_err(),
            RemoteUtilityError::UnsupportedLoginShell
        );
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
            .foreground_executor()
            .block_test(client.list_directories_with_cancellation(
                remote_directory("/srv/projects"),
                SshCancellationToken::default(),
            ))
            .unwrap();

        assert_eq!(listing.names(), ["Space Term", "after hostile"]);
        assert!(listing.is_truncated());
    }

    #[gpui::test]
    fn path_requests_should_be_single_quoted_without_shell_interpolation(cx: &mut TestAppContext) {
        let (client, runner) = client([success(response("probe", "missing", &[], ""))]);
        let path = "/tmp/space ' $(touch should-not-run) `false`";

        let state = cx
            .foreground_executor()
            .block_test(client.probe_exact_path_with_cancellation(
                remote_directory(path),
                SshCancellationToken::default(),
            ))
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
            cx.foreground_executor()
                .block_test(client.probe_exact_path_with_cancellation(
                    remote_directory(&accepted),
                    SshCancellationToken::default()
                ))
                .unwrap(),
            RemoteDirectoryProbe::Missing
        );
        let accepted_script_length = runner.state.lock().unwrap().scripts[0].len();
        assert!(accepted_script_length <= MAXIMUM_REMOTE_UTILITY_REQUEST_BYTES);

        let rejected = format!("/{}", "'".repeat(MAXIMUM_REMOTE_PATH_BYTES));
        assert_eq!(
            cx.foreground_executor()
                .block_test(client.probe_exact_path_with_cancellation(
                    remote_directory(&rejected),
                    SshCancellationToken::default()
                ))
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
    fn physical_failed_response_maps_to_remote_failure() {
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
            cx.foreground_executor()
                .block_test(client.probe_exact_path_with_cancellation(
                    directory.clone(),
                    SshCancellationToken::default()
                ))
                .unwrap(),
            RemoteDirectoryProbe::ReadableDirectory
        );
        cx.foreground_executor()
            .block_test(client.create_directory_recursively_with_cancellation(
                directory.clone(),
                SshCancellationToken::default(),
            ))
            .unwrap();
        assert_eq!(
            cx.foreground_executor()
                .block_test(client.resolve_physical_directory_with_cancellation(
                    directory,
                    SshCancellationToken::default()
                ))
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
            cx.foreground_executor()
                .block_test(client.probe_exact_path_with_cancellation(
                    remote_directory("/srv/missing"),
                    SshCancellationToken::default()
                ))
                .unwrap(),
            RemoteDirectoryProbe::Missing
        );
        assert_eq!(
            cx.foreground_executor()
                .block_test(client.probe_exact_path_with_cancellation(
                    remote_directory("/srv/file/child"),
                    SshCancellationToken::default()
                ))
                .unwrap_err(),
            RemoteUtilityError::NotDirectory
        );
        assert_eq!(
            cx.foreground_executor()
                .block_test(client.probe_exact_path_with_cancellation(
                    remote_directory("/srv/private/child"),
                    SshCancellationToken::default()
                ))
                .unwrap_err(),
            RemoteUtilityError::PermissionDenied
        );
    }

    #[gpui::test]
    fn malformed_truncated_and_oversized_output_should_be_rejected(cx: &mut TestAppContext) {
        let (client, _) = client([
            success(b"SPACETERM-REMOTE/2\nprobe\nok\n.\n".to_vec()),
            success(b"SPACETERM-REMOTE/1\nphysical\nok\n12:/short".to_vec()),
            success(vec![b'x'; MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES + 1]),
        ]);

        assert_eq!(
            cx.foreground_executor()
                .block_test(client.probe_exact_path_with_cancellation(
                    remote_directory("/one"),
                    SshCancellationToken::default()
                ))
                .unwrap_err(),
            RemoteUtilityError::InvalidResponse
        );
        assert_eq!(
            cx.foreground_executor()
                .block_test(client.resolve_physical_directory_with_cancellation(
                    remote_directory("/two"),
                    SshCancellationToken::default()
                ))
                .unwrap_err(),
            RemoteUtilityError::InvalidResponse
        );
        assert_eq!(
            cx.foreground_executor()
                .block_test(client.probe_exact_path_with_cancellation(
                    remote_directory("/three"),
                    SshCancellationToken::default()
                ))
                .unwrap_err(),
            RemoteUtilityError::OutputTooLarge
        );
    }
    #[gpui::test]
    fn remote_command_failure_and_cancellation_should_remain_typed(cx: &mut TestAppContext) {
        let (failed, _) = client([Ok(RemoteUtilityProcessOutput::new(
            ProcessExit::unsuccessful(Some(1)),
            Vec::new(),
        ))]);
        assert_eq!(
            cx.foreground_executor()
                .block_test(failed.probe_exact_path_with_cancellation(
                    remote_directory("/srv"),
                    SshCancellationToken::default()
                ))
                .unwrap_err(),
            RemoteUtilityError::CommandFailed(Some(1))
        );

        let (refused, _) = client([Ok(RemoteUtilityProcessOutput::new(
            ProcessExit::unsuccessful(Some(255)),
            Vec::new(),
        ))]);
        assert_eq!(
            cx.foreground_executor()
                .block_test(refused.probe_exact_path_with_cancellation(
                    remote_directory("/srv"),
                    SshCancellationToken::default()
                ))
                .unwrap_err(),
            RemoteUtilityError::SessionUnavailable
        );

        let cancellation = SshCancellationToken::default();
        cancellation.cancel();
        let command = SshCommandContext::new(
            OpenSshExecutable::new(PathBuf::from("/selected/openssh")).unwrap(),
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
            cx.foreground_executor()
                .block_test(
                    cancelled.discover_account_with_cancellation(SshCancellationToken::default())
                )
                .unwrap_err(),
            RemoteUtilityError::Cancelled
        );
        assert!(runner.state.lock().unwrap().scripts.is_empty());

        let (cancelled_by_runner, _) = client([Err(RemoteUtilityRunError::Cancelled)]);
        assert_eq!(
            cx.foreground_executor()
                .block_test(
                    cancelled_by_runner
                        .discover_account_with_cancellation(SshCancellationToken::default())
                )
                .unwrap_err(),
            RemoteUtilityError::Cancelled
        );
    }

    mod repository {
        use super::*;
        use crate::ssh::fake_remote_utility_server::{
            REPOSITORY_CONFIG, REPOSITORY_DISCOVERY, REPOSITORY_HEADERS, raw_response,
            repository_count_response, repository_probe_fields, repository_probe_response,
        };

        fn probe_with(fields: &[&[u8]]) -> Result<RemoteRepositoryProbe, RemoteUtilityError> {
            parse_repository_probe(&raw_response("repository-probe", "ok", fields, b""))
        }

        fn probe_replacing(
            index: usize,
            value: &[u8],
        ) -> Result<RemoteRepositoryProbe, RemoteUtilityError> {
            let mut fields = repository_probe_fields();
            fields[index] = value;
            probe_with(&fields)
        }

        fn script_text(runner: &FakeRunner, index: usize) -> String {
            String::from_utf8(runner.state.lock().unwrap().scripts[index].clone()).unwrap()
        }

        #[gpui::test]
        fn probe_should_round_trip_raw_git_output_and_markers(cx: &mut TestAppContext) {
            let (client, runner) = client([success(repository_probe_response())]);

            let probe = cx
                .foreground_executor()
                .block_test(client.probe_repository_with_cancellation(
                    remote_directory("~/work's $(id)"),
                    SshCancellationToken::default(),
                ))
                .unwrap();

            assert_eq!(probe.outcome, RemoteProbeOutcome::Repository);
            assert_eq!(probe.git_version, b"git version 2.47.0\n");
            assert!(probe.discovery_succeeded);
            assert_eq!(probe.discovery, REPOSITORY_DISCOVERY);
            assert_eq!(probe.physical_home, b"/home/tester");
            assert_eq!(probe.status_headers, REPOSITORY_HEADERS);
            assert_eq!(probe.config, REPOSITORY_CONFIG);
            assert_eq!(
                probe.markers,
                OperationMarkers {
                    rebase_merge: Some(StepMarkers {
                        current: Some(b"3\n".to_vec()),
                        total: Some(b"7\n".to_vec()),
                    }),
                    merge_head: true,
                    ..OperationMarkers::default()
                }
            );
            let script = script_text(&runner, 0);
            assert!(script.contains("input_path='~/work'\"'\"'s $(id)'\n"));
            assert!(script.contains("git_fsmonitor=disabled\n"));
            assert!(script.contains("-c core.fsmonitor=false"));
            assert!(script.contains("emit_header repository-probe ok"));
        }

        #[test]
        fn probe_should_classify_every_outcome() {
            let mut not_repository = repository_probe_fields();
            not_repository[1] = b"128";
            not_repository[2] = b"";
            not_repository[6] = b"000000000000";
            not_repository[7] = b"";
            not_repository[8] = b"";
            let not_repository = probe_with(&not_repository).unwrap();
            let mut hidden = repository_probe_fields();
            hidden[1] = b"128";
            hidden[2] = b"true\ntrue\n/srv/repo.git\n.\n";
            let hidden = probe_with(&hidden).unwrap();

            assert_eq!(not_repository.outcome, RemoteProbeOutcome::NotRepository);
            assert!(!not_repository.discovery_succeeded);
            assert_eq!(hidden.outcome, RemoteProbeOutcome::Repository);
            assert!(!hidden.discovery_succeeded);
            for (status, outcome) in [
                (
                    "directory-unavailable",
                    RemoteProbeOutcome::DirectoryUnavailable,
                ),
                ("git-missing", RemoteProbeOutcome::GitMissing),
            ] {
                let probe =
                    parse_repository_probe(&raw_response("repository-probe", status, &[], b""))
                        .unwrap();
                assert_eq!(
                    probe,
                    RemoteRepositoryProbe {
                        outcome,
                        ..RemoteRepositoryProbe::default()
                    }
                );
            }
            assert_eq!(
                parse_repository_probe(&raw_response("repository-probe", "failed", &[], b""))
                    .unwrap_err(),
                RemoteUtilityError::RemoteFailed
            );
        }

        #[test]
        fn probe_should_keep_only_whole_configuration_records() {
            assert_eq!(
                probe_replacing(5, b"core.fsmonitor\ntrue\0remote.origin.url\nhttps://ex")
                    .unwrap()
                    .config,
                b"core.fsmonitor\ntrue\0"
            );
            assert!(
                probe_replacing(5, b"remote.origin.url\nhttps://ex")
                    .unwrap()
                    .config
                    .is_empty()
            );
        }

        #[test]
        fn probe_should_decode_apply_markers_and_the_marker_limit() {
            let full_marker = [b'9'; MAXIMUM_MARKER_BYTES];
            let mut fields = repository_probe_fields();
            fields[6] = b"000111110111";
            fields[7] = b"";
            fields[8] = b"";
            fields[9] = b"2\n";
            fields[10] = &full_marker;

            assert_eq!(
                probe_with(&fields).unwrap().markers,
                OperationMarkers {
                    rebase_apply: Some(ApplyMarkers {
                        step: StepMarkers {
                            current: Some(b"2\n".to_vec()),
                            total: Some(full_marker.to_vec()),
                        },
                        rebasing: true,
                        applying: true,
                    }),
                    revert_head: true,
                    cherry_pick_head: true,
                    bisect_log: true,
                    ..OperationMarkers::default()
                }
            );
        }

        #[test]
        fn probe_should_reject_malformed_responses() {
            let oversized_field = vec![b'x'; MAXIMUM_REMOTE_FIELD_BYTES + 1];
            let oversized_marker = vec![b'1'; MAXIMUM_MARKER_BYTES + 1];
            for (index, value) in [
                (1, b"".as_slice()),
                (1, b"01"),
                (1, b"256"),
                (1, b"-1"),
                (6, b"11100000100"),
                (6, b"1110000010002"),
                (6, b"11100000100x"),
                // A step file below an absent marker directory.
                (6, b"011000001000"),
                (6, b"000010000000"),
                // Step content without its presence flag.
                (6, b"101000001000"),
                (7, &oversized_marker),
                (4, &oversized_field),
            ] {
                assert_eq!(
                    probe_replacing(index, value).unwrap_err(),
                    RemoteUtilityError::InvalidResponse,
                    "field {index} accepted {:?}",
                    String::from_utf8_lossy(value)
                );
            }
            let fields = repository_probe_fields();
            assert_eq!(
                probe_with(&fields[..10]).unwrap_err(),
                RemoteUtilityError::InvalidResponse
            );
            let mut trailing = repository_probe_response();
            trailing.extend_from_slice(b"0\n");
            for malformed in [
                trailing,
                raw_response("repository-probe", "unknown", &[], b""),
                raw_response("repository-count", "ok", &fields, b""),
                raw_response("repository-probe", "git-missing", &[b"x"], b""),
            ] {
                assert_eq!(
                    parse_repository_probe(&malformed).unwrap_err(),
                    RemoteUtilityError::InvalidResponse
                );
            }
        }

        #[gpui::test]
        fn count_should_round_trip_status_and_truncation(cx: &mut TestAppContext) {
            let cut = vec![b'?'; MAXIMUM_REMOTE_REPOSITORY_STATUS_BYTES];
            let (client, runner) = client([
                success(repository_count_response(b"? a\0? b\nc\0", false)),
                success(repository_count_response(&cut, true)),
            ]);

            let whole = cx
                .foreground_executor()
                .block_test(client.count_repository_with_cancellation(
                    "/srv/it's here",
                    FsmonitorPolicy::Disabled,
                    SshCancellationToken::default(),
                ))
                .unwrap();
            let truncated = cx
                .foreground_executor()
                .block_test(client.count_repository_with_cancellation(
                    "/srv/repo",
                    FsmonitorPolicy::Builtin,
                    SshCancellationToken::default(),
                ))
                .unwrap();

            assert_eq!(whole.status, b"? a\0? b\nc\0");
            assert!(!whole.truncated);
            assert_eq!(truncated.status, cut);
            assert!(truncated.truncated);
            let disabled = script_text(&runner, 0);
            assert!(disabled.contains("input_path='/srv/it'\"'\"'s here'\n"));
            assert!(disabled.contains("git_fsmonitor=disabled\n"));
            assert!(disabled.contains(&format!(
                "head -c {} ",
                MAXIMUM_REMOTE_REPOSITORY_STATUS_BYTES + 1
            )));
            let builtin = script_text(&runner, 1);
            assert!(builtin.contains("git_fsmonitor=builtin\n"));
            assert!(builtin.contains("-c core.fsmonitor=true"));
        }

        #[test]
        fn count_should_map_failures_and_reject_malformed_responses() {
            for (status, error) in [
                ("directory-unavailable", RemoteUtilityError::Missing),
                ("git-missing", RemoteUtilityError::ToolMissing),
                ("failed", RemoteUtilityError::RemoteFailed),
                ("unknown", RemoteUtilityError::InvalidResponse),
            ] {
                assert_eq!(
                    parse_repository_count(&raw_response("repository-count", status, &[], b""))
                        .unwrap_err(),
                    error
                );
            }
            let oversized = vec![b'x'; MAXIMUM_REMOTE_REPOSITORY_STATUS_BYTES + 1];
            for malformed in [
                repository_count_response(&oversized, true),
                raw_response("repository-count", "ok", &[b"a", b"b"], b"0\n"),
                raw_response("repository-count", "ok", &[], b"0\n"),
                raw_response("repository-count", "ok", &[b"a"], b"2\n"),
                raw_response("repository-count", "ok", &[b"a"], b""),
                raw_response("repository-count", "ok", &[b"a"], b"0\n0\n"),
                raw_response("repository-probe", "ok", &[b"a"], b"0\n"),
            ] {
                assert_eq!(
                    parse_repository_count(&malformed).unwrap_err(),
                    RemoteUtilityError::InvalidResponse
                );
            }
        }

        #[gpui::test]
        fn count_should_refuse_roots_the_script_cannot_carry(cx: &mut TestAppContext) {
            let (client, runner) = client([]);

            for root in ["relative/repo", "~/repo", "/srv/nul\0byte"] {
                assert_eq!(
                    cx.foreground_executor()
                        .block_test(client.count_repository_with_cancellation(
                            root,
                            FsmonitorPolicy::Disabled,
                            SshCancellationToken::default(),
                        ))
                        .unwrap_err(),
                    RemoteUtilityError::InvalidResponse
                );
            }
            let long_root = format!("/{}", "x".repeat(MAXIMUM_REMOTE_PATH_BYTES));
            assert_eq!(
                cx.foreground_executor()
                    .block_test(client.count_repository_with_cancellation(
                        &long_root,
                        FsmonitorPolicy::Disabled,
                        SshCancellationToken::default(),
                    ))
                    .unwrap_err(),
                RemoteUtilityError::RequestTooLarge
            );
            assert!(runner.state.lock().unwrap().scripts.is_empty());
        }

        #[gpui::test]
        fn a_utility_deadline_should_stay_distinct_from_transport_failure(cx: &mut TestAppContext) {
            let (client, _) = client([Err(RemoteUtilityRunError::TimedOut)]);

            assert_eq!(
                cx.foreground_executor()
                    .block_test(client.probe_repository_with_cancellation(
                        remote_directory("/srv/repo"),
                        SshCancellationToken::default(),
                    ))
                    .unwrap_err(),
                RemoteUtilityError::TimedOut
            );
        }
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "../platform/unix_adapter_tests/remote_utility.rs"]
mod unix_adapter_tests;
