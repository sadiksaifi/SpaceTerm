//! Real private socket, permissions, replacement identity, and cleanup integration.
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::future::pending;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

use gpui::TestAppContext;

use super::*;
use crate::domain::SshDestination;
use crate::platform::app_paths::{AppPathEnvironment, AppPathHostFacts, AppPaths};
use crate::platform::macos_control_socket::MacosControlSocketProbe;
use crate::platform::macos_secure_filesystem::MacosSecureFilesystem;
use crate::ssh::command::{OpenSshExecutable, SshCommandSpec};
use crate::ssh::process::{ProcessExit, ProcessRunError, ProcessSignal, SshProcessBackend};

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!(
            "/private/tmp/stc-{}-{sequence}",
            std::process::id()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }

    fn paths(&self) -> AppPaths {
        let environment = AppPathEnvironment {
            home: None,
            xdg_config_home: Some(self.0.join("config").into_os_string()),
            xdg_data_home: Some(self.0.join("data").into_os_string()),
            xdg_state_home: Some(self.0.join("state").into_os_string()),
            xdg_cache_home: Some(self.0.join("cache").into_os_string()),
            xdg_runtime_dir: Some(self.0.join("runtime").into_os_string()),
        };
        let host = AppPathHostFacts::new(self.0.join("temporary"), 103).unwrap();
        AppPaths::resolve(&environment, &host, Arc::new(MacosSecureFilesystem)).unwrap()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct FakeChild {
    listener: Option<UnixListener>,
    socket_path: PathBuf,
    create_socket_during_cleanup: bool,
    reaped: bool,
}

struct FakeState {
    readiness: VecDeque<ProcessExit>,
    early_exits: VecDeque<Option<ProcessExit>>,
    records: Vec<Vec<OsString>>,
    delays: Vec<Duration>,
    socket_path: Option<PathBuf>,
    reaps: usize,
    cancel_on_delay: Option<SshCancellationToken>,
    pending_delay: bool,
    elapsed: Duration,
    hang_readiness: bool,
    hang_shutdown: bool,
    exit_after_shutdown: bool,
    exit_on_signal: ProcessSignal,
    signals: Vec<ProcessSignal>,
    master_error_output: Option<TransientSshErrorOutput>,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            readiness: VecDeque::new(),
            early_exits: VecDeque::new(),
            records: Vec::new(),
            delays: Vec::new(),
            socket_path: None,
            reaps: 0,
            cancel_on_delay: None,
            pending_delay: false,
            elapsed: Duration::ZERO,
            hang_readiness: false,
            hang_shutdown: false,
            exit_after_shutdown: true,
            exit_on_signal: ProcessSignal::Terminate,
            signals: Vec::new(),
            master_error_output: None,
        }
    }
}

struct FakeBackend {
    epoch: Instant,
    state: Mutex<FakeState>,
    environment: super::super::process::SshProcessEnvironment,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            state: Mutex::new(FakeState::default()),
            environment: super::super::process::SshProcessEnvironment::new_without_authentication(
                PathBuf::from("/private/tmp"),
                None,
            )
            .unwrap(),
        }
    }
}

impl FakeBackend {
    fn with_readiness(readiness: impl IntoIterator<Item = ProcessExit>) -> Self {
        Self {
            epoch: Instant::now(),
            state: Mutex::new(FakeState {
                readiness: readiness.into_iter().collect(),
                ..FakeState::default()
            }),
            environment: super::super::process::SshProcessEnvironment::new_without_authentication(
                PathBuf::from("/private/tmp"),
                None,
            )
            .unwrap(),
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

    fn environment(&self) -> &super::super::process::SshProcessEnvironment {
        &self.environment
    }

    fn now(&self) -> Instant {
        self.epoch + self.state.lock().unwrap().elapsed
    }

    async fn spawn(&self, spec: SshCommandSpec) -> Result<Self::Child, SshProcessMechanismError> {
        let arguments = spec.arguments().to_vec();
        let socket_path = argument_after(&arguments, "-S").unwrap();
        let listener =
            UnixListener::bind(&socket_path).map_err(|_| SshProcessMechanismError::LaunchFailed)?;
        let mut state = self.state.lock().unwrap();
        state.records.push(arguments);
        state.socket_path = Some(socket_path);
        Ok(FakeChild {
            listener: Some(listener),
            socket_path: state.socket_path.clone().unwrap(),
            create_socket_during_cleanup: false,
            reaped: false,
        })
    }

    async fn run(
        &self,
        spec: SshCommandSpec,
        cancellation: SshCancellationToken,
        deadline: Instant,
    ) -> Result<ProcessExit, ProcessRunError> {
        let arguments = spec.arguments().to_vec();
        let is_readiness = contains_pair(&arguments, "-O", "check");
        let is_shutdown = contains_pair(&arguments, "-O", "exit");
        let should_hang = {
            let mut state = self.state.lock().unwrap();
            state.records.push(arguments);
            (is_readiness && state.hang_readiness) || (is_shutdown && state.hang_shutdown)
        };
        if should_hang {
            loop {
                if cancellation.is_cancelled() {
                    return Err(ProcessRunError::Cancelled);
                }
                let now = self.now();
                if now >= deadline {
                    return Err(ProcessRunError::TimedOut);
                }
                self.delay(PROCESS_POLL_INTERVAL.min(deadline.duration_since(now)))
                    .await;
            }
        }
        let mut state = self.state.lock().unwrap();
        if is_readiness {
            Ok(state
                .readiness
                .pop_front()
                .unwrap_or(ProcessExit::unsuccessful(Some(255))))
        } else {
            if is_shutdown && state.exit_after_shutdown {
                state.early_exits.push_back(Some(ProcessExit::successful()));
            }
            Ok(ProcessExit::successful())
        }
    }

    fn try_wait(
        &self,
        child: &mut Self::Child,
    ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
        let mut state = self.state.lock().unwrap();
        let exit = state.early_exits.pop_front().flatten();
        if exit.is_some() {
            child.listener.take();
            if !child.reaped {
                child.reaped = true;
                state.reaps += 1;
            }
        }
        Ok(exit)
    }

    fn signal_process_group(
        &self,
        _child: &mut Self::Child,
        signal: ProcessSignal,
    ) -> Result<(), SshProcessMechanismError> {
        let mut state = self.state.lock().unwrap();
        state.signals.push(signal);
        if signal == state.exit_on_signal {
            state.early_exits.push_back(Some(ProcessExit::successful()));
        }
        Ok(())
    }

    fn begin_cleanup(
        &self,
        mut child: Self::Child,
        after: Option<ProcessCleanupCallback>,
    ) -> SshProcessCleanup {
        child.listener.take();
        if !child.reaped {
            child.reaped = true;
            self.state.lock().unwrap().reaps += 1;
        }
        if child.create_socket_during_cleanup {
            let late_socket = UnixListener::bind(&child.socket_path).unwrap();
            drop(late_socket);
        }
        if let Some(after) = after {
            after();
        }
        SshProcessCleanup::completed()
    }

    fn take_error_output(&self, _child: &mut Self::Child) -> Option<TransientSshErrorOutput> {
        self.state.lock().unwrap().master_error_output.take()
    }

    async fn delay(&self, duration: Duration) {
        let (cancel, should_remain_pending) = {
            let mut state = self.state.lock().unwrap();
            state.delays.push(duration);
            state.elapsed += duration;
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
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
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
fn separate_ready_connections_should_have_distinct_initial_bindings(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let cancellation = SshCancellationToken::default();
    let first = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            Arc::new(FakeBackend::with_readiness([ProcessExit::successful()])),
            &cancellation,
            timing(),
        ))
        .unwrap();
    let second = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            Arc::new(FakeBackend::with_readiness([ProcessExit::successful()])),
            &cancellation,
            timing(),
        ))
        .unwrap();

    assert!(first.live_binding().unwrap() != second.live_binding().unwrap());
}

#[gpui::test]
fn ready_connection_should_prepare_utility_and_single_use_pane_commands(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::with_readiness([ProcessExit::successful()]));
    let cancellation = SshCancellationToken::default();
    let connection = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            backend,
            &cancellation,
            timing(),
        ))
        .unwrap();

    assert!(connection.remote_utility_command().is_ok());
    let pane = connection
        .prepare_pane_channel(
            ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
        )
        .unwrap();
    assert!(pane.take().is_ok());

    connection
        .authority
        .as_ref()
        .unwrap()
        .transition(LiveConnectionState::ShuttingDown);
    assert!(matches!(
        connection.remote_utility_command(),
        Err(ControlConnectionError::NotReady)
    ));
    assert!(matches!(
        connection.prepare_pane_channel(
            ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap()
        ),
        Err(ControlConnectionError::NotReady)
    ));
}

#[gpui::test]
fn shell_launch_should_preserve_prepared_environment_and_reject_revoked_channel(
    cx: &mut TestAppContext,
) {
    use crate::platform::shell_launch::{PreparedShellLaunch, ShellLaunchFailure};
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::with_readiness([ProcessExit::successful()]));
    let connection = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            backend,
            &SshCancellationToken::default(),
            timing(),
        ))
        .unwrap();
    let prepare = || {
        connection
            .prepare_pane_channel(
                ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
            )
            .unwrap()
    };
    let channel = prepare();
    let launch = PreparedShellLaunch::remote(Path::new("/tmp"), channel.take().unwrap()).unwrap();
    assert!(!launch.inherit_environment());
    assert_eq!(launch.working_directory(), Path::new("/private/tmp"));
    assert!(launch.environment_removals().is_empty());
    assert_eq!(
        launch.environment(),
        &[
            (OsString::from("HOME"), OsString::from("/private/tmp")),
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (OsString::from("TERM"), OsString::from("xterm-256color")),
        ]
    );
    assert!(channel.take().is_err());
    let command = prepare().take().unwrap();
    connection
        .authority
        .as_ref()
        .unwrap()
        .transition(LiveConnectionState::ShuttingDown);
    let error = PreparedShellLaunch::remote(Path::new("/tmp"), command).unwrap_err();
    assert_eq!(error, ShellLaunchFailure::RemoteChannelUnavailable);
    assert!(!format!("{launch:?} {error:?} {error}").contains("/private/tmp"));
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
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            Arc::clone(&backend),
            &cancellation,
            timing(),
        ))
        .err()
        .unwrap();

    assert!(
        matches!(error, ControlConnectionError::ReadinessTimedOut { .. })
            && backend.reap_count() == 1,
        "error={error:?}, reaps={}",
        backend.reap_count()
    );
}

#[gpui::test]
fn connect_should_report_an_early_master_exit_as_reaped(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::default());
    {
        let mut state = backend.state.lock().unwrap();
        state
            .early_exits
            .push_back(Some(ProcessExit::unsuccessful(Some(7))));
        state.master_error_output =
            TransientSshErrorOutput::from_untrusted_bytes(b"bad\x1b config");
    }
    let cancellation = SshCancellationToken::default();

    let error = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            Arc::clone(&backend),
            &cancellation,
            timing(),
        ))
        .err()
        .unwrap();

    assert!(matches!(
        &error,
        ControlConnectionError::MasterExited { exit, error_output: Some(output) }
            if *exit == ProcessExit::unsuccessful(Some(7))
                && output.as_str() == "bad  config"
                && !format!("{error:?}").contains("bad")
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
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
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
fn failed_connect_should_preserve_its_unregistered_socket(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::default());
    let cancellation = SshCancellationToken::default();
    let unrelated = directory.0.join("unrelated");
    fs::write(&unrelated, b"keep").unwrap();

    let _ = cx.executor().block(OpenSshControlConnection::connect(
        &paths,
        OpenSshExecutable::for_test(),
        &MacosControlSocketProbe,
        destination(),
        Arc::clone(&backend),
        &cancellation,
        timing(),
    ));

    let socket_path = backend.socket_path();
    assert!(socket_path.exists() && unrelated.exists());
}

#[test]
fn dropped_connecting_control_should_preserve_an_unregistered_replacement_socket() {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let runtime_owner = paths
        .create_runtime_owner(CONTROL_RUNTIME_OWNER_KIND)
        .unwrap();
    let socket_path = runtime_owner
        .socket_path(CONTROL_RUNTIME_SOCKET_NAME)
        .unwrap();
    let backend = Arc::new(FakeBackend::default());
    let replacement = UnixListener::bind(&socket_path).unwrap();
    let launch = ConnectingControl {
        backend: Arc::clone(&backend),
        child: Some(FakeChild {
            listener: None,
            socket_path: socket_path.clone(),
            create_socket_during_cleanup: false,
            reaped: false,
        }),
        runtime_owner: Some(runtime_owner),
        registered_socket: None,
    };

    drop(launch);

    assert!(socket_path.exists() && backend.reap_count() == 1);
    drop(replacement);
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
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
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

#[gpui::test]
fn hanging_readiness_check_should_obey_the_wall_clock_deadline_and_reap(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::default());
    backend.state.lock().unwrap().hang_readiness = true;

    let error = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            Arc::clone(&backend),
            &SshCancellationToken::default(),
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
fn hanging_exit_command_should_retain_ready_master_ownership(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::with_readiness([ProcessExit::successful()]));
    let mut connection = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            Arc::clone(&backend),
            &SshCancellationToken::default(),
            timing(),
        ))
        .unwrap();
    backend.state.lock().unwrap().hang_shutdown = true;

    let error = cx.executor().block(connection.shutdown()).unwrap_err();

    assert!(
        matches!(
            error,
            ControlConnectionError::ShutdownCommand {
                source: ProcessRunError::TimedOut
            }
        ) && connection.state() == ControlConnectionState::Ready
            && connection.control_path().exists()
            && backend.reap_count() == 0
    );
}

#[gpui::test]
fn shutdown_should_grace_then_terminate_then_force_the_owned_group(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::with_readiness([ProcessExit::successful()]));
    let mut connection = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            Arc::clone(&backend),
            &SshCancellationToken::default(),
            timing(),
        ))
        .unwrap();
    {
        let mut state = backend.state.lock().unwrap();
        state.exit_after_shutdown = false;
        state.exit_on_signal = ProcessSignal::Kill;
    }

    cx.executor().block(connection.shutdown()).unwrap();

    assert_eq!(
        backend.state.lock().unwrap().signals,
        vec![ProcessSignal::Terminate, ProcessSignal::Kill]
    );
}

#[gpui::test]
fn master_death_should_invalidate_stale_pane_and_utility_commands(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::with_readiness([ProcessExit::successful()]));
    let connection = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            Arc::clone(&backend),
            &SshCancellationToken::default(),
            timing(),
        ))
        .unwrap();
    let pane = connection
        .prepare_pane_channel(
            ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
        )
        .unwrap();
    let utility = connection.remote_utility_command().unwrap();
    let lifecycle = connection.lifecycle_observer().unwrap();
    backend
        .state
        .lock()
        .unwrap()
        .early_exits
        .push_back(Some(ProcessExit::unsuccessful(Some(9))));

    for _ in 0..100 {
        if connection.state() == ControlConnectionState::Failed {
            break;
        }
        std::thread::sleep(PROCESS_POLL_INTERVAL);
    }

    assert_eq!(connection.state(), ControlConnectionState::Failed);
    assert_eq!(
        cx.executor().block(lifecycle.terminal()),
        crate::ssh::live_connection::ControlConnectionTerminalState::Failed
    );
    assert!(matches!(
        pane.take(),
        Err(crate::ssh::command::PreparedSshPaneChannelError::Unavailable)
    ));
    assert!(
        utility
            .connection_cancellation()
            .is_some_and(|cancellation| cancellation.is_cancelled())
    );
}

#[gpui::test]
fn dropping_a_ready_connection_should_publish_closed_once(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::with_readiness([ProcessExit::successful()]));
    let connection = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            backend,
            &SshCancellationToken::default(),
            timing(),
        ))
        .unwrap();
    let lifecycle = connection.lifecycle_observer().unwrap();

    drop(connection);

    assert_eq!(
        cx.executor().block(lifecycle.terminal()),
        crate::ssh::live_connection::ControlConnectionTerminalState::Closed
    );
}

#[gpui::test]
fn socket_replacement_should_block_command_use_and_never_be_unlinked(cx: &mut TestAppContext) {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::with_readiness([ProcessExit::successful()]));
    let connection = cx
        .executor()
        .block(OpenSshControlConnection::connect(
            &paths,
            OpenSshExecutable::for_test(),
            &MacosControlSocketProbe,
            destination(),
            Arc::clone(&backend),
            &SshCancellationToken::default(),
            timing(),
        ))
        .unwrap();
    let pane = connection
        .prepare_pane_channel(
            ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
        )
        .unwrap();
    let command = pane.take().unwrap();
    let socket_path = connection.control_path().to_path_buf();
    connection
        .child
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .listener
        .take();
    fs::remove_file(&socket_path).unwrap();
    let replacement = UnixListener::bind(&socket_path).unwrap();

    assert!(matches!(
        command.into_pane_launch_parts(),
        Err(crate::ssh::command::PreparedSshPaneChannelError::Unavailable)
    ));
    drop(connection);
    assert!(socket_path.exists());

    drop(replacement);
    fs::remove_file(socket_path).unwrap();
}

#[test]
fn dropping_a_pending_connect_future_should_reap_and_preserve_unregistered_socket() {
    let directory = TestDirectory::new();
    let paths = directory.paths();
    let backend = Arc::new(FakeBackend::default());
    backend.state.lock().unwrap().pending_delay = true;
    let cancellation = SshCancellationToken::default();
    let mut future = Box::pin(OpenSshControlConnection::connect(
        &paths,
        OpenSshExecutable::for_test(),
        &MacosControlSocketProbe,
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
    assert!(backend.reap_count() == 1 && socket_path.exists());
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

#[test]
fn timing_should_reject_unbounded_or_zero_polling() {
    assert!(
        ControlConnectionTiming::new(Duration::from_secs(61), Duration::from_millis(10)).is_err()
            && ControlConnectionTiming::new(Duration::from_secs(1), Duration::ZERO).is_err()
    );
}

#[test]
fn production_timing_should_not_impose_a_connection_deadline() {
    assert!(ControlConnectionTiming::default().timeout.is_none());
}
