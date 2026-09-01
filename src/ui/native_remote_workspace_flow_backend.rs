use std::path::PathBuf;
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
use crate::platform::macos_askpass_transport::{
    AskPassAttemptObservation, AskPassBrokerLease, NativeAskPassBrokerFactory,
};
use crate::ssh::alias_usage::{ActiveSshAliasLease, ActiveSshAliasRegistry};
use crate::ssh::cancellation::SshCancellationToken;
use crate::ssh::command::{
    RemotePaneShellCommandBuilder, SshCapability, ValidatedRemoteLoginShell,
};
use crate::ssh::control_connection::{
    ControlConnectionError, ControlConnectionState, ControlConnectionTiming,
    OpenSshControlConnection,
};
use crate::ssh::destination::{SshHostAlias, resolve_destination_query};
use crate::ssh::host_config::{
    HostConfigRoots, HostDiscovery, HostDiscoveryLimits, NativeHostConfigFilesystem,
    discover_ssh_hosts,
};
use crate::ssh::live_connection::{ControlConnectionObserver, LiveConnectionBinding};
use crate::ssh::managed_hosts::{
    ManagedHostsError, ManagedHostsStore, ManagedSshHost, NativeManagedHostsFilesystem,
};
use crate::ssh::process::{NativeSshProcessBackend, SshProcessEnvironment};
use crate::ssh::remote_utility::NativeSshRemoteUtilityRunner;
use crate::ssh::remote_workspace_provider::SshRemoteWorkspaceProvider;
use crate::ssh::startup_environment::StartupSshEnvironment;
use crate::terminal::{
    RemoteChannelRevalidationError, RemoteChannelUnavailable, RemoteTerminalChannelProvider,
};

const CONNECT_CANCELLATION_POLL: Duration = Duration::from_millis(15);

/// Production SSH adapter for the window-independent remote Workspace flow.
///
/// The backend shares captured startup paths, environment, capability, and alias registry. Every
/// connection creates a fresh AskPass attempt, sanitized process backend, private control master,
/// and request-cancellable utility provider. Typed UI errors never retain raw prompts, secrets, or
/// remote output beyond the bounded sanitized connection-detail value.
pub(super) struct NativeRemoteWorkspaceFlowBackend {
    paths: Arc<AppPaths>,
    local_home: PathBuf,
    startup_environment: StartupSshEnvironment,
    startup_capability: SshCapability,
    aliases: ActiveSshAliasRegistry,
    askpass: Arc<NativeAskPassBrokerFactory>,
    executor: BackgroundExecutor,
}

impl NativeRemoteWorkspaceFlowBackend {
    /// Creates an adapter from capture-once startup inputs and a main-thread AskPass factory.
    pub(super) fn new(
        paths: Arc<AppPaths>,
        local_home: PathBuf,
        startup_environment: StartupSshEnvironment,
        startup_capability: SshCapability,
        aliases: ActiveSshAliasRegistry,
        askpass: Arc<NativeAskPassBrokerFactory>,
        executor: BackgroundExecutor,
    ) -> Self {
        Self {
            paths,
            local_home,
            startup_environment,
            startup_capability,
            aliases,
            askpass,
            executor,
        }
    }

    fn roots(&self) -> HostConfigRoots {
        HostConfigRoots {
            managed: self.paths.managed_ssh_config(),
            user: self.local_home.join(".ssh/config"),
            home: self.local_home.clone(),
        }
    }

    fn fresh_discovery(&self) -> HostDiscovery {
        discover_ssh_hosts(
            &NativeHostConfigFilesystem,
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
pub(crate) struct NativeRemoteWorkspaceFlowBackendFactory {
    paths: Arc<AppPaths>,
    local_home: PathBuf,
    startup_environment: StartupSshEnvironment,
    startup_capability: SshCapability,
    aliases: ActiveSshAliasRegistry,
}

impl NativeRemoteWorkspaceFlowBackendFactory {
    pub(crate) fn new(
        paths: Arc<AppPaths>,
        local_home: PathBuf,
        startup_environment: StartupSshEnvironment,
        startup_capability: SshCapability,
        aliases: ActiveSshAliasRegistry,
    ) -> Self {
        Self {
            paths,
            local_home,
            startup_environment,
            startup_capability,
            aliases,
        }
    }
}

impl RemoteWorkspaceFlowBackendFactory for NativeRemoteWorkspaceFlowBackendFactory {
    fn unavailable_reason(&self) -> Option<String> {
        match &self.startup_capability {
            SshCapability::Available(_) => None,
            SshCapability::Unavailable(reason) => Some(reason.to_string()),
        }
    }

    fn create(
        &self,
        window: &Window,
        cx: &mut App,
    ) -> Result<Arc<dyn RemoteWorkspaceFlowBackend>, RemoteWorkspaceFlowBackendError> {
        let askpass = NativeAskPassBrokerFactory::new(window, cx)
            .map(Arc::new)
            .map_err(|_| RemoteWorkspaceFlowBackendError::ConnectionFailed)?;
        Ok(Arc::new(NativeRemoteWorkspaceFlowBackend::new(
            Arc::clone(&self.paths),
            self.local_home.clone(),
            self.startup_environment.clone(),
            self.startup_capability.clone(),
            self.aliases.clone(),
            askpass,
            cx.background_executor().clone(),
        )))
    }
}

impl RemoteWorkspaceFlowBackend for NativeRemoteWorkspaceFlowBackend {
    fn discover_hosts(&self) -> HostDiscovery {
        self.fresh_discovery()
    }

    fn host_in_active_use(&self, alias: &SshHostAlias) -> bool {
        self.aliases.is_active(alias)
    }

    fn managed_host(&self, alias: &SshHostAlias) -> Option<ManagedSshHost> {
        ManagedHostsStore::new(&self.paths, &NativeManagedHostsFilesystem)
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
        let paths = self.paths.clone();
        let roots = self.roots();
        let aliases = self.aliases.clone();
        self.executor.spawn(async move {
            let mut mutated_aliases = vec![host.alias().clone()];
            mutated_aliases.extend(editing_alias.iter().cloned());
            let _mutation = aliases
                .begin_mutation(mutated_aliases)
                .map_err(|_| ManagedHostFormBackendError::HostInUse)?;
            let discovery = discover_ssh_hosts(
                &NativeHostConfigFilesystem,
                &roots,
                HostDiscoveryLimits::default(),
            );
            ManagedHostsStore::new(&paths, &NativeManagedHostsFilesystem)
                .upsert(host, &discovery.hosts, editing_alias.as_ref())
                .map_err(map_save_error)
        })
    }

    fn delete_managed_host(
        &self,
        alias: SshHostAlias,
    ) -> Task<Result<(), RemoteWorkspaceFlowBackendError>> {
        let paths = self.paths.clone();
        let aliases = self.aliases.clone();
        self.executor.spawn(async move {
            let _mutation = aliases
                .begin_mutation([alias.clone()])
                .map_err(|_| RemoteWorkspaceFlowBackendError::HostInUse)?;
            ManagedHostsStore::new(&paths, &NativeManagedHostsFilesystem)
                .delete(&alias)
                .map_err(|_| RemoteWorkspaceFlowBackendError::DeleteFailed)
        })
    }

    fn connect(
        &self,
        destination: SshDestination,
        context: RemoteWorkspaceConnectContext,
    ) -> Task<Result<RemoteWorkspaceConnectedSession, RemoteWorkspaceFlowBackendError>> {
        context.report(RemoteWorkspaceConnectionProgress::CheckingCompatibility);
        if !matches!(self.startup_capability, SshCapability::Available(_)) {
            return Task::ready(Err(RemoteWorkspaceFlowBackendError::OpenSshUnavailable));
        }
        let alias_lease = match acquire_destination_alias(&self.aliases, &destination, || {
            self.fresh_discovery()
                .hosts
                .into_iter()
                .map(|host| host.alias().clone())
                .collect()
        }) {
            Ok(lease) => lease,
            Err(error) => return Task::ready(Err(error)),
        };
        let attempt = match self.askpass.start_attempt(&self.paths) {
            Ok(attempt) => attempt,
            Err(_) => return Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)),
        };
        let authentication = attempt.lease();
        let observation = attempt.observation();
        let environment = match SshProcessEnvironment::new(
            self.local_home.clone(),
            authentication.clone(),
            &self.startup_environment,
        ) {
            Ok(environment) => environment,
            Err(_) => return Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)),
        };
        let paths = self.paths.clone();
        let executor = self.executor.clone();
        self.executor.spawn(async move {
            let flow_cancellation = SshCancellationToken::default();
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
            context.report(RemoteWorkspaceConnectionProgress::Connecting);
            let backend = Arc::new(NativeSshProcessBackend::new(
                executor.clone(),
                environment.clone(),
            ));
            let connection = OpenSshControlConnection::connect(
                &paths,
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
            let utility_runner = Arc::new(NativeSshRemoteUtilityRunner::new(environment));
            let provider: Arc<dyn RemoteWorkspaceProvider + Send + Sync> =
                Arc::new(SshRemoteWorkspaceProvider::new(
                    utility_command,
                    utility_runner,
                    cancellation.clone(),
                    executor.clone(),
                ));
            let control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>> =
                Arc::new(Mutex::new(Some(Box::new(connection))));
            let owner = NativeRemoteWorkspaceSessionOwner {
                control,
                lifecycle: Some(lifecycle),
                utility: Arc::clone(&provider),
                authentication: Some(authentication),
                alias: alias_lease,
                cancellation,
                executor,
                closed: false,
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
        while !observation.prompt_started() && !cancellation.is_cancelled() {
            executor.timer(CONNECT_CANCELLATION_POLL).await;
        }
        if observation.prompt_started() && !cancellation.is_cancelled() {
            context.report(RemoteWorkspaceConnectionProgress::Authenticating);
        }
    })
}

fn map_save_error(error: ManagedHostsError) -> ManagedHostFormBackendError {
    match error {
        ManagedHostsError::AliasCollision { .. } => ManagedHostFormBackendError::AliasCollision,
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
/// performs bounded control shutdown once, tears down AskPass, and releases the session alias lease
/// only after cleanup. Workspace-lifetime alias pins are acquired as independent registry counts.
struct NativeRemoteWorkspaceSessionOwner {
    control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>>,
    lifecycle: Option<ControlConnectionObserver>,
    utility: Arc<dyn RemoteWorkspaceProvider + Send + Sync>,
    authentication: Option<AskPassBrokerLease>,
    alias: Option<ActiveSshAliasLease>,
    cancellation: SshCancellationToken,
    executor: BackgroundExecutor,
    closed: bool,
}

impl RemoteWorkspaceSessionOwner for NativeRemoteWorkspaceSessionOwner {
    fn acquire_workspace_alias_pin(
        &self,
    ) -> Result<Option<RemoteWorkspaceAliasPin>, RemoteWorkspaceAliasPinError> {
        self.alias
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
            control: Arc::downgrade(&self.control),
            directory: directory.clone(),
            expected_identity: expected_identity.clone(),
            utility: Arc::clone(&self.utility),
            login_shell: login_shell.clone(),
            executor: self.executor.clone(),
            grant: Arc::new(Mutex::new(ChannelGrantState::default())),
        }))
    }

    fn take_lifecycle_observer(&mut self) -> Option<ControlConnectionObserver> {
        self.lifecycle.take()
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.cancellation.cancel();
        let connection = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mut connection) = connection {
            connection.shutdown(&self.executor);
        }
        if let Some(authentication) = self.authentication.take() {
            authentication.cancel();
        }
        self.alias.take();
        self.closed = true;
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

    fn shutdown(&mut self, executor: &BackgroundExecutor);
}

impl NativeSessionControl for OpenSshControlConnection<NativeSshProcessBackend> {
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

    fn shutdown(&mut self, executor: &BackgroundExecutor) {
        let _ = executor.block(OpenSshControlConnection::shutdown(self));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui::TestAppContext;

    use super::*;
    use crate::platform::app_paths::AppPathEnvironment;
    use crate::ssh::command::{OpenSshVersion, SshUnavailableReason};

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

    fn factory_with_capability(
        startup_capability: SshCapability,
    ) -> NativeRemoteWorkspaceFlowBackendFactory {
        let paths = AppPaths::resolve(&AppPathEnvironment {
            home: Some("/Users/test".into()),
            macos_temporary_directory: PathBuf::from("/private/tmp"),
            ..AppPathEnvironment::default()
        })
        .unwrap();
        NativeRemoteWorkspaceFlowBackendFactory::new(
            Arc::new(paths),
            PathBuf::from("/Users/test"),
            StartupSshEnvironment::default(),
            startup_capability,
            ActiveSshAliasRegistry::default(),
        )
    }

    #[test]
    fn native_factory_should_gate_missing_openssh_before_askpass_construction() {
        let factory =
            factory_with_capability(SshCapability::Unavailable(SshUnavailableReason::NotFound));

        assert_eq!(
            factory.unavailable_reason().as_deref(),
            Some("OpenSSH was not found at /usr/bin/ssh")
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

        fn shutdown(&mut self, _: &BackgroundExecutor) {
            assert!(self.aliases.is_active(&self.alias));
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
        }
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
            control: Arc::clone(&control),
            lifecycle: None,
            utility: Arc::new(FakeIdentityProvider::returning([Ok(
                RemoteDirectoryIdentity::new("/home/test/src".to_owned()).unwrap(),
            )])),
            authentication: None,
            alias: Some(alias_lease),
            cancellation: cancellation.clone(),
            executor: cx.executor(),
            closed: false,
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
            map_save_error(ManagedHostsError::AliasCollision {
                alias: "work".to_owned(),
            }),
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
