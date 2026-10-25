//! Temporary SSH provisioning with paired WSS admission as its completion boundary.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{
    askpass::AskPassAttemptFactory,
    command::{
        OpenSshExecutable, SshCapability, SshCapabilityProbe, SshCommandContext,
        ValidatedRemoteShellCommand,
    },
    host_config::{HostConfigFilesystem, HostConfigFilesystemError},
    process::{
        CapturedEffectError, CapturedProcessError, CapturedProcessOutput, SshProcessAdapter,
        SshProcessCounts, SshProcessEnvironment, TrackedSshProcessAdapter, run_captured_effect,
        run_captured_process, run_captured_transfer,
    },
    startup_environment::StartupSshEnvironment,
};
use crate::{
    application_identity::ApplicationIdentity,
    cancellation::CancellationToken,
    direct_connections::{DirectConnections, DirectRouteId},
    domain::SshDestination,
    platform::app_paths::AppPaths,
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
    service::verify_candidate_bytes,
};

const OBSERVATION: Duration = Duration::from_secs(15);
const MAX_DISCOVERY: usize = 16 * 1024;
const MAX_ARTIFACT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub(crate) struct SetupApprovals {
    pub install: Option<InstallationApproval>,
    pub start: Option<StartupApproval>,
    pub expose_network: Option<NetworkApproval>,
}
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct InstallationApproval {
    destination: SshDestination,
    account: u32,
    target: String,
    digest: String,
}
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct StartupApproval {
    destination: SshDestination,
    account: u32,
    server: Option<[u8; 32]>,
    build: String,
    commit: String,
    stopped: bool,
}
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct NetworkApproval {
    destination: SshDestination,
    account: u32,
    pin: ServerPin,
    endpoint: SocketAddr,
    policy: Option<NetworkPolicy>,
}
impl std::fmt::Debug for InstallationApproval {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InstallationApproval(<redacted>)")
    }
}
impl std::fmt::Debug for StartupApproval {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StartupApproval(<redacted>)")
    }
}
impl std::fmt::Debug for NetworkApproval {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NetworkApproval(<redacted>)")
    }
}
pub(crate) struct SetupRequest {
    pub destination: SshDestination,
    pub endpoint: SocketAddr,
    pub name: ClientName,
    pub prompts: Arc<dyn AskPassAttemptFactory>,
    pub approvals: SetupApprovals,
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
    InstallationOffered {
        version: String,
        platform: (String, String),
        approval: InstallationApproval,
    },
    StartupRequired {
        approval: StartupApproval,
    },
    NetworkAuthorizationRequired {
        current: Option<SocketAddr>,
        approval: NetworkApproval,
    },
    Incompatible(IncompatibilityKind),
    Refused(RefusalKind),
    Unavailable(UnavailableKind),
    Unconfirmed(SetupOperation),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EndpointProblem {
    InvalidAddress,
}
pub(crate) fn validate_endpoint(endpoint: SocketAddr) -> Result<(), EndpointProblem> {
    if endpoint.port() == 0
        || endpoint.ip().is_unspecified()
        || endpoint.ip().is_multicast()
        || endpoint.ip().is_loopback()
    {
        return Err(EndpointProblem::InvalidAddress);
    }
    Ok(())
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
    config: NativeSshSetupConfig<TrackedSshProcessAdapter<A>>,
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
        Self {
            config: NativeSshSetupConfig {
                paths,
                filesystem,
                bootstrap,
                home,
                startup,
                executable,
                adapter: TrackedSshProcessAdapter::new(adapter),
                artifacts,
            },
            direct,
        }
    }
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
        let result = self.run_setup(request, cancellation, progress);
        if self.process_counts().active() != 0 {
            return Err(SetupFailure::Unconfirmed(SetupOperation::Enrollment));
        }
        result
    }
}

impl<A: SshProcessAdapter> NativeSshSetup<A> {
    fn run_setup(
        &self,
        request: SetupRequest,
        cancellation: &CancellationToken,
        progress: &SetupProgress,
    ) -> Result<DirectRouteId, SetupFailure> {
        validate_endpoint(request.endpoint)
            .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidEndpoint))?;
        check_cancelled(cancellation)?;
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
        let session = SetupSession {
            config: &self.config,
            destination: &request.destination,
            environment,
            completed_effect: std::cell::Cell::new(None),
        };
        progress.report(SetupPhase::Discovering);
        let discovered = session.discover(&ssh_cancellation)?;
        let discovery = match discovered {
            Discovery::Existing(discovery) => discovery,
            Discovery::Missing { account, target } => {
                let artifact = self
                    .config
                    .artifacts
                    .candidate(self.config.filesystem.as_ref(), &target)?;
                authorize_installation(
                    &request.destination,
                    account,
                    &target,
                    &artifact,
                    request.approvals.install.as_ref(),
                )?;
                progress.report(SetupPhase::Installing);
                session.install(&artifact, &ssh_cancellation)?;
                match session.discover(&ssh_cancellation) {
                    Ok(Discovery::Existing(discovery))
                        if discovery.account == account && discovery.target == target =>
                    {
                        discovery
                    }
                    _ => return Err(SetupFailure::Unconfirmed(SetupOperation::Install)),
                }
            }
        };
        let discovery = if !discovery.running {
            authorize_start(
                &request.destination,
                &discovery,
                request.approvals.start.as_ref(),
            )?;
            progress.report(SetupPhase::Starting);
            let action = if discovery.stopped {
                "service-start"
            } else {
                "ssh-prepare"
            };
            session.effect(action, Vec::new(), &ssh_cancellation, SetupOperation::Start)?;
            match session.discover(&ssh_cancellation) {
                Ok(Discovery::Existing(current))
                    if current.running
                        && current.account == discovery.account
                        && current.server == discovery.server =>
                {
                    current
                }
                _ => return Err(SetupFailure::Unconfirmed(SetupOperation::Start)),
            }
        } else {
            discovery
        };
        let status = session.read("setup-direct-status", &ssh_cancellation)?;
        let status: DirectStatus = serde_json::from_slice(&status.stdout)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::ServiceUnavailable))?;
        if status.pin.build != identity() || Some(status.pin.server) != discovery.server {
            return Err(SetupFailure::Refused(RefusalKind::ServerChanged));
        }
        let network_approval = NetworkApproval {
            destination: request.destination.clone(),
            account: discovery.account,
            pin: status.pin.clone(),
            endpoint: request.endpoint,
            policy: status.policy.clone(),
        };
        if request.approvals.expose_network.as_ref() != Some(&network_approval)
            && (request.approvals.expose_network.is_some()
                || status.policy.as_ref().map(|policy| policy.bind) != Some(request.endpoint))
        {
            return Err(SetupFailure::NetworkAuthorizationRequired {
                current: status.policy.as_ref().map(|policy| policy.bind),
                approval: network_approval,
            });
        }
        session.check_cancelled(&ssh_cancellation)?;
        let endpoint = format!("wss://{}/spaceterm", request.endpoint);
        let prepared = self
            .direct
            .prepare_setup(endpoint.clone(), status.pin.clone())
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
        })
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::ServiceUnavailable))?;
        progress.report(SetupPhase::Enrolling);
        let prepared_reply = session.effect(
            &format!("setup-direct-prepare {}", request.endpoint),
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
                &format!("setup-direct {}", request.endpoint),
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
        if self.process_counts().active() != 0 {
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
                    map_direct_error(error)
                }
            })
    }
}

struct SetupSession<'a, A: SshProcessAdapter> {
    config: &'a NativeSshSetupConfig<A>,
    destination: &'a SshDestination,
    environment: SshProcessEnvironment,
    completed_effect: std::cell::Cell<Option<SetupOperation>>,
}
impl<A: SshProcessAdapter> Drop for SetupSession<'_, A> {
    fn drop(&mut self) {
        self.environment.finish_authentication();
    }
}
impl<A: SshProcessAdapter> SetupSession<'_, A> {
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
        Ok(SshCommandContext::setup(
            self.config.executable.clone(),
            self.destination,
            command,
        ))
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
            Instant::now() + OBSERVATION,
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
        let command = SshCommandContext::setup_cleanup(
            self.config.executable.clone(),
            self.destination,
            command,
        );
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
            Instant::now() + OBSERVATION,
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

fn authorize_installation(
    destination: &SshDestination,
    account: u32,
    target: &str,
    artifact: &VerifiedArtifact,
    approved: Option<&InstallationApproval>,
) -> Result<(), SetupFailure> {
    let approval = InstallationApproval {
        destination: destination.clone(),
        account,
        target: target.to_owned(),
        digest: artifact.digest.clone(),
    };
    if approved == Some(&approval) {
        return Ok(());
    }
    Err(SetupFailure::InstallationOffered {
        version: artifact.version.clone(),
        platform: artifact.platform.clone(),
        approval,
    })
}
fn authorize_start(
    destination: &SshDestination,
    discovery: &AccountDiscovery,
    approved: Option<&StartupApproval>,
) -> Result<(), SetupFailure> {
    let approval = StartupApproval {
        destination: destination.clone(),
        account: discovery.account,
        server: discovery.server,
        build: discovery.build.clone(),
        commit: discovery.commit.clone(),
        stopped: discovery.stopped,
    };
    if approved == Some(&approval) {
        return Ok(());
    }
    Err(SetupFailure::StartupRequired { approval })
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
    Existing(AccountDiscovery),
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
    Ok(Discovery::Existing(discovery))
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

#[cfg(test)]
pub(crate) mod testing {
    use super::{InstallationApproval, NetworkApproval, ServerPin, StartupApproval};
    use crate::domain::SshDestination;

    /// An approval bound to `destination`, as an offer from that host carries.
    pub(crate) fn installation_approval(destination: &str) -> InstallationApproval {
        InstallationApproval {
            destination: SshDestination::new(destination.to_owned()).unwrap(),
            account: 501,
            target: "x86_64-unknown-linux-gnu".into(),
            digest: "a".repeat(64),
        }
    }

    pub(crate) fn startup_approval(destination: &str) -> StartupApproval {
        StartupApproval {
            destination: SshDestination::new(destination.to_owned()).unwrap(),
            account: 501,
            server: Some([1; 32]),
            build: super::identity().into(),
            commit: "a".repeat(40),
            stopped: true,
        }
    }

    pub(crate) fn network_approval(destination: &str) -> NetworkApproval {
        NetworkApproval {
            destination: SshDestination::new(destination.to_owned()).unwrap(),
            account: 501,
            pin: ServerPin {
                build: super::identity().into(),
                server: [1; 32],
                public_key: [2; 32],
                certificate: [3; 32],
            },
            endpoint: "192.0.2.1:9231".parse().unwrap(),
            policy: None,
        }
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "../platform/unix_adapter_tests/setup_consent.rs"]
mod consent_tests;

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "../platform/unix_adapter_tests/server_setup.rs"]
mod unix_adapter_tests;
