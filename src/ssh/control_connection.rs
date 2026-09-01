use std::io;
use std::os::unix::net::UnixListener;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use thiserror::Error;

pub(crate) use super::cancellation::SshCancellationToken;
use super::command::{
    PreparedSshPaneChannelCommand, SshCommandContext, SshCommandContextError,
    ValidatedRemoteShellCommand,
};
use super::live_connection::{
    ControlConnectionLifecycleObserver, LiveConnectionAuthority, LiveConnectionBinding,
    LiveConnectionState,
};
use super::process::{
    ProcessExit, ProcessRunError, ProcessSignal, SshProcessBackend, TransientSshErrorOutput,
};
use super::remote_utility::PreparedSshRemoteUtilityCommand;
use crate::domain::SshDestination;
use crate::platform::app_paths::{AppPaths, AppPathsError, RegisteredRuntimeSocket, RuntimeOwner};

const CONTROL_OWNER_KIND: &str = "ssh-control";
const CONTROL_SOCKET_NAME: &str = "master.sock";
#[cfg(test)]
const MAXIMUM_READINESS_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_MASTER_GRACE: Duration = Duration::from_secs(2);
const SHUTDOWN_TERMINATE_GRACE: Duration = Duration::from_secs(1);
const SHUTDOWN_KILL_DEADLINE: Duration = Duration::from_secs(1);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
/// Invalid bounded readiness timing supplied to a control connection.
#[cfg(test)]
pub(crate) enum ControlConnectionTimingError {
    #[error("SSH readiness timeout must be between one nanosecond and 60 seconds")]
    InvalidTimeout,
    #[error("SSH readiness polling interval must be nonzero and no longer than the timeout")]
    InvalidPollInterval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Readiness polling policy for one control-master launch.
///
/// Production may leave the overall timeout unset so OpenSSH's configured connection timeout
/// remains authoritative; every individual readiness command is still bounded.
pub(crate) struct ControlConnectionTiming {
    timeout: Option<Duration>,
    poll_interval: Duration,
    readiness_check_timeout: Duration,
}

impl ControlConnectionTiming {
    #[cfg(test)]
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
            timeout: Some(timeout),
            poll_interval,
            readiness_check_timeout: timeout,
        })
    }
}

impl Default for ControlConnectionTiming {
    fn default() -> Self {
        Self {
            timeout: None,
            poll_interval: Duration::from_millis(100),
            readiness_check_timeout: Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Observable ownership state of one control master.
pub(crate) enum ControlConnectionState {
    Ready,
    ShuttingDown,
    Closed,
    Failed,
}

#[derive(Debug, Error)]
/// Typed control-master failure with raw process output excluded.
///
/// The sole diagnostic payload is an optional bounded, control-free, non-Debug tail suitable for
/// the active failure alert. Authentication prompts and secrets are never carried by this error.
pub(crate) enum ControlConnectionError {
    #[error("SSH control connection was cancelled")]
    Cancelled,
    #[error("SSH control master did not become ready within {timeout:?}")]
    ReadinessTimedOut { timeout: Duration },
    #[error("SSH control master exited before becoming ready: {exit:?}")]
    MasterExited {
        exit: ProcessExit,
        error_output: Option<TransientSshErrorOutput>,
    },
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
        source: ProcessRunError,
    },
    #[error("SSH shutdown command could not be run: {source}")]
    ShutdownCommand {
        #[source]
        source: ProcessRunError,
    },
    #[error("SSH control master rejected graceful shutdown: {0:?}")]
    ShutdownRejected(ProcessExit),
    #[error("SSH control master could not be reaped: {source}")]
    Reap {
        #[source]
        source: io::Error,
    },
    #[error("SSH control master did not terminate before its cleanup deadline")]
    MasterTerminationTimedOut,
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
    #[error("SSH control master supervisor could not be started: {source}")]
    StartSupervisor {
        #[source]
        source: io::Error,
    },
    #[error("SSH control connection is not ready")]
    NotReady,
}

/// Singular owner of one workspace-scoped OpenSSH control master.
///
/// The connection owns the child process group, private runtime directory, registered socket,
/// live authority, and supervisor. It is intentionally non-clone. Child commands are authorized
/// only while the exact socket identity and live connection instance and generation remain ready.
/// Drop performs exact owned-process cleanup and removes only registered runtime artifacts.
pub(crate) struct OpenSshControlConnection<B: SshProcessBackend> {
    backend: Arc<B>,
    commands: SshCommandContext,
    child: Arc<Mutex<Option<B::Child>>>,
    runtime_owner: Option<RuntimeOwner>,
    authority: Option<Arc<LiveConnectionAuthority>>,
    supervisor_stop: Arc<AtomicBool>,
    supervisor: Option<JoinHandle<()>>,
    #[cfg(test)]
    control_path: PathBuf,
}

impl<B: SshProcessBackend> OpenSshControlConnection<B> {
    /// Creates a content-free observer for the first terminal `Failed` or `Closed` transition.
    pub(crate) fn lifecycle_observer(
        &self,
    ) -> Result<ControlConnectionLifecycleObserver, ControlConnectionError> {
        self.authority
            .as_ref()
            .map(|authority| authority.observe_lifecycle())
            .ok_or(ControlConnectionError::Ownership)
    }

    /// Launches and supervises a fresh master using a bounded private control socket.
    ///
    /// Cancellation retains no child, socket, or runtime owner. Readiness never falls back to a
    /// direct SSH connection when the registered control socket is unavailable.
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
        reserve_socket(&runtime_owner)?;
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
            registered_socket: None,
        };
        let deadline = timing
            .timeout
            .map(|timeout| deadline_after(backend.now(), timeout));

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
                let error_output = backend.take_error_output(child);
                launch.child.take();
                return Err(ControlConnectionError::MasterExited { exit, error_output });
            }
            if deadline.is_some_and(|deadline| backend.now() >= deadline) {
                return Err(ControlConnectionError::ReadinessTimedOut {
                    timeout: timing.timeout.unwrap_or_default(),
                });
            }
            let now = backend.now();
            let readiness_deadline =
                deadline.unwrap_or_else(|| deadline_after(now, timing.readiness_check_timeout));
            let readiness = match backend
                .run(
                    commands.readiness_check(),
                    cancellation.clone(),
                    readiness_deadline,
                )
                .await
            {
                Ok(readiness) => readiness,
                Err(ProcessRunError::Cancelled) => return Err(ControlConnectionError::Cancelled),
                Err(ProcessRunError::TimedOut) if timing.timeout.is_some() => {
                    return Err(ControlConnectionError::ReadinessTimedOut {
                        timeout: timing.timeout.unwrap_or_default(),
                    });
                }
                Err(ProcessRunError::TimedOut) => ProcessExit::unsuccessful(None),
                Err(source) => return Err(ControlConnectionError::Readiness { source }),
            };
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
            let now = backend.now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                return Err(ControlConnectionError::ReadinessTimedOut {
                    timeout: timing.timeout.unwrap_or_default(),
                });
            }
            let delay = deadline.map_or(timing.poll_interval, |deadline| {
                timing.poll_interval.min(deadline.duration_since(now))
            });
            backend.delay(delay).await;
        }
    }

    /// Returns the current state without transferring live authority.
    pub(crate) fn state(&self) -> ControlConnectionState {
        match self.authority.as_ref().map(|authority| authority.state()) {
            Some(LiveConnectionState::Ready) => ControlConnectionState::Ready,
            Some(LiveConnectionState::ShuttingDown) => ControlConnectionState::ShuttingDown,
            Some(LiveConnectionState::Failed) => ControlConnectionState::Failed,
            Some(LiveConnectionState::Closed) | None => ControlConnectionState::Closed,
        }
    }

    /// Borrows the bounded private socket path owned by this connection.
    #[cfg(test)]
    pub(crate) fn control_path(&self) -> &Path {
        &self.control_path
    }

    /// Prepares a no-TTY utility command bound to the current live authority.
    pub(crate) fn remote_utility_command(
        &self,
    ) -> Result<PreparedSshRemoteUtilityCommand, ControlConnectionError> {
        if self.state() != ControlConnectionState::Ready {
            return Err(ControlConnectionError::NotReady);
        }
        let capability = self
            .authority
            .as_ref()
            .ok_or(ControlConnectionError::Ownership)?
            .capability();
        capability
            .authorize()
            .map_err(|_| ControlConnectionError::NotReady)?;
        Ok(PreparedSshRemoteUtilityCommand::new_live(
            self.commands.remote_utility(),
            capability,
        ))
    }

    /// Prepares a pane command bound to the current live authority and sanitized environment.
    pub(crate) fn prepare_pane_channel(
        &self,
        command: ValidatedRemoteShellCommand,
    ) -> Result<PreparedSshPaneChannelCommand, ControlConnectionError> {
        if self.state() != ControlConnectionState::Ready {
            return Err(ControlConnectionError::NotReady);
        }
        let capability = self
            .authority
            .as_ref()
            .ok_or(ControlConnectionError::Ownership)?
            .capability();
        capability
            .authorize()
            .map_err(|_| ControlConnectionError::NotReady)?;
        Ok(PreparedSshPaneChannelCommand::new(
            self.commands.pane_channel(command),
            Some(capability),
            Some(self.backend.environment().clone()),
        ))
    }

    /// Returns the opaque connection-instance and generation binding currently authorized.
    pub(crate) fn live_binding(&self) -> Result<LiveConnectionBinding, ControlConnectionError> {
        if self.state() != ControlConnectionState::Ready {
            return Err(ControlConnectionError::NotReady);
        }
        let capability = self
            .authority
            .as_ref()
            .ok_or(ControlConnectionError::Ownership)?
            .capability();
        capability
            .binding()
            .map_err(|_| ControlConnectionError::NotReady)
    }

    /// Shuts down exactly once, with bounded graceful, terminate, and kill phases.
    ///
    /// Ownership is released only after the process group is reaped and private runtime cleanup
    /// succeeds. A graceful-command failure restores ready supervision so the caller can retry.
    pub(crate) async fn shutdown(&mut self) -> Result<(), ControlConnectionError> {
        if self.state() == ControlConnectionState::Closed {
            return Ok(());
        }

        if self.state() == ControlConnectionState::Ready {
            self.transition(LiveConnectionState::ShuttingDown)?;
            self.stop_supervisor();
            let deadline = deadline_after(self.backend.now(), SHUTDOWN_OPERATION_TIMEOUT);
            match self
                .backend
                .run(
                    self.commands.graceful_exit(),
                    SshCancellationToken::default(),
                    deadline,
                )
                .await
            {
                Ok(exit) if exit.is_success() => {}
                Ok(exit) => {
                    self.restore_ready_supervision()?;
                    return Err(ControlConnectionError::ShutdownRejected(exit));
                }
                Err(source) => {
                    self.restore_ready_supervision()?;
                    return Err(ControlConnectionError::ShutdownCommand { source });
                }
            }
        }

        if !self
            .wait_for_master_exit(deadline_after(self.backend.now(), SHUTDOWN_MASTER_GRACE))
            .await?
        {
            self.signal_master(ProcessSignal::Terminate)?;
            if !self
                .wait_for_master_exit(deadline_after(self.backend.now(), SHUTDOWN_TERMINATE_GRACE))
                .await?
            {
                self.signal_master(ProcessSignal::Kill)?;
                if !self
                    .wait_for_master_exit(deadline_after(
                        self.backend.now(),
                        SHUTDOWN_KILL_DEADLINE,
                    ))
                    .await?
                {
                    return Err(ControlConnectionError::MasterTerminationTimedOut);
                }
            }
        }
        self.transition(LiveConnectionState::Closed)?;
        self.authority.take();
        if let Some(owner) = self.runtime_owner.take() {
            owner.close().map_err(ControlConnectionError::Cleanup)?;
        }
        Ok(())
    }

    async fn wait_for_master_exit(
        &mut self,
        deadline: Instant,
    ) -> Result<bool, ControlConnectionError> {
        loop {
            let exited = {
                let mut child_slot = self
                    .child
                    .lock()
                    .map_err(|_| ControlConnectionError::Ownership)?;
                let Some(child) = child_slot.as_mut() else {
                    return Ok(true);
                };
                if self
                    .backend
                    .try_wait(child)
                    .map_err(|source| ControlConnectionError::Reap { source })?
                    .is_some()
                {
                    child_slot.take();
                    true
                } else {
                    false
                }
            };
            if exited {
                return Ok(true);
            }
            let now = self.backend.now();
            if now >= deadline {
                return Ok(false);
            }
            self.backend
                .delay(PROCESS_POLL_INTERVAL.min(deadline.duration_since(now)))
                .await;
        }
    }

    fn signal_master(&mut self, signal: ProcessSignal) -> Result<(), ControlConnectionError> {
        let mut child_slot = self
            .child
            .lock()
            .map_err(|_| ControlConnectionError::Ownership)?;
        let child = child_slot
            .as_mut()
            .ok_or(ControlConnectionError::Ownership)?;
        self.backend
            .signal_process_group(child, signal)
            .map_err(|source| ControlConnectionError::Reap { source })
    }

    fn transition(&self, state: LiveConnectionState) -> Result<(), ControlConnectionError> {
        self.authority
            .as_ref()
            .ok_or(ControlConnectionError::Ownership)?
            .transition(state);
        Ok(())
    }

    fn restore_ready_supervision(&mut self) -> Result<(), ControlConnectionError> {
        self.transition(LiveConnectionState::Ready)?;
        self.start_supervisor()
    }

    fn start_supervisor(&mut self) -> Result<(), ControlConnectionError> {
        let authority = self
            .authority
            .as_ref()
            .ok_or(ControlConnectionError::Ownership)?;
        let stop = Arc::new(AtomicBool::new(false));
        let supervisor = spawn_supervisor(
            Arc::clone(&self.backend),
            Arc::clone(&self.child),
            Arc::clone(authority),
            Arc::clone(&stop),
        )?;
        self.supervisor_stop = stop;
        self.supervisor = Some(supervisor);
        Ok(())
    }

    fn stop_supervisor(&mut self) {
        self.supervisor_stop.store(true, Ordering::Release);
        if let Some(supervisor) = self.supervisor.take() {
            let _ = supervisor.join();
        }
    }
}

impl<B: SshProcessBackend> Drop for OpenSshControlConnection<B> {
    fn drop(&mut self) {
        self.stop_supervisor();
        let child = self.child.lock().ok().and_then(|mut child| child.take());
        if let Some(child) = child {
            self.backend.force_cleanup(child);
        }
        if let Some(authority) = self.authority.take() {
            authority.transition(LiveConnectionState::Closed);
        }
        self.runtime_owner.take();
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
        let authority = LiveConnectionAuthority::new(registered_socket);
        let child = Arc::new(Mutex::new(Some(child)));
        let supervisor_stop = Arc::new(AtomicBool::new(false));
        let supervisor = match spawn_supervisor(
            Arc::clone(&self.backend),
            Arc::clone(&child),
            Arc::clone(&authority),
            Arc::clone(&supervisor_stop),
        ) {
            Ok(supervisor) => supervisor,
            Err(error) => {
                if let Ok(mut child) = child.lock()
                    && let Some(child) = child.take()
                {
                    self.backend.force_cleanup(child);
                }
                return Err(error);
            }
        };
        #[cfg(not(test))]
        drop(control_path);
        Ok(OpenSshControlConnection {
            backend: Arc::clone(&self.backend),
            commands,
            child,
            runtime_owner: Some(runtime_owner),
            authority: Some(authority),
            supervisor_stop,
            supervisor: Some(supervisor),
            #[cfg(test)]
            control_path,
        })
    }
}

impl<B: SshProcessBackend> Drop for ConnectingControl<B> {
    fn drop(&mut self) {
        if self.registered_socket.is_none()
            && let Some(owner) = self.runtime_owner.as_ref()
            && let Ok(socket) = owner.register_socket(CONTROL_SOCKET_NAME)
        {
            self.registered_socket = Some(socket);
        }
        if let Some(child) = self.child.take() {
            self.backend.force_cleanup(child);
        }
        self.registered_socket.take();
        self.runtime_owner.take();
    }
}

fn deadline_after(now: Instant, duration: Duration) -> Instant {
    now.checked_add(duration).unwrap_or(now)
}

fn spawn_supervisor<B: SshProcessBackend>(
    backend: Arc<B>,
    child: Arc<Mutex<Option<B::Child>>>,
    authority: Arc<LiveConnectionAuthority>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, ControlConnectionError> {
    std::thread::Builder::new()
        .name("spaceterm-ssh-supervisor".to_owned())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                std::thread::sleep(PROCESS_POLL_INTERVAL);
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let result = child.lock().map_or_else(
                    |_| Err(io::Error::other("SSH master ownership lock was poisoned")),
                    |mut child| match child.as_mut() {
                        Some(process) => backend.try_wait(process).inspect(|exit| {
                            if exit.is_some() {
                                child.take();
                            }
                        }),
                        None => Ok(Some(ProcessExit::unsuccessful(None))),
                    },
                );
                match result {
                    Ok(None) => {}
                    Ok(Some(_)) | Err(_) => {
                        authority.transition(LiveConnectionState::Failed);
                        return;
                    }
                }
            }
        })
        .map_err(|source| ControlConnectionError::StartSupervisor { source })
}

fn reserve_socket(owner: &RuntimeOwner) -> Result<(), ControlConnectionError> {
    let path = owner.socket_path(CONTROL_SOCKET_NAME)?;
    let listener = UnixListener::bind(&path)
        .map_err(|source| ControlConnectionError::SocketReservation { source })?;
    let registered = owner.register_socket(CONTROL_SOCKET_NAME)?;
    drop(listener);
    owner.remove_registered_socket(registered)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::fs;
    use std::future::pending;
    use std::io;
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
    use crate::platform::app_paths::{AppPathEnvironment, AppPaths};
    use crate::ssh::command::SshCommandSpec;
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
                environment:
                    super::super::process::SshProcessEnvironment::new_without_authentication(
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
                environment:
                    super::super::process::SshProcessEnvironment::new_without_authentication(
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

        async fn spawn(&self, spec: SshCommandSpec) -> io::Result<Self::Child> {
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

        fn try_wait(&self, child: &mut Self::Child) -> io::Result<Option<ProcessExit>> {
            let mut state = self.state.lock().unwrap();
            let exit = state.early_exits.pop_front().flatten();
            if exit.is_some() {
                child.listener.take();
                state.reaps += 1;
            }
            Ok(exit)
        }

        fn signal_process_group(
            &self,
            _child: &mut Self::Child,
            signal: ProcessSignal,
        ) -> io::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.signals.push(signal);
            if signal == state.exit_on_signal {
                state.early_exits.push_back(Some(ProcessExit::successful()));
            }
            Ok(())
        }

        fn force_cleanup(&self, mut child: Self::Child) {
            child.listener.take();
            self.state.lock().unwrap().reaps += 1;
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
                destination(),
                Arc::new(FakeBackend::with_readiness([ProcessExit::successful()])),
                &cancellation,
                timing(),
            ))
            .unwrap();

        assert!(first.live_binding().unwrap() != second.live_binding().unwrap());
    }

    #[gpui::test]
    fn ready_connection_should_prepare_utility_and_single_use_pane_commands(
        cx: &mut TestAppContext,
    ) {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let backend = Arc::new(FakeBackend::with_readiness([ProcessExit::successful()]));
        let cancellation = SshCancellationToken::default();
        let connection = cx
            .executor()
            .block(OpenSshControlConnection::connect(
                &paths,
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

    #[gpui::test]
    fn hanging_readiness_check_should_obey_the_wall_clock_deadline_and_reap(
        cx: &mut TestAppContext,
    ) {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let backend = Arc::new(FakeBackend::default());
        backend.state.lock().unwrap().hang_readiness = true;

        let error = cx
            .executor()
            .block(OpenSshControlConnection::connect(
                &paths,
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

    #[test]
    fn production_timing_should_not_impose_a_connection_deadline() {
        assert!(ControlConnectionTiming::default().timeout.is_none());
    }
}
