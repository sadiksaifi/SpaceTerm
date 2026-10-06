//! A fake SSH server that enforces a session limit on one Control Connection.
//!
//! A process started while every session is occupied exits with 255, as OpenSSH does when the
//! server refuses a multiplexed session.
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::BackgroundExecutor;

use super::cancellation::SshCancellationToken;
use super::command::{OpenSshExecutable, SshCommandContext, SshCommandSpec};
use super::process::ProcessExit;
use super::remote_directory_provider::SshRemoteDirectoryProvider;
use super::remote_utility::{
    PreparedSshRemoteUtilityCommand, RemoteUtilityProcessOutput, RemoteUtilityRunError,
    RemoteUtilitySession, SshRemoteUtilityRunner,
};
use crate::domain::SshDestination;

/// How long an admitted process takes to answer.
const RESPONSE_LATENCY: Duration = Duration::from_millis(20);
/// How often a running process observes cancellation, standing in for kill and reap latency.
const CANCELLATION_POLL: Duration = Duration::from_millis(5);
const HOME: &str = "/home/tester";
const LISTED_DIRECTORIES: [&str; 3] = ["Projects", "Documents", "srv"];

#[derive(Default)]
struct ServerState {
    maximum_sessions: usize,
    terminal_session_channels: usize,
    utility_sessions: usize,
    peak_utility_sessions: usize,
    refused_sessions: usize,
}

impl ServerState {
    fn admit(&mut self) -> bool {
        if self.terminal_session_channels + self.utility_sessions >= self.maximum_sessions {
            self.refused_sessions += 1;
            return false;
        }
        self.utility_sessions += 1;
        self.peak_utility_sessions = self.peak_utility_sessions.max(self.utility_sessions);
        true
    }
}

#[derive(Clone)]
pub(crate) struct FakeRemoteUtilityServer {
    executor: BackgroundExecutor,
    state: Arc<Mutex<ServerState>>,
    connection: SshCancellationToken,
}

impl FakeRemoteUtilityServer {
    pub(crate) fn new(executor: BackgroundExecutor, maximum_sessions: usize) -> Self {
        Self {
            executor,
            state: Arc::new(Mutex::new(ServerState {
                maximum_sessions,
                ..ServerState::default()
            })),
            connection: SshCancellationToken::default(),
        }
    }

    /// Occupies `count` sessions with Terminal Session Channels, replacing any earlier count.
    pub(crate) fn open_terminal_session_channels(&self, count: usize) {
        self.state.lock().unwrap().terminal_session_channels = count;
    }

    /// Ends the Control Connection, which revokes the provider's cancellation scope.
    pub(crate) fn end_connection(&self) {
        self.connection.cancel();
    }

    pub(crate) fn peak_utility_sessions(&self) -> usize {
        self.state.lock().unwrap().peak_utility_sessions
    }

    pub(crate) fn refused_sessions(&self) -> usize {
        self.state.lock().unwrap().refused_sessions
    }

    /// Creates the provider a connected Remote Workspace would use for this connection.
    pub(crate) fn provider(&self) -> SshRemoteDirectoryProvider<Self> {
        let command = SshCommandContext::new(
            OpenSshExecutable::for_test(),
            PathBuf::from("/private/config/spaceterm/ssh_config"),
            SshDestination::new("remote".to_owned()).unwrap(),
            PathBuf::from("/private/runtime/spaceterm/master.sock"),
        )
        .unwrap()
        .remote_utility();
        SshRemoteDirectoryProvider::new(
            PreparedSshRemoteUtilityCommand::new(command),
            Arc::new(self.clone()),
            self.connection.clone(),
            self.executor.clone(),
        )
    }
}

impl SshRemoteUtilityRunner for FakeRemoteUtilityServer {
    fn run(
        &self,
        _command: Arc<SshCommandSpec>,
        script: Vec<u8>,
        _maximum_output_bytes: usize,
        cancellation: SshCancellationToken,
        session: RemoteUtilitySession,
    ) -> impl Future<Output = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>> + Send
    {
        let (sender, receiver) = async_channel::bounded(1);
        let state = Arc::clone(&self.state);
        let connection = self.connection.clone();
        let executor = self.executor.clone();
        self.executor
            .spawn(async move {
                // The simulated process holds its admission until it exits, as a real one does.
                let _session = session;
                if !state.lock().unwrap().admit() {
                    let refused = RemoteUtilityProcessOutput::new(
                        ProcessExit::unsuccessful(Some(255)),
                        Vec::new(),
                    );
                    let _ = sender.send(Ok(refused)).await;
                    return;
                }
                let mut waited = Duration::ZERO;
                let result = loop {
                    if connection.is_cancelled() {
                        break Ok(RemoteUtilityProcessOutput::new(
                            ProcessExit::unsuccessful(Some(255)),
                            Vec::new(),
                        ));
                    }
                    if cancellation.is_cancelled() {
                        break Err(RemoteUtilityRunError::Cancelled);
                    }
                    if waited >= RESPONSE_LATENCY {
                        break respond(&script);
                    }
                    executor.timer(CANCELLATION_POLL).await;
                    waited += CANCELLATION_POLL;
                };
                state.lock().unwrap().utility_sessions -= 1;
                let _ = sender.send(result).await;
            })
            .detach();
        async move {
            receiver
                .recv()
                .await
                .unwrap_or(Err(RemoteUtilityRunError::WorkerUnavailable))
        }
    }
}

fn respond(script: &[u8]) -> Result<RemoteUtilityProcessOutput, RemoteUtilityRunError> {
    let script = String::from_utf8_lossy(script);
    let stdout = if script.contains("emit_header account ok") {
        response(
            "account",
            &["tester", "501", HOME, "/bin/zsh", HOME, "not-applicable"],
            "",
        )
    } else if script.contains("emit_header list ok") {
        response("list", &LISTED_DIRECTORIES, "0\n")
    } else if script.contains("emit_empty probe ok") {
        response("probe", &[], "")
    } else {
        return Err(RemoteUtilityRunError::WorkerUnavailable);
    };
    Ok(RemoteUtilityProcessOutput::new(
        ProcessExit::successful(),
        stdout,
    ))
}

fn response(kind: &str, fields: &[&str], tail: &str) -> Vec<u8> {
    let mut response = format!("SPACETERM-REMOTE/1\n{kind}\nok\n").into_bytes();
    for field in fields {
        response.extend_from_slice(format!("{}:", field.len()).as_bytes());
        response.extend_from_slice(field.as_bytes());
        response.push(b',');
    }
    response.extend_from_slice(b".\n");
    response.extend_from_slice(tail.as_bytes());
    response
}
