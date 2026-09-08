use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use gpui::{App, BackgroundExecutor, Task, Window};

use super::remote_workspace_flow::{
    RemoteWorkspaceAliasPin, RemoteWorkspaceAliasPinError, RemoteWorkspaceConnectContext,
    RemoteWorkspaceConnectedSession, RemoteWorkspaceConnectionProgress, RemoteWorkspaceFlowBackend,
    RemoteWorkspaceFlowBackendError, RemoteWorkspaceFlowBackendFactory,
    RemoteWorkspaceSessionOwner,
};
use super::remote_workspace_picker::RemoteWorkspaceProvider;
use super::ssh_host_form::ManagedHostFormBackendError;
use crate::domain::{RemoteDirectoryIdentity, RemoteWorkspaceDirectory, SshDestination};
use crate::platform::app_paths::AppPaths;
use crate::platform::askpass::{
    AskPassAttemptFactory, AskPassAttemptObservation, AskPassBrokerLease, AskPassWindowFactory,
};
use crate::platform::control_socket::ControlSocketProbe;
use crate::ssh::alias_usage::{ActiveSshAliasLease, ActiveSshAliasRegistry};
use crate::ssh::cancellation::SshCancellationToken;
use crate::ssh::command::{
    OpenSshExecutable, RemotePaneShellCommandBuilder, SshCapability, ValidatedRemoteLoginShell,
};
use crate::ssh::control_connection::{
    ControlConnectionError, ControlConnectionState, ControlConnectionTiming,
    OpenSshControlConnection,
};
use crate::ssh::destination::{SshHostAlias, resolve_destination_query};
use crate::ssh::host_config::{
    HostConfigFilesystem, HostConfigRoots, HostDiscovery, HostDiscoveryLimits, discover_ssh_hosts,
};
use crate::ssh::live_connection::{ControlConnectionObserver, LiveConnectionBinding};
use crate::ssh::managed_hosts::{ManagedHostsError, ManagedHostsStore, ManagedSshHost};
use crate::ssh::process::{
    SshProcessAdapter, SshProcessCleanup, SshProcessCleanupScope, SshProcessEnvironment,
    SshProcessSupervisor,
};
use crate::ssh::remote_utility::SshRemoteUtilityProcessRunner;
use crate::ssh::remote_workspace_provider::SshRemoteWorkspaceProvider;
use crate::ssh::startup_environment::StartupSshEnvironment;
use crate::terminal::{
    RemoteChannelRevalidationError, RemoteChannelUnavailable, RemoteTerminalChannelProvider,
};

const CONNECT_CANCELLATION_POLL: Duration = Duration::from_millis(15);

#[derive(Clone)]
/// Capture-once portable SSH runtime supplied by Host Composition.
pub(crate) struct RemoteWorkspaceSshRuntime<A: SshProcessAdapter> {
    pub(crate) paths: Arc<AppPaths>,
    pub(crate) local_home: PathBuf,
    pub(crate) startup_environment: StartupSshEnvironment,
    pub(crate) startup_capability: SshCapability,
    pub(crate) aliases: ActiveSshAliasRegistry,
    pub(crate) executable: OpenSshExecutable,
    pub(crate) process_adapter: A,
    pub(crate) control_socket_probe: Arc<dyn ControlSocketProbe>,
    pub(crate) host_config_filesystem: Arc<dyn HostConfigFilesystem>,
}

/// Production SSH adapter for the window-independent remote Workspace flow.
///
/// The backend shares captured startup paths, environment, capability, and alias registry. Every
/// connection creates a fresh AskPass attempt, sanitized process backend, private control master,
/// and request-cancellable utility provider. Typed UI errors never retain raw prompts, secrets, or
/// remote output beyond the bounded sanitized connection-detail value.
pub(super) struct NativeRemoteWorkspaceFlowBackend<A: SshProcessAdapter> {
    runtime: RemoteWorkspaceSshRuntime<A>,
    askpass: Arc<dyn AskPassAttemptFactory>,
    executor: BackgroundExecutor,
    cleanup: NativeRemoteCleanupRegistry,
}

impl<A: SshProcessAdapter> NativeRemoteWorkspaceFlowBackend<A> {
    /// Creates an adapter from capture-once startup inputs and a main-thread AskPass factory.
    pub(super) fn new(
        runtime: RemoteWorkspaceSshRuntime<A>,
        askpass: Arc<dyn AskPassAttemptFactory>,
        executor: BackgroundExecutor,
    ) -> Self {
        Self {
            runtime,
            askpass,
            executor,
            cleanup: NativeRemoteCleanupRegistry::default(),
        }
    }

    fn roots(&self) -> HostConfigRoots {
        HostConfigRoots {
            managed: self.runtime.paths.managed_ssh_config(),
            user: self.runtime.local_home.join(".ssh/config"),
            home: self.runtime.local_home.clone(),
        }
    }

    fn fresh_discovery(&self) -> HostDiscovery {
        discover_ssh_hosts(
            self.runtime.host_config_filesystem.as_ref(),
            &self.roots(),
            HostDiscoveryLimits::default(),
        )
    }
}

fn resolve_configured_alias(
    destination: &SshDestination,
    aliases: &[SshHostAlias],
) -> Option<SshHostAlias> {
    resolve_destination_query(destination.as_str(), aliases, 1024)
        .ok()
        .and_then(|resolution| match resolution {
            crate::ssh::destination::DestinationQueryResolution::Configured { alias, .. } => {
                Some(alias)
            }
            crate::ssh::destination::DestinationQueryResolution::AddHost { .. } => None,
        })
}

fn acquire_destination_alias(
    registry: &ActiveSshAliasRegistry,
    destination: &SshDestination,
    mut discover_aliases: impl FnMut() -> Vec<SshHostAlias>,
) -> Result<Option<ActiveSshAliasLease>, RemoteWorkspaceFlowBackendError> {
    let Some(initial) = resolve_configured_alias(destination, &discover_aliases()) else {
        return if resolve_configured_alias(destination, &discover_aliases()).is_none() {
            Ok(None)
        } else {
            Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)
        };
    };
    let lease = registry
        .acquire(initial.clone())
        .map_err(|_| RemoteWorkspaceFlowBackendError::HostInUse)?;
    let confirmed = resolve_configured_alias(destination, &discover_aliases())
        .ok_or(RemoteWorkspaceFlowBackendError::ConnectionFailed)?;
    if confirmed != initial {
        return Err(RemoteWorkspaceFlowBackendError::ConnectionFailed);
    }
    Ok(Some(lease))
}

/// Main-thread factory that gates Remote availability using the pinned startup SSH capability.
///
/// Creating a backend captures only an attempt factory from the live window. Background connect
/// futures do not retain the `Window` or access ambient process state.
pub(crate) struct NativeRemoteWorkspaceFlowBackendFactory<A: SshProcessAdapter> {
    runtime: RemoteWorkspaceSshRuntime<A>,
    askpass: Arc<dyn AskPassWindowFactory>,
}

impl<A: SshProcessAdapter> NativeRemoteWorkspaceFlowBackendFactory<A> {
    pub(crate) fn new(
        runtime: RemoteWorkspaceSshRuntime<A>,
        askpass: Arc<dyn AskPassWindowFactory>,
    ) -> Self {
        Self { runtime, askpass }
    }
}

impl<A: SshProcessAdapter> RemoteWorkspaceFlowBackendFactory
    for NativeRemoteWorkspaceFlowBackendFactory<A>
{
    fn unavailable_reason(&self) -> Option<String> {
        match &self.runtime.startup_capability {
            SshCapability::Available(_) => None,
            SshCapability::Unavailable(reason) => Some(reason.to_string()),
        }
    }

    fn create(
        &self,
        window: &Window,
        cx: &mut App,
    ) -> Result<Arc<dyn RemoteWorkspaceFlowBackend>, RemoteWorkspaceFlowBackendError> {
        let askpass = self
            .askpass
            .create(window, cx)
            .map_err(|_| RemoteWorkspaceFlowBackendError::ConnectionFailed)?;
        let backend = NativeRemoteWorkspaceFlowBackend::new(
            self.runtime.clone(),
            askpass,
            cx.background_executor().clone(),
        );
        let cleanup = backend.cleanup.clone();
        cx.on_app_quit(move |cx| {
            // GPUI only polls returned quit futures for 100 ms. Complete owned SSH cleanup
            // here, while its executor is alive, before permitting process exit.
            cx.background_executor().block(cleanup.shutdown());
            async {}
        })
        .detach();
        Ok(Arc::new(backend))
    }
}

impl<A: SshProcessAdapter> RemoteWorkspaceFlowBackend for NativeRemoteWorkspaceFlowBackend<A> {
    fn discover_hosts(&self) -> HostDiscovery {
        self.fresh_discovery()
    }

    fn host_in_active_use(&self, alias: &SshHostAlias) -> bool {
        self.runtime.aliases.is_active(alias)
    }

    fn managed_host(&self, alias: &SshHostAlias) -> Option<ManagedSshHost> {
        ManagedHostsStore::new(&self.runtime.paths)
            .load()
            .ok()?
            .into_iter()
            .find(|host| host.alias() == alias)
    }

    fn save_managed_host(
        &self,
        host: ManagedSshHost,
        editing_alias: Option<SshHostAlias>,
    ) -> Task<Result<(), ManagedHostFormBackendError>> {
        let paths = self.runtime.paths.clone();
        let roots = self.roots();
        let aliases = self.runtime.aliases.clone();
        let host_config_filesystem = Arc::clone(&self.runtime.host_config_filesystem);
        self.executor.spawn(async move {
            let mut mutated_aliases = vec![host.alias().clone()];
            mutated_aliases.extend(editing_alias.iter().cloned());
            let _mutation = aliases
                .begin_mutation(mutated_aliases)
                .map_err(|_| ManagedHostFormBackendError::HostInUse)?;
            let discovery = discover_ssh_hosts(
                host_config_filesystem.as_ref(),
                &roots,
                HostDiscoveryLimits::default(),
            );
            ManagedHostsStore::new(&paths)
                .upsert(host, &discovery.hosts, editing_alias.as_ref())
                .map_err(map_save_error)
        })
    }

    fn delete_managed_host(
        &self,
        alias: SshHostAlias,
    ) -> Task<Result<(), RemoteWorkspaceFlowBackendError>> {
        let paths = self.runtime.paths.clone();
        let aliases = self.runtime.aliases.clone();
        self.executor.spawn(async move {
            let _mutation = aliases
                .begin_mutation([alias.clone()])
                .map_err(|_| RemoteWorkspaceFlowBackendError::HostInUse)?;
            ManagedHostsStore::new(&paths)
                .delete(&alias)
                .map_err(|_| RemoteWorkspaceFlowBackendError::DeleteFailed)
        })
    }

    fn connect(
        &self,
        destination: SshDestination,
        context: RemoteWorkspaceConnectContext,
    ) -> Task<Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>> {
        let cleanup = self.cleanup.clone();
        context.report(RemoteWorkspaceConnectionProgress::CheckingCompatibility);
        if !matches!(self.runtime.startup_capability, SshCapability::Available(_)) {
            return Task::ready(Err(RemoteWorkspaceFlowBackendError::OpenSshUnavailable));
        }
        let alias_lease =
            match acquire_destination_alias(&self.runtime.aliases, &destination, || {
                self.fresh_discovery()
                    .hosts
                    .into_iter()
                    .map(|host| host.alias().clone())
                    .collect()
            }) {
                Ok(lease) => lease,
                Err(error) => return Task::ready(Err(error)),
            };
        let attempt = match self.askpass.start_attempt(&self.runtime.paths) {
            Ok(attempt) => attempt,
            Err(_) => {
                return Task::ready(Err(RemoteWorkspaceFlowBackendError::SshRuntimeUnavailable));
            }
        };
        let authentication = attempt.lease.clone();
        let observation = attempt.observation.clone();
        let environment = match SshProcessEnvironment::new(
            self.runtime.local_home.clone(),
            authentication.clone(),
            &self.runtime.startup_environment,
        ) {
            Ok(environment) => environment,
            Err(_) => return Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)),
        };
        let paths = self.runtime.paths.clone();
        let executable = self.runtime.executable.clone();
        let process_adapter = self.runtime.process_adapter.clone();
        let control_socket_probe = Arc::clone(&self.runtime.control_socket_probe);
        let executor = self.executor.clone();
        let flow_cancellation = SshCancellationToken::default();
        let Some(connecting) = cleanup.begin_connect(flow_cancellation.clone()) else {
            return Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed));
        };
        self.executor.spawn(async move {
            let authentication_cancellation =
                SshCancellationToken::observing(observation.cancellation_flag());
            let cancellation =
                SshCancellationToken::linked(&flow_cancellation, &authentication_cancellation);
            let cancellation_watch = watch_flow_cancellation(
                context.clone(),
                flow_cancellation.clone(),
                executor.clone(),
            );
            let authentication_watch = watch_authentication(
                context.clone(),
                observation.clone(),
                cancellation.clone(),
                executor.clone(),
            );

            if context.is_cancelled() {
                cancellation.cancel();
                return Err(RemoteWorkspaceFlowBackendError::ConnectionFailed);
            }
            ManagedHostsStore::new(&paths)
                .ensure_exists()
                .map_err(|_| RemoteWorkspaceFlowBackendError::SshConfigurationUnavailable)?;
            if context.is_cancelled() {
                cancellation.cancel();
                return Err(RemoteWorkspaceFlowBackendError::ConnectionFailed);
            }
            context.report(RemoteWorkspaceConnectionProgress::Connecting);
            let backend = Arc::new(SshProcessSupervisor::new(
                executor.clone(),
                environment.clone(),
                process_adapter.clone(),
                connecting,
            ));
            let connection = OpenSshControlConnection::connect(
                &paths,
                executable,
                control_socket_probe.as_ref(),
                destination,
                Arc::clone(&backend),
                &cancellation,
                ControlConnectionTiming::default(),
            )
            .await;
            drop((cancellation_watch, authentication_watch));
            let connection = match connection {
                Ok(connection) if !context.is_cancelled() && !observation.cancelled() => connection,
                Ok(mut late_connection) => {
                    cancellation.cancel();
                    let _ = late_connection.shutdown().await;
                    return if observation.cancelled() && !context.is_cancelled() {
                        Err(RemoteWorkspaceFlowBackendError::AuthenticationCancelled)
                    } else {
                        Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)
                    };
                }
                Err(error) => {
                    return Err(map_control_connection_error(
                        error,
                        observation.cancelled(),
                        context.is_cancelled(),
                    ));
                }
            };
            let utility_command = connection
                .remote_utility_command()
                .map_err(|_| RemoteWorkspaceFlowBackendError::ConnectionFailed)?;
            let lifecycle = connection
                .lifecycle_observer()
                .map_err(|_| RemoteWorkspaceFlowBackendError::ConnectionFailed)?;
            let utility_runner = Arc::new(SshRemoteUtilityProcessRunner::new(
                process_adapter,
                environment,
            ));
            let provider: Arc<dyn RemoteWorkspaceProvider + Send + Sync> =
                Arc::new(SshRemoteWorkspaceProvider::new(
                    utility_command,
                    utility_runner,
                    cancellation.clone(),
                    executor.clone(),
                ));
            let control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>> =
                Arc::new(Mutex::new(Some(Box::new(connection))));
            let resources = Arc::new(NativeSessionResources {
                control,
                authentication: Mutex::new(Some(authentication)),
                alias: Mutex::new(alias_lease),
                cancellation,
                executor,
                completion: Mutex::new(None),
            });
            cleanup.register(&resources);
            let owner = NativeRemoteWorkspaceSessionOwner {
                resources,
                lifecycle: Some(lifecycle),
                utility: Arc::clone(&provider),
            };
            Ok(RemoteWorkspaceConnectedSession::new(
                Box::new(owner),
                provider,
            ))
        })
    }
}

fn watch_flow_cancellation(
    context: RemoteWorkspaceConnectContext,
    cancellation: SshCancellationToken,
    executor: BackgroundExecutor,
) -> Task<()> {
    executor.clone().spawn(async move {
        while !context.is_cancelled() && !cancellation.is_cancelled() {
            executor.timer(CONNECT_CANCELLATION_POLL).await;
        }
        if context.is_cancelled() {
            cancellation.cancel();
        }
    })
}

fn watch_authentication(
    context: RemoteWorkspaceConnectContext,
    observation: AskPassAttemptObservation,
    cancellation: SshCancellationToken,
    executor: BackgroundExecutor,
) -> Task<()> {
    executor.clone().spawn(async move {
        let mut prompt_was_active = false;
        while !cancellation.is_cancelled() {
            let prompt_is_active = observation.prompt_active();
            if prompt_is_active != prompt_was_active {
                context.report(if prompt_is_active {
                    RemoteWorkspaceConnectionProgress::Authenticating
                } else {
                    RemoteWorkspaceConnectionProgress::Connecting
                });
                prompt_was_active = prompt_is_active;
            }
            executor.timer(CONNECT_CANCELLATION_POLL).await;
        }
    })
}

fn map_save_error(error: ManagedHostsError) -> ManagedHostFormBackendError {
    match error {
        ManagedHostsError::AliasCollision => ManagedHostFormBackendError::AliasCollision,
        _ => ManagedHostFormBackendError::SaveFailed,
    }
}

fn map_control_connection_error(
    error: ControlConnectionError,
    authentication_cancelled: bool,
    flow_cancelled: bool,
) -> RemoteWorkspaceFlowBackendError {
    if authentication_cancelled && !flow_cancelled {
        return RemoteWorkspaceFlowBackendError::AuthenticationCancelled;
    }
    match error {
        ControlConnectionError::MasterExited {
            error_output: Some(detail),
            ..
        } => RemoteWorkspaceFlowBackendError::ConnectionFailedWithDetail(detail),
        _ => RemoteWorkspaceFlowBackendError::ConnectionFailed,
    }
}

/// Non-clone owner of one connected session's control, authentication, alias, and cancellation.
///
/// The owner pairs its lifecycle observer with the same control generation. Close cancels work,
/// transfers bounded control shutdown and AskPass teardown to retained background ownership, and
/// releases the session alias lease only after cleanup. Workspace-lifetime alias pins are acquired
/// as independent registry counts.
struct NativeRemoteWorkspaceSessionOwner {
    resources: Arc<NativeSessionResources>,
    lifecycle: Option<ControlConnectionObserver>,
    utility: Arc<dyn RemoteWorkspaceProvider + Send + Sync>,
}

#[derive(Clone, Default)]
struct NativeRemoteCleanupRegistry(Arc<Mutex<NativeRemoteCleanupState>>);

#[derive(Default)]
struct NativeRemoteCleanupState {
    sessions: Vec<Weak<NativeSessionResources>>,
    connections: Vec<(SshCancellationToken, SshProcessCleanup)>,
    quitting: bool,
}

impl NativeRemoteCleanupRegistry {
    fn begin_connect(&self, cancellation: SshCancellationToken) -> Option<SshProcessCleanupScope> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.quitting {
            return None;
        }
        state
            .connections
            .retain(|(_, completion)| !completion.is_complete());
        let (finished, completion) = SshProcessCleanup::scope();
        state.connections.push((cancellation, completion));
        Some(finished)
    }

    fn register(&self, resources: &Arc<NativeSessionResources>) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.sessions.retain(|entry| entry.strong_count() != 0);
        state.sessions.push(Arc::downgrade(resources));
        if state.quitting {
            resources.close();
        }
    }

    async fn shutdown(&self) {
        let connections = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.quitting = true;
            std::mem::take(&mut state.connections)
        };
        for (cancellation, _) in &connections {
            cancellation.cancel();
        }
        let resources: Vec<_> = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let completions: Vec<_> = resources
            .iter()
            .map(NativeSessionResources::close)
            .collect();
        for completion in completions {
            let _ = completion.recv().await;
        }
        // Late successful connections close on registration. Failed or cancelled connects
        // retain this barrier through native spawn, process reaping, and runtime cleanup.
        for (_, completion) in connections {
            completion.wait().await;
        }
    }
}

struct NativeSessionResources {
    control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>>,
    authentication: Mutex<Option<AskPassBrokerLease>>,
    alias: Mutex<Option<ActiveSshAliasLease>>,
    cancellation: SshCancellationToken,
    executor: BackgroundExecutor,
    completion: Mutex<Option<async_channel::Receiver<()>>>,
}

impl NativeSessionResources {
    fn close(self: &Arc<Self>) -> async_channel::Receiver<()> {
        let mut completion = self
            .completion
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(completion) = &*completion {
            return completion.clone();
        }
        self.cancellation.cancel();
        let connection = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let authentication = self
            .authentication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let alias = self
            .alias
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let (finished, receiver) = async_channel::bounded::<()>(1);
        *completion = Some(receiver.clone());
        let resources = Arc::clone(self);
        self.executor
            .spawn(async move {
                if let Some(authentication) = authentication {
                    authentication.cancel();
                }
                if let Some(connection) = connection {
                    connection.shutdown().await;
                }
                drop(alias);
                drop(finished);
                drop(resources);
            })
            .detach();
        receiver
    }
}

impl RemoteWorkspaceSessionOwner for NativeRemoteWorkspaceSessionOwner {
    fn acquire_workspace_alias_pin(
        &self,
    ) -> Result<Option<RemoteWorkspaceAliasPin>, RemoteWorkspaceAliasPinError> {
        self.resources
            .alias
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|alias| {
                alias
                    .try_duplicate()
                    .map(RemoteWorkspaceAliasPin::new)
                    .map_err(|_| RemoteWorkspaceAliasPinError)
            })
            .transpose()
    }

    fn bind_terminal_channels_for_identity(
        &self,
        directory: &RemoteWorkspaceDirectory,
        expected_identity: &RemoteDirectoryIdentity,
        login_shell: &ValidatedRemoteLoginShell,
    ) -> Result<Arc<dyn RemoteTerminalChannelProvider>, RemoteWorkspaceFlowBackendError> {
        RemotePaneShellCommandBuilder::new(directory, login_shell)
            .build()
            .map_err(|_| RemoteWorkspaceFlowBackendError::IncompatibleServer)?;
        Ok(Arc::new(NativeRemoteTerminalChannelProvider {
            control: Arc::downgrade(&self.resources.control),
            directory: directory.clone(),
            expected_identity: expected_identity.clone(),
            utility: Arc::clone(&self.utility),
            login_shell: login_shell.clone(),
            executor: self.resources.executor.clone(),
            grant: Arc::new(Mutex::new(ChannelGrantState::default())),
        }))
    }

    fn take_lifecycle_observer(&mut self) -> Option<ControlConnectionObserver> {
        self.lifecycle.take()
    }

    fn close(&mut self) {
        self.resources.close();
    }
}

impl Drop for NativeRemoteWorkspaceSessionOwner {
    fn drop(&mut self) {
        self.close();
    }
}

/// Fallible terminal-channel source bound to one directory identity and live control authority.
///
/// Revalidation must observe the expected physical identity through the session utility provider
/// and grants exactly one prepare for the same opaque connection instance and generation.
struct NativeRemoteTerminalChannelProvider {
    control: Weak<Mutex<Option<Box<dyn NativeSessionControl>>>>,
    directory: RemoteWorkspaceDirectory,
    expected_identity: RemoteDirectoryIdentity,
    utility: Arc<dyn RemoteWorkspaceProvider + Send + Sync>,
    login_shell: ValidatedRemoteLoginShell,
    executor: BackgroundExecutor,
    grant: Arc<Mutex<ChannelGrantState>>,
}

#[derive(Default)]
struct ChannelGrantState {
    validation_epoch: u64,
    granted_binding: Option<LiveConnectionBinding>,
}

impl RemoteTerminalChannelProvider for NativeRemoteTerminalChannelProvider {
    fn is_ready(&self) -> bool {
        self.control.upgrade().is_some_and(|control| {
            control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_some_and(|connection| connection.is_ready())
        })
    }

    fn revalidate(&self) -> Task<Result<(), RemoteChannelRevalidationError>> {
        let validation_epoch = {
            let mut grant = self
                .grant
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            grant.granted_binding = None;
            let Some(validation_epoch) = grant.validation_epoch.checked_add(1) else {
                return Task::ready(Err(RemoteChannelRevalidationError::ConnectionUnavailable));
            };
            grant.validation_epoch = validation_epoch;
            validation_epoch
        };
        let Some(control) = self.control.upgrade() else {
            return Task::ready(Err(RemoteChannelRevalidationError::ConnectionUnavailable));
        };
        let binding = control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|connection| connection.live_binding());
        let Some(binding) = binding else {
            return Task::ready(Err(RemoteChannelRevalidationError::ConnectionUnavailable));
        };
        let validation = self
            .utility
            .validate_physical_identity(self.directory.clone());
        let expected_identity = self.expected_identity.clone();
        let control = Arc::downgrade(&control);
        let grant = Arc::clone(&self.grant);
        self.executor.spawn(async move {
            let observed_identity = validation.await.map_err(|error| match error {
                super::remote_workspace_picker::RemoteWorkspaceProviderError::ConnectionLost => {
                    RemoteChannelRevalidationError::ConnectionUnavailable
                }
                _ => RemoteChannelRevalidationError::DirectoryUnavailable,
            })?;
            if observed_identity != expected_identity {
                return Err(RemoteChannelRevalidationError::IdentityChanged);
            }
            let current_binding = control
                .upgrade()
                .and_then(|control| {
                    control
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        .and_then(|connection| connection.live_binding())
                })
                .ok_or(RemoteChannelRevalidationError::ConnectionUnavailable)?;
            if current_binding != binding {
                return Err(RemoteChannelRevalidationError::ConnectionUnavailable);
            }
            let mut grant = grant
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if grant.validation_epoch != validation_epoch {
                return Err(RemoteChannelRevalidationError::ConnectionUnavailable);
            }
            grant.granted_binding = Some(binding);
            Ok(())
        })
    }

    fn prepare(
        &self,
    ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable> {
        let granted_binding = self
            .grant
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .granted_binding
            .take()
            .ok_or(RemoteChannelUnavailable)?;
        let control = self.control.upgrade().ok_or(RemoteChannelUnavailable)?;
        let command = RemotePaneShellCommandBuilder::new(&self.directory, &self.login_shell)
            .build()
            .map_err(|_| RemoteChannelUnavailable)?;
        let control = control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let connection = control.as_ref().ok_or(RemoteChannelUnavailable)?;
        if connection.live_binding() != Some(granted_binding) {
            return Err(RemoteChannelUnavailable);
        }
        connection.prepare_pane_channel(command)
    }
}

/// Narrow object-safe control boundary retained only by the native session owner.
///
/// UI-facing providers receive a weak reference and can neither clone nor shut down the control.
trait NativeSessionControl: Send {
    fn is_ready(&self) -> bool;

    fn prepare_pane_channel(
        &self,
        command: crate::ssh::command::ValidatedRemoteShellCommand,
    ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable>;

    fn live_binding(&self) -> Option<LiveConnectionBinding>;

    fn shutdown(self: Box<Self>) -> NativeSessionShutdown;
}

type NativeSessionShutdown = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

impl<A: SshProcessAdapter> NativeSessionControl
    for OpenSshControlConnection<SshProcessSupervisor<A>>
{
    fn is_ready(&self) -> bool {
        self.state() == ControlConnectionState::Ready
    }

    fn prepare_pane_channel(
        &self,
        command: crate::ssh::command::ValidatedRemoteShellCommand,
    ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable> {
        OpenSshControlConnection::prepare_pane_channel(self, command)
            .map_err(|_| RemoteChannelUnavailable)
    }

    fn live_binding(&self) -> Option<LiveConnectionBinding> {
        OpenSshControlConnection::live_binding(self).ok()
    }

    fn shutdown(mut self: Box<Self>) -> NativeSessionShutdown {
        Box::pin(async move {
            let _ = OpenSshControlConnection::shutdown(&mut *self).await;
            let _ = OpenSshControlConnection::finish_cleanup(&mut *self).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;

    use gpui::{
        Context, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render, Styled,
        TestAppContext, div,
    };

    use super::*;
    use crate::platform::app_paths::{AppPathEnvironment, AppPathHostFacts};
    use crate::platform::testing::{
        EmptyHostConfigFilesystem, RecordingControlSocketProbe, RecordingFilesystem,
    };
    use crate::ssh::command::{OpenSshVersion, SshUnavailableReason};
    use crate::ssh::process::testing::RecordingAdapter;

    struct FakeIdentityProvider {
        validations: Mutex<
            VecDeque<
                Result<
                    RemoteDirectoryIdentity,
                    super::super::remote_workspace_picker::RemoteWorkspaceProviderError,
                >,
            >,
        >,
    }

    impl FakeIdentityProvider {
        fn returning(
            results: impl IntoIterator<
                Item = Result<
                    RemoteDirectoryIdentity,
                    super::super::remote_workspace_picker::RemoteWorkspaceProviderError,
                >,
            >,
        ) -> Self {
            Self {
                validations: Mutex::new(results.into_iter().collect()),
            }
        }
    }

    impl RemoteWorkspaceProvider for FakeIdentityProvider {
        fn discover_account(
            &self,
        ) -> Task<
            Result<
                super::super::remote_workspace_picker::RemoteWorkspaceAccount,
                super::super::remote_workspace_picker::RemoteWorkspaceProviderError,
            >,
        > {
            Task::ready(Err(
                super::super::remote_workspace_picker::RemoteWorkspaceProviderError::Other,
            ))
        }

        fn list_directories(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> Task<
            Result<
                super::super::remote_workspace_picker::RemoteWorkspaceDirectoryListing,
                super::super::remote_workspace_picker::RemoteWorkspaceProviderError,
            >,
        > {
            Task::ready(Err(
                super::super::remote_workspace_picker::RemoteWorkspaceProviderError::Other,
            ))
        }

        fn probe_exact_path(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> Task<
            Result<
                super::super::remote_workspace_picker::RemoteWorkspaceExactPathState,
                super::super::remote_workspace_picker::RemoteWorkspaceProviderError,
            >,
        > {
            Task::ready(Err(
                super::super::remote_workspace_picker::RemoteWorkspaceProviderError::Other,
            ))
        }

        fn create_directory_recursively(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> Task<Result<(), super::super::remote_workspace_picker::RemoteWorkspaceProviderError>>
        {
            Task::ready(Err(
                super::super::remote_workspace_picker::RemoteWorkspaceProviderError::Other,
            ))
        }

        fn validate_physical_identity(
            &self,
            _: RemoteWorkspaceDirectory,
        ) -> Task<
            Result<
                RemoteDirectoryIdentity,
                super::super::remote_workspace_picker::RemoteWorkspaceProviderError,
            >,
        > {
            Task::ready(self.validations.lock().unwrap().pop_front().unwrap_or(Err(
                super::super::remote_workspace_picker::RemoteWorkspaceProviderError::Other,
            )))
        }
    }

    struct RejectAskPassFactory;
    impl AskPassWindowFactory for RejectAskPassFactory {
        fn create(
            &self,
            _: &Window,
            _: &mut App,
        ) -> Result<Arc<dyn AskPassAttemptFactory>, crate::platform::askpass::AskPassUnavailable>
        {
            Err(crate::platform::askpass::AskPassUnavailable)
        }
    }

    fn factory_with_capability(
        startup_capability: SshCapability,
    ) -> NativeRemoteWorkspaceFlowBackendFactory<RecordingAdapter> {
        let environment = AppPathEnvironment {
            home: Some("/Users/test".into()),
            ..AppPathEnvironment::default()
        };
        let host = AppPathHostFacts::new(PathBuf::from("/private/tmp"), 103).unwrap();
        let paths = AppPaths::resolve(
            &environment,
            &host,
            Arc::new(RecordingFilesystem::default()),
        )
        .unwrap();
        NativeRemoteWorkspaceFlowBackendFactory::new(
            RemoteWorkspaceSshRuntime {
                paths: Arc::new(paths),
                local_home: PathBuf::from("/Users/test"),
                startup_environment: StartupSshEnvironment::default(),
                startup_capability,
                aliases: ActiveSshAliasRegistry::default(),
                executable: OpenSshExecutable::for_test(),
                process_adapter: RecordingAdapter::default(),
                control_socket_probe: Arc::new(RecordingControlSocketProbe::default()),
                host_config_filesystem: Arc::new(EmptyHostConfigFilesystem),
            },
            Arc::new(RejectAskPassFactory),
        )
    }

    #[test]
    fn native_factory_should_gate_missing_openssh_before_askpass_construction() {
        let factory =
            factory_with_capability(SshCapability::Unavailable(SshUnavailableReason::NotFound));

        assert_eq!(
            factory.unavailable_reason().as_deref(),
            Some("the selected OpenSSH client is unavailable")
        );
    }

    #[test]
    fn native_factory_should_gate_openssh_older_than_the_supported_minimum() {
        let factory =
            factory_with_capability(SshCapability::Unavailable(SshUnavailableReason::TooOld {
                found: OpenSshVersion::new(8, 1),
                minimum: OpenSshVersion::new(8, 2),
            }));

        assert_eq!(
            factory.unavailable_reason().as_deref(),
            Some("OpenSSH 8.2 or newer is required; found 8.1")
        );
    }

    #[test]
    fn native_factory_should_gate_unrecognized_ssh_clients() {
        let factory = factory_with_capability(SshCapability::Unavailable(
            SshUnavailableReason::Unrecognized,
        ));

        assert_eq!(
            factory.unavailable_reason().as_deref(),
            Some("the installed SSH client did not report a recognized OpenSSH version")
        );
    }

    #[test]
    fn native_factory_should_enable_the_source_at_the_supported_minimum() {
        let factory = factory_with_capability(SshCapability::Available(OpenSshVersion::new(8, 2)));

        assert_eq!(factory.unavailable_reason(), None);
    }

    fn discovered(alias: &str) -> Vec<SshHostAlias> {
        vec![SshHostAlias::new(alias.to_owned()).unwrap()]
    }

    #[test]
    fn configured_alias_lease_should_be_held_during_fresh_confirmation() {
        let aliases = ActiveSshAliasRegistry::default();
        let observed = aliases.clone();
        let destination = SshDestination::new("root@work".to_owned()).unwrap();
        let discoveries = Mutex::new(vec![discovered("work"), discovered("work")].into_iter());
        let mut calls = 0;

        let lease = acquire_destination_alias(&aliases, &destination, || {
            calls += 1;
            if calls == 2 {
                assert!(observed.is_active(&SshHostAlias::new("work".to_owned()).unwrap()));
            }
            discoveries.lock().unwrap().next().unwrap()
        })
        .unwrap();

        assert!(aliases.is_active(&SshHostAlias::new("work".to_owned()).unwrap()));
        drop(lease);
    }

    #[test]
    fn configured_alias_change_should_not_fall_back_to_an_unleased_destination() {
        let aliases = ActiveSshAliasRegistry::default();
        let destination = SshDestination::new("root@work".to_owned()).unwrap();
        let discoveries = Mutex::new(vec![discovered("work"), Vec::new()].into_iter());

        let result = acquire_destination_alias(&aliases, &destination, || {
            discoveries.lock().unwrap().next().unwrap()
        });

        assert!(
            matches!(
                result,
                Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)
            ) && !aliases.is_active(&SshHostAlias::new("work".to_owned()).unwrap())
        );
    }

    #[test]
    fn raw_destination_should_remain_unpinned_after_two_fresh_unconfigured_discoveries() {
        let aliases = ActiveSshAliasRegistry::default();
        let destination = SshDestination::new("root@server.example:2222".to_owned()).unwrap();
        let mut calls = 0;

        let result = acquire_destination_alias(&aliases, &destination, || {
            calls += 1;
            Vec::new()
        });

        assert!(matches!(result, Ok(None)) && calls == 2);
    }

    #[test]
    fn newly_configured_raw_destination_should_not_continue_without_an_alias_lease() {
        let aliases = ActiveSshAliasRegistry::default();
        let destination = SshDestination::new("root@work".to_owned()).unwrap();
        let discoveries = Mutex::new(vec![Vec::new(), discovered("work")].into_iter());

        let result = acquire_destination_alias(&aliases, &destination, || {
            discoveries.lock().unwrap().next().unwrap()
        });

        assert!(
            matches!(
                result,
                Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)
            ) && !aliases.is_active(&SshHostAlias::new("work".to_owned()).unwrap())
        );
    }

    struct FakeSessionControl {
        shutdowns: Arc<AtomicUsize>,
        preparations: Arc<AtomicUsize>,
        binding: Arc<Mutex<LiveConnectionBinding>>,
        alias: SshHostAlias,
        aliases: ActiveSshAliasRegistry,
    }

    impl NativeSessionControl for FakeSessionControl {
        fn is_ready(&self) -> bool {
            true
        }

        fn prepare_pane_channel(
            &self,
            command: crate::ssh::command::ValidatedRemoteShellCommand,
        ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable>
        {
            self.preparations.fetch_add(1, Ordering::SeqCst);
            Ok(crate::ssh::command::SshCommandContext::new(
                crate::ssh::command::OpenSshExecutable::for_test(),
                PathBuf::from("/private/config/spaceterm/ssh_config"),
                SshDestination::new("work".to_owned()).unwrap(),
                PathBuf::from("/private/runtime/spaceterm/master.sock"),
            )
            .unwrap()
            .prepare_pane_channel(command))
        }

        fn live_binding(&self) -> Option<LiveConnectionBinding> {
            Some(
                self.binding
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            )
        }

        fn shutdown(self: Box<Self>) -> NativeSessionShutdown {
            Box::pin(async move {
                assert!(self.aliases.is_active(&self.alias));
                self.shutdowns.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    struct PendingShutdownControl {
        release: async_channel::Receiver<()>,
        started: Arc<AtomicUsize>,
        terminated: Arc<AtomicUsize>,
        reaped: Arc<AtomicUsize>,
        artifacts_removed: Arc<AtomicUsize>,
    }

    impl NativeSessionControl for PendingShutdownControl {
        fn is_ready(&self) -> bool {
            true
        }

        fn prepare_pane_channel(
            &self,
            _: crate::ssh::command::ValidatedRemoteShellCommand,
        ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable>
        {
            Err(RemoteChannelUnavailable)
        }

        fn live_binding(&self) -> Option<LiveConnectionBinding> {
            Some(LiveConnectionBinding::for_test(1))
        }

        fn shutdown(self: Box<Self>) -> NativeSessionShutdown {
            Box::pin(async move {
                self.started.fetch_add(1, Ordering::SeqCst);
                let _ = self.release.recv().await;
                self.terminated.fetch_add(1, Ordering::SeqCst);
                self.reaped.fetch_add(1, Ordering::SeqCst);
                self.artifacts_removed.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    #[gpui::test]
    fn quit_waits_for_live_and_already_closing_workspace_cleanup(cx: &mut TestAppContext) {
        for close_workspace_first in [false, true] {
            let registry = NativeRemoteCleanupRegistry::default();
            let (release, released) = async_channel::bounded(1);
            let started = Arc::new(AtomicUsize::new(0));
            let terminated = Arc::new(AtomicUsize::new(0));
            let reaped = Arc::new(AtomicUsize::new(0));
            let artifacts_removed = Arc::new(AtomicUsize::new(0));
            let resources = Arc::new(NativeSessionResources {
                control: Arc::new(Mutex::new(Some(Box::new(PendingShutdownControl {
                    release: released,
                    started: Arc::clone(&started),
                    terminated: Arc::clone(&terminated),
                    reaped: Arc::clone(&reaped),
                    artifacts_removed: Arc::clone(&artifacts_removed),
                })))),
                authentication: Mutex::new(None),
                alias: Mutex::new(None),
                cancellation: SshCancellationToken::default(),
                executor: cx.executor(),
                completion: Mutex::new(None),
            });
            registry.register(&resources);
            if close_workspace_first {
                resources.close();
                drop(resources);
            }
            let (quit_finished, finished) = async_channel::bounded::<()>(1);
            let cleanup = registry.clone();
            cx.executor()
                .spawn(async move {
                    cleanup.shutdown().await;
                    drop(quit_finished);
                })
                .detach();
            cx.run_until_parked();
            assert_eq!(started.load(Ordering::SeqCst), 1);
            assert!(!finished.is_closed());
            assert_eq!(reaped.load(Ordering::SeqCst), 0);
            release.try_send(()).unwrap();
            cx.run_until_parked();
            assert!(finished.is_closed());
            assert_eq!(terminated.load(Ordering::SeqCst), 1);
            assert_eq!(reaped.load(Ordering::SeqCst), 1);
            assert_eq!(artifacts_removed.load(Ordering::SeqCst), 1);
            cx.executor().block(registry.shutdown());
            assert_eq!(started.load(Ordering::SeqCst), 1);
        }
    }

    #[gpui::test]
    fn quit_cancels_and_awaits_connecting_work_and_refuses_new_connections(
        cx: &mut TestAppContext,
    ) {
        let registry = NativeRemoteCleanupRegistry::default();
        let cancellation = SshCancellationToken::default();
        let connecting = registry.begin_connect(cancellation.clone()).unwrap();
        let (finished, completion) = async_channel::bounded::<()>(1);
        let cleanup = registry.clone();
        cx.executor()
            .spawn(async move {
                cleanup.shutdown().await;
                drop(finished);
            })
            .detach();
        cx.run_until_parked();
        assert!(cancellation.is_cancelled());
        assert!(!completion.is_closed());
        assert!(
            registry
                .begin_connect(SshCancellationToken::default())
                .is_none()
        );
        drop(connecting);
        cx.run_until_parked();
        assert!(completion.is_closed());
    }

    #[derive(Clone)]
    struct PendingConnectReaper {
        inner: RecordingAdapter,
        cancellation: SshCancellationToken,
        cancel_on_spawn: bool,
        reaping: async_channel::Sender<()>,
        release: async_channel::Receiver<()>,
        reaped: Arc<AtomicUsize>,
        killed: Arc<AtomicUsize>,
    }

    impl SshProcessAdapter for PendingConnectReaper {
        type Process = crate::ssh::process::testing::RecordingProcess;

        fn spawn(
            &self,
            request: crate::ssh::process::SshProcessSpawnRequest,
        ) -> Result<
            crate::ssh::process::SpawnedSshProcess<Self::Process>,
            crate::ssh::process::SshProcessMechanismError,
        > {
            let child = self.inner.spawn(request)?;
            if self.cancel_on_spawn {
                self.cancellation.cancel();
            }
            Ok(child)
        }

        fn try_status(
            &self,
            _: &mut Self::Process,
        ) -> Result<
            Option<crate::ssh::process::ProcessExit>,
            crate::ssh::process::SshProcessMechanismError,
        > {
            Err(crate::ssh::process::SshProcessMechanismError::StatusFailed)
        }

        fn signal(
            &self,
            process: &mut Self::Process,
            signal: crate::ssh::process::ProcessSignal,
        ) -> Result<(), crate::ssh::process::SshProcessMechanismError> {
            self.killed.fetch_add(1, Ordering::SeqCst);
            self.inner.signal(process, signal)
        }

        fn reap(
            &self,
            process: Self::Process,
        ) -> Result<(), crate::ssh::process::SshProcessMechanismError> {
            self.reaping.try_send(()).unwrap();
            self.release.recv_blocking().unwrap();
            self.inner.reap(process)?;
            self.reaped.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[gpui::test]
    fn quit_waits_for_connect_failure_reaper_and_runtime_artifact_cleanup(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        for cancel_on_spawn in [true, false] {
            let registry = NativeRemoteCleanupRegistry::default();
            let cancellation = SshCancellationToken::default();
            let scope = registry.begin_connect(cancellation.clone()).unwrap();
            let filesystem = Arc::new(RecordingFilesystem::default());
            let paths = AppPaths::resolve(
                &AppPathEnvironment {
                    home: Some("/fixture/home".into()),
                    ..Default::default()
                },
                &AppPathHostFacts::new(PathBuf::from("/fixture/tmp"), 103).unwrap(),
                filesystem.clone(),
            )
            .unwrap();
            let (release, released) = async_channel::bounded(1);
            let (reaping, started) = async_channel::bounded(1);
            let reaped = Arc::new(AtomicUsize::new(0));
            let killed = Arc::new(AtomicUsize::new(0));
            let backend = Arc::new(SshProcessSupervisor::new(
                cx.executor(),
                SshProcessEnvironment::new_without_authentication(
                    PathBuf::from("/fixture/home"),
                    None,
                )
                .unwrap(),
                PendingConnectReaper {
                    inner: RecordingAdapter::default(),
                    cancellation: cancellation.clone(),
                    cancel_on_spawn,
                    reaping,
                    release: released,
                    reaped: Arc::clone(&reaped),
                    killed: Arc::clone(&killed),
                },
                scope,
            ));
            let result = cx.executor().block(OpenSshControlConnection::connect(
                &paths,
                OpenSshExecutable::for_test(),
                &RecordingControlSocketProbe(Arc::clone(&filesystem)),
                SshDestination::new("fixture".to_owned()).unwrap(),
                backend,
                &cancellation,
                ControlConnectionTiming::default(),
            ));
            assert!(if cancel_on_spawn {
                matches!(result, Err(ControlConnectionError::Cancelled))
            } else {
                matches!(result, Err(ControlConnectionError::MasterStatus { .. }))
            });
            cx.executor().block(started.recv()).unwrap();
            let (finished, completion) = async_channel::bounded::<()>(1);
            let quit = cx.executor().spawn(async move {
                registry.shutdown().await;
                drop(finished);
            });
            cx.run_until_parked();
            assert!(!completion.is_closed());
            assert_eq!(killed.load(Ordering::SeqCst), 1);
            assert_eq!(reaped.load(Ordering::SeqCst), 0);
            assert!(!filesystem.events.lock().unwrap().contains(&"remove-owner"));
            release.try_send(()).unwrap();
            cx.executor().block(quit);
            assert!(completion.is_closed());
            assert_eq!(reaped.load(Ordering::SeqCst), 1);
            assert!(filesystem.events.lock().unwrap().contains(&"remove-owner"));
        }
    }

    struct NativeCloseHarness {
        session: Option<RemoteWorkspaceConnectedSession>,
        prior_focus: FocusHandle,
        transient_focus: FocusHandle,
    }

    impl NativeCloseHarness {
        fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            self.session.take();
            self.prior_focus.focus(window);
            cx.notify();
        }
    }

    impl Render for NativeCloseHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.prior_focus)
                .child(div().track_focus(&self.transient_focus))
        }
    }

    #[gpui::test]
    fn pending_native_shutdown_should_not_block_cancel_or_focus_restoration(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let (release, pending) = async_channel::bounded(1);
        let started = Arc::new(AtomicUsize::new(0));
        let terminated = Arc::new(AtomicUsize::new(0));
        let reaped = Arc::new(AtomicUsize::new(0));
        let artifacts_removed = Arc::new(AtomicUsize::new(0));
        let aliases = ActiveSshAliasRegistry::default();
        let alias = SshHostAlias::new("work".to_owned()).unwrap();
        let alias_lease = aliases.acquire(alias.clone()).unwrap();
        let control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>> =
            Arc::new(Mutex::new(Some(Box::new(PendingShutdownControl {
                release: pending,
                started: Arc::clone(&started),
                terminated: Arc::clone(&terminated),
                reaped: Arc::clone(&reaped),
                artifacts_removed: Arc::clone(&artifacts_removed),
            }))));
        let owner = NativeRemoteWorkspaceSessionOwner {
            resources: Arc::new(NativeSessionResources {
                control,
                authentication: Mutex::new(None),
                alias: Mutex::new(Some(alias_lease)),
                cancellation: SshCancellationToken::default(),
                executor: cx.executor(),
                completion: Mutex::new(None),
            }),
            lifecycle: None,
            utility: Arc::new(FakeIdentityProvider::returning([])),
        };
        let session = RemoteWorkspaceConnectedSession::new(
            Box::new(owner),
            Arc::new(FakeIdentityProvider::returning([])),
        );
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let prior_focus = cx.focus_handle();
            let transient_focus = cx.focus_handle();
            transient_focus.focus(window);
            NativeCloseHarness {
                session: Some(session),
                prior_focus,
                transient_focus,
            }
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert!(cx.update(|window, cx| harness.read(cx).transient_focus.is_focused(window)));

        let (returned, wait_for_return) = mpsc::sync_channel(1);
        let blocked = Arc::new(AtomicBool::new(false));
        let watchdog_blocked = Arc::clone(&blocked);
        let watchdog_release = release.clone();
        let watchdog = thread::spawn(move || {
            if wait_for_return
                .recv_timeout(Duration::from_secs(1))
                .is_err()
            {
                watchdog_blocked.store(true, Ordering::Release);
                let _ = watchdog_release.send_blocking(());
            }
        });

        cx.update(|window, cx| {
            harness.update(cx, |harness, cx| harness.cancel(window, cx));
        });
        returned.send(()).unwrap();
        watchdog.join().unwrap();

        assert!(!blocked.load(Ordering::Acquire));
        assert!(cx.update(|window, cx| harness.read(cx).prior_focus.is_focused(window)));
        cx.run_until_parked();
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(terminated.load(Ordering::SeqCst), 0);
        assert_eq!(reaped.load(Ordering::SeqCst), 0);
        assert_eq!(artifacts_removed.load(Ordering::SeqCst), 0);
        assert!(aliases.is_active(&alias));

        release.send_blocking(()).unwrap();
        cx.run_until_parked();
        assert_eq!(terminated.load(Ordering::SeqCst), 1);
        assert_eq!(reaped.load(Ordering::SeqCst), 1);
        assert_eq!(artifacts_removed.load(Ordering::SeqCst), 1);
        assert!(!aliases.is_active(&alias));
    }

    #[gpui::test]
    fn native_owner_should_close_exactly_once_without_releasing_the_workspace_alias_pin(
        cx: &mut TestAppContext,
    ) {
        let aliases = ActiveSshAliasRegistry::default();
        let alias = SshHostAlias::new("work".to_owned()).unwrap();
        let alias_lease = aliases.acquire(alias.clone()).unwrap();
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let preparations = Arc::new(AtomicUsize::new(0));
        let binding = Arc::new(Mutex::new(LiveConnectionBinding::for_test(1)));
        let control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>> =
            Arc::new(Mutex::new(Some(Box::new(FakeSessionControl {
                shutdowns: Arc::clone(&shutdowns),
                preparations,
                binding,
                alias: alias.clone(),
                aliases: aliases.clone(),
            }))));
        let cancellation = SshCancellationToken::default();
        let mut owner = NativeRemoteWorkspaceSessionOwner {
            resources: Arc::new(NativeSessionResources {
                control: Arc::clone(&control),
                authentication: Mutex::new(None),
                alias: Mutex::new(Some(alias_lease)),
                cancellation: cancellation.clone(),
                executor: cx.executor(),
                completion: Mutex::new(None),
            }),
            lifecycle: None,
            utility: Arc::new(FakeIdentityProvider::returning([Ok(
                RemoteDirectoryIdentity::new("/home/test/src".to_owned()).unwrap(),
            )])),
        };
        let login_shell = ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap();
        let provider = owner
            .bind_terminal_channels_for_identity(
                &RemoteWorkspaceDirectory::new("~/src".to_owned()).unwrap(),
                &RemoteDirectoryIdentity::new("/home/test/src".to_owned()).unwrap(),
                &login_shell,
            )
            .unwrap();
        assert!(provider.is_ready());
        assert_eq!(cx.executor().block(provider.revalidate()), Ok(()));
        let workspace_alias = owner.acquire_workspace_alias_pin().unwrap().unwrap();

        owner.close();
        owner.close();
        cx.run_until_parked();

        assert!(cancellation.is_cancelled());
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
        assert!(aliases.is_active(&alias));
        assert!(!provider.is_ready());
        assert!(provider.prepare().is_err());
        drop(workspace_alias);
        assert!(!aliases.is_active(&alias));
    }

    #[gpui::test]
    fn native_channel_should_require_one_fresh_identity_grant_per_preparation(
        cx: &mut TestAppContext,
    ) {
        let binding = Arc::new(Mutex::new(LiveConnectionBinding::for_test(7)));
        let preparations = Arc::new(AtomicUsize::new(0));
        let control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>> =
            Arc::new(Mutex::new(Some(Box::new(FakeSessionControl {
                shutdowns: Arc::new(AtomicUsize::new(0)),
                preparations: Arc::clone(&preparations),
                binding: Arc::clone(&binding),
                alias: SshHostAlias::new("work".to_owned()).unwrap(),
                aliases: ActiveSshAliasRegistry::default(),
            }))));
        let expected = RemoteDirectoryIdentity::new("/srv/project".to_owned()).unwrap();
        let provider = NativeRemoteTerminalChannelProvider {
            control: Arc::downgrade(&control),
            directory: RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap(),
            expected_identity: expected.clone(),
            utility: Arc::new(FakeIdentityProvider::returning([
                Ok(expected.clone()),
                Ok(expected),
            ])),
            login_shell: ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap(),
            executor: cx.executor(),
            grant: Arc::new(Mutex::new(ChannelGrantState::default())),
        };

        assert!(provider.prepare().is_err());
        assert_eq!(cx.executor().block(provider.revalidate()), Ok(()));
        assert!(provider.prepare().is_ok());
        assert!(provider.prepare().is_err());
        assert_eq!(preparations.load(Ordering::SeqCst), 1);

        assert_eq!(cx.executor().block(provider.revalidate()), Ok(()));
        let next_generation = binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .with_generation(8);
        *binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next_generation;
        assert!(provider.prepare().is_err());
        assert_eq!(preparations.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn native_channel_should_preserve_verified_posix_sh_through_preparation(
        cx: &mut TestAppContext,
    ) {
        let control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>> =
            Arc::new(Mutex::new(Some(Box::new(FakeSessionControl {
                shutdowns: Arc::new(AtomicUsize::new(0)),
                preparations: Arc::new(AtomicUsize::new(0)),
                binding: Arc::new(Mutex::new(LiveConnectionBinding::for_test(7))),
                alias: SshHostAlias::new("work".to_owned()).unwrap(),
                aliases: ActiveSshAliasRegistry::default(),
            }))));
        let expected = RemoteDirectoryIdentity::new("/srv/project".to_owned()).unwrap();
        let provider = NativeRemoteTerminalChannelProvider {
            control: Arc::downgrade(&control),
            directory: RemoteWorkspaceDirectory::new("/srv/project".to_owned()).unwrap(),
            expected_identity: expected.clone(),
            utility: Arc::new(FakeIdentityProvider::returning([Ok(expected)])),
            login_shell: ValidatedRemoteLoginShell::from_discovery(
                "/bin/sh".to_owned(),
                crate::ssh::command::PosixShLoginCapability::LoginOptionSupported,
            )
            .unwrap(),
            executor: cx.executor(),
            grant: Arc::new(Mutex::new(ChannelGrantState::default())),
        };

        assert_eq!(cx.executor().block(provider.revalidate()), Ok(()));
        let prepared = provider.prepare().unwrap();
        let command = prepared.take().unwrap();

        assert_eq!(
            command.arguments().last().unwrap(),
            "cd '/srv/project' && SPACETERM='1' COLORTERM='truecolor' exec '/bin/sh' -l"
        );
    }

    #[gpui::test]
    fn native_channel_should_reject_a_same_generation_replacement_control(cx: &mut TestAppContext) {
        let old_preparations = Arc::new(AtomicUsize::new(0));
        let old_binding = LiveConnectionBinding::for_test(1);
        let control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>> =
            Arc::new(Mutex::new(Some(Box::new(FakeSessionControl {
                shutdowns: Arc::new(AtomicUsize::new(0)),
                preparations: Arc::clone(&old_preparations),
                binding: Arc::new(Mutex::new(old_binding)),
                alias: SshHostAlias::new("work".to_owned()).unwrap(),
                aliases: ActiveSshAliasRegistry::default(),
            }))));
        let expected = RemoteDirectoryIdentity::new("/srv/project".to_owned()).unwrap();
        let provider = NativeRemoteTerminalChannelProvider {
            control: Arc::downgrade(&control),
            directory: RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap(),
            expected_identity: expected.clone(),
            utility: Arc::new(FakeIdentityProvider::returning([Ok(expected)])),
            login_shell: ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap(),
            executor: cx.executor(),
            grant: Arc::new(Mutex::new(ChannelGrantState::default())),
        };

        assert_eq!(cx.executor().block(provider.revalidate()), Ok(()));

        let replacement_preparations = Arc::new(AtomicUsize::new(0));
        *control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Box::new(FakeSessionControl {
                shutdowns: Arc::new(AtomicUsize::new(0)),
                preparations: Arc::clone(&replacement_preparations),
                binding: Arc::new(Mutex::new(LiveConnectionBinding::for_test(1))),
                alias: SshHostAlias::new("work".to_owned()).unwrap(),
                aliases: ActiveSshAliasRegistry::default(),
            }));

        assert!(provider.prepare().is_err());
        assert_eq!(old_preparations.load(Ordering::SeqCst), 0);
        assert_eq!(replacement_preparations.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn native_channel_should_reject_identity_replacement_without_granting_prepare(
        cx: &mut TestAppContext,
    ) {
        let preparations = Arc::new(AtomicUsize::new(0));
        let control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>> =
            Arc::new(Mutex::new(Some(Box::new(FakeSessionControl {
                shutdowns: Arc::new(AtomicUsize::new(0)),
                preparations: Arc::clone(&preparations),
                binding: Arc::new(Mutex::new(LiveConnectionBinding::for_test(1))),
                alias: SshHostAlias::new("work".to_owned()).unwrap(),
                aliases: ActiveSshAliasRegistry::default(),
            }))));
        let provider = NativeRemoteTerminalChannelProvider {
            control: Arc::downgrade(&control),
            directory: RemoteWorkspaceDirectory::new("~/project".to_owned()).unwrap(),
            expected_identity: RemoteDirectoryIdentity::new("/srv/project".to_owned()).unwrap(),
            utility: Arc::new(FakeIdentityProvider::returning([Ok(
                RemoteDirectoryIdentity::new("/attacker/project".to_owned()).unwrap(),
            )])),
            login_shell: ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap(),
            executor: cx.executor(),
            grant: Arc::new(Mutex::new(ChannelGrantState::default())),
        };

        assert_eq!(
            cx.executor().block(provider.revalidate()),
            Err(RemoteChannelRevalidationError::IdentityChanged)
        );
        assert!(provider.prepare().is_err());
        assert_eq!(preparations.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn save_error_mapping_should_keep_collision_actionable_without_exposing_io() {
        assert_eq!(
            map_save_error(ManagedHostsError::AliasCollision),
            ManagedHostFormBackendError::AliasCollision
        );
        assert_eq!(
            map_save_error(ManagedHostsError::NonCanonical),
            ManagedHostFormBackendError::SaveFailed
        );
    }

    #[test]
    fn early_master_exit_should_preserve_only_the_sanitized_transient_detail() {
        let detail = crate::ssh::process::TransientSshErrorOutput::from_untrusted_bytes(
            b"bad\x1b[31m config\n",
        )
        .unwrap();

        let error = map_control_connection_error(
            ControlConnectionError::MasterExited {
                exit: crate::ssh::process::ProcessExit::unsuccessful(Some(255)),
                error_output: Some(detail),
            },
            false,
            false,
        );

        assert_eq!(error.connection_detail(), Some("bad [31m config"));
        assert!(!format!("{error:?}").contains("config"));
        assert!(!error.to_string().contains("config"));
    }

    #[test]
    fn authentication_cancel_should_discard_master_detail_before_flow_mapping() {
        let detail = crate::ssh::process::TransientSshErrorOutput::from_untrusted_bytes(
            b"Password for private key: correct horse battery staple",
        )
        .unwrap();

        let error = map_control_connection_error(
            ControlConnectionError::MasterExited {
                exit: crate::ssh::process::ProcessExit::unsuccessful(Some(255)),
                error_output: Some(detail),
            },
            true,
            false,
        );

        assert_eq!(
            error,
            RemoteWorkspaceFlowBackendError::AuthenticationCancelled
        );
        assert_eq!(error.connection_detail(), None);
        assert!(!format!("{error:?}").contains("Password"));
        assert!(!error.to_string().contains("correct horse"));
    }
}
