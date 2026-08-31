use std::fs;
use std::io;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use thiserror::Error;

use super::command::{SshCommandContext, SshCommandContextError};
use super::process::{ProcessExit, SshProcessBackend};
use crate::domain::SshDestination;
use crate::platform::app_paths::{AppPaths, AppPathsError, RegisteredRuntimeSocket, RuntimeOwner};

const CONTROL_OWNER_KIND: &str = "ssh-control";
const CONTROL_SOCKET_NAME: &str = "master.sock";
const MAXIMUM_READINESS_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub(crate) struct SshCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl SshCancellationToken {
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ControlConnectionTimingError {
    #[error("SSH readiness timeout must be between one nanosecond and 60 seconds")]
    InvalidTimeout,
    #[error("SSH readiness polling interval must be nonzero and no longer than the timeout")]
    InvalidPollInterval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ControlConnectionTiming {
    timeout: Duration,
    poll_interval: Duration,
}

impl ControlConnectionTiming {
    pub(crate) fn new(
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<Self, ControlConnectionTimingError> {
        if timeout.is_zero() || timeout > MAXIMUM_READINESS_TIMEOUT {
            return Err(ControlConnectionTimingError::InvalidTimeout);
        }
        if poll_interval.is_zero() || poll_interval > timeout {
            return Err(ControlConnectionTimingError::InvalidPollInterval);
        }
        Ok(Self {
            timeout,
            poll_interval,
        })
    }
}

impl Default for ControlConnectionTiming {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            poll_interval: Duration::from_millis(100),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlConnectionState {
    Ready,
    ShuttingDown,
    Closed,
}

#[derive(Debug, Error)]
pub(crate) enum ControlConnectionError {
    #[error("SSH control connection was cancelled")]
    Cancelled,
    #[error("SSH control master did not become ready within {timeout:?}")]
    ReadinessTimedOut { timeout: Duration },
    #[error("SSH control master exited before becoming ready: {0:?}")]
    MasterExited(ProcessExit),
    #[error("SSH control master could not be launched: {source}")]
    Launch {
        #[source]
        source: io::Error,
    },
    #[error("SSH control master status could not be checked: {source}")]
    MasterStatus {
        #[source]
        source: io::Error,
    },
    #[error("SSH readiness command could not be run: {source}")]
    Readiness {
        #[source]
        source: io::Error,
    },
    #[error("SSH shutdown command could not be run: {source}")]
    ShutdownCommand {
        #[source]
        source: io::Error,
    },
    #[error("SSH control master rejected graceful shutdown: {0:?}")]
    ShutdownRejected(ProcessExit),
    #[error("SSH control master could not be reaped: {source}")]
    Reap {
        #[source]
        source: io::Error,
    },
    #[error("SSH private runtime cleanup failed: {0}")]
    Cleanup(AppPathsError),
    #[error("SSH private socket reservation failed: {source}")]
    SocketReservation {
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Paths(#[from] AppPathsError),
    #[error(transparent)]
    Command(#[from] SshCommandContextError),
    #[error("SSH control connection ownership was lost")]
    Ownership,
}

pub(crate) struct OpenSshControlConnection<B: SshProcessBackend> {
    backend: Arc<B>,
    commands: SshCommandContext,
    child: Option<B::Child>,
    runtime_owner: Option<RuntimeOwner>,
    registered_socket: Option<RegisteredRuntimeSocket>,
    control_path: PathBuf,
    state: ControlConnectionState,
}

impl<B: SshProcessBackend> OpenSshControlConnection<B> {
    pub(crate) async fn connect(
        paths: &AppPaths,
        destination: SshDestination,
        backend: Arc<B>,
        cancellation: &SshCancellationToken,
        timing: ControlConnectionTiming,
    ) -> Result<Self, ControlConnectionError> {
        if cancellation.is_cancelled() {
            return Err(ControlConnectionError::Cancelled);
        }
        let runtime_owner = paths.create_runtime_owner(CONTROL_OWNER_KIND)?;
        let control_path = runtime_owner.socket_path(CONTROL_SOCKET_NAME)?;
        let reservation = reserve_socket(&runtime_owner)?;
        let commands = SshCommandContext::new(
            paths.managed_ssh_config(),
            destination,
            control_path.clone(),
        )?;
        if cancellation.is_cancelled() {
            return Err(ControlConnectionError::Cancelled);
        }
        let child = backend
            .spawn(commands.master())
            .await
            .map_err(|source| ControlConnectionError::Launch { source })?;
        let mut launch = ConnectingControl {
            backend: Arc::clone(&backend),
            child: Some(child),
            runtime_owner: Some(runtime_owner),
            registered_socket: Some(reservation),
        };
        let mut elapsed = Duration::ZERO;

        loop {
            if cancellation.is_cancelled() {
                return Err(ControlConnectionError::Cancelled);
            }
            let child = launch
                .child
                .as_mut()
                .ok_or(ControlConnectionError::Ownership)?;
            if let Some(exit) = backend
                .try_wait(child)
                .map_err(|source| ControlConnectionError::MasterStatus { source })?
            {
                launch.child.take();
                return Err(ControlConnectionError::MasterExited(exit));
            }
            if elapsed >= timing.timeout {
                return Err(ControlConnectionError::ReadinessTimedOut {
                    timeout: timing.timeout,
                });
            }
            let readiness = backend
                .run(commands.readiness_check())
                .await
                .map_err(|source| ControlConnectionError::Readiness { source })?;
            if cancellation.is_cancelled() {
                return Err(ControlConnectionError::Cancelled);
            }
            if readiness.is_success() {
                let owner = launch
                    .runtime_owner
                    .as_ref()
                    .ok_or(ControlConnectionError::Ownership)?;
                let registered = owner.register_socket(CONTROL_SOCKET_NAME)?;
                launch.registered_socket = Some(registered);
                return launch.finish(commands, control_path);
            }
            let delay = timing.poll_interval.min(timing.timeout - elapsed);
            backend.delay(delay).await;
            elapsed += delay;
        }
    }

    pub(crate) const fn state(&self) -> ControlConnectionState {
        self.state
    }

    pub(crate) fn control_path(&self) -> &Path {
        &self.control_path
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), ControlConnectionError> {
        if self.state == ControlConnectionState::Closed {
            return Ok(());
        }

        let mut first_error = None;
        if self.state == ControlConnectionState::Ready {
            self.state = ControlConnectionState::ShuttingDown;
            match self.backend.run(self.commands.graceful_exit()).await {
                Ok(exit) if exit.is_success() => {}
                Ok(exit) => first_error = Some(ControlConnectionError::ShutdownRejected(exit)),
                Err(source) => {
                    first_error = Some(ControlConnectionError::ShutdownCommand { source });
                }
            }
        }
        if let Some(mut child) = self.child.take()
            && let Err(source) = self.backend.terminate_and_reap(&mut child)
            && first_error.is_none()
        {
            first_error = Some(ControlConnectionError::Reap { source });
        }
        self.registered_socket.take();
        if let Some(owner) = self.runtime_owner.take()
            && let Err(error) = owner.close()
            && first_error.is_none()
        {
            first_error = Some(ControlConnectionError::Cleanup(error));
        }
        self.state = ControlConnectionState::Closed;
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl<B: SshProcessBackend> Drop for OpenSshControlConnection<B> {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = self.backend.terminate_and_reap(&mut child);
        }
        self.registered_socket.take();
        self.runtime_owner.take();
        self.state = ControlConnectionState::Closed;
    }
}

struct ConnectingControl<B: SshProcessBackend> {
    backend: Arc<B>,
    child: Option<B::Child>,
    runtime_owner: Option<RuntimeOwner>,
    registered_socket: Option<RegisteredRuntimeSocket>,
}

impl<B: SshProcessBackend> ConnectingControl<B> {
    fn finish(
        mut self,
        commands: SshCommandContext,
        control_path: PathBuf,
    ) -> Result<OpenSshControlConnection<B>, ControlConnectionError> {
        let child = self.child.take().ok_or(ControlConnectionError::Ownership)?;
        let runtime_owner = self
            .runtime_owner
            .take()
            .ok_or(ControlConnectionError::Ownership)?;
        let registered_socket = self
            .registered_socket
            .take()
            .ok_or(ControlConnectionError::Ownership)?;
        Ok(OpenSshControlConnection {
            backend: Arc::clone(&self.backend),
            commands,
            child: Some(child),
            runtime_owner: Some(runtime_owner),
            registered_socket: Some(registered_socket),
            control_path,
            state: ControlConnectionState::Ready,
        })
    }
}

impl<B: SshProcessBackend> Drop for ConnectingControl<B> {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = self.backend.terminate_and_reap(&mut child);
        }
        self.registered_socket.take();
        self.runtime_owner.take();
    }
}

fn reserve_socket(owner: &RuntimeOwner) -> Result<RegisteredRuntimeSocket, ControlConnectionError> {
    let path = owner.socket_path(CONTROL_SOCKET_NAME)?;
    let listener = UnixListener::bind(&path)
        .map_err(|source| ControlConnectionError::SocketReservation { source })?;
    let registered = owner.register_socket(CONTROL_SOCKET_NAME)?;
    drop(listener);
    fs::remove_file(&path)
        .map_err(|source| ControlConnectionError::SocketReservation { source })?;
    Ok(registered)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::fs;
    use std::future::{Future, pending};
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Wake, Waker};
    use std::time::Duration;

    use gpui::TestAppContext;

    use super::*;
    use crate::domain::SshDestination;
    use crate::platform::app_paths::{AppPathEnvironment, AppPaths};
    use crate::ssh::command::SshCommandSpec;
    use crate::ssh::process::{ProcessExit, SshProcessBackend};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = PathBuf::from(format!(
                "/private/tmp/spaceterm-control-{}-{sequence}",
                std::process::id()
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }

        fn paths(&self) -> AppPaths {
            AppPaths::resolve(&AppPathEnvironment {
                home: None,
                xdg_config_home: Some(self.0.join("config").into_os_string()),
                xdg_data_home: Some(self.0.join("data").into_os_string()),
                xdg_state_home: Some(self.0.join("state").into_os_string()),
                xdg_cache_home: Some(self.0.join("cache").into_os_string()),
                xdg_runtime_dir: Some(self.0.join("runtime").into_os_string()),
                macos_temporary_directory: self.0.join("temporary"),
            })
            .unwrap()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FakeChild {
        listener: Option<UnixListener>,
    }

    #[derive(Default)]
    struct FakeState {
        readiness: VecDeque<ProcessExit>,
        early_exits: VecDeque<Option<ProcessExit>>,
        records: Vec<Vec<OsString>>,
        delays: Vec<Duration>,
        socket_path: Option<PathBuf>,
        reaps: usize,
        cancel_on_delay: Option<SshCancellationToken>,
        pending_delay: bool,
    }

    #[derive(Default)]
    struct FakeBackend {
        state: Mutex<FakeState>,
    }

    impl FakeBackend {
        fn with_readiness(readiness: impl IntoIterator<Item = ProcessExit>) -> Self {
            Self {
                state: Mutex::new(FakeState {
                    readiness: readiness.into_iter().collect(),
                    ..FakeState::default()
                }),
            }
        }

        fn records(&self) -> Vec<Vec<OsString>> {
            self.state.lock().unwrap().records.clone()
        }

        fn socket_path(&self) -> PathBuf {
            self.state.lock().unwrap().socket_path.clone().unwrap()
        }

        fn reap_count(&self) -> usize {
            self.state.lock().unwrap().reaps
        }
    }

    impl SshProcessBackend for FakeBackend {
        type Child = FakeChild;

        fn spawn(
            &self,
            spec: SshCommandSpec,
        ) -> impl Future<Output = io::Result<Self::Child>> + Send {
            async move {
                let arguments = spec.arguments().to_vec();
                let socket_path = argument_after(&arguments, "-S").unwrap();
                let listener = UnixListener::bind(&socket_path)?;
                let mut state = self.state.lock().unwrap();
                state.records.push(arguments);
                state.socket_path = Some(socket_path);
                Ok(FakeChild {
                    listener: Some(listener),
                })
            }
        }

        fn run(
            &self,
            spec: SshCommandSpec,
        ) -> impl Future<Output = io::Result<ProcessExit>> + Send {
            async move {
                let arguments = spec.arguments().to_vec();
                let is_readiness = contains_pair(&arguments, "-O", "check");
                let mut state = self.state.lock().unwrap();
                state.records.push(arguments);
                if is_readiness {
                    Ok(state
                        .readiness
                        .pop_front()
                        .unwrap_or(ProcessExit::unsuccessful(Some(255))))
                } else {
                    Ok(ProcessExit::successful())
                }
            }
        }

        fn try_wait(&self, child: &mut Self::Child) -> io::Result<Option<ProcessExit>> {
            let mut state = self.state.lock().unwrap();
            let exit = state.early_exits.pop_front().flatten();
            if exit.is_some() {
                child.listener.take();
                state.reaps += 1;
            }
            Ok(exit)
        }

        fn terminate_and_reap(&self, child: &mut Self::Child) -> io::Result<()> {
            child.listener.take();
            self.state.lock().unwrap().reaps += 1;
            Ok(())
        }

        fn delay(&self, duration: Duration) -> impl Future<Output = ()> + Send {
            async move {
                let (cancel, should_remain_pending) = {
                    let mut state = self.state.lock().unwrap();
                    state.delays.push(duration);
                    (state.cancel_on_delay.clone(), state.pending_delay)
                };
                if let Some(cancel) = cancel {
                    cancel.cancel();
                }
                if should_remain_pending {
                    pending::<()>().await;
                }
            }
        }
    }

    fn argument_after(arguments: &[OsString], flag: &str) -> Option<PathBuf> {
        arguments
            .windows(2)
            .find_map(|pair| (pair[0] == flag).then(|| PathBuf::from(pair[1].clone())))
    }

    fn contains_pair(arguments: &[OsString], left: &str, right: &str) -> bool {
        arguments
            .windows(2)
            .any(|pair| pair[0] == left && pair[1] == right)
    }

    fn destination() -> SshDestination {
        SshDestination::new("work".to_owned()).unwrap()
    }

    fn timing() -> ControlConnectionTiming {
        ControlConnectionTiming::new(Duration::from_millis(100), Duration::from_millis(50)).unwrap()
    }

    #[gpui::test]
    fn connect_should_own_a_ready_private_control_socket(cx: &mut TestAppContext) {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let backend = Arc::new(FakeBackend::with_readiness([
            ProcessExit::unsuccessful(Some(255)),
            ProcessExit::successful(),
        ]));
        let cancellation = SshCancellationToken::default();

        let connection = cx
            .executor()
            .block(OpenSshControlConnection::connect(
                &paths,
                destination(),
                Arc::clone(&backend),
                &cancellation,
                timing(),
            ))
            .unwrap();

        let mode = fs::metadata(connection.control_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            (connection.state(), mode),
            (ControlConnectionState::Ready, 0o600)
        );
    }

    #[gpui::test]
    fn connect_should_time_out_and_reap_the_master(cx: &mut TestAppContext) {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let backend = Arc::new(FakeBackend::default());
        let cancellation = SshCancellationToken::default();

        let error = cx
            .executor()
            .block(OpenSshControlConnection::connect(
                &paths,
                destination(),
                Arc::clone(&backend),
                &cancellation,
                timing(),
            ))
            .err()
            .unwrap();

        assert!(
            matches!(error, ControlConnectionError::ReadinessTimedOut { .. })
                && backend.reap_count() == 1
        );
    }

    #[gpui::test]
    fn connect_should_report_an_early_master_exit_as_reaped(cx: &mut TestAppContext) {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let backend = Arc::new(FakeBackend::default());
        backend
            .state
            .lock()
            .unwrap()
            .early_exits
            .push_back(Some(ProcessExit::unsuccessful(Some(7))));
        let cancellation = SshCancellationToken::default();

        let error = cx
            .executor()
            .block(OpenSshControlConnection::connect(
                &paths,
                destination(),
                Arc::clone(&backend),
                &cancellation,
                timing(),
            ))
            .err()
            .unwrap();

        assert!(matches!(
            error,
            ControlConnectionError::MasterExited(exit) if exit == ProcessExit::unsuccessful(Some(7))
        ));
    }

    #[gpui::test]
    fn connect_should_cancel_during_readiness_and_cleanup(cx: &mut TestAppContext) {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let backend = Arc::new(FakeBackend::default());
        let cancellation = SshCancellationToken::default();
        backend.state.lock().unwrap().cancel_on_delay = Some(cancellation.clone());

        let error = cx
            .executor()
            .block(OpenSshControlConnection::connect(
                &paths,
                destination(),
                Arc::clone(&backend),
                &cancellation,
                timing(),
            ))
            .err()
            .unwrap();

        assert!(matches!(error, ControlConnectionError::Cancelled) && backend.reap_count() == 1);
    }

    #[gpui::test]
    fn failed_connect_should_remove_only_its_registered_stale_socket(cx: &mut TestAppContext) {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let backend = Arc::new(FakeBackend::default());
        let cancellation = SshCancellationToken::default();
        let unrelated = directory.0.join("unrelated");
        fs::write(&unrelated, b"keep").unwrap();

        let _ = cx.executor().block(OpenSshControlConnection::connect(
            &paths,
            destination(),
            Arc::clone(&backend),
            &cancellation,
            timing(),
        ));

        let socket_path = backend.socket_path();
        assert!(!socket_path.exists() && unrelated.exists());
    }

    #[gpui::test]
    fn shutdown_should_send_one_exact_exit_then_reap_and_cleanup(cx: &mut TestAppContext) {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let backend = Arc::new(FakeBackend::with_readiness([ProcessExit::successful()]));
        let cancellation = SshCancellationToken::default();
        let mut connection = cx
            .executor()
            .block(OpenSshControlConnection::connect(
                &paths,
                destination(),
                Arc::clone(&backend),
                &cancellation,
                timing(),
            ))
            .unwrap();
        let socket_path = connection.control_path().to_path_buf();

        cx.executor().block(connection.shutdown()).unwrap();
        cx.executor().block(connection.shutdown()).unwrap();

        let exit_commands = backend
            .records()
            .iter()
            .filter(|arguments| contains_pair(arguments, "-O", "exit"))
            .count();
        assert!(
            exit_commands == 1
                && backend.reap_count() == 1
                && !socket_path.exists()
                && connection.state() == ControlConnectionState::Closed
        );
    }

    #[test]
    fn dropping_a_pending_connect_future_should_reap_and_cleanup() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let backend = Arc::new(FakeBackend::default());
        backend.state.lock().unwrap().pending_delay = true;
        let cancellation = SshCancellationToken::default();
        let mut future = Box::pin(OpenSshControlConnection::connect(
            &paths,
            destination(),
            Arc::clone(&backend),
            &cancellation,
            timing(),
        ));
        let waker = Waker::from(Arc::new(NoopWake));
        let mut context = Context::from_waker(&waker);

        assert!(matches!(
            Pin::as_mut(&mut future).poll(&mut context),
            Poll::Pending
        ));
        drop(future);

        let socket_path = backend.socket_path();
        assert!(backend.reap_count() == 1 && !socket_path.exists());
    }

    struct NoopWake;

    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    #[test]
    fn timing_should_reject_unbounded_or_zero_polling() {
        assert!(
            ControlConnectionTiming::new(Duration::from_secs(61), Duration::from_millis(10))
                .is_err()
                && ControlConnectionTiming::new(Duration::from_secs(1), Duration::ZERO).is_err()
        );
    }
}
