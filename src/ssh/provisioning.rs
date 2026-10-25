//! Temporary SSH provisioning with paired WSS admission as its completion boundary.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(all(test, feature = "native-tests"))]
use super::process::{SshProcessCounts, TrackedSshProcessAdapter};
use super::{
    askpass::{AskPassAttemptFactory, AskPassBrokerLease},
    command::{
        OpenSshExecutable, SshCapability, SshCapabilityProbe, SshCommandContext,
        ValidatedRemoteShellCommand, setup_configuration, validate_setup_configuration_path,
    },
    host_config::{HostConfigFilesystem, HostConfigFilesystemError},
    process::{
        CapturedEffectError, CapturedProcessDeadline, CapturedProcessError, CapturedProcessOutput,
        SetupMaster, SshProcessAdapter, SshProcessEnvironment, run_captured_effect,
        run_captured_process, run_captured_transfer,
    },
    startup_environment::StartupSshEnvironment,
};
use crate::{
    application_identity::ApplicationIdentity,
    cancellation::CancellationToken,
    direct_connections::{DirectConnections, DirectRouteId},
    domain::SshDestination,
    platform::app_paths::{AppPaths, RuntimeOwner},
};
#[cfg(all(test, feature = "native-tests"))]
use sha2::{Digest, Sha256};
use spaceterm_server::{
    ServerError,
    direct::{DirectError, DirectStatus, NetworkPolicy, ServerPin},
    pane::control::ClientName,
    provisioning::{
        AccountDiscovery, SetupCancellation, SetupCancellationReply,
        SetupReply as EnrollmentResponse, SetupRequest as EnrollmentRequest,
    },
    service::{SetupStartObservation, verify_candidate_bytes},
};

const OBSERVATION: Duration = Duration::from_secs(15);
const MAX_DISCOVERY: usize = 16 * 1024;
const MAX_ARTIFACT: u64 = 256 * 1024 * 1024;

pub(crate) use spaceterm_server::direct::Endpoint as SetupAddress;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SetupOffer {
    Installation {
        version: String,
        platform: (String, String),
    },
    Startup,
    Network {
        address: SetupAddress,
        bind: SocketAddr,
        current: Option<SocketAddr>,
        enabled: bool,
    },
}

/// Waits for consent on the setup worker, returning false when declined or cancelled.
/// Implementations must observe cancellation while waiting for their window's response.
pub(crate) trait SetupApprovalHandler: Send + Sync {
    fn approve(&self, offer: SetupOffer, cancellation: &CancellationToken) -> bool;
}

pub(crate) struct SetupRequest {
    pub destination: SshDestination,
    pub address: Option<SetupAddress>,
    pub name: ClientName,
    pub prompts: Arc<dyn AskPassAttemptFactory>,
    pub approvals: Arc<dyn SetupApprovalHandler>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetupPhase {
    Authenticating,
    Discovering,
    Installing,
    Starting,
    Enrolling,
    Connecting,
}
pub(crate) struct SetupProgress(Arc<dyn Fn(SetupPhase) + Send + Sync>);
impl SetupProgress {
    pub(crate) fn new(callback: impl Fn(SetupPhase) + Send + Sync + 'static) -> Self {
        Self(Arc::new(callback))
    }
    fn report(&self, phase: SetupPhase) {
        (self.0)(phase);
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetupOperation {
    Install,
    Start,
    Enrollment,
    Save,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IncompatibilityKind {
    Protocol,
    Capability,
    Platform,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefusalKind {
    AuthorizationRefused,
    InstallRefused,
    MissingArtifact,
    InvalidArtifact,
    ServerChanged,
    InvalidEndpoint,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnavailableKind {
    SshFailed,
    HostUnreachable,
    ServiceUnavailable,
    EndpointUnreachable,
    CredentialUnavailable,
    Storage,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SetupFailure {
    Cancelled,
    Unreachable { address: SetupAddress },
    Incompatible(IncompatibilityKind),
    Refused(RefusalKind),
    Unavailable(UnavailableKind),
    Unconfirmed(SetupOperation),
}
pub(crate) trait SshSetupBackend: Send + Sync {
    fn setup(
        &self,
        request: SetupRequest,
        cancellation: &CancellationToken,
        progress: &SetupProgress,
    ) -> Result<DirectRouteId, SetupFailure>;
}

pub(crate) struct NativeSshSetupConfig<A: SshProcessAdapter> {
    pub paths: Arc<AppPaths>,
    pub filesystem: Arc<dyn HostConfigFilesystem>,
    pub bootstrap: Arc<dyn SshServerBootstrap>,
    pub home: PathBuf,
    pub startup: StartupSshEnvironment,
    pub executable: OpenSshExecutable,
    pub adapter: A,
    pub artifacts: ServerArtifactCatalog,
}

pub(crate) struct NativeSshSetup<A: SshProcessAdapter> {
    #[cfg(all(test, feature = "native-tests"))]
    config: NativeSshSetupConfig<TrackedSshProcessAdapter<A>>,
    #[cfg(not(all(test, feature = "native-tests")))]
    config: NativeSshSetupConfig<A>,
    direct: DirectConnections,
}

impl<A: SshProcessAdapter> NativeSshSetup<A> {
    pub(crate) fn new(config: NativeSshSetupConfig<A>, direct: DirectConnections) -> Self {
        let NativeSshSetupConfig {
            paths,
            filesystem,
            bootstrap,
            home,
            startup,
            executable,
            adapter,
            artifacts,
        } = config;
        #[cfg(all(test, feature = "native-tests"))]
        let adapter = TrackedSshProcessAdapter::new(adapter);
        Self {
            config: NativeSshSetupConfig {
                paths,
                filesystem,
                bootstrap,
                home,
                startup,
                executable,
                adapter,
                artifacts,
            },
            direct,
        }
    }
    #[cfg(all(test, feature = "native-tests"))]
    pub(crate) fn process_counts(&self) -> SshProcessCounts {
        self.config.adapter.counts()
    }
}

impl<A: SshProcessAdapter> SshSetupBackend for NativeSshSetup<A> {
    fn setup(
        &self,
        request: SetupRequest,
        cancellation: &CancellationToken,
        progress: &SetupProgress,
    ) -> Result<DirectRouteId, SetupFailure> {
        self.run_setup(request, cancellation, progress)
    }
}

impl<A: SshProcessAdapter> NativeSshSetup<A> {
    fn run_setup(
        &self,
        request: SetupRequest,
        cancellation: &CancellationToken,
        progress: &SetupProgress,
    ) -> Result<DirectRouteId, SetupFailure> {
        check_cancelled(cancellation)?;
        validate_setup_configuration_path(self.config.executable.as_path())
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let configuration_owner = self
            .config
            .paths
            .create_runtime_owner("s")
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let configuration = configuration_owner
            .create_private_file(
                "config",
                &setup_configuration(&self.config.home)
                    .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?,
            )
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        validate_setup_configuration_path(&configuration)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let socket = configuration_owner
            .socket_path("m")
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        validate_setup_configuration_path(&socket)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        progress.report(SetupPhase::Authenticating);
        let probe = SshCapabilityProbe::from_startup(
            self.config.executable.clone(),
            self.config.home.clone(),
            &self.config.startup,
            self.config.adapter.clone(),
        )
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let capability = probe.probe(cancellation);
        check_cancelled(cancellation)?;
        if let SshCapability::Unavailable(reason) = capability {
            let _classification = reason;
            return Err(SetupFailure::Unavailable(UnavailableKind::SshFailed));
        }
        let attempt = request
            .prompts
            .start_attempt(&self.config.paths)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let _authentication = SetupAuthentication(attempt.lease.clone());
        let authentication_cancel =
            CancellationToken::observing(attempt.observation.cancellation_flag());
        let ssh_cancellation = CancellationToken::linked(cancellation, &authentication_cancel);
        let environment = match SshProcessEnvironment::new(
            self.config.home.clone(),
            attempt.lease.clone(),
            &self.config.startup,
        ) {
            Ok(environment) => environment,
            Err(_) => {
                attempt.lease.finish();
                return Err(SetupFailure::Unavailable(UnavailableKind::SshFailed));
            }
        };
        let resolve = SshCommandContext::resolve_setup(
            self.config.executable.clone(),
            &configuration,
            &request.destination,
        )
        .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidEndpoint))?;
        let resolved = run_captured_process(
            &self.config.adapter,
            &resolve,
            &environment,
            vec![],
            256 * 1024,
            &ssh_cancellation,
            Instant::now() + OBSERVATION,
        )
        .map_err(map_process_error)?;
        let address = match request.address.clone() {
            Some(address) => address,
            None => resolved_setup_address(&resolved)?,
        };
        let master_command = SshCommandContext::setup_master(
            self.config.executable.clone(),
            &configuration,
            &socket,
            &request.destination,
        )
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let master = SetupMaster::start(
            &self.config.adapter,
            &master_command,
            &environment,
            &ssh_cancellation,
        );
        // Authentication can fail after OpenSSH publishes its socket but before the ready
        // preamble arrives. Capture that socket after the process has been reaped too.
        let registered_socket = configuration_owner.register_socket("m");
        let master = master.map_err(map_process_error)?;
        registered_socket.map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let session = SetupSession {
            config: &self.config,
            destination: &request.destination,
            environment,
            configuration,
            socket,
            master,
            _configuration_owner: configuration_owner,
            completed_effect: std::cell::Cell::new(None),
        };
        progress.report(SetupPhase::Discovering);
        let mut discovered = session.discover(&ssh_cancellation)?;
        let discovery = loop {
            match discovered {
                Discovery::Existing(discovery) => break discovery,
                Discovery::Missing { account, target } => {
                    let artifact = self
                        .config
                        .artifacts
                        .candidate(self.config.filesystem.as_ref(), &target)?;
                    session.approve(
                        request.approvals.as_ref(),
                        SetupOffer::Installation {
                            version: artifact.version.clone(),
                            platform: artifact.platform.clone(),
                        },
                        &ssh_cancellation,
                    )?;
                    let current = self
                        .config
                        .artifacts
                        .candidate(self.config.filesystem.as_ref(), &target)?;
                    discovered = session.discover(&ssh_cancellation)?;
                    if current.digest != artifact.digest
                        || !matches!(&discovered,
                    Discovery::Missing { account: current_account, target: current_target } if *current_account == account && current_target == &target)
                    {
                        continue;
                    }
                    progress.report(SetupPhase::Installing);
                    session.install(&artifact, &ssh_cancellation)?;
                    match session.discover(&ssh_cancellation) {
                        Ok(Discovery::Existing(discovery))
                            if discovery.account == account && discovery.target == target =>
                        {
                            break discovery;
                        }
                        _ => return Err(SetupFailure::Unconfirmed(SetupOperation::Install)),
                    }
                }
            }
        };
        let mut discovery = discovery;
        while !discovery.running {
            let observation = startup_observation(&discovery)?;
            session.approve(
                request.approvals.as_ref(),
                SetupOffer::Startup,
                &ssh_cancellation,
            )?;
            let current = match session.discover(&ssh_cancellation)? {
                Discovery::Existing(current) => current,
                Discovery::Missing { .. } => {
                    return Err(SetupFailure::Refused(RefusalKind::ServerChanged));
                }
            };
            if current.start_observation.as_ref() != Some(&observation) {
                discovery = current;
                continue;
            }
            progress.report(SetupPhase::Starting);
            let input = serde_json::to_vec(&observation)
                .map_err(|_| SetupFailure::Refused(RefusalKind::ServerChanged))?;
            session.effect(
                "setup-start",
                input,
                &ssh_cancellation,
                SetupOperation::Start,
            )?;
            match session.discover(&ssh_cancellation) {
                Ok(Discovery::Existing(current))
                    if current.running
                        && current.account == discovery.account
                        && current.server == discovery.server
                        && current.start_observation.as_ref().is_some_and(|current| {
                            current.account == observation.account
                                && current.build == observation.build
                                && current.server == observation.server
                                && current.installation == observation.installation
                                && current.version == observation.version
                                && current.commit == observation.commit
                                && !current.stopped
                        }) =>
                {
                    discovery = current;
                }
                _ => return Err(SetupFailure::Unconfirmed(SetupOperation::Start)),
            }
        }
        let (status, bind) = loop {
            let status = session.direct_status(&ssh_cancellation)?;
            if status.pin.build != identity() || Some(status.pin.server) != discovery.server {
                return Err(SetupFailure::Refused(RefusalKind::ServerChanged));
            }
            let bind_ip = status
                .policy
                .as_ref()
                .map(|policy| policy.bind.ip())
                .filter(|configured| {
                    // An explicit reachable address may forward to another server interface.
                    // The default hostname follows the authenticated SSH destination instead.
                    request.address.is_some()
                        || configured.is_unspecified()
                        || Some(*configured) == discovery.ssh_local_address
                })
                .or(discovery.ssh_local_address)
                .ok_or(SetupFailure::Refused(RefusalKind::InvalidEndpoint))?;
            let bind = request
                .address
                .as_ref()
                .and_then(|_| status.policy.as_ref().map(|policy| policy.bind))
                .unwrap_or_else(|| SocketAddr::new(bind_ip, address.port()));
            NetworkPolicy::new(bind)
                .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidEndpoint))?;
            if !status.enabled || status.policy.as_ref().map(|policy| policy.bind) != Some(bind) {
                session.approve(
                    request.approvals.as_ref(),
                    SetupOffer::Network {
                        address: address.clone(),
                        bind,
                        current: status.policy.as_ref().map(|policy| policy.bind),
                        enabled: status.enabled,
                    },
                    &ssh_cancellation,
                )?;
                let current = session.direct_status(&ssh_cancellation)?;
                if current.pin != status.pin
                    || current.policy != status.policy
                    || current.enabled != status.enabled
                {
                    if current.pin != status.pin {
                        return Err(SetupFailure::Refused(RefusalKind::ServerChanged));
                    }
                    continue;
                }
            }
            break (status, bind);
        };
        session.check_cancelled(&ssh_cancellation)?;
        let endpoint = address.url();
        let prepared = self
            .direct
            .prepare_setup(
                endpoint.clone(),
                status.pin.clone(),
                status.setup_generation,
            )
            .map_err(|error| match error {
                DirectError::StorageUnavailable => {
                    SetupFailure::Unavailable(UnavailableKind::Storage)
                }
                error => map_direct_error(error),
            })?;
        let input = serde_json::to_vec(&EnrollmentRequest {
            operation: prepared.operation(),
            public_key: prepared.public_key(),
            name: request.name,
            pin: status.pin.clone(),
            expected_policy: status.policy.clone(),
            expected_enabled: status.enabled,
            endpoint: endpoint.clone(),
            generation: status.setup_generation,
        })
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::ServiceUnavailable))?;
        progress.report(SetupPhase::Enrolling);
        let prepared_reply = session.effect(
            &format!("setup-direct-prepare {}", bind),
            input.clone(),
            &ssh_cancellation,
            SetupOperation::Enrollment,
        );
        let prepared_reply = prepared_reply.and_then(|output| {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Prepared {
                prepared: bool,
            }
            let reply: Prepared = serde_json::from_slice(&output.stdout)
                .map_err(|_| SetupFailure::Unconfirmed(SetupOperation::Enrollment))?;
            if !reply.prepared {
                return Err(SetupFailure::Unconfirmed(SetupOperation::Enrollment));
            }
            Ok(())
        });
        if let Err(error) = prepared_reply {
            let confirmed = session.cancel_enrollment(prepared.operation(), &status.pin);
            drop(session);
            if confirmed {
                let _ = self.direct.confirm_setup_cancelled(&prepared);
            } else {
                let _ = self.direct.cancel_setup(&prepared);
            }
            return Err(error);
        }
        let enrolled = session
            .effect(
                &format!("setup-direct {}", bind),
                input,
                &ssh_cancellation,
                SetupOperation::Enrollment,
            )
            .and_then(|output| {
                let response: EnrollmentResponse = serde_json::from_slice(&output.stdout)
                    .map_err(|_| SetupFailure::Unconfirmed(SetupOperation::Enrollment))?;
                if response.schema != 1
                    || response.endpoint != endpoint
                    || response.paired.pin != status.pin
                {
                    return Err(SetupFailure::Refused(RefusalKind::ServerChanged));
                }
                Ok(response)
            });
        let cancelled = cancellation.is_cancelled();
        let cleanup_confirmed = (enrolled.is_err() || cancelled)
            && session.cancel_enrollment(prepared.operation(), &status.pin);
        // Every command is reaped synchronously. The broker/socket also closes before WSS.
        drop(session);
        let enrolled = match enrolled {
            Ok(enrolled) => enrolled,
            Err(error) => {
                if cleanup_confirmed {
                    let _ = self.direct.confirm_setup_cancelled(&prepared);
                } else {
                    let _ = self.direct.cancel_setup(&prepared);
                }
                return Err(error);
            }
        };
        if cancelled || cancellation.is_cancelled() {
            if cleanup_confirmed {
                let _ = self.direct.confirm_setup_cancelled(&prepared);
            } else {
                let _ = self.direct.cancel_setup(&prepared);
            }
            return Err(SetupFailure::Unconfirmed(SetupOperation::Enrollment));
        }
        progress.report(SetupPhase::Connecting);
        self.direct
            .complete_setup(
                &prepared,
                enrolled.paired,
                enrolled.server_name,
                cancellation,
            )
            .map_err(|error| {
                if cancellation.is_cancelled() {
                    SetupFailure::Unconfirmed(SetupOperation::Enrollment)
                } else {
                    match map_direct_error(error) {
                        SetupFailure::Unavailable(UnavailableKind::EndpointUnreachable) => {
                            SetupFailure::Unreachable {
                                address: address.clone(),
                            }
                        }
                        failure => failure,
                    }
                }
            })
    }
}

struct SetupAuthentication(AskPassBrokerLease);
impl Drop for SetupAuthentication {
    fn drop(&mut self) {
        self.0.finish();
    }
}

struct SetupSession<'a, A: SshProcessAdapter> {
    config: &'a NativeSshSetupConfig<A>,
    destination: &'a SshDestination,
    environment: SshProcessEnvironment,
    configuration: PathBuf,
    socket: PathBuf,
    master: SetupMaster<A>,
    _configuration_owner: RuntimeOwner,
    completed_effect: std::cell::Cell<Option<SetupOperation>>,
}
impl<A: SshProcessAdapter> Drop for SetupSession<'_, A> {
    fn drop(&mut self) {
        self.master.close();
        self.environment.finish_authentication();
    }
}
impl<A: SshProcessAdapter> SetupSession<'_, A> {
    fn approve(
        &self,
        approvals: &dyn SetupApprovalHandler,
        offer: SetupOffer,
        cancellation: &CancellationToken,
    ) -> Result<(), SetupFailure> {
        self.check_cancelled(cancellation)?;
        if !approvals.approve(offer, cancellation) {
            return Err(self.process_failure(CapturedProcessError::Cancelled));
        }
        self.check_cancelled(cancellation)
    }
    fn direct_status(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<DirectStatus, SetupFailure> {
        serde_json::from_slice(&self.read("setup-direct-status", cancellation)?.stdout)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::ServiceUnavailable))
    }
    fn process_failure(&self, error: CapturedProcessError) -> SetupFailure {
        if matches!(error, CapturedProcessError::Cancelled)
            && let Some(operation) = self.completed_effect.get()
        {
            return SetupFailure::Unconfirmed(operation);
        }
        map_process_error(error)
    }
    fn check_cancelled(&self, cancellation: &CancellationToken) -> Result<(), SetupFailure> {
        if cancellation.is_cancelled() {
            return Err(self.process_failure(CapturedProcessError::Cancelled));
        }
        Ok(())
    }
    fn command(&self, script: &str) -> Result<super::command::SshCommandSpec, SetupFailure> {
        let command = ValidatedRemoteShellCommand::server_script(script)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        SshCommandContext::setup_channel(
            self.config.executable.clone(),
            &self.configuration,
            &self.socket,
            self.destination,
            command,
        )
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))
    }
    fn read(
        &self,
        action: &str,
        cancellation: &CancellationToken,
    ) -> Result<CapturedProcessOutput, SetupFailure> {
        let command = self.command(&self.config.bootstrap.locator(
            identity(),
            action,
            action == "ssh-discover",
        ))?;
        let output = run_captured_process(
            &self.config.adapter,
            &command,
            &self.environment,
            Vec::new(),
            MAX_DISCOVERY,
            cancellation,
            CapturedProcessDeadline::RemoteCommand(OBSERVATION),
        )
        .map_err(|error| self.process_failure(error))?;
        if !output.exit.is_success() {
            return Err(SetupFailure::Unavailable(
                UnavailableKind::ServiceUnavailable,
            ));
        }
        Ok(output)
    }
    fn cancel_enrollment(&self, operation: [u8; 32], pin: &ServerPin) -> bool {
        let Ok(command) = ValidatedRemoteShellCommand::server_script(
            &self
                .config
                .bootstrap
                .locator(identity(), "setup-direct-cancel", false),
        ) else {
            return false;
        };
        let Ok(command) = SshCommandContext::setup_channel(
            self.config.executable.clone(),
            &self.configuration,
            &self.socket,
            self.destination,
            command,
        ) else {
            return false;
        };
        let Ok(input) = serde_json::to_vec(&SetupCancellation {
            operation,
            pin: pin.clone(),
        }) else {
            return false;
        };
        let result = run_captured_effect(
            &self.config.adapter,
            &command,
            &self.environment,
            input,
            MAX_DISCOVERY,
            &CancellationToken::default(),
            Instant::now() + OBSERVATION,
        );
        match result {
            Ok(output) if output.exit.is_success() => {
                serde_json::from_slice::<SetupCancellationReply>(&output.stdout).is_ok_and(
                    |reply| reply.cancelled && reply.operation == operation && reply.pin == *pin,
                )
            }
            _ => false,
        }
    }
    fn discover(&self, cancellation: &CancellationToken) -> Result<Discovery, SetupFailure> {
        parse_discovery(&self.read("ssh-discover", cancellation)?.stdout)
    }
    fn effect(
        &self,
        action: &str,
        input: Vec<u8>,
        cancellation: &CancellationToken,
        operation: SetupOperation,
    ) -> Result<CapturedProcessOutput, SetupFailure> {
        let command = self.command(&self.config.bootstrap.locator(identity(), action, false))?;
        let output = run_captured_effect(
            &self.config.adapter,
            &command,
            &self.environment,
            input,
            MAX_DISCOVERY,
            cancellation,
            CapturedProcessDeadline::RemoteCommand(OBSERVATION),
        )
        .map_err(|error| match error {
            CapturedEffectError::NotAdmitted(error) => self.process_failure(error),
            CapturedEffectError::Unconfirmed => SetupFailure::Unconfirmed(operation),
        })?;
        if !output.exit.is_success() {
            return Err(SetupFailure::Unconfirmed(operation));
        }
        self.completed_effect.set(Some(operation));
        Ok(output)
    }
    fn install(
        &self,
        artifact: &VerifiedArtifact,
        cancellation: &CancellationToken,
    ) -> Result<(), SetupFailure> {
        let transfer = |password, input, cancellation: &CancellationToken| {
            let command = self.command(&self.config.bootstrap.install(
                identity(),
                artifact.bytes.len(),
                &artifact.signature,
                &artifact.digest,
                password,
            ))?;
            run_captured_transfer(
                &self.config.adapter,
                &command,
                &self.environment,
                input,
                MAX_DISCOVERY,
                cancellation,
            )
            .map_err(|_| SetupFailure::Unconfirmed(SetupOperation::Install))
        };
        self.check_cancelled(cancellation)?;
        let mut output = transfer(false, artifact.bytes.clone(), cancellation)?;
        if output.exit.code() == Some(77) {
            let prompt = self
                .environment
                .remote_authorization_command()
                .map_err(|_| SetupFailure::Refused(RefusalKind::AuthorizationRefused))?;
            let answer = run_captured_transfer(
                &self.config.adapter,
                &prompt,
                &self.environment,
                Vec::new(),
                4096,
                cancellation,
            )
            .map_err(|error| self.process_failure(error))?;
            let mut secret = zeroize::Zeroizing::new(answer.stdout);
            if !answer.exit.is_success() {
                return Err(SetupFailure::Refused(RefusalKind::AuthorizationRefused));
            }
            if secret.last() == Some(&b'\n') {
                secret.pop();
            }
            if secret.iter().any(|byte| matches!(byte, b'\n' | b'\r' | 0)) {
                return Err(SetupFailure::Refused(RefusalKind::AuthorizationRefused));
            }
            let mut input = zeroize::Zeroizing::new(Vec::with_capacity(
                secret.len() + artifact.bytes.len() + 1,
            ));
            input.extend_from_slice(&secret);
            input.push(b'\n');
            input.extend_from_slice(&artifact.bytes);
            output = transfer(true, std::mem::take(&mut *input), cancellation)?;
            if output.exit.code() == Some(77) {
                return Err(SetupFailure::Refused(RefusalKind::AuthorizationRefused));
            }
        }
        if !output.exit.is_success() {
            return Err(SetupFailure::Refused(RefusalKind::InstallRefused));
        }
        self.completed_effect.set(Some(SetupOperation::Install));
        Ok(())
    }
}

fn startup_observation(
    discovery: &AccountDiscovery,
) -> Result<SetupStartObservation, SetupFailure> {
    let observation = discovery
        .start_observation
        .as_ref()
        .ok_or(SetupFailure::Refused(RefusalKind::ServerChanged))?;
    if observation.account != discovery.account
        || observation.build != discovery.build
        || Some(observation.server) != discovery.server
        || observation.stopped != discovery.stopped
    {
        return Err(SetupFailure::Refused(RefusalKind::ServerChanged));
    }
    Ok(observation.clone())
}

fn resolved_setup_address(output: &CapturedProcessOutput) -> Result<SetupAddress, SetupFailure> {
    if !output.exit.is_success() {
        return Err(SetupFailure::Refused(RefusalKind::InvalidEndpoint));
    }
    let output = std::str::from_utf8(&output.stdout)
        .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidEndpoint))?;
    let mut hostname = None;
    let mut port = None;
    for line in output.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "hostname" if hostname.is_none() => hostname = Some(value),
            "port" if port.is_none() => port = value.parse::<u16>().ok().filter(|port| *port != 0),
            "hostname" | "port" => return Err(SetupFailure::Refused(RefusalKind::InvalidEndpoint)),
            _ => (),
        }
    }
    // SSH's port selects its authentication transport, never the WSS listener.
    if port.is_none() {
        return Err(SetupFailure::Refused(RefusalKind::InvalidEndpoint));
    }
    SetupAddress::parse(hostname.ok_or(SetupFailure::Refused(RefusalKind::InvalidEndpoint))?)
        .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidEndpoint))
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), SetupFailure> {
    if cancellation.is_cancelled() {
        Err(SetupFailure::Cancelled)
    } else {
        Ok(())
    }
}
fn map_process_error(error: CapturedProcessError) -> SetupFailure {
    match error {
        CapturedProcessError::Cancelled => SetupFailure::Cancelled,
        _ => SetupFailure::Unavailable(UnavailableKind::HostUnreachable),
    }
}
fn map_direct_error(error: DirectError) -> SetupFailure {
    match error {
        DirectError::CredentialUnavailable | DirectError::InteractionRequired => {
            SetupFailure::Unavailable(UnavailableKind::CredentialUnavailable)
        }
        DirectError::StorageUnavailable => SetupFailure::Unconfirmed(SetupOperation::Save),
        DirectError::PolicyChanged => SetupFailure::Unconfirmed(SetupOperation::Enrollment),
        DirectError::IncompatibleProtocol => {
            SetupFailure::Incompatible(IncompatibilityKind::Protocol)
        }
        DirectError::PinMismatch
        | DirectError::WrongServer
        | DirectError::AuthenticationRefused
        | DirectError::Substituted
        | DirectError::Revoked => SetupFailure::Refused(RefusalKind::ServerChanged),
        _ => SetupFailure::Unavailable(UnavailableKind::EndpointUnreachable),
    }
}
fn map_server_error(error: ServerError) -> SetupFailure {
    match error {
        ServerError::IncompatibleProtocol => {
            SetupFailure::Incompatible(IncompatibilityKind::Protocol)
        }
        ServerError::MissingRequiredCapability => {
            SetupFailure::Incompatible(IncompatibilityKind::Capability)
        }
        ServerError::AuthorizationRequired => {
            SetupFailure::Refused(RefusalKind::AuthorizationRefused)
        }
        _ => SetupFailure::Refused(RefusalKind::InvalidArtifact),
    }
}

pub(crate) trait SshServerBootstrap: Send + Sync {
    fn locator(&self, identity: &str, action: &str, missing: bool) -> String;
    fn install(
        &self,
        identity: &str,
        archive_length: usize,
        signature: &[u8],
        digest: &str,
        password: bool,
    ) -> String;
}

/// Signed server packages captured by host composition.
#[derive(Clone)]
pub(crate) struct ServerArtifactCatalog {
    root: Option<PathBuf>,
}
impl ServerArtifactCatalog {
    pub(crate) fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }
    fn candidate(
        &self,
        filesystem: &dyn HostConfigFilesystem,
        target: &str,
    ) -> Result<VerifiedArtifact, SetupFailure> {
        let platform = platform(target)?;
        let directory = self
            .root
            .as_ref()
            .ok_or(SetupFailure::Refused(RefusalKind::MissingArtifact))?
            .join(target);
        let archive = directory.join("artifact.tar.gz");
        let signature_path = directory.join("artifact.sig");
        let bytes = read_bounded(
            filesystem,
            &archive,
            MAX_ARTIFACT,
            RefusalKind::MissingArtifact,
        )?;
        // A present archive without its authentication material is an unverified package.
        let signature = read_bounded(
            filesystem,
            &signature_path,
            64,
            RefusalKind::InvalidArtifact,
        )?;
        let digest_bytes = read_bounded(
            filesystem,
            &directory.join("artifact.sha256"),
            65,
            RefusalKind::InvalidArtifact,
        )?;
        let digest = std::str::from_utf8(&digest_bytes)
            .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidArtifact))?
            .trim()
            .to_owned();
        let metadata = verify_candidate_bytes(
            identity(),
            &bytes,
            &signature,
            &digest,
            target,
            1,
            &["workspace_policy"],
        )
        .map_err(map_server_error)?;
        Ok(VerifiedArtifact {
            bytes,
            signature,
            digest,
            version: metadata.version,
            platform,
        })
    }
}
fn read_bounded(
    filesystem: &dyn HostConfigFilesystem,
    path: &std::path::Path,
    limit: u64,
    missing: RefusalKind,
) -> Result<Vec<u8>, SetupFailure> {
    let bytes = filesystem
        .read_artifact_file_limited(path, limit as usize)
        .map_err(|error| {
            SetupFailure::Refused(match error {
                HostConfigFilesystemError::Missing => missing,
                HostConfigFilesystemError::Unavailable => RefusalKind::InvalidArtifact,
            })
        })?;
    if bytes.len() as u64 > limit {
        return Err(SetupFailure::Refused(RefusalKind::InvalidArtifact));
    }
    Ok(bytes)
}
pub(super) struct VerifiedArtifact {
    bytes: Vec<u8>,
    signature: Vec<u8>,
    digest: String,
    version: String,
    platform: (String, String),
}
fn identity() -> &'static str {
    ApplicationIdentity::current().directory_name()
}
#[cfg(all(test, feature = "native-tests"))]
fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn platform(target: &str) -> Result<(String, String), SetupFailure> {
    match target {
        "aarch64-apple-darwin" => Ok(("macOS".into(), "arm64".into())),
        "x86_64-unknown-linux-gnu" => Ok(("linux".into(), "x86_64".into())),
        _ => Err(SetupFailure::Incompatible(IncompatibilityKind::Platform)),
    }
}
enum Discovery {
    Existing(Box<AccountDiscovery>),
    Missing { account: u32, target: String },
}
fn parse_discovery(bytes: &[u8]) -> Result<Discovery, SetupFailure> {
    if bytes.len() > MAX_DISCOVERY {
        return Err(SetupFailure::Unavailable(
            UnavailableKind::ServiceUnavailable,
        ));
    }
    if let Ok(text) = std::str::from_utf8(bytes)
        && let Some(missing) = text.strip_prefix("SPACETERM_MISSING ")
    {
        let parts: Vec<_> = missing.split_whitespace().collect();
        if let [account, target] = parts.as_slice() {
            let account = account
                .parse::<u32>()
                .ok()
                .filter(|account| *account != 0)
                .ok_or(SetupFailure::Refused(RefusalKind::InstallRefused))?;
            platform(target)?;
            return Ok(Discovery::Missing {
                account,
                target: (*target).to_owned(),
            });
        }
    }
    let discovery: AccountDiscovery = serde_json::from_slice(bytes)
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::ServiceUnavailable))?;
    validate_discovery(&discovery)?;
    Ok(Discovery::Existing(Box::new(discovery)))
}
fn validate_discovery(discovery: &AccountDiscovery) -> Result<(), SetupFailure> {
    if discovery.schema != 1 || discovery.account == 0 || discovery.build != identity() {
        return Err(SetupFailure::Refused(RefusalKind::ServerChanged));
    }
    platform(&discovery.target)?;
    if !discovery.protocol_majors.contains(&1) {
        return Err(SetupFailure::Incompatible(IncompatibilityKind::Protocol));
    }
    if !discovery
        .capabilities
        .iter()
        .any(|capability| capability == "workspace_policy")
    {
        return Err(SetupFailure::Incompatible(IncompatibilityKind::Capability));
    }
    Ok(())
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "../platform/unix_adapter_tests/server_setup.rs"]
mod unix_adapter_tests;
