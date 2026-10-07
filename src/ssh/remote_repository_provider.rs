//! Repository Status reads for one Remote Workspace through its Control Connection.
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::cancellation::SshCancellationToken;
use super::remote_utility::{RemoteUtilityError, SshRemoteUtilityClient, SshRemoteUtilityRunner};
use crate::domain::RemoteDirectory;
use crate::repository_status::{
    FsmonitorPolicy, RemoteRepositoryCount, RemoteRepositoryProbe, RemoteRepositoryReader,
    RepositoryReadError,
};

/// Waits before each new attempt after the server refuses a session on a live connection.
const SESSION_RETRY_DELAYS: [Duration; 5] = [
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
    Duration::from_millis(1600),
];
/// How often a waiting caller observes its cancellation.
const CANCELLATION_POLL: Duration = Duration::from_millis(10);

/// Blocking [`RemoteRepositoryReader`] over the utility client of one Control Connection.
///
/// The provider shares the client, and so its utility session limit, with the connection's
/// Directory Picker provider. At most one repository read runs at a time: a second caller waits
/// for the running read to finish, and gives up with [`RepositoryReadError::Cancelled`] when its
/// own cancellation fires first. Reads are not coalesced; the caller's scheduler merges triggers.
/// A session refused on a live connection is retried with backoff before the read reports it.
pub(crate) struct SshRemoteRepositoryProvider<R: SshRemoteUtilityRunner> {
    client: Arc<SshRemoteUtilityClient<R>>,
    turn: ReadTurn,
}

#[allow(
    dead_code,
    reason = "Repository Status composition creates one provider per Remote Workspace in a later commit"
)]
impl<R: SshRemoteUtilityRunner> SshRemoteRepositoryProvider<R> {
    pub(crate) fn new(client: Arc<SshRemoteUtilityClient<R>>) -> Self {
        Self {
            client,
            turn: ReadTurn::default(),
        }
    }

    fn read<T>(
        &self,
        cancellation: &SshCancellationToken,
        mut attempt: impl FnMut(SshCancellationToken) -> Result<T, RemoteUtilityError>,
    ) -> Result<T, RepositoryReadError> {
        let _turn = self.turn.take(cancellation)?;
        let mut delays = SESSION_RETRY_DELAYS.into_iter();
        loop {
            match (attempt(cancellation.clone()), delays.next()) {
                (Err(RemoteUtilityError::SessionUnavailable), Some(delay)) => {
                    sleep_unless_cancelled(delay, cancellation)?;
                }
                (result, _) => return result.map_err(|error| map_error(error, cancellation)),
            }
        }
    }
}

impl<R: SshRemoteUtilityRunner> RemoteRepositoryReader for SshRemoteRepositoryProvider<R> {
    fn probe(
        &self,
        directory: &RemoteDirectory,
        cancellation: &SshCancellationToken,
    ) -> Result<RemoteRepositoryProbe, RepositoryReadError> {
        self.read(cancellation, |attempt_cancellation| {
            pollster::block_on(
                self.client
                    .probe_repository_with_cancellation(directory.clone(), attempt_cancellation),
            )
        })
    }

    fn count(
        &self,
        root: &str,
        fsmonitor: FsmonitorPolicy,
        cancellation: &SshCancellationToken,
    ) -> Result<RemoteRepositoryCount, RepositoryReadError> {
        self.read(cancellation, |attempt_cancellation| {
            pollster::block_on(self.client.count_repository_with_cancellation(
                root,
                fsmonitor,
                attempt_cancellation,
            ))
        })
    }
}

/// Admits one repository read at a time.
#[derive(Default)]
struct ReadTurn {
    busy: Mutex<bool>,
    released: Condvar,
}

impl ReadTurn {
    fn take(&self, cancellation: &SshCancellationToken) -> Result<HeldTurn<'_>, RepositoryReadError> {
        let mut busy = self.lock();
        loop {
            if cancellation.is_cancelled() {
                return Err(RepositoryReadError::Cancelled);
            }
            if !*busy {
                *busy = true;
                return Ok(HeldTurn { turn: self });
            }
            busy = self
                .released
                .wait_timeout(busy, CANCELLATION_POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn lock(&self) -> MutexGuard<'_, bool> {
        self.busy.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct HeldTurn<'a> {
    turn: &'a ReadTurn,
}

impl Drop for HeldTurn<'_> {
    fn drop(&mut self) {
        *self.turn.lock() = false;
        self.turn.released.notify_one();
    }
}

fn sleep_unless_cancelled(
    delay: Duration,
    cancellation: &SshCancellationToken,
) -> Result<(), RepositoryReadError> {
    let deadline = Instant::now() + delay;
    loop {
        if cancellation.is_cancelled() {
            return Err(RepositoryReadError::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        thread::sleep(remaining.min(CANCELLATION_POLL));
    }
}

/// The client reports an ended Control Connection as cancellation, which is not the caller's.
fn map_error(error: RemoteUtilityError, cancellation: &SshCancellationToken) -> RepositoryReadError {
    match error {
        RemoteUtilityError::Cancelled if cancellation.is_cancelled() => {
            RepositoryReadError::Cancelled
        }
        RemoteUtilityError::Cancelled
        | RemoteUtilityError::Transport
        | RemoteUtilityError::SessionUnavailable => RepositoryReadError::ConnectionUnavailable,
        RemoteUtilityError::TimedOut => RepositoryReadError::TimedOut,
        RemoteUtilityError::OutputTooLarge => RepositoryReadError::OutputTooLarge,
        RemoteUtilityError::InvalidResponse => RepositoryReadError::InvalidResponse,
        RemoteUtilityError::ToolMissing => RepositoryReadError::ToolMissing,
        RemoteUtilityError::RequestTooLarge
        | RemoteUtilityError::CommandFailed(_)
        | RemoteUtilityError::UnsupportedLoginShell
        | RemoteUtilityError::Missing
        | RemoteUtilityError::NotDirectory
        | RemoteUtilityError::PermissionDenied
        | RemoteUtilityError::RemoteFailed => RepositoryReadError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::domain::SshDestination;
    use crate::repository_status::RemoteProbeOutcome;
    use crate::ssh::command::{OpenSshExecutable, SshCommandContext, SshCommandSpec};
    use crate::ssh::fake_remote_utility_server::{
        REPOSITORY_DISCOVERY, REPOSITORY_STATUS, raw_response,
        repository_count_response, repository_probe_response,
    };
    use crate::ssh::process::ProcessExit;
    use crate::ssh::remote_utility::{
        PreparedSshRemoteUtilityCommand, RemoteUtilityProcessOutput, RemoteUtilityRunError,
        RemoteUtilitySession,
    };

    type RunResult = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>;

    /// Answers each run at once from a queue.
    #[derive(Default)]
    struct ScriptedRunner {
        results: Mutex<VecDeque<RunResult>>,
        runs: AtomicUsize,
    }

    impl ScriptedRunner {
        fn new(results: impl IntoIterator<Item = RunResult>) -> Self {
            Self {
                results: Mutex::new(results.into_iter().collect()),
                runs: AtomicUsize::new(0),
            }
        }
    }

    impl SshRemoteUtilityRunner for ScriptedRunner {
        fn run(
            &self,
            _command: Arc<SshCommandSpec>,
            _script: Vec<u8>,
            _maximum_output_bytes: usize,
            _cancellation: SshCancellationToken,
            _session: RemoteUtilitySession,
        ) -> impl Future<Output = RunResult> + Send {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let result = self
                .results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(RemoteUtilityRunError::WorkerUnavailable));
            async move { result }
        }
    }

    /// Holds every run until the test releases it or its cancellation fires, as a process does.
    #[derive(Default)]
    struct HeldRunner {
        started: Mutex<Vec<(SshCancellationToken, async_channel::Sender<RunResult>)>>,
    }

    impl HeldRunner {
        fn started(&self) -> usize {
            self.started.lock().unwrap().len()
        }

        fn wait_for_start(&self, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.started() < count {
                assert!(Instant::now() < deadline, "run {count} never started");
                thread::sleep(Duration::from_millis(1));
            }
        }

        fn release(&self, index: usize, result: RunResult) {
            let sender = self.started.lock().unwrap()[index].1.clone();
            sender.send_blocking(result).unwrap();
        }
    }

    impl SshRemoteUtilityRunner for HeldRunner {
        fn run(
            &self,
            _command: Arc<SshCommandSpec>,
            _script: Vec<u8>,
            _maximum_output_bytes: usize,
            cancellation: SshCancellationToken,
            session: RemoteUtilitySession,
        ) -> impl Future<Output = RunResult> + Send {
            let (release, released) = async_channel::bounded(1);
            let (finish, finished) = async_channel::bounded(1);
            self.started
                .lock()
                .unwrap()
                .push((cancellation.clone(), release));
            thread::spawn(move || {
                let _session = session;
                let result = loop {
                    if cancellation.is_cancelled() {
                        break Err(RemoteUtilityRunError::Cancelled);
                    }
                    if let Ok(result) = released.try_recv() {
                        break result;
                    }
                    thread::sleep(Duration::from_millis(1));
                };
                let _ = finish.send_blocking(result);
            });
            async move {
                finished
                    .recv()
                    .await
                    .unwrap_or(Err(RemoteUtilityRunError::WorkerUnavailable))
            }
        }
    }

    fn client<R: SshRemoteUtilityRunner>(
        runner: Arc<R>,
        connection: SshCancellationToken,
    ) -> Arc<SshRemoteUtilityClient<R>> {
        let command = SshCommandContext::new(
            OpenSshExecutable::for_test(),
            PathBuf::from("/private/config/spaceterm/ssh_config"),
            SshDestination::new("remote".to_owned()).unwrap(),
            PathBuf::from("/private/runtime/spaceterm/master.sock"),
        )
        .unwrap()
        .remote_utility();
        Arc::new(SshRemoteUtilityClient::new(
            PreparedSshRemoteUtilityCommand::new(command),
            runner,
            connection,
        ))
    }

    fn scripted(
        results: impl IntoIterator<Item = RunResult>,
    ) -> (SshRemoteRepositoryProvider<ScriptedRunner>, Arc<ScriptedRunner>) {
        let runner = Arc::new(ScriptedRunner::new(results));
        (
            SshRemoteRepositoryProvider::new(client(
                Arc::clone(&runner),
                SshCancellationToken::default(),
            )),
            runner,
        )
    }

    fn held() -> (Arc<SshRemoteRepositoryProvider<HeldRunner>>, Arc<HeldRunner>) {
        let runner = Arc::new(HeldRunner::default());
        (
            Arc::new(SshRemoteRepositoryProvider::new(client(
                Arc::clone(&runner),
                SshCancellationToken::default(),
            ))),
            runner,
        )
    }

    fn success(stdout: Vec<u8>) -> RunResult {
        Ok(RemoteUtilityProcessOutput::new(
            ProcessExit::successful(),
            stdout,
        ))
    }

    fn exit(code: i32) -> RunResult {
        Ok(RemoteUtilityProcessOutput::new(
            ProcessExit::unsuccessful(Some(code)),
            Vec::new(),
        ))
    }

    fn directory() -> RemoteDirectory {
        RemoteDirectory::new("/srv/repo".to_owned()).unwrap()
    }

    fn probe_result(
        provider: &SshRemoteRepositoryProvider<impl SshRemoteUtilityRunner>,
    ) -> Result<RemoteRepositoryProbe, RepositoryReadError> {
        provider.probe(&directory(), &SshCancellationToken::default())
    }

    fn count_result(
        provider: &SshRemoteRepositoryProvider<impl SshRemoteUtilityRunner>,
    ) -> Result<RemoteRepositoryCount, RepositoryReadError> {
        provider.count(
            "/srv/repo",
            FsmonitorPolicy::Disabled,
            &SshCancellationToken::default(),
        )
    }

    #[test]
    fn probe_and_count_should_decode_typed_raw_results() {
        let (provider, _) = scripted([
            success(repository_probe_response()),
            success(repository_count_response(REPOSITORY_STATUS, true)),
        ]);

        let probe = probe_result(&provider).unwrap();
        let count = count_result(&provider).unwrap();

        assert_eq!(probe.outcome, RemoteProbeOutcome::Repository);
        assert_eq!(probe.discovery, REPOSITORY_DISCOVERY);
        assert_eq!(count.status, REPOSITORY_STATUS);
        assert!(count.truncated);
    }

    #[test]
    fn every_failure_should_map_to_a_content_free_read_error() {
        for (result, error) in [
            (
                Err(RemoteUtilityRunError::TimedOut),
                RepositoryReadError::TimedOut,
            ),
            (
                Err(RemoteUtilityRunError::OutputTooLarge),
                RepositoryReadError::OutputTooLarge,
            ),
            (
                Err(RemoteUtilityRunError::WorkerUnavailable),
                RepositoryReadError::ConnectionUnavailable,
            ),
            (
                success(b"SPACETERM-REMOTE/2\nrepository-probe\nok\n.\n".to_vec()),
                RepositoryReadError::InvalidResponse,
            ),
            (
                success(raw_response("repository-probe", "failed", &[], b"")),
                RepositoryReadError::Unavailable,
            ),
            (exit(70), RepositoryReadError::Unavailable),
        ] {
            let (provider, _) = scripted([result]);
            assert_eq!(probe_result(&provider).unwrap_err(), error);
        }
        for (status, error) in [
            ("git-missing", RepositoryReadError::ToolMissing),
            ("directory-unavailable", RepositoryReadError::Unavailable),
        ] {
            let (provider, _) = scripted([success(raw_response(
                "repository-count",
                status,
                &[],
                b"",
            ))]);
            assert_eq!(count_result(&provider).unwrap_err(), error);
        }
    }

    #[test]
    fn an_ended_control_connection_should_not_read_as_caller_cancellation() {
        let connection = SshCancellationToken::default();
        let runner = Arc::new(ScriptedRunner::default());
        let provider =
            SshRemoteRepositoryProvider::new(client(Arc::clone(&runner), connection.clone()));
        connection.cancel();

        assert_eq!(
            probe_result(&provider).unwrap_err(),
            RepositoryReadError::ConnectionUnavailable
        );
        assert_eq!(runner.runs.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_refused_session_should_be_retried_before_the_read_fails() {
        let (provider, runner) = scripted([exit(255), success(repository_probe_response())]);

        assert!(probe_result(&provider).is_ok());
        assert_eq!(runner.runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn cancelling_a_running_read_should_cancel_its_utility_process() {
        let (provider, runner) = held();
        let cancellation = SshCancellationToken::default();
        let reader = {
            let provider = Arc::clone(&provider);
            let cancellation = cancellation.clone();
            thread::spawn(move || provider.probe(&directory(), &cancellation))
        };
        runner.wait_for_start(1);

        cancellation.cancel();

        assert_eq!(
            reader.join().unwrap().unwrap_err(),
            RepositoryReadError::Cancelled
        );
        assert!(runner.started.lock().unwrap()[0].0.is_cancelled());
    }

    #[test]
    fn a_second_read_should_wait_for_the_running_read() {
        let (provider, runner) = held();
        let first = {
            let provider = Arc::clone(&provider);
            thread::spawn(move || probe_result(&provider))
        };
        runner.wait_for_start(1);
        let second = {
            let provider = Arc::clone(&provider);
            thread::spawn(move || count_result(&provider))
        };
        thread::sleep(Duration::from_millis(50));
        assert_eq!(runner.started(), 1);

        runner.release(0, success(repository_probe_response()));
        runner.wait_for_start(2);
        runner.release(1, success(repository_count_response(b"", false)));

        assert!(first.join().unwrap().is_ok());
        assert!(second.join().unwrap().is_ok());
    }

    #[test]
    fn a_waiting_read_should_give_up_when_cancelled() {
        let (provider, runner) = held();
        let first = {
            let provider = Arc::clone(&provider);
            thread::spawn(move || probe_result(&provider))
        };
        runner.wait_for_start(1);
        let cancellation = SshCancellationToken::default();
        let waiting = {
            let provider = Arc::clone(&provider);
            let cancellation = cancellation.clone();
            thread::spawn(move || {
                provider.count("/srv/repo", FsmonitorPolicy::Disabled, &cancellation)
            })
        };
        thread::sleep(Duration::from_millis(20));

        cancellation.cancel();

        assert_eq!(
            waiting.join().unwrap().unwrap_err(),
            RepositoryReadError::Cancelled
        );
        runner.release(0, success(repository_probe_response()));
        assert!(first.join().unwrap().is_ok());
        assert_eq!(runner.started(), 1);
    }

    #[test]
    fn repository_reads_should_share_the_connections_utility_session_limit() {
        let runner = Arc::new(HeldRunner::default());
        let client = client(Arc::clone(&runner), SshCancellationToken::default());
        let provider = Arc::new(SshRemoteRepositoryProvider::new(Arc::clone(&client)));
        let listings = (0..2)
            .map(|_| {
                let client = Arc::clone(&client);
                thread::spawn(move || {
                    pollster::block_on(client.list_directories_with_cancellation(
                        directory(),
                        SshCancellationToken::default(),
                    ))
                })
            })
            .collect::<Vec<_>>();
        runner.wait_for_start(2);
        let reader = {
            let provider = Arc::clone(&provider);
            thread::spawn(move || probe_result(&provider))
        };
        thread::sleep(Duration::from_millis(50));
        assert_eq!(runner.started(), 2);

        runner.release(0, success(raw_response("list", "ok", &[], b"0\n")));
        runner.wait_for_start(3);
        runner.release(1, success(raw_response("list", "ok", &[], b"0\n")));
        runner.release(2, success(repository_probe_response()));

        for listing in listings {
            assert!(listing.join().unwrap().is_ok());
        }
        assert!(reader.join().unwrap().is_ok());
    }
}
