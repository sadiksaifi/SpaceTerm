use std::future::Future;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::str;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use thiserror::Error;

use super::cancellation::SshCancellationToken;
use super::command::SshCommandSpec;
use super::live_connection::LiveConnectionCapability;
use super::process::ProcessExit;
use crate::domain::RemoteWorkspaceDirectory;

pub(crate) const MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES: usize = 384 * 1024;
const MAXIMUM_REMOTE_UTILITY_REQUEST_BYTES: usize = 32 * 1024;
const MAXIMUM_REMOTE_FIELD_BYTES: usize = 16 * 1024;
const MAXIMUM_REMOTE_DIRECTORY_NAMES: usize = 1024;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PROTOCOL_HEADER: &str = "SPACETERM-REMOTE/1";

#[derive(Debug)]
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
pub(crate) enum RemoteUtilityRunError {
    #[error("remote utility command was cancelled")]
    Cancelled,
    #[error("remote utility output exceeded its safety limit")]
    OutputTooLarge,
    #[error("remote utility process failed")]
    Io(#[source] io::Error),
}

pub(crate) trait SshRemoteUtilityRunner: Send + Sync + 'static {
    fn run(
        &self,
        command: Arc<SshCommandSpec>,
        script: Vec<u8>,
        maximum_output_bytes: usize,
        cancellation: SshCancellationToken,
    ) -> impl Future<Output = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>> + Send;
}

/// A reusable utility channel command created only by the centralized SSH command policy.
pub(crate) struct PreparedSshRemoteUtilityCommand {
    command: Arc<SshCommandSpec>,
    capability: Option<LiveConnectionCapability>,
}

impl PreparedSshRemoteUtilityCommand {
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

#[derive(Clone, Copy, Default)]
pub(crate) struct NativeSshRemoteUtilityRunner;

impl SshRemoteUtilityRunner for NativeSshRemoteUtilityRunner {
    fn run(
        &self,
        command: Arc<SshCommandSpec>,
        script: Vec<u8>,
        maximum_output_bytes: usize,
        cancellation: SshCancellationToken,
    ) -> impl Future<Output = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>> + Send
    {
        async move { run_native_command(command, script, maximum_output_bytes, &cancellation) }
    }
}

fn run_native_command(
    spec: Arc<SshCommandSpec>,
    script: Vec<u8>,
    maximum_output_bytes: usize,
    cancellation: &SshCancellationToken,
) -> Result<RemoteUtilityProcessOutput, RemoteUtilityRunError> {
    if cancellation.is_cancelled() {
        return Err(RemoteUtilityRunError::Cancelled);
    }
    let mut child = Command::new(spec.executable())
        .args(spec.arguments())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(RemoteUtilityRunError::Io)?;
    let Some(mut stdin) = child.stdin.take() else {
        terminate_and_reap(&mut child);
        return Err(RemoteUtilityRunError::Io(io::Error::other(
            "SSH stdin pipe was unavailable",
        )));
    };
    let Some(mut stdout) = child.stdout.take() else {
        terminate_and_reap(&mut child);
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
            terminate_and_reap(&mut child);
            let _ = writer.join();
            if let Some(reader) = reader.take() {
                let _ = reader.join();
            }
            return Err(RemoteUtilityRunError::Cancelled);
        }
        if reader.as_ref().is_some_and(|reader| reader.is_finished()) {
            let Some(finished_reader) = reader.take() else {
                terminate_and_reap(&mut child);
                let _ = writer.join();
                return Err(RemoteUtilityRunError::Io(io::Error::other(
                    "SSH stdout reader ownership was lost",
                )));
            };
            match finished_reader.join() {
                Ok(Err(ReadBoundedError::OutputTooLarge)) => {
                    terminate_and_reap(&mut child);
                    let _ = writer.join();
                    return Err(RemoteUtilityRunError::OutputTooLarge);
                }
                Ok(Err(ReadBoundedError::Io(error))) => {
                    terminate_and_reap(&mut child);
                    let _ = writer.join();
                    return Err(RemoteUtilityRunError::Io(error));
                }
                Ok(Ok(stdout)) => {
                    captured_stdout = Some(stdout);
                }
                Err(_) => {
                    terminate_and_reap(&mut child);
                    let _ = writer.join();
                    return Err(RemoteUtilityRunError::Io(io::Error::other(
                        "SSH stdout reader failed",
                    )));
                }
            }
        }
        match child.try_wait() {
            Err(error) => {
                terminate_and_reap(&mut child);
                let _ = writer.join();
                if let Some(reader) = reader.take() {
                    let _ = reader.join();
                }
                return Err(RemoteUtilityRunError::Io(error));
            }
            Ok(Some(status)) => break ProcessExit::from(status),
            Ok(None) => thread::sleep(PROCESS_POLL_INTERVAL),
        }
    };
    if let Ok(Err(error)) = writer.join() {
        if exit.is_success() {
            return Err(RemoteUtilityRunError::Io(error));
        }
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

fn terminate_and_reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
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
pub(crate) struct RemoteAccountMetadata {
    user: String,
    uid: u64,
    home: String,
    login_shell: String,
    physical_home: String,
}

impl RemoteAccountMetadata {
    pub(crate) fn user(&self) -> &str {
        &self.user
    }

    pub(crate) const fn uid(&self) -> u64 {
        self.uid
    }

    pub(crate) fn home(&self) -> &str {
        &self.home
    }

    pub(crate) fn login_shell(&self) -> &str {
        &self.login_shell
    }

    pub(crate) fn physical_home(&self) -> &str {
        &self.physical_home
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
pub(crate) enum RemoteDirectoryProbe {
    ReadableDirectory,
    Missing,
}

pub(crate) struct SshRemoteUtilityClient<R: SshRemoteUtilityRunner> {
    command: PreparedSshRemoteUtilityCommand,
    runner: Arc<R>,
    cancellation: SshCancellationToken,
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
        }
    }

    pub(crate) async fn discover_account(
        &self,
    ) -> Result<RemoteAccountMetadata, RemoteUtilityError> {
        let output = self.execute(build_account_script()).await?;
        parse_account(&output)
    }

    pub(crate) async fn list_directories(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Result<RemoteUtilityDirectoryListing, RemoteUtilityError> {
        let output = self
            .execute(build_path_script("list", directory.as_str()))
            .await?;
        parse_listing(&output)
    }

    pub(crate) async fn probe_exact_path(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Result<RemoteDirectoryProbe, RemoteUtilityError> {
        let output = self
            .execute(build_path_script("probe", directory.as_str()))
            .await?;
        parse_probe(&output)
    }

    pub(crate) async fn create_directory_recursively(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Result<(), RemoteUtilityError> {
        let output = self
            .execute(build_path_script("mkdir", directory.as_str()))
            .await?;
        parse_empty_success(&output, "mkdir")
    }

    pub(crate) async fn resolve_physical_directory(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Result<String, RemoteUtilityError> {
        let output = self
            .execute(build_path_script("physical", directory.as_str()))
            .await?;
        parse_physical(&output)
    }

    async fn execute(&self, script: Vec<u8>) -> Result<Vec<u8>, RemoteUtilityError> {
        if self.cancellation.is_cancelled() {
            return Err(RemoteUtilityError::Cancelled);
        }
        if script.len() > MAXIMUM_REMOTE_UTILITY_REQUEST_BYTES {
            return Err(RemoteUtilityError::RequestTooLarge);
        }
        let operation_cancellation = match &self.command.capability {
            Some(capability) => {
                capability
                    .authorize()
                    .map_err(|_| RemoteUtilityError::Transport)?;
                SshCancellationToken::linked(&self.cancellation, &capability.cancellation())
            }
            None => self.cancellation.clone(),
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
                RemoteUtilityRunError::Io(_) => RemoteUtilityError::Transport,
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

fn build_path_script(operation: &str, path: &str) -> Vec<u8> {
    let operation_script = match operation {
        "list" => LIST_SCRIPT,
        "probe" => PROBE_SCRIPT,
        "mkdir" => MKDIR_SCRIPT,
        "physical" => PHYSICAL_SCRIPT,
        _ => unreachable!("remote utility operation is fixed by the caller"),
    };
    format!(
        "{COMMON_SCRIPT}\ninput_path={}\n{PATH_EXPANSION_SCRIPT}\n{operation_script}",
        quote_for_posix_shell(path)
    )
    .into_bytes()
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

const ACCOUNT_SCRIPT: &str = r#"user=$(id -un 2>/dev/null) || { emit_empty account failed; exit 0; }
uid=$(id -u 2>/dev/null) || { emit_empty account failed; exit 0; }
home=${HOME-}
login_shell=${SHELL-}
[ -n "$user" ] && [ -n "$uid" ] && [ -n "$home" ] && [ -n "$login_shell" ] || { emit_empty account failed; exit 0; }
physical_home_output=$(cd "$home" 2>/dev/null && { pwd -P && printf .; }) || { emit_empty account failed; exit 0; }
physical_home_with_separator=${physical_home_output%?}
physical_home=${physical_home_with_separator%?}
emit_header account ok
emit_field "$user"
emit_field "$uid"
emit_field "$home"
emit_field "$login_shell"
emit_field "$physical_home"
printf '.\n'
"#;

const PROBE_SCRIPT: &str = r#"if [ ! -e "$remote_path" ]; then
    emit_empty probe missing
elif [ ! -d "$remote_path" ]; then
    emit_empty probe not-directory
elif [ ! -r "$remote_path" ] || [ ! -x "$remote_path" ]; then
    emit_empty probe permission-denied
else
    emit_empty probe ok
fi
"#;

const LIST_SCRIPT: &str = r#"if [ ! -e "$remote_path" ]; then
    emit_empty list missing
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
emit_header list ok
set +f
directory_count=0
listing_truncated=0
for child in "$remote_path"/* "$remote_path"/.[!.]* "$remote_path"/..?*; do
    [ -d "$child" ] || continue
    directory_count=$((directory_count + 1))
    if [ "$directory_count" -gt 1024 ]; then
        listing_truncated=1
        break
    fi
    child_name=${child##*/}
    emit_field "$child_name"
done
printf '.\n%s\n' "$listing_truncated"
"#;

const MKDIR_SCRIPT: &str = r#"if [ -e "$remote_path" ] && [ ! -d "$remote_path" ]; then
    emit_empty mkdir not-directory
elif mkdir -p "$remote_path" 2>/dev/null && [ -d "$remote_path" ]; then
    emit_empty mkdir ok
else
    emit_empty mkdir permission-denied
fi
"#;

const PHYSICAL_SCRIPT: &str = r#"if [ ! -e "$remote_path" ]; then
    emit_empty physical missing
    exit 0
fi
if [ ! -d "$remote_path" ]; then
    emit_empty physical not-directory
    exit 0
fi
physical_path_output=$(cd "$remote_path" 2>/dev/null && { pwd -P && printf .; }) || { emit_empty physical permission-denied; exit 0; }
physical_path_with_separator=${physical_path_output%?}
physical_path=${physical_path_with_separator%?}
emit_header physical ok
emit_field "$physical_path"
printf '.\n'
"#;

fn parse_account(output: &[u8]) -> Result<RemoteAccountMetadata, RemoteUtilityError> {
    let mut response = ResponseParser::new(output, "account")?;
    response.require_ok()?;
    let fields = response.finish_fields(5)?;
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
        || fields[2..]
            .iter()
            .any(|field| field.chars().any(char::is_control))
    {
        return Err(RemoteUtilityError::InvalidResponse);
    }
    Ok(RemoteAccountMetadata {
        user: fields[0].clone(),
        uid,
        home: fields[2].clone(),
        login_shell: fields[3].clone(),
        physical_home: fields[4].clone(),
    })
}

fn parse_listing(output: &[u8]) -> Result<RemoteUtilityDirectoryListing, RemoteUtilityError> {
    let mut response = ResponseParser::new(output, "list")?;
    response.require_ok()?;
    let names = response.read_fields(MAXIMUM_REMOTE_DIRECTORY_NAMES)?;
    let truncated = match response.read_line()? {
        "0" => false,
        "1" => true,
        _ => return Err(RemoteUtilityError::InvalidResponse),
    };
    response.require_end()?;
    Ok(RemoteUtilityDirectoryListing { names, truncated })
}

fn parse_probe(output: &[u8]) -> Result<RemoteDirectoryProbe, RemoteUtilityError> {
    let mut response = ResponseParser::new(output, "probe")?;
    match response.status {
        "ok" => {
            response.finish_fields(0)?;
            Ok(RemoteDirectoryProbe::ReadableDirectory)
        }
        "missing" => {
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
    input: &'a str,
    cursor: usize,
    status: &'a str,
}

impl<'a> ResponseParser<'a> {
    fn new(output: &'a [u8], expected_kind: &str) -> Result<Self, RemoteUtilityError> {
        if output.len() > MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES {
            return Err(RemoteUtilityError::OutputTooLarge);
        }
        let input = str::from_utf8(output).map_err(|_| RemoteUtilityError::InvalidResponse)?;
        let mut response = Self {
            input,
            cursor: 0,
            status: "",
        };
        if response.read_line()? != PROTOCOL_HEADER || response.read_line()? != expected_kind {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        response.status = response.read_line()?;
        Ok(response)
    }

    fn require_ok(&mut self) -> Result<(), RemoteUtilityError> {
        if self.status == "ok" {
            Ok(())
        } else {
            self.finish_fields(0)?;
            Err(self.status_error())
        }
    }

    fn status_error(&self) -> RemoteUtilityError {
        match self.status {
            "missing" => RemoteUtilityError::Missing,
            "not-directory" => RemoteUtilityError::NotDirectory,
            "permission-denied" => RemoteUtilityError::PermissionDenied,
            "failed" => RemoteUtilityError::RemoteFailed,
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
        let mut fields = Vec::new();
        loop {
            if self.remaining().starts_with(".\n") {
                self.cursor += 2;
                return Ok(fields);
            }
            if fields.len() >= maximum_count {
                return Err(RemoteUtilityError::InvalidResponse);
            }
            fields.push(self.read_netstring()?);
        }
    }

    fn read_netstring(&mut self) -> Result<String, RemoteUtilityError> {
        let remaining = self.remaining();
        let colon = remaining
            .find(':')
            .ok_or(RemoteUtilityError::InvalidResponse)?;
        let length_spelling = &remaining[..colon];
        if length_spelling.is_empty()
            || !length_spelling.bytes().all(|byte| byte.is_ascii_digit())
            || (length_spelling.len() > 1 && length_spelling.starts_with('0'))
        {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        let length = length_spelling
            .parse::<usize>()
            .map_err(|_| RemoteUtilityError::InvalidResponse)?;
        if length > MAXIMUM_REMOTE_FIELD_BYTES {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        let field_start = self.cursor + colon + 1;
        let field_end = field_start
            .checked_add(length)
            .ok_or(RemoteUtilityError::InvalidResponse)?;
        if self.input.as_bytes().get(field_end) != Some(&b',')
            || !self.input.is_char_boundary(field_start)
            || !self.input.is_char_boundary(field_end)
        {
            return Err(RemoteUtilityError::InvalidResponse);
        }
        let field = self.input[field_start..field_end].to_owned();
        self.cursor = field_end + 1;
        Ok(field)
    }

    fn read_line(&mut self) -> Result<&'a str, RemoteUtilityError> {
        let remaining = self.remaining();
        let newline = remaining
            .find('\n')
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

    fn remaining(&self) -> &'a str {
        &self.input[self.cursor..]
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::io;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};

    use gpui::TestAppContext;

    use super::*;
    use crate::domain::{RemoteWorkspaceDirectory, SshDestination};
    use crate::ssh::command::{SshCommandContext, SshCommandSpec};
    use crate::ssh::control_connection::SshCancellationToken;
    use crate::ssh::process::ProcessExit;

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
                    Err(RemoteUtilityRunError::Io(io::Error::new(
                        io::ErrorKind::Other,
                        "missing fake response",
                    )))
                })
            };
            async move { result }
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
            build_path_script("list", "/tmp/space ' with quote"),
            build_path_script("probe", "~/project"),
            build_path_script("mkdir", "/tmp/-leading"),
            build_path_script("physical", "/tmp/project"),
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
            ],
            "",
        ))]);

        let metadata = cx.executor().block(client.discover_account()).unwrap();

        assert_eq!(metadata.user(), "tester");
        assert_eq!(metadata.uid(), 501);
        assert_eq!(metadata.home(), "/Users/tester");
        assert_eq!(metadata.login_shell(), "/bin/zsh");
        assert_eq!(metadata.physical_home(), "/Users/tester");
    }

    #[gpui::test]
    fn listing_should_preserve_spaces_and_leading_dashes(cx: &mut TestAppContext) {
        let (client, _) = client([success(response(
            "list",
            "ok",
            &["Space Term", "-archive", ".config", "line\nbreak"],
            "1\n",
        ))]);

        let listing = cx
            .executor()
            .block(client.list_directories(remote_directory("/srv/projects")))
            .unwrap();

        assert_eq!(
            listing.names(),
            ["Space Term", "-archive", ".config", "line\nbreak"]
        );
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
