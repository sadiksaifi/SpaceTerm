#![expect(
    dead_code,
    reason = "the native Remote Workspace backend lands before Workspace Manager wiring"
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use gpui::{BackgroundExecutor, Task};

use super::remote_workspace_flow::{
    RemoteWorkspaceConnectContext, RemoteWorkspaceConnectedSession,
    RemoteWorkspaceConnectionProgress, RemoteWorkspaceFlowBackend, RemoteWorkspaceFlowBackendError,
    RemoteWorkspaceSessionOwner,
};
use super::remote_workspace_picker::RemoteWorkspaceProvider;
use super::ssh_host_form::ManagedHostFormBackendError;
use crate::domain::{RemoteWorkspaceDirectory, SshDestination};
use crate::platform::app_paths::AppPaths;
use crate::platform::macos_askpass_transport::{
    AskPassAttemptObservation, AskPassBrokerLease, NativeAskPassBrokerFactory,
};
use crate::ssh::alias_usage::{ActiveSshAliasLease, ActiveSshAliasRegistry};
use crate::ssh::cancellation::SshCancellationToken;
use crate::ssh::command::{
    NativeSshProbeRunner, RemotePaneShellCommandBuilder, SshCapability, ValidatedRemoteLoginShell,
};
use crate::ssh::control_connection::{
    ControlConnectionState, ControlConnectionTiming, OpenSshControlConnection,
};
use crate::ssh::destination::{SshHostAlias, resolve_destination_query};
use crate::ssh::host_config::{
    HostConfigRoots, HostDiscovery, HostDiscoveryLimits, NativeHostConfigFilesystem,
    discover_ssh_hosts,
};
use crate::ssh::live_connection::ControlConnectionObserver;
use crate::ssh::managed_hosts::{
    ManagedHostsError, ManagedHostsStore, ManagedSshHost, NativeManagedHostsFilesystem,
};
use crate::ssh::process::{NativeSshProcessBackend, SshProcessEnvironment};
use crate::ssh::remote_utility::NativeSshRemoteUtilityRunner;
use crate::ssh::remote_workspace_provider::SshRemoteWorkspaceProvider;
use crate::ssh::startup_environment::StartupSshEnvironment;
use crate::terminal::{RemoteChannelUnavailable, RemoteTerminalChannelProvider};

const CONNECT_CANCELLATION_POLL: Duration = Duration::from_millis(15);

pub(super) struct NativeRemoteWorkspaceFlowBackend {
    paths: AppPaths,
    local_home: PathBuf,
    startup_environment: StartupSshEnvironment,
    aliases: ActiveSshAliasRegistry,
    askpass: Arc<NativeAskPassBrokerFactory>,
    executor: BackgroundExecutor,
}

impl NativeRemoteWorkspaceFlowBackend {
    pub(super) fn new(
        paths: AppPaths,
        local_home: PathBuf,
        startup_environment: StartupSshEnvironment,
        aliases: ActiveSshAliasRegistry,
        askpass: Arc<NativeAskPassBrokerFactory>,
        executor: BackgroundExecutor,
    ) -> Self {
        Self {
            paths,
            local_home,
            startup_environment,
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

    fn destination_alias(&self, destination: &SshDestination) -> Option<SshHostAlias> {
        let discovery = self.fresh_discovery();
        let aliases: Vec<_> = discovery
            .hosts
            .iter()
            .map(|host| host.alias().clone())
            .collect();
        resolve_destination_query(destination.as_str(), &aliases, 1024)
            .ok()
            .and_then(|resolution| match resolution {
                crate::ssh::destination::DestinationQueryResolution::Configured {
                    alias, ..
                } => Some(alias),
                crate::ssh::destination::DestinationQueryResolution::AddHost { .. } => None,
            })
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
        let attempt = match self.askpass.start_attempt(&self.paths) {
            Ok(attempt) => attempt,
            Err(_) => return Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)),
        };
        let authentication = attempt.lease();
        let observation = attempt.observation();
        let environment = match SshProcessEnvironment::new(
            self.local_home.clone(),
            authentication.clone(),
            self.startup_environment.cloned_agent_socket(),
        ) {
            Ok(environment) => environment,
            Err(_) => return Task::ready(Err(RemoteWorkspaceFlowBackendError::ConnectionFailed)),
        };
        let paths = self.paths.clone();
        let alias = self.destination_alias(&destination);
        let aliases = self.aliases.clone();
        let executor = self.executor.clone();
        self.executor.spawn(async move {
            let cancellation = SshCancellationToken::default();
            let cancellation_watch =
                watch_flow_cancellation(context.clone(), cancellation.clone(), executor.clone());
            let authentication_watch = watch_authentication(
                context.clone(),
                observation.clone(),
                cancellation.clone(),
                executor.clone(),
            );

            let capability = NativeSshProbeRunner::new(environment.clone())
                .probe(cancellation.clone())
                .await;
            if !matches!(capability, SshCapability::Available(_)) {
                cancellation.cancel();
                drop((cancellation_watch, authentication_watch));
                return Err(RemoteWorkspaceFlowBackendError::OpenSshUnavailable);
            }
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
                Ok(connection) if !context.is_cancelled() => connection,
                Ok(mut late_connection) => {
                    cancellation.cancel();
                    let _ = late_connection.shutdown().await;
                    return Err(RemoteWorkspaceFlowBackendError::ConnectionFailed);
                }
                Err(_) if observation.cancelled() && !context.is_cancelled() => {
                    return Err(RemoteWorkspaceFlowBackendError::AuthenticationCancelled);
                }
                Err(_) => return Err(RemoteWorkspaceFlowBackendError::ConnectionFailed),
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
            let alias_lease = match alias {
                Some(alias) => Some(
                    aliases
                        .acquire(alias)
                        .map_err(|_| RemoteWorkspaceFlowBackendError::HostInUse)?,
                ),
                None => None,
            };
            let control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>> =
                Arc::new(Mutex::new(Some(Box::new(connection))));
            let owner = NativeRemoteWorkspaceSessionOwner {
                control,
                lifecycle: Some(lifecycle),
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

struct NativeRemoteWorkspaceSessionOwner {
    control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>>,
    lifecycle: Option<ControlConnectionObserver>,
    authentication: Option<AskPassBrokerLease>,
    alias: Option<ActiveSshAliasLease>,
    cancellation: SshCancellationToken,
    executor: BackgroundExecutor,
    closed: bool,
}

impl RemoteWorkspaceSessionOwner for NativeRemoteWorkspaceSessionOwner {
    fn bind_terminal_channels(
        &self,
        directory: &RemoteWorkspaceDirectory,
        login_shell: &str,
    ) -> Result<Arc<dyn RemoteTerminalChannelProvider>, RemoteWorkspaceFlowBackendError> {
        let login_shell_path = login_shell.to_owned();
        let login_shell = ValidatedRemoteLoginShell::new(login_shell_path.clone())
            .map_err(|_| RemoteWorkspaceFlowBackendError::IncompatibleServer)?;
        RemotePaneShellCommandBuilder::new(directory, &login_shell)
            .build()
            .map_err(|_| RemoteWorkspaceFlowBackendError::IncompatibleServer)?;
        Ok(Arc::new(NativeRemoteTerminalChannelProvider {
            control: Arc::downgrade(&self.control),
            directory: directory.clone(),
            login_shell: login_shell_path,
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

struct NativeRemoteTerminalChannelProvider {
    control: Weak<Mutex<Option<Box<dyn NativeSessionControl>>>>,
    directory: RemoteWorkspaceDirectory,
    login_shell: String,
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

    fn prepare(
        &self,
    ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable> {
        let control = self.control.upgrade().ok_or(RemoteChannelUnavailable)?;
        let login_shell = ValidatedRemoteLoginShell::new(self.login_shell.clone())
            .map_err(|_| RemoteChannelUnavailable)?;
        let command = RemotePaneShellCommandBuilder::new(&self.directory, &login_shell)
            .build()
            .map_err(|_| RemoteChannelUnavailable)?;
        control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .ok_or(RemoteChannelUnavailable)?
            .prepare_pane_channel(command)
            .map_err(|_| RemoteChannelUnavailable)
    }
}

trait NativeSessionControl: Send {
    fn is_ready(&self) -> bool;

    fn prepare_pane_channel(
        &self,
        command: crate::ssh::command::ValidatedRemoteShellCommand,
    ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable>;

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

    fn shutdown(&mut self, executor: &BackgroundExecutor) {
        let _ = executor.block(OpenSshControlConnection::shutdown(self));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui::TestAppContext;

    use super::*;

    struct FakeSessionControl {
        shutdowns: Arc<AtomicUsize>,
        alias: SshHostAlias,
        aliases: ActiveSshAliasRegistry,
    }

    impl NativeSessionControl for FakeSessionControl {
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

        fn shutdown(&mut self, _: &BackgroundExecutor) {
            assert!(self.aliases.is_active(&self.alias));
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[gpui::test]
    fn native_owner_should_cancel_and_close_exactly_once_before_releasing_alias(
        cx: &mut TestAppContext,
    ) {
        let aliases = ActiveSshAliasRegistry::default();
        let alias = SshHostAlias::new("work".to_owned()).unwrap();
        let alias_lease = aliases.acquire(alias.clone()).unwrap();
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let control: Arc<Mutex<Option<Box<dyn NativeSessionControl>>>> =
            Arc::new(Mutex::new(Some(Box::new(FakeSessionControl {
                shutdowns: Arc::clone(&shutdowns),
                alias: alias.clone(),
                aliases: aliases.clone(),
            }))));
        let cancellation = SshCancellationToken::default();
        let mut owner = NativeRemoteWorkspaceSessionOwner {
            control: Arc::clone(&control),
            lifecycle: None,
            authentication: None,
            alias: Some(alias_lease),
            cancellation: cancellation.clone(),
            executor: cx.executor(),
            closed: false,
        };
        let provider = owner
            .bind_terminal_channels(
                &RemoteWorkspaceDirectory::new("~/src".to_owned()).unwrap(),
                "/bin/zsh",
            )
            .unwrap();
        assert!(provider.is_ready());

        owner.close();
        owner.close();

        assert!(cancellation.is_cancelled());
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
        assert!(!aliases.is_active(&alias));
        assert!(!provider.is_ready());
        assert!(provider.prepare().is_err());
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
}
