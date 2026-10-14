//! Account composition retains admission through the actor's final store close.

use super::{AccountOwnerEnrollment, AccountStartup, StartupFailure};
use crate::{
    adapters::{
        core::CorePorts,
        terminal::{AccountLaunchFacts, AccountLaunchProvider, NativeRuntime, NativeRuntimeClock},
    },
    api::{
        ListenerFactories, ServerConfig, ServerHandle, ServerHostContext, ServerLimits,
        StartupAuthorityPort,
    },
    contracts::*,
    extensions::ExtensionObserverPort,
    failure::failure,
    git::{
        directories::{DirectoryOrdinaryDispatch, DirectoryService},
        dispatch::GitOrdinaryDispatch,
        host::NativeServerGitHost,
        recovery::GitExternalWork,
        writes::{GitLimits, GitService},
    },
    listeners::{
        ProductionListenerConfig, ProductionListenerPorts,
        production_listener_factories_with_dispatch,
    },
    pairing::{
        protocol_lookup::{PairingProtocolLookup, PairingSnapshotPort},
        service::PairingService,
    },
    service::RecoveredServer,
    shutdown::{ShutdownReport, StoppedAuthorizationPort},
};
use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    identity::ServerId,
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    observations::ConfirmedDirectoryObservation,
    operation_ledger::canonical::CanonicalCodec,
    operation_ledger_contracts::CanonicalOperationRequest,
    organization::ServerDirectory,
    pairing::{PairingWrites, policy::PairingSnapshot},
    retirement::SessionCleanupObservation,
    storage::CommitReceipt,
};
use spaceterm_host::{
    account_service::{ServiceFailure, ServiceSupervisor, TrustedAccountFacts},
    organization_store::StoreIdentity,
    server_directories::native::NativeDirectoryProvider,
    shell_integration::{ShellEnvironment, ShellIntegrationMode, ShellIntegrationPolicy},
    shell_launch::{ShellLaunchPlanner, ShellTerminalIdentity, ShellTerminfoIdentity},
};
use spaceterm_protocol::ProtocolError;
use spaceterm_terminal::{
    appearance::{Appearance, AppearanceGeneration, TerminalColors},
    metadata::LocalMachine,
    runtime::budget::RuntimeLimits,
    runtime_contracts::RuntimeAppearance,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

struct AccountLaunch(AccountLaunchFacts);
impl AccountLaunchProvider for AccountLaunch {
    fn account(&self) -> Result<AccountLaunchFacts, ServerFailure> {
        Ok(AccountLaunchFacts {
            uid: self.0.uid,
            home: self.0.home.clone(),
            planner: self.0.planner.clone(),
            machine: LocalMachine::new(
                self.0.machine.user(),
                self.0.machine.hostname(),
                self.0.home.to_str(),
            ),
        })
    }
}

struct Admission(Arc<spaceterm_host::account_service::AccountStartup>);
impl StartupAuthorityPort for Admission {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure> {
        self.0.revalidate().map_err(native_failure)?;
        Ok(ServerId::new(self.0.registration().server_pin().identity))
    }
    fn validate_credentials(
        &self,
        credentials: &dyn ServerCredentialPort,
        server: ServerId,
    ) -> Result<(), ServerFailure> {
        let expected = self.validate_startup()?;
        if server != expected
            || credentials.public_key() != self.0.registration().server_pin().public_key
        {
            return Err(failure(ServerFailureClass::Authentication));
        }
        Ok(())
    }
}

struct NativeManagement(
    Arc<ServiceSupervisor>,
    Option<Arc<dyn spaceterm_protocol::authentication::SigningKey>>,
);
impl HostManagementPort for NativeManagement {
    fn prepare_next_instance<'a>(
        &'a self,
        server: ServerId,
        next_epoch: ServerEpoch,
    ) -> ServerFuture<'a, Result<(), ServerFailure>> {
        Box::pin(async move {
            let Some(signer) = &self.1 else {
                return Ok(());
            };
            let registration = self
                .0
                .discover()
                .map_err(native_failure)?
                .ok_or_else(|| native_failure(ServiceFailure::Missing))?;
            if registration.server_pin().identity != server.bytes() {
                return Err(native_failure(ServiceFailure::IdentityMismatch));
            }
            spaceterm_host::account_service::runtime_inventory::RuntimeCustody::prepare_start_for_epoch(&registration, signer.as_ref(), next_epoch).map_err(native_failure)?;
            Ok(())
        })
    }
    fn reconcile_previous_instance<'a>(
        &'a self,
        server: ServerId,
        epoch: ServerEpoch,
        expected: &'a [spaceterm_core::retirement::RetiredSession],
    ) -> ServerFuture<'a, Result<Vec<SessionCleanupObservation>, ServerFailure>> {
        Box::pin(async move {
            self.0
                .reconcile_previous_instance(server, epoch, expected)
                .await
                .map_err(native_failure)
        })
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        server: ServerId,
        epoch: ServerEpoch,
        sessions: &'a [SessionKey],
    ) -> ServerFuture<'a, Result<Vec<SessionCleanupObservation>, ServerFailure>> {
        Box::pin(async move {
            self.0
                .previous_instance_cleanup(server, epoch, sessions)
                .await
                .map_err(native_failure)
        })
    }
}
impl StoppedAuthorizationPort for NativeManagement {
    fn verify_stopped(
        &self,
        server: ServerId,
        epoch: ServerEpoch,
    ) -> Result<(NativeManagementRequestRef, NativeRetirementAction), ServerFailure> {
        self.0
            .verify_retirement_request(server, epoch)
            .map_err(native_failure)
    }
}
fn native_failure(failure_value: ServiceFailure) -> ServerFailure {
    failure(match failure_value {
        ServiceFailure::IdentityMismatch | ServiceFailure::RequestConflict => {
            ServerFailureClass::ScopeMismatch
        }
        ServiceFailure::StorageLocked | ServiceFailure::StorageUnavailable => {
            ServerFailureClass::StorageUnavailable
        }
        ServiceFailure::CleanupUnresolved => ServerFailureClass::CleanupIncomplete,
        _ => ServerFailureClass::NativeUnavailable,
    })
}

/// Recovery needs a lookup before its actor exists. This source remains denied
/// until bound once to that recovered actor, before any listener is opened.
#[derive(Default)]
struct ActorPairing(Mutex<Option<ServerHandle>>);
impl PairingSnapshotPort for Arc<ActorPairing> {
    fn snapshot(&self) -> Result<PairingSnapshot, ProtocolError> {
        let source = self.0.lock().map_err(|_| ProtocolError::PairingDenied)?;
        PairingSnapshotPort::snapshot(source.as_ref().ok_or(ProtocolError::PairingDenied)?)
    }
}

/// Pairing revocation is installed by the listener decorator. Git's dispatcher
/// owns write scheduling. Neither requires a second store-backed projection.
struct ActorExtensions;
impl ExtensionObserverPort for ActorExtensions {
    fn admitted(
        &self,
        _: CanonicalOperationRequest,
        _: CommitReceipt,
        _: ServerHandle,
    ) -> Result<(), ServerFailure> {
        Err(failure(ServerFailureClass::ScopeMismatch))
    }
    fn pairing_committed(&self, _: &PairingWrites, _: CommitReceipt) -> Result<(), ServerFailure> {
        Ok(())
    }
    fn confirmed_directory_committed(&self, _: &ConfirmedDirectoryObservation, _: CommitReceipt) {}
}

/// One recovered actor, its real transports and its original native instance lease.
/// Constructor injection supplies trusted host facts, never protocol path authority.
pub struct RecoveredAccountServer {
    server: RecoveredServer,
    listeners: ListenerFactories,
    pairing: PairingService,
    management: Arc<NativeManagement>,
    admission: Arc<spaceterm_host::account_service::AccountStartup>,
    owner_supervisor: Arc<ServiceSupervisor>,
    account_runtime: Arc<crate::account_runtime::AccountRuntimeProcesses>,
}
impl RecoveredAccountServer {
    pub async fn recover(
        supervisor: Arc<ServiceSupervisor>,
        startup: AccountStartup,
        launch: AccountLaunchFacts,
    ) -> Result<Self, StartupFailure> {
        Self::recover_with_adapters(supervisor, startup, launch, None, None).await
    }

    /// Supplies native process resources and supported protocol profiles while
    /// retaining the production account store, actor, authentication and listeners.
    pub async fn recover_with_native_pty(
        supervisor: Arc<ServiceSupervisor>,
        startup: AccountStartup,
        launch: AccountLaunchFacts,
        pty: Arc<dyn spaceterm_host::pty::NativePtyAdapterFactory>,
        protocol_majors: Vec<u32>,
    ) -> Result<Self, StartupFailure> {
        Self::recover_with_adapters(
            supervisor,
            startup,
            launch,
            Some(pty),
            Some(protocol_majors),
        )
        .await
    }

    async fn recover_with_adapters(
        supervisor: Arc<ServiceSupervisor>,
        startup: AccountStartup,
        launch: AccountLaunchFacts,
        pty: Option<Arc<dyn spaceterm_host::pty::NativePtyAdapterFactory>>,
        protocol_majors: Option<Vec<u32>>,
    ) -> Result<Self, StartupFailure> {
        if launch.uid != startup.registration().key().account_uid
            || launch.uid == 0
            || !launch.home.is_absolute()
        {
            return Err(StartupFailure::Account(ServiceFailure::IdentityMismatch));
        }
        let server_id = ServerId::new(startup.registration().server_pin().identity);
        let home = ServerDirectory::new(
            server_id,
            launch.home.as_os_str().as_encoded_bytes().to_vec(),
        )
        .map_err(|_| StartupFailure::Account(ServiceFailure::InvalidRecord))?;
        let mut account = startup.open_store(home.clone())?;
        let admission = account.admission.ok_or(StartupFailure::Account(
            ServiceFailure::ConfirmationRequired,
        ))?;
        admission.revalidate().map_err(StartupFailure::Account)?;
        let recovered = account
            .store
            .recover_bundle()
            .map_err(StartupFailure::Store)?
            .core;
        let sessions: Vec<_> = crate::recovery::expected_cleanup(&recovered)
            .map_err(StartupFailure::Runtime)?
            .keys()
            .copied()
            .collect();
        spaceterm_host::account_service::runtime_inventory::StoppedRuntimeCustody::resume_start(
            admission.as_ref(),
            recovered.last_epoch,
            &sessions,
            account.credentials.as_ref(),
        )
        .map_err(StartupFailure::Account)?;
        let identity = StoreIdentity {
            server: server_id,
            account: account.registration.key(),
            server_public_key: account.credentials.pin().public_key,
        };
        let clock = Arc::new(NativeRuntimeClock::default());
        let directories = Arc::new(
            NativeDirectoryProvider::new(server_id, launch.home.clone()).map_err(|_| {
                StartupFailure::Runtime(failure(ServerFailureClass::RuntimeUnavailable))
            })?,
        );
        let git_host = Arc::new(NativeServerGitHost::for_account(
            PathBuf::from("/usr/bin/git"),
            launch.home.clone(),
            spaceterm_host::git::MINIMUM_GIT_VERSION,
            directories.clone(),
        ));
        let core = CorePorts::production_with_git_host(
            identity,
            home.clone(),
            clock.clone(),
            git_host.clone(),
        )
        .map_err(StartupFailure::Runtime)?;
        let external = Arc::new(
            GitExternalWork::new(identity, home, clock.clone(), git_host.clone())
                .map_err(StartupFailure::Runtime)?,
        );
        let uid = launch.uid;
        let launch_provider = Arc::new(AccountLaunch(launch));
        let runtime = match pty {
            Some(pty) => NativeRuntime::with_pty_factory(
                server_id,
                uid,
                launch_provider,
                clock.clone(),
                RuntimeLimits::default(),
                pty,
            ),
            None => NativeRuntime::new(
                server_id,
                uid,
                launch_provider,
                clock.clone(),
                RuntimeLimits::default(),
            ),
        }
        .map_err(StartupFailure::Runtime)?;
        let management = Arc::new(NativeManagement(
            supervisor.clone(),
            Some(account.credentials.clone()),
        ));
        let native_status = crate::listeners::AccountNativeStatusObserver::new(supervisor.clone());
        let ports = match protocol_majors {
            Some(majors) => ProductionListenerPorts::with_native_status_and_protocol_majors(
                native_status,
                majors,
            )
            .map_err(StartupFailure::Runtime)?,
            None => ProductionListenerPorts::with_native_status(native_status),
        };
        let source = Arc::new(ActorPairing::default());
        let lookup = Arc::new(PairingProtocolLookup::new(source.clone()));
        let config = ServerConfig {
            limits: ServerLimits::default(),
            host: ServerHostContext {
                authority: Arc::new(Admission(admission.clone())),
            },
            core,
            launch: runtime.clone(),
            cleanup: runtime.clone(),
            extensions: ports.extensions(Arc::new(ActorExtensions)),
            initial_appearance: appearance(),
            outcomes: ports.outcomes.clone(),
        };
        let dependencies = ServerDependencies {
            store: Box::new(account.store),
            credentials: account.credentials.clone(),
            pairing: lookup.clone(),
            runtime,
            external_work: external.clone(),
            host_management: management.clone(),
            clock,
        };
        let server = RecoveredServer::recover(config, dependencies)
            .await
            .map_err(StartupFailure::Runtime)?;
        let handle = server.handle();
        *source.0.lock().map_err(|_| {
            StartupFailure::Runtime(failure(ServerFailureClass::RecoveryRequired))
        })? = Some(handle.clone());
        let account_runtime = crate::account_runtime::AccountRuntimeProcesses::new(
            handle.clone(),
            supervisor.as_ref().clone(),
            account.credentials.clone(),
        )
        .map_err(StartupFailure::Account)?;
        ports
            .install_account_runtime(account_runtime.clone())
            .map_err(StartupFailure::Runtime)?;
        let git = GitService::new(
            server_id,
            handle.snapshot().epoch(),
            git_host,
            Arc::new(handle.clone()),
            GitLimits::default(),
        )
        .map_err(|_| StartupFailure::Runtime(failure(ServerFailureClass::RuntimeUnavailable)))?;
        git.recover(Vec::new())
            .await
            .map_err(|_| StartupFailure::Runtime(failure(ServerFailureClass::RecoveryRequired)))?;
        external
            .bind(git.clone())
            .map_err(StartupFailure::Runtime)?;
        let codec = Arc::new(CanonicalCodec);
        let pairing = PairingService::new(
            Arc::new(handle.clone()),
            account.credentials.clone(),
            codec.clone(),
            Arc::new(AccountOwnerEnrollment::new(supervisor.clone())),
            account.registration.server_pin(),
        )
        .map_err(|_| StartupFailure::Runtime(failure(ServerFailureClass::Authentication)))?;
        let directory_service = Arc::new(DirectoryService::new(
            server_id,
            directories,
            Arc::new(handle.clone()),
        ));
        let listeners = production_listener_factories_with_dispatch(
            handle,
            ProductionListenerConfig {
                registration: &account.registration,
                identity: account.credentials,
                running: crate::distribution_metadata::version()
                    .map_err(StartupFailure::Distribution)?,
                codec: codec.clone(),
                pairing: pairing.clone(),
                ports,
            },
            lookup,
            Arc::new(DirectoryOrdinaryDispatch::new(
                directory_service,
                Arc::new(GitOrdinaryDispatch::new(git, codec)),
            )),
        )
        .map_err(StartupFailure::Runtime)?;
        Ok(Self {
            server,
            listeners,
            pairing,
            management,
            admission,
            owner_supervisor: supervisor,
            account_runtime,
        })
    }
    pub fn handle(&self) -> ServerHandle {
        self.server.handle()
    }
    pub fn pairing(&self) -> &PairingService {
        &self.pairing
    }

    /// A durable native retirement is verified again by the actor. An OS signal
    /// ends this instance gracefully without changing the retained Running intent.
    pub async fn run_until(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> Result<ShutdownReport, StartupFailure> {
        let Self {
            server,
            listeners,
            management,
            admission,
            owner_supervisor,
            account_runtime,
            ..
        } = self;
        admission.revalidate().map_err(StartupFailure::Account)?;
        let owner_endpoint =
            match crate::deployment::owner::OwnerEndpoint::start(owner_supervisor, server.handle())
            {
                Ok(endpoint) => endpoint,
                Err(_) => {
                    server
                        .close_owner_enrollment()
                        .await
                        .map_err(StartupFailure::Runtime)?;
                    return Err(StartupFailure::Runtime(failure(
                        ServerFailureClass::NativeUnavailable,
                    )));
                }
            };
        let handle = server.handle();
        let cleanup = account_runtime.clone();
        let monitor = async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            #[cfg(target_os = "macos")]
            let mut checkpoint_ticks = 0_u8;
            loop {
                interval.tick().await;
                #[cfg(target_os = "macos")]
                {
                    if checkpoint_ticks == 0 {
                        // Sampling is not absence evidence. A failed or missed sample
                        // retains conservative refusal rather than fabricating ancestry.
                        let _ = account_runtime.checkpoint_lineage();
                    }
                    checkpoint_ticks = (checkpoint_ticks + 1) % 10;
                }
                let snapshot = handle.snapshot();
                if management
                    .verify_stopped(snapshot.state().server, snapshot.epoch())
                    .is_ok()
                {
                    if account_runtime.start_original_retirement().await.is_ok() {
                        std::future::pending::<()>().await;
                    }
                    break;
                }
            }
        };
        let shutdown = async {
            tokio::select! { () = shutdown => {}, () = monitor => {} }
        };
        let result = server
            .run_until(listeners, shutdown)
            .await
            .map_err(StartupFailure::Runtime);
        let owner_closed = owner_endpoint
            .close()
            .await
            .map_err(|_| StartupFailure::Runtime(failure(ServerFailureClass::CleanupIncomplete)));
        let retirements = cleanup
            .finish_retirements()
            .await
            .map_err(StartupFailure::Account);
        // Hold admission until both the actor and the owner endpoint have joined
        // and signed whole-instance cleanup has been durably published.
        owner_closed?;
        retirements?;
        let report = result?;
        cleanup
            .finish_instance(report)
            .map_err(StartupFailure::Account)?;
        drop(admission);
        Ok(report)
    }
}
fn appearance() -> RuntimeAppearance {
    let colors = TerminalColors::default();
    let rgb = |value: spaceterm_terminal::appearance::Color| spaceterm_core::content::Rgb {
        r: value.r,
        g: value.g,
        b: value.b,
    };
    // Match the existing terminal snapshot's configured palette until the
    // controlling client supplies its appearance. No server theme is introduced.
    let mut palette = [rgb(colors.foreground); 256];
    palette[..8].copy_from_slice(&colors.normal.map(rgb));
    palette[8..16].copy_from_slice(&colors.bright.map(rgb));
    RuntimeAppearance {
        generation: AppearanceGeneration::INITIAL,
        foreground: rgb(colors.foreground),
        background: rgb(colors.background),
        palette,
        bold_as_bright: false,
        appearance: Appearance::Dark,
    }
}

/// Native account discovery supplies shell and home. Ambient HOME and SHELL do
/// not select registration, runtime identity or local filesystem authority.
fn native_launch(
    facts: &TrustedAccountFacts,
    home: PathBuf,
) -> Result<AccountLaunchFacts, StartupFailure> {
    let executable = std::env::current_exe()
        .map_err(|_| StartupFailure::Account(ServiceFailure::SupervisorUnavailable))?;
    let parent = executable
        .parent()
        .ok_or(StartupFailure::Account(ServiceFailure::InvalidRecord))?;
    #[cfg(target_os = "macos")]
    let resources = parent.join("../Resources");
    #[cfg(target_os = "linux")]
    let resources = parent.join("../share/spaceterm-server");
    let planner = ShellLaunchPlanner::new(
        facts.login_shell().to_owned(),
        resources.clone(),
        ShellIntegrationMode::Automatic,
        ShellEnvironment {
            xdg_data_dirs: std::env::var_os("XDG_DATA_DIRS"),
            zdotdir: std::env::var_os("ZDOTDIR"),
            env: std::env::var_os("ENV"),
        },
        ShellIntegrationPolicy {
            supported: true,
            path_list_separator: ':',
            fallback_xdg_data_dirs: "/usr/local/share:/usr/share".into(),
        },
        ShellTerminalIdentity {
            fallback_term: "xterm-256color",
            colorterm: "truecolor",
            program_name: "SpaceTerm",
            program_version: env!("SPACETERM_SERVER_VERSION"),
            terminfo: Some(ShellTerminfoIdentity {
                directory: resources.join("terminfo"),
                terminal_name: "xterm-spaceterm",
            }),
        },
    );
    Ok(AccountLaunchFacts {
        uid: facts.uid(),
        machine: LocalMachine::new(None, None, home.to_str()),
        home,
        planner,
    })
}

pub async fn production_run() -> Result<ShutdownReport, StartupFailure> {
    let facts = TrustedAccountFacts::current().map_err(StartupFailure::Account)?;
    let supervisor = Arc::new(ServiceSupervisor::current(None).map_err(StartupFailure::Account)?);
    let startup =
        AccountStartup::admit_with_storage_wait(&supervisor, Arc::new(super::NativeStartupStorage))
            .await?;
    let launch = native_launch(&facts, facts.home().to_owned())?;
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|_| StartupFailure::Account(ServiceFailure::SupervisorUnavailable))?;
    let mut interrupt =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|_| StartupFailure::Account(ServiceFailure::SupervisorUnavailable))?;
    RecoveredAccountServer::recover(supervisor, startup, launch)
        .await?
        .run_until(async move {
            tokio::select! { _ = terminate.recv() => {}, _ = interrupt.recv() => {} }
        })
        .await
}

struct UnadmittedOwner;
impl StartupAuthorityPort for UnadmittedOwner {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure> {
        Err(failure(ServerFailureClass::Authentication))
    }
    fn validate_credentials(
        &self,
        _: &dyn ServerCredentialPort,
        _: ServerId,
    ) -> Result<(), ServerFailure> {
        Err(failure(ServerFailureClass::Authentication))
    }
}

/// Reuses production recovery providers while the existing stopped executor owns
/// the one database and instance lease. No service Start or listener is opened.
pub async fn production_stopped_owner(
    supervisor: Arc<ServiceSupervisor>,
    home: PathBuf,
    request: spaceterm_host::native_deployment::contracts::NativeRequestId,
    claim: &spaceterm_protocol::pairing_adapter::VerifiedDeviceClaim,
) -> Result<
    crate::account_owner_enrollment::AccountOwnerRequestExecutor,
    spaceterm_core::pairing::PairingFailure,
> {
    use crate::account_owner_enrollment::{AccountOwnerRequestExecutor, StoppedOwnerProviders};
    use spaceterm_core::pairing::PairingFailure;
    let map = |_: StartupFailure| PairingFailure::AuthorizationUnavailable;
    let registration = supervisor
        .discover()
        .map_err(super::owner_failure)?
        .ok_or(PairingFailure::AuthorizationRequired)?;
    let server = ServerId::new(registration.server_pin().identity);
    let directory = ServerDirectory::new(server, home.as_os_str().as_encoded_bytes().to_vec())
        .map_err(|_| PairingFailure::InvalidRequest)?;
    let identity = StoreIdentity {
        server,
        account: registration.key(),
        server_public_key: registration.server_pin().public_key,
    };
    let facts = TrustedAccountFacts::current().map_err(super::owner_failure)?;
    if facts.uid() != identity.account.account_uid {
        return Err(PairingFailure::IdentityMismatch);
    }
    let launch = native_launch(&facts, home.clone()).map_err(map)?;
    let clock = Arc::new(NativeRuntimeClock::default());
    let directories = Arc::new(
        NativeDirectoryProvider::new(server, home.clone())
            .map_err(|_| PairingFailure::AuthorizationUnavailable)?,
    );
    let git_host = Arc::new(NativeServerGitHost::for_account(
        PathBuf::from("/usr/bin/git"),
        home,
        spaceterm_host::git::MINIMUM_GIT_VERSION,
        directories,
    ));
    let external = Arc::new(
        GitExternalWork::new(identity, directory.clone(), clock.clone(), git_host.clone())
            .map_err(|_| PairingFailure::RecoveryRequired)?,
    );
    let runtime = NativeRuntime::new(
        server,
        facts.uid(),
        Arc::new(AccountLaunch(launch)),
        clock.clone(),
        RuntimeLimits::default(),
    )
    .map_err(|_| PairingFailure::AuthorizationUnavailable)?;
    let ports = ProductionListenerPorts::new();
    let config = ServerConfig {
        limits: ServerLimits::default(),
        // The stopped constructor replaces this denied authority only after its
        // retained native request and exclusive stopped admission are validated.
        host: ServerHostContext {
            authority: Arc::new(UnadmittedOwner),
        },
        core: CorePorts::production(identity, directory.clone(), clock.clone())
            .map_err(|_| PairingFailure::RecoveryRequired)?,
        launch: runtime.clone(),
        cleanup: runtime,
        extensions: Arc::new(ActorExtensions),
        initial_appearance: appearance(),
        outcomes: ports.outcomes.clone(),
    };
    let executor = AccountOwnerRequestExecutor::stopped(
        supervisor.clone(),
        directory,
        request,
        claim,
        StoppedOwnerProviders {
            config,
            host_management: Arc::new(NativeManagement(supervisor, None)),
            external_work: external.clone(),
            clock,
        },
    )
    .await?;
    let handle = executor.handle();
    let composed = async {
        let git = GitService::new(
            server,
            handle.snapshot().epoch(),
            git_host,
            Arc::new(handle),
            GitLimits::default(),
        )
        .map_err(|_| PairingFailure::RecoveryRequired)?;
        git.recover(Vec::new())
            .await
            .map_err(|_| PairingFailure::RecoveryRequired)?;
        external
            .bind(git)
            .map_err(|_| PairingFailure::RecoveryRequired)
    }
    .await;
    if let Err(error) = composed {
        let _ = executor.close().await;
        return Err(error);
    }
    Ok(executor)
}
