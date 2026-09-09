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

pub(crate) const MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES: usize = 384 * 1024;
const MAXIMUM_REMOTE_UTILITY_REQUEST_BYTES: usize = 32 * 1024;
const MAXIMUM_REMOTE_FIELD_BYTES: usize = 16 * 1024;
const MAXIMUM_REMOTE_DIRECTORY_NAMES: usize = 1024;
const MAXIMUM_REMOTE_DIRECTORY_ENTRIES_EXAMINED: usize = 1024;
const MAXIMUM_REMOTE_PATH_BYTES: usize = 4096;
const UTILITY_TIMEOUT: Duration = Duration::from_secs(60);
const PROTOCOL_HEADER: &str = "SPACETERM-REMOTE/1";

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

    #[cfg(all(test, feature = "macos-native-tests"))]
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
        directory: RemoteDirectory,
    ) -> Result<RemoteUtilityDirectoryListing, RemoteUtilityError> {
        self.list_directories_with_cancellation(directory, SshCancellationToken::default())
            .await
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

    #[cfg(test)]
    pub(crate) async fn probe_exact_path(
        &self,
        directory: RemoteDirectory,
    ) -> Result<RemoteDirectoryProbe, RemoteUtilityError> {
        self.probe_exact_path_with_cancellation(directory, SshCancellationToken::default())
            .await
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

    #[cfg(test)]
    pub(crate) async fn create_directory_recursively(
        &self,
        directory: RemoteDirectory,
    ) -> Result<(), RemoteUtilityError> {
        self.create_directory_recursively_with_cancellation(
            directory,
            SshCancellationToken::default(),
        )
        .await
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

    #[cfg(test)]
    pub(crate) async fn resolve_physical_directory(
        &self,
        directory: RemoteDirectory,
    ) -> Result<String, RemoteUtilityError> {
        self.resolve_physical_directory_with_cancellation(
            directory,
            SshCancellationToken::default(),
        )
        .await
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
                RemoteUtilityRunError::TimedOut
                | RemoteUtilityRunError::Process(_)
                | RemoteUtilityRunError::WorkerUnavailable => RemoteUtilityError::Transport,
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
    use std::future::Future;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use gpui::TestAppContext;

    use super::*;
    use crate::domain::{RemoteDirectory, SshDestination};
    use crate::ssh::command::{OpenSshExecutable, SshCommandContext, SshCommandSpec};
    use crate::ssh::control_connection::SshCancellationToken;
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

#[cfg(all(test, target_os = "macos", feature = "macos-native-tests"))]
#[path = "../platform/macos_adapter_tests/remote_utility.rs"]
mod macos_adapter_tests;
