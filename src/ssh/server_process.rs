//! OpenSSH account-server discovery, verified first installation and non-PTY transport.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use gpui::BackgroundExecutor;
use sha2::{Digest, Sha256};
use spaceterm_server::{
    LocalServer, ServerError,
    account_bridge::{AccountBridge, AccountBridgeIdentity, AccountConnector, AccountDiscovery},
    service::Service,
};

use super::askpass::AskPassAttemptFactory;
use super::cancellation::SshCancellationToken;
use super::command::{OpenSshExecutable, SshCommandContext, ValidatedRemoteShellCommand};
use super::control_connection::{
    ControlConnectionState, ControlConnectionTiming, OpenSshControlConnection,
};
use super::host_config::HostConfigFilesystem;
use super::process::{
    CapturedProcessError, SshProcessAdapter, SshProcessCleanup, SshProcessEnvironment,
    SshProcessSupervisor, run_captured_process, run_captured_transfer, spawn_account_bridge,
};
use super::quiet_continuation::{ConfigProof, QuietSshRoute};
use super::server_connection::*;
use super::startup_environment::StartupSshEnvironment;
use crate::application_identity::ApplicationIdentity;
use crate::domain::SshDestination;
use crate::platform::{app_paths::AppPaths, control_socket::ControlSocketProbe};

const OBSERVATION: Duration = Duration::from_secs(15);
const MAX_DISCOVERY: usize = 16 * 1024;
const MAX_ARTIFACT: u64 = 256 * 1024 * 1024;

/// Captured source directory; no candidate is inspected until an authenticated account is missing.
#[derive(Clone)]
pub(crate) struct SshServerArtifacts {
    root: Option<PathBuf>,
}
impl SshServerArtifacts {
    pub(crate) fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }
    fn candidate(
        &self,
        filesystem: &dyn HostConfigFilesystem,
        target: &str,
    ) -> Result<VerifiedArtifact, SshConnectFailure> {
        let platform = platform(target)?;
        let directory = self
            .root
            .as_ref()
            .ok_or(SshConnectFailure::Unavailable(
                UnavailableKind::CandidateUnavailable,
            ))?
            .join(target);
        let archive = directory.join("artifact.tar.gz");
        let signature = directory.join("artifact.sig");
        let digest_bytes = read_bounded(filesystem, &directory.join("artifact.sha256"), 65)?;
        let digest = std::str::from_utf8(&digest_bytes)
            .map_err(|_| SshConnectFailure::Refused(RefusalKind::InvalidArtifact))?
            .trim()
            .to_owned();
        let metadata = Service::for_identity(identity())
            .and_then(|service| {
                service.inspect_candidate_for_target(
                    &archive,
                    &signature,
                    &digest,
                    target,
                    1,
                    &["workspace_policy"],
                )
            })
            .map_err(map_server_error)?;
        let bytes = read_bounded(filesystem, &archive, MAX_ARTIFACT)?;
        let signature = read_bounded(filesystem, &signature, 64)?;
        // Pin the bytes that cross SSH, rather than rereading a replaceable source after verification.
        if signature.len() != 64 || hex_digest(&bytes) != digest {
            return Err(SshConnectFailure::Refused(RefusalKind::InvalidArtifact));
        }
        // A second verification detects replacement of either source while it was being read.
        let second = Service::for_identity(identity())
            .and_then(|service| {
                service.inspect_candidate_for_target(
                    &archive,
                    &directory.join("artifact.sig"),
                    &digest,
                    target,
                    1,
                    &["workspace_policy"],
                )
            })
            .map_err(map_server_error)?;
        if second != metadata {
            return Err(SshConnectFailure::Refused(RefusalKind::InvalidArtifact));
        }
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
) -> Result<Vec<u8>, SshConnectFailure> {
    let bytes = filesystem
        .read_file_limited(path, limit as usize)
        .map_err(|_| SshConnectFailure::Refused(RefusalKind::InvalidArtifact))?;
    if bytes.len() as u64 > limit {
        return Err(SshConnectFailure::Refused(RefusalKind::InvalidArtifact));
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
impl VerifiedArtifact {
    /// The verified whole-package digest, which names this build everywhere it is retained.
    pub(super) fn digest(&self) -> &str {
        &self.digest
    }
    pub(super) fn version(&self) -> &str {
        &self.version
    }
}

/// All dependencies are captured by host composition; construction starts no SSH process.
pub(crate) struct NativeSshServerConfig<A: SshProcessAdapter> {
    pub paths: Arc<AppPaths>,
    pub filesystem: Arc<dyn HostConfigFilesystem>,
    pub bootstrap: Arc<dyn SshServerBootstrap>,
    pub home: PathBuf,
    pub startup: StartupSshEnvironment,
    pub executable: OpenSshExecutable,
    pub adapter: A,
    pub socket_probe: Arc<dyn ControlSocketProbe>,
    pub askpass: Arc<dyn AskPassAttemptFactory>,
    pub executor: BackgroundExecutor,
    pub artifacts: SshServerArtifacts,
}
type QuietRoutes = Mutex<HashMap<String, ([u8; 32], Arc<QuietSshRoute>)>>;

pub(crate) struct NativeSshConnectBackend<A: SshProcessAdapter> {
    config: Arc<NativeSshServerConfig<A>>,
    routes: Mutex<HashMap<String, Arc<NativeRoute<A>>>>,
    clients: Arc<Mutex<HashMap<[u8; 32], NativeCachedClient<A>>>>,
    quiet: QuietRoutes,
}
impl<A: SshProcessAdapter> NativeSshConnectBackend<A> {
    pub(crate) fn new(config: NativeSshServerConfig<A>) -> Self {
        Self {
            config: Arc::new(config),
            routes: Mutex::new(HashMap::new()),
            clients: Arc::new(Mutex::new(HashMap::new())),
            quiet: Mutex::new(HashMap::new()),
        }
    }
    fn route(
        &self,
        destination: &SshDestination,
        mode: ConnectMode,
        cancellation: &SshCancellationToken,
        progress: &ConnectProgress<'_>,
    ) -> Result<Arc<NativeRoute<A>>, SshConnectFailure> {
        if cancellation.is_cancelled() {
            return Err(SshConnectFailure::Cancelled);
        }
        let existing = self
            .routes
            .lock()
            .ok()
            .and_then(|routes| routes.get(destination.as_str()).cloned());
        if let Some(route) = existing
            && route.is_alive()
        {
            return Ok(route);
        }
        let (environment, route_id, frozen, proof_inputs, authentication_cancel) = if mode
            == ConnectMode::QuietContinuation
        {
            let quiet = self
                .quiet
                .lock()
                .ok()
                .and_then(|proofs| proofs.get(destination.as_str()).cloned())
                .ok_or(SshConnectFailure::NeedsPerson(
                    InteractionReason::CredentialProvider,
                ))?;
            if !quiet.1.unchanged() {
                return Err(SshConnectFailure::Changed(ContinuityChange::HostChanged));
            }
            let environment = SshProcessEnvironment::noninteractive(
                self.config.home.clone(),
                &self.config.startup,
            )
            .map_err(|_| SshConnectFailure::NeedsPerson(InteractionReason::CredentialProvider))?;
            (
                environment,
                quiet.0,
                Some(quiet.1.config_path()),
                None,
                SshCancellationToken::default(),
            )
        } else {
            progress(ConnectPhase::Authenticating);
            ensure_connection_configuration(&self.config.paths)?;
            let attempt = self
                .config
                .askpass
                .start_attempt(&self.config.paths)
                .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::SshFailed))?;
            let authentication_cancel =
                SshCancellationToken::observing(attempt.observation.cancellation_flag());
            let environment = SshProcessEnvironment::new(
                self.config.home.clone(),
                attempt.lease,
                &self.config.startup,
            )
            .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::SshFailed))?;
            let commands = SshCommandContext::new(
                self.config.executable.clone(),
                self.config.paths.managed_ssh_config(),
                destination.clone(),
                self.config.paths.managed_ssh_config(),
            )
            .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::SshFailed))?;
            let proof = ConfigProof::capture(
                self.config.filesystem.as_ref(),
                &self.config.paths.managed_ssh_config(),
                &self.config.home,
            );
            let configuration = run_captured_process(
                &self.config.adapter,
                &commands.configuration(),
                &environment,
                Vec::new(),
                256 * 1024,
                cancellation,
                Instant::now() + OBSERVATION,
            )
            .map_err(map_process_error)?;
            if !configuration.exit.is_success() {
                return Err(SshConnectFailure::Unavailable(UnavailableKind::SshFailed));
            }
            let route_id: [u8; 32] = Sha256::digest(&configuration.stdout).into();
            (
                environment,
                route_id,
                None,
                proof.map(|proof| (proof, configuration.stdout)),
                authentication_cancel,
            )
        };
        let cancellation = SshCancellationToken::linked(cancellation, &authentication_cancel);
        let (scope, _cleanup) = SshProcessCleanup::scope();
        let backend = Arc::new(
            SshProcessSupervisor::new(
                self.config.executor.clone(),
                environment.clone(),
                self.config.adapter.clone(),
                scope,
            )
            .on_worker(),
        );
        let control = pollster::block_on(OpenSshControlConnection::connect_configuration(
            &self.config.paths,
            self.config.executable.clone(),
            self.config.socket_probe.as_ref(),
            destination.clone(),
            backend,
            &cancellation,
            if mode == ConnectMode::QuietContinuation {
                ControlConnectionTiming::quiet()
            } else {
                ControlConnectionTiming::default()
            },
            frozen,
        ))
        .map_err(|error| map_control_error(error, mode))?;
        if cancellation.is_cancelled() {
            return Err(SshConnectFailure::Cancelled);
        }
        if let Some((proof, effective)) = proof_inputs {
            let proof =
                QuietSshRoute::prove(&self.config, &effective, proof, &cancellation).map(Arc::new);
            if let Ok(mut proofs) = self.quiet.lock() {
                proofs.remove(destination.as_str());
                if let Some(proof) = proof {
                    proofs.insert(destination.as_str().to_owned(), (route_id, proof));
                }
            }
        }
        let route = Arc::new(NativeRoute {
            control: Mutex::new(control),
            config: self.config.clone(),
            environment,
            route_id,
            clients: self.clients.clone(),
            cancellation: cancellation.clone(),
            presented: std::sync::atomic::AtomicBool::new(false),
            claims: std::sync::atomic::AtomicUsize::new(0),
        });
        {
            let mut routes = self
                .routes
                .lock()
                .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::SshFailed))?;
            if cancellation.is_cancelled() {
                return Err(SshConnectFailure::Cancelled);
            }
            routes.insert(destination.as_str().to_owned(), route.clone());
        }
        let weak = Arc::downgrade(&route);
        if std::thread::Builder::new()
            .name("spaceterm-ssh-route-owner".into())
            .spawn(move || {
                loop {
                    let Some(route) = weak.upgrade() else {
                        return;
                    };
                    if route.cancellation.is_cancelled() {
                        route.environment.cancel_authentication();
                        if let Ok(mut control) = route.control.lock() {
                            control.cancel_owned();
                        }
                        return;
                    }
                    drop(route);
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
            .is_err()
        {
            self.disconnect(destination);
            return Err(SshConnectFailure::Unavailable(UnavailableKind::SshFailed));
        }
        Ok(route)
    }
}
impl<A: SshProcessAdapter> NativeSshConnectBackend<A> {
    /// Acquires the route one maintenance exchange runs on, from the cache Connect shares.
    pub(super) fn maintenance_route(
        &self,
        destination: &SshDestination,
        mode: ConnectMode,
        cancellation: &SshCancellationToken,
        progress: &ConnectProgress<'_>,
    ) -> Result<Arc<NativeRoute<A>>, SshConnectFailure> {
        self.route(destination, mode, cancellation, progress)
    }
    /// Ends a route no presented connection depends on, with the processes it owns. A route
    /// Connect presented a server over belongs to the connection registry, which ends it through
    /// Disconnect, so releasing a maintenance exchange leaves it connected.
    pub(super) fn release_owned_route(&self, destination: &SshDestination) {
        let owned = self.routes.lock().ok().and_then(|routes| {
            routes.get(destination.as_str()).map(|route| {
                !route.presented.load(std::sync::atomic::Ordering::Acquire)
                    && route.claims.load(std::sync::atomic::Ordering::Acquire) == 0
            })
        });
        if owned == Some(true) {
            self.disconnect(destination);
        }
    }
}
impl<A: SshProcessAdapter> SshConnectBackend for NativeSshConnectBackend<A> {
    fn disconnect(&self, destination: &SshDestination) {
        let route = self
            .routes
            .lock()
            .ok()
            .and_then(|mut routes| routes.remove(destination.as_str()));
        if let Some(route) = route {
            route.environment.cancel_authentication();
            if let Ok(mut control) = route.control.lock() {
                control.cancel_owned();
            }
        }
    }
    fn forget(&self, destination: &SshDestination) {
        let route_id = self
            .routes
            .lock()
            .ok()
            .and_then(|routes| routes.get(destination.as_str()).map(|route| route.route_id));
        self.disconnect(destination);
        if let Some(route_id) = route_id
            && let Ok(mut clients) = self.clients.lock()
        {
            clients.remove(&route_id);
        }
        if let Ok(mut quiet) = self.quiet.lock() {
            quiet.remove(destination.as_str());
        }
    }
    fn connect(
        &self,
        destination: &SshDestination,
        mode: ConnectMode,
        expected: Option<&SshServerContinuity>,
        cancellation: &SshCancellationToken,
        progress: &ConnectProgress<'_>,
    ) -> Result<ConnectedSshServer, SshConnectFailure> {
        let route = self.route(destination, mode, cancellation, progress)?;
        // A Connect presents its server only at the end, so the route is held meanwhile.
        let _claim = RouteClaim::hold(&route);
        if expected.is_some_and(|expected| expected.route != route.route_id) {
            return Err(SshConnectFailure::Changed(ContinuityChange::HostChanged));
        }
        progress(ConnectPhase::Discovering);
        let discovery = route.discover(cancellation)?;
        match discovery {
            Discovery::Existing(discovery) => {
                route.finish(discovery, expected, cancellation, progress)
            }
            Discovery::Missing { account, target } => {
                if expected.is_some() {
                    return Err(SshConnectFailure::Changed(ContinuityChange::ServerChanged));
                }
                let artifact = route
                    .config
                    .artifacts
                    .candidate(route.config.filesystem.as_ref(), &target)?;
                Err(SshConnectFailure::InstallationOffered(
                    SshInstallationOffer {
                        version: artifact.version.clone(),
                        platform: artifact.platform.clone(),
                        installation: Arc::new(NativeInstallation {
                            route,
                            artifact,
                            account,
                            target,
                        }),
                    },
                ))
            }
        }
    }
    fn observe(
        &self,
        destination: &SshDestination,
        expected: Option<&SshServerContinuity>,
        cancellation: &SshCancellationToken,
        progress: &ConnectProgress<'_>,
    ) -> Result<SshServerObservation, SshConnectFailure> {
        let route = self.route(destination, ConnectMode::Requested, cancellation, progress)?;
        let _claim = RouteClaim::hold(&route);
        if expected.is_some_and(|expected| expected.route != route.route_id) {
            return Err(SshConnectFailure::Changed(ContinuityChange::HostChanged));
        }
        progress(ConnectPhase::Discovering);
        observation(route.discover(cancellation)?, expected, route.route_id)
    }
}
pub(super) struct NativeRoute<A: SshProcessAdapter> {
    control: Mutex<OpenSshControlConnection<SshProcessSupervisor<A>>>,
    config: Arc<NativeSshServerConfig<A>>,
    environment: SshProcessEnvironment,
    route_id: [u8; 32],
    clients: Arc<Mutex<HashMap<[u8; 32], NativeCachedClient<A>>>>,
    cancellation: SshCancellationToken,
    /// Whether Connect has presented a server over this route. Such a route belongs to the
    /// connection registry, which alone ends it.
    presented: std::sync::atomic::AtomicBool,
    /// How much connection work holds this route now. A Connect only presents its server at the
    /// end, so the flag above says nothing about one still underway.
    claims: std::sync::atomic::AtomicUsize,
}

/// One holder of a route, counted while connection work runs on it.
struct RouteClaim<A: SshProcessAdapter>(Arc<NativeRoute<A>>);
impl<A: SshProcessAdapter> RouteClaim<A> {
    fn hold(route: &Arc<NativeRoute<A>>) -> Self {
        route
            .claims
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(Arc::clone(route))
    }
}
impl<A: SshProcessAdapter> Drop for RouteClaim<A> {
    fn drop(&mut self) {
        self.0
            .claims
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
impl<A: SshProcessAdapter> SshServerLease for NativeRoute<A> {
    fn is_alive(&self) -> bool {
        !self.cancellation.is_cancelled()
            && self
                .control
                .lock()
                .is_ok_and(|control| control.state() == ControlConnectionState::Ready)
    }
}
impl<A: SshProcessAdapter> NativeRoute<A> {
    fn command(
        &self,
        script: &str,
    ) -> Result<
        (
            super::command::SshCommandSpec,
            super::live_connection::LiveConnectionCapability,
        ),
        SshConnectFailure,
    > {
        let command = ValidatedRemoteShellCommand::server_script(script)
            .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::SshFailed))?;
        self.control
            .lock()
            .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::SshFailed))?
            .server_command(command)
            .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::Network))
    }
    fn run(
        &self,
        script: &str,
        input: Vec<u8>,
        timeout: Duration,
        cancellation: &SshCancellationToken,
    ) -> Result<super::process::CapturedProcessOutput, SshConnectFailure> {
        let (command, capability) = self.command(script)?;
        capability
            .authorize()
            .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::Network))?;
        let cancellation = SshCancellationToken::linked(cancellation, &capability.cancellation());
        run_captured_process(
            &self.config.adapter,
            &command,
            &self.environment,
            input,
            MAX_DISCOVERY,
            &cancellation,
            Instant::now() + timeout,
        )
        .map_err(map_process_error)
    }
    /// The digest of the configuration that admitted this route, which binds an original to it.
    pub(super) fn digest(&self) -> [u8; 32] {
        self.route_id
    }
    /// One bounded read of the approved installation. Nothing is dispatched, so a lost reply
    /// leaves the account as it was.
    pub(super) fn server_read(
        &self,
        action: &str,
        input: Vec<u8>,
        timeout: Duration,
        cancellation: &SshCancellationToken,
    ) -> Result<super::process::CapturedProcessOutput, SshConnectFailure> {
        self.run(
            &self.config.bootstrap.locator(identity(), action, false),
            input,
            timeout,
            cancellation,
        )
    }
    /// One bounded action of the approved installation that may take effect.
    pub(super) fn server_effect(
        &self,
        action: &str,
        input: Vec<u8>,
        timeout: Duration,
        cancellation: &SshCancellationToken,
    ) -> Result<super::process::CapturedProcessOutput, SshDispatchFailure> {
        use super::process::{CapturedEffectError, run_captured_effect};
        let script = self.config.bootstrap.locator(identity(), action, false);
        let (command, capability) = self
            .command(&script)
            .map_err(SshDispatchFailure::NotDispatched)?;
        capability.authorize().map_err(|_| {
            SshDispatchFailure::NotDispatched(SshConnectFailure::Unavailable(
                UnavailableKind::Network,
            ))
        })?;
        let cancellation = SshCancellationToken::linked(cancellation, &capability.cancellation());
        run_captured_effect(
            &self.config.adapter,
            &command,
            &self.environment,
            input,
            MAX_DISCOVERY,
            &cancellation,
            Instant::now() + timeout,
        )
        .map_err(|error| match error {
            CapturedEffectError::NotAdmitted(error) => {
                SshDispatchFailure::NotDispatched(map_process_error(error))
            }
            CapturedEffectError::Unconfirmed => SshDispatchFailure::Unconfirmed,
        })
    }
    /// The account's existing discovery. For a bound original, a missing server is a changed one.
    pub(super) fn account_discovery(
        &self,
        cancellation: &SshCancellationToken,
    ) -> Result<AccountDiscovery, SshConnectFailure> {
        match self.discover(cancellation)? {
            Discovery::Existing(discovery) => Ok(discovery),
            Discovery::Missing { .. } => {
                Err(SshConnectFailure::Changed(ContinuityChange::ServerChanged))
            }
        }
    }
    /// Verifies the exact artifact this client holds for `target` before anything is transferred.
    pub(super) fn candidate(&self, target: &str) -> Result<VerifiedArtifact, SshConnectFailure> {
        self.config
            .artifacts
            .candidate(self.config.filesystem.as_ref(), target)
    }
    /// Transfers one verified artifact for the account to retain beside its approved
    /// installation. As first installation does, the transfer is cancellable and carries no
    /// whole-journey deadline, while each later observation stays bounded.
    pub(super) fn stage(
        &self,
        artifact: &VerifiedArtifact,
        cancellation: &SshCancellationToken,
    ) -> Result<super::process::CapturedProcessOutput, SshConnectFailure> {
        let script = self.config.bootstrap.stage(
            identity(),
            artifact.bytes.len(),
            &artifact.signature,
            &artifact.digest,
        );
        let (command, capability) = self.command(&script)?;
        capability
            .authorize()
            .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::Network))?;
        let cancellation = SshCancellationToken::linked(cancellation, &capability.cancellation());
        run_captured_transfer(
            &self.config.adapter,
            &command,
            &self.environment,
            artifact.bytes.clone(),
            MAX_DISCOVERY,
            &cancellation,
        )
        .map_err(map_process_error)
    }
    fn prepare_service(
        &self,
        cancellation: &SshCancellationToken,
    ) -> Result<AccountDiscovery, SshConnectFailure> {
        let output = self
            .server_effect("ssh-prepare", Vec::new(), OBSERVATION, cancellation)
            .map_err(|error| match error {
                SshDispatchFailure::NotDispatched(error) => error,
                SshDispatchFailure::Unconfirmed => {
                    SshConnectFailure::Unconfirmed(UnconfirmedOperation::Connect)
                }
            })?;
        observe_preparation(output.exit.is_success(), || self.discover(cancellation))
    }
    fn discover(
        &self,
        cancellation: &SshCancellationToken,
    ) -> Result<Discovery, SshConnectFailure> {
        let output = self.run(
            &self
                .config
                .bootstrap
                .locator(identity(), "ssh-discover", true),
            Vec::new(),
            OBSERVATION,
            cancellation,
        )?;
        if !output.exit.is_success() {
            return Err(SshConnectFailure::Unavailable(
                UnavailableKind::ServiceUnavailable,
            ));
        }
        parse_discovery(&output.stdout)
    }
    fn finish(
        self: &Arc<Self>,
        mut discovery: AccountDiscovery,
        expected: Option<&SshServerContinuity>,
        cancellation: &SshCancellationToken,
        progress: &ConnectProgress<'_>,
    ) -> Result<ConnectedSshServer, SshConnectFailure> {
        discovery = prepare_if_needed(discovery, expected, self.route_id, || {
            progress(ConnectPhase::Starting);
            self.prepare_service(cancellation)
        })?;
        let server = discovery.server.filter(|server| *server != [0; 32]).ok_or(
            SshConnectFailure::Unavailable(UnavailableKind::ServiceUnavailable),
        )?;
        let facts = facts(&discovery)?;
        let continuity = SshServerContinuity {
            route: self.route_id,
            account: discovery.account,
            build: discovery.build,
            server,
        };
        if let Some(expected) = expected {
            expected.verify(&continuity)?;
        }
        if cancellation.is_cancelled() {
            return Err(SshConnectFailure::Cancelled);
        }
        progress(ConnectPhase::Connecting);
        let bridge_identity = AccountBridgeIdentity {
            account: continuity.account,
            build: continuity.build.clone(),
            server: continuity.server,
        };
        let mut cache = self
            .clients
            .lock()
            .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::ServiceUnavailable))?;
        let client = match cache.get(&self.route_id) {
            Some(cached) if cached.continuity == continuity => {
                *cached.route.lock().map_err(|_| {
                    SshConnectFailure::Unavailable(UnavailableKind::ServiceUnavailable)
                })? = Arc::downgrade(self);
                cached.client.clone()
            }
            Some(_) if expected.is_some() => {
                return Err(SshConnectFailure::Changed(ContinuityChange::ServerChanged));
            }
            _ => {
                let slot = Arc::new(Mutex::new(Arc::downgrade(self)));
                let connector = Arc::new(NativeConnector {
                    route: slot.clone(),
                    expected: bridge_identity,
                });
                let mut namespace = Sha256::new();
                namespace.update(self.route_id);
                namespace.update(continuity.server);
                namespace.update(continuity.account.to_be_bytes());
                let client = LocalServer::remote(
                    identity(),
                    namespace.finalize().into(),
                    connector.expected.clone(),
                    connector,
                )
                .map_err(map_server_error)?;
                cache.insert(
                    self.route_id,
                    NativeCachedClient {
                        continuity: continuity.clone(),
                        client: client.clone(),
                        route: slot,
                    },
                );
                client
            }
        };
        drop(cache);
        let status = client.refresh().map_err(map_server_error)?;
        let health = client.connect_pane().map_err(map_server_error)?;
        self.presented
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(ConnectedSshServer {
            server: client.clone(),
            continuity,
            lease: Arc::new(NativeConnectedLease {
                route: self.clone(),
                health: Mutex::new(health),
            }),
            protocol_major: status.protocol_major,
            facts,
        })
    }
}
struct NativeConnectedLease<A: SshProcessAdapter> {
    route: Arc<NativeRoute<A>>,
    health: Mutex<spaceterm_server::pane::PaneConnection>,
}
impl<A: SshProcessAdapter> SshServerLease for NativeConnectedLease<A> {
    fn is_alive(&self) -> bool {
        self.route.is_alive()
            && self.health.lock().is_ok_and(|mut health| {
                matches!(
                    health.exchange(&spaceterm_server::pane::Request::Capabilities),
                    Ok(spaceterm_server::pane::Reply::Capabilities { .. })
                )
            })
    }
}
struct NativeCachedClient<A: SshProcessAdapter> {
    continuity: SshServerContinuity,
    client: LocalServer,
    route: Arc<Mutex<Weak<NativeRoute<A>>>>,
}
struct NativeConnector<A: SshProcessAdapter> {
    route: Arc<Mutex<Weak<NativeRoute<A>>>>,
    expected: AccountBridgeIdentity,
}
impl<A: SshProcessAdapter> AccountConnector for NativeConnector<A> {
    fn connect(&self) -> Result<AccountBridge, ServerError> {
        let route = self
            .route
            .lock()
            .map_err(|_| ServerError::NotRunning)?
            .upgrade()
            .ok_or(ServerError::NotRunning)?;
        let (command, capability) = route
            .command(
                &route
                    .config
                    .bootstrap
                    .locator(identity(), "ssh-connect", false),
            )
            .map_err(|_| ServerError::NotRunning)?;
        capability
            .authorize()
            .map_err(|_| ServerError::NotRunning)?;
        let (read, write, cleanup) = spawn_account_bridge(
            route.config.adapter.clone(),
            &command,
            &route.environment,
            capability.cancellation(),
        )
        .map_err(|_| ServerError::NotRunning)?;
        if capability.authorize().is_err() {
            cleanup();
            return Err(ServerError::NotRunning);
        }
        AccountBridge::from_streams(read, write, cleanup, &self.expected)
    }
}
struct NativeInstallation<A: SshProcessAdapter> {
    route: Arc<NativeRoute<A>>,
    artifact: VerifiedArtifact,
    account: u32,
    target: String,
}
impl<A: SshProcessAdapter> SshInstallation for NativeInstallation<A> {
    fn install(
        &self,
        cancellation: &SshCancellationToken,
        progress: &ConnectProgress<'_>,
    ) -> Result<ConnectedSshServer, SshConnectFailure> {
        if cancellation.is_cancelled() {
            return Err(SshConnectFailure::Cancelled);
        }
        // Recheck absence on the original authenticated route. A concurrent install wins reuse.
        match self.route.discover(cancellation)? {
            Discovery::Existing(discovery) => {
                return self.route.finish(discovery, None, cancellation, progress);
            }
            Discovery::Missing { account, target }
                if account == self.account && target == self.target => {}
            _ => return Err(SshConnectFailure::Changed(ContinuityChange::AccountChanged)),
        }
        progress(ConnectPhase::Installing);
        let script = self.route.config.bootstrap.install(
            identity(),
            self.artifact.bytes.len(),
            &self.artifact.signature,
            &self.artifact.digest,
            false,
        );
        // Upload has no whole-flow deadline. The server bounds lock/supervisor observation.
        // A long transfer is cancellable, while local per-phase native operations remain bounded.
        let (command, capability) = self.route.command(&script)?;
        capability
            .authorize()
            .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::Network))?;
        let transfer_cancel =
            SshCancellationToken::linked(cancellation, &capability.cancellation());
        let result = run_captured_transfer(
            &self.route.config.adapter,
            &command,
            &self.route.environment,
            self.artifact.bytes.clone(),
            MAX_DISCOVERY,
            &transfer_cancel,
        )
        .map_err(map_process_error);
        let mut output = match result {
            Ok(output) => output,
            Err(SshConnectFailure::Cancelled | SshConnectFailure::Unavailable(_)) => {
                return Err(SshConnectFailure::Unconfirmed(
                    UnconfirmedOperation::Install,
                ));
            }
            Err(error) => return Err(error),
        };
        if output.exit.code() == Some(77) {
            let prompt = self
                .route
                .environment
                .remote_authorization_command()
                .map_err(|_| SshConnectFailure::Refused(RefusalKind::AuthorizationRefused))?;
            let answer = run_captured_transfer(
                &self.route.config.adapter,
                &prompt,
                &self.route.environment,
                Vec::new(),
                4096,
                cancellation,
            )
            .map_err(map_process_error)?;
            let mut secret = zeroize::Zeroizing::new(answer.stdout);
            if !answer.exit.is_success() {
                return Err(SshConnectFailure::Refused(
                    RefusalKind::AuthorizationRefused,
                ));
            }
            if secret.last() == Some(&b'\n') {
                secret.pop();
            }
            if secret.iter().any(|byte| matches!(byte, b'\n' | b'\r' | 0)) {
                return Err(SshConnectFailure::Refused(
                    RefusalKind::AuthorizationRefused,
                ));
            }
            let mut input = zeroize::Zeroizing::new(Vec::with_capacity(
                secret.len() + self.artifact.bytes.len() + 1,
            ));
            input.extend_from_slice(&secret);
            input.push(b'\n');
            input.extend_from_slice(&self.artifact.bytes);
            let (command, capability) =
                self.route.command(&self.route.config.bootstrap.install(
                    identity(),
                    self.artifact.bytes.len(),
                    &self.artifact.signature,
                    &self.artifact.digest,
                    true,
                ))?;
            capability
                .authorize()
                .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::Network))?;
            let cancellation =
                SshCancellationToken::linked(cancellation, &capability.cancellation());
            output = run_captured_transfer(
                &self.route.config.adapter,
                &command,
                &self.route.environment,
                std::mem::take(&mut *input),
                MAX_DISCOVERY,
                &cancellation,
            )
            .map_err(|_| SshConnectFailure::Unconfirmed(UnconfirmedOperation::Install))?;
            if output.exit.code() == Some(77) {
                return Err(SshConnectFailure::Refused(
                    RefusalKind::AuthorizationRefused,
                ));
            }
        }
        if !output.exit.is_success() {
            return Err(SshConnectFailure::Refused(RefusalKind::InstallRefused));
        }
        match self.route.discover(cancellation) {
            Ok(Discovery::Existing(discovery)) => {
                self.route.finish(discovery, None, cancellation, progress)
            }
            _ => Err(SshConnectFailure::Unconfirmed(
                UnconfirmedOperation::Install,
            )),
        }
    }
}
fn observe_preparation(
    successful: bool,
    discover: impl FnOnce() -> Result<Discovery, SshConnectFailure>,
) -> Result<AccountDiscovery, SshConnectFailure> {
    let latest =
        discover().map_err(|_| SshConnectFailure::Unconfirmed(UnconfirmedOperation::Connect))?;
    let Discovery::Existing(latest) = latest else {
        return Err(SshConnectFailure::Unconfirmed(
            UnconfirmedOperation::Connect,
        ));
    };
    if latest.stopped {
        return Err(SshConnectFailure::Stopped);
    }
    if !successful {
        return Err(SshConnectFailure::Unconfirmed(
            UnconfirmedOperation::Connect,
        ));
    }
    Ok(latest)
}

fn prepare_if_needed(
    discovery: AccountDiscovery,
    expected: Option<&SshServerContinuity>,
    route: [u8; 32],
    prepare: impl FnOnce() -> Result<AccountDiscovery, SshConnectFailure>,
) -> Result<AccountDiscovery, SshConnectFailure> {
    validate_discovery(&discovery)?;
    let observed = SshServerContinuity {
        route,
        account: discovery.account,
        build: discovery.build.clone(),
        server: discovery.server.unwrap_or([0; 32]),
    };
    if let Some(expected) = expected {
        expected.verify(&observed)?;
    }
    if discovery.stopped {
        return Err(SshConnectFailure::Stopped);
    }
    if !discovery.installed {
        return Err(SshConnectFailure::Unavailable(
            UnavailableKind::ServiceUnavailable,
        ));
    }
    if discovery.running {
        return Ok(discovery);
    }
    let prepared = prepare()?;
    validate_discovery(&prepared)?;
    let current = SshServerContinuity {
        route,
        account: prepared.account,
        build: prepared.build.clone(),
        server: prepared.server.unwrap_or([0; 32]),
    };
    if let Some(expected) = expected {
        expected.verify(&current)?;
    } else if observed.server != [0; 32] {
        observed.verify(&current)?;
    }
    if prepared.stopped {
        return Err(SshConnectFailure::Stopped);
    }
    if !prepared.running {
        return Err(SshConnectFailure::Unavailable(
            UnavailableKind::ServiceUnavailable,
        ));
    }
    Ok(prepared)
}

/// The explicit Connect worker prepares the app-owned -F entry point on first use.
/// Existing private configuration is read by OpenSSH and is never replaced here.
fn ensure_connection_configuration(paths: &AppPaths) -> Result<(), SshConnectFailure> {
    let target = paths.managed_ssh_config_file();
    let directory = paths
        .open_secure_root(target.root())
        .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::SshFailed))?;
    if let Some(directory) = directory
        && paths
            .filesystem()
            .read_private_file(&directory, target.file_name(), 1024 * 1024)
            .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::SshFailed))?
            .is_some()
    {
        return Ok(());
    }
    super::managed_hosts::ManagedHostsStore::new(paths)
        .ensure_exists()
        .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::SshFailed))
}

fn identity() -> &'static str {
    ApplicationIdentity::current().directory_name()
}
fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn platform(target: &str) -> Result<(String, String), SshConnectFailure> {
    match target {
        "aarch64-apple-darwin" => Ok(("macOS".into(), "arm64".into())),
        "x86_64-unknown-linux-gnu" => Ok(("linux".into(), "x86_64".into())),
        _ => Err(SshConnectFailure::Incompatible(
            IncompatibilityKind::Platform,
        )),
    }
}
/// What Settings may present from a validated discovery. The version is reduced to the
/// characters a release version uses, so the other computer cannot place arbitrary text there.
fn facts(discovery: &AccountDiscovery) -> Result<SshServerFacts, SshConnectFailure> {
    let version = Some(discovery.version.as_str())
        .filter(|version| {
            !version.is_empty()
                && version.len() <= 64
                && version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        })
        .map(str::to_owned);
    Ok(SshServerFacts {
        platform: platform(&discovery.target)?,
        version,
        starts_automatically: discovery.enabled,
    })
}
/// What Check Status reports for one discovery. It refuses what Connect refuses, so a server Connect
/// would call incompatible or changed never reads as available.
fn observation(
    discovery: Discovery,
    expected: Option<&SshServerContinuity>,
    route: [u8; 32],
) -> Result<SshServerObservation, SshConnectFailure> {
    match discovery {
        Discovery::Existing(discovery) => {
            validate_discovery(&discovery)?;
            if let Some(expected) = expected {
                expected.verify(&SshServerContinuity {
                    route,
                    account: discovery.account,
                    build: discovery.build.clone(),
                    server: discovery.server.unwrap_or([0; 32]),
                })?;
            }
            let observed = SshServerContinuity {
                route,
                account: discovery.account,
                build: discovery.build.clone(),
                server: discovery.server.unwrap_or([0; 32]),
            };
            if discovery.stopped {
                return Err(SshConnectFailure::Stopped);
            }
            Ok(SshServerObservation {
                availability: if discovery.installed && discovery.running {
                    ServerAvailability::Running
                } else {
                    ServerAvailability::NotRunning
                },
                facts: facts(&discovery)?,
                continuity: (observed.server != [0; 32]).then_some(observed),
            })
        }
        Discovery::Missing { target, .. } => {
            if expected.is_some() {
                return Err(SshConnectFailure::Changed(ContinuityChange::ServerChanged));
            }
            Ok(SshServerObservation {
                availability: ServerAvailability::NotInstalled,
                facts: SshServerFacts {
                    platform: platform(&target)?,
                    version: None,
                    starts_automatically: false,
                },
                continuity: None,
            })
        }
    }
}
enum Discovery {
    Existing(AccountDiscovery),
    Missing { account: u32, target: String },
}
/// Why one action of the approved installation produced no bounded reply. `Unconfirmed` means the
/// process was admitted, so the action may have taken effect and must never be repeated.
pub(super) enum SshDispatchFailure {
    NotDispatched(SshConnectFailure),
    Unconfirmed,
}
fn parse_discovery(bytes: &[u8]) -> Result<Discovery, SshConnectFailure> {
    if bytes.len() > MAX_DISCOVERY {
        return Err(SshConnectFailure::Unavailable(
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
                .ok_or(SshConnectFailure::Refused(RefusalKind::InstallRefused))?;
            platform(target)?;
            return Ok(Discovery::Missing {
                account,
                target: (*target).to_owned(),
            });
        }
    }
    let discovery: AccountDiscovery = serde_json::from_slice(bytes)
        .map_err(|_| SshConnectFailure::Unavailable(UnavailableKind::ServiceUnavailable))?;
    validate_discovery(&discovery)?;
    Ok(Discovery::Existing(discovery))
}
fn validate_discovery(discovery: &AccountDiscovery) -> Result<(), SshConnectFailure> {
    if discovery.schema != 1 || discovery.account == 0 || discovery.build != identity() {
        return Err(SshConnectFailure::Changed(ContinuityChange::BuildChanged));
    }
    platform(&discovery.target)?;
    if !discovery.protocol_majors.contains(&1) {
        return Err(SshConnectFailure::Incompatible(
            IncompatibilityKind::Protocol,
        ));
    }
    if !discovery
        .capabilities
        .iter()
        .any(|capability| capability == "workspace_policy")
    {
        return Err(SshConnectFailure::Incompatible(
            IncompatibilityKind::Capability,
        ));
    }
    Ok(())
}
fn map_control_error(
    error: super::control_connection::ControlConnectionError,
    mode: ConnectMode,
) -> SshConnectFailure {
    use super::control_connection::ControlConnectionError;
    match error {
        ControlConnectionError::Cancelled => SshConnectFailure::Cancelled,
        ControlConnectionError::Paths(
            crate::platform::app_paths::AppPathsError::LocalIpcAddressTooLong { .. },
        ) => SshConnectFailure::Unavailable(UnavailableKind::LocalIpcUnavailable),
        ControlConnectionError::ReadinessTimedOut { .. } => {
            SshConnectFailure::Unavailable(UnavailableKind::HostUnreachable)
        }
        ControlConnectionError::MasterExited {
            ref error_output, ..
        } if error_output
            .as_ref()
            .is_some_and(|output| output.as_str().contains("too long for Unix domain socket")) =>
        {
            SshConnectFailure::Unavailable(UnavailableKind::LocalIpcUnavailable)
        }
        ControlConnectionError::MasterExited { error_output, .. }
            if mode == ConnectMode::QuietContinuation =>
        {
            let detail = error_output.as_ref().map_or("", |output| output.as_str());
            if [
                "Host key verification failed",
                "REMOTE HOST IDENTIFICATION HAS CHANGED",
                "No host key is known",
            ]
            .iter()
            .any(|classification| detail.contains(classification))
            {
                return SshConnectFailure::NeedsPerson(InteractionReason::HostVerification);
            }
            if [
                "Connection refused",
                "Connection timed out",
                "No route to host",
                "Network is unreachable",
                "Connection reset",
                "Broken pipe",
                "Connection closed",
                "Could not resolve hostname",
            ]
            .iter()
            .any(|classification| detail.contains(classification))
            {
                return SshConnectFailure::Unavailable(UnavailableKind::HostUnreachable);
            }
            SshConnectFailure::NeedsPerson(InteractionReason::Authentication)
        }
        _ => SshConnectFailure::Unavailable(UnavailableKind::SshFailed),
    }
}
fn map_process_error(error: CapturedProcessError) -> SshConnectFailure {
    match error {
        CapturedProcessError::Cancelled => SshConnectFailure::Cancelled,
        _ => SshConnectFailure::Unavailable(UnavailableKind::Network),
    }
}
fn map_server_error(error: ServerError) -> SshConnectFailure {
    match error {
        ServerError::Stopped => SshConnectFailure::Stopped,
        ServerError::AuthorizationRequired => {
            SshConnectFailure::Refused(RefusalKind::AuthorizationRefused)
        }
        ServerError::IncompatibleProtocol => {
            SshConnectFailure::Incompatible(IncompatibilityKind::Protocol)
        }
        ServerError::MissingRequiredCapability => {
            SshConnectFailure::Incompatible(IncompatibilityKind::Capability)
        }
        ServerError::AuthenticationRefused | ServerError::Changed => {
            SshConnectFailure::Changed(ContinuityChange::ServerChanged)
        }
        ServerError::RuntimeUnavailable | ServerError::CandidateChanged => {
            SshConnectFailure::Refused(RefusalKind::InvalidArtifact)
        }
        _ => SshConnectFailure::Unavailable(UnavailableKind::ServiceUnavailable),
    }
}
/// Native shell construction only; policy owns admission and verified artifact bytes.
pub(crate) trait SshServerBootstrap: Send + Sync {
    fn key_public(&self, path: &std::path::Path) -> Option<super::command::SshCommandSpec>;
    fn locator(&self, identity: &str, action: &str, missing: bool) -> String;
    fn install(
        &self,
        identity: &str,
        archive_length: usize,
        signature: &[u8],
        digest: &str,
        password: bool,
    ) -> String;
    /// Retains one verified transferred artifact beside the approved installation. It installs
    /// nothing, activates nothing, starts nothing and changes no service policy.
    fn stage(
        &self,
        identity: &str,
        archive_length: usize,
        signature: &[u8],
        digest: &str,
    ) -> String;
}

#[cfg(test)]
pub(crate) mod testing {
    //! One deterministic account server behind the real route, discovery and exchange code.
    use std::io::{Cursor, Read, Write, sink};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::platform::testing::{
        EmptyHostConfigFilesystem, RecordingControlSocketProbe, RecordingFilesystem,
    };
    use crate::ssh::askpass::{
        AskPassAttempt, AskPassAttemptObservation, AskPassBrokerLease, AskPassLease,
        AskPassUnavailable,
    };
    use crate::ssh::process::{
        ProcessExit, ProcessSignal, SpawnedSshProcess, SshProcessMechanismError, SshProcessPipes,
        SshProcessSpawnRequest, SshProcessStdio,
    };

    /// How the fake account answers one remote command: its reply and its exit code. It receives
    /// the command and whatever the client wrote on standard input.
    type AccountAnswer = dyn Fn(&str, &[u8]) -> (Vec<u8>, i32) + Send + Sync;

    /// The account this fake answers for, and every remote command it was asked to run with the
    /// bytes the client sent it.
    pub(crate) struct FakeAccount {
        /// The effective OpenSSH configuration, whose digest is the route an original binds.
        configuration: Vec<u8>,
        answer: Box<AccountAnswer>,
        commands: Mutex<Vec<(String, Vec<u8>)>>,
    }
    impl FakeAccount {
        /// `answer` receives one remote command and what the client wrote to it, and returns its
        /// reply and exit code.
        pub(crate) fn new(
            configuration: &str,
            answer: impl Fn(&str, &[u8]) -> (Vec<u8>, i32) + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Self {
                configuration: configuration.as_bytes().to_vec(),
                answer: Box::new(answer),
                commands: Mutex::new(Vec::new()),
            })
        }
        /// Every remote command this account ran, in order.
        pub(crate) fn commands(&self) -> Vec<String> {
            self.commands
                .lock()
                .unwrap()
                .iter()
                .map(|(command, _)| command.clone())
                .collect()
        }
        /// What the client wrote to each command naming `action`, in order.
        pub(crate) fn sent(&self, action: &str) -> Vec<Vec<u8>> {
            self.commands
                .lock()
                .unwrap()
                .iter()
                .filter(|(command, _)| command.contains(action))
                .map(|(_, sent)| sent.clone())
                .collect()
        }
        /// How many commands name `action`.
        pub(crate) fn ran(&self, action: &str) -> usize {
            self.commands()
                .iter()
                .filter(|command| command.contains(action))
                .count()
        }
    }

    /// The bytes a fake process receives, handed to its reply once the client closes them.
    struct FakeInput {
        sent: Vec<u8>,
        complete: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    }
    impl Write for FakeInput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.sent.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Drop for FakeInput {
        fn drop(&mut self) {
            if let Some(complete) = self.complete.take() {
                let _ = complete.send(std::mem::take(&mut self.sent));
            }
        }
    }

    /// A reply the account composes only after the client finished writing its request, so an
    /// answer can depend on what was asked.
    struct FakeReply {
        account: Arc<FakeAccount>,
        command: String,
        sent: std::sync::mpsc::Receiver<Vec<u8>>,
        exit: Arc<Mutex<Option<ProcessExit>>>,
        reply: Option<Cursor<Vec<u8>>>,
    }
    impl Read for FakeReply {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.reply.is_none() {
                let sent = self.sent.recv().unwrap_or_default();
                let (reply, code) = (self.account.answer)(&self.command, &sent);
                self.account
                    .commands
                    .lock()
                    .unwrap()
                    .push((self.command.clone(), sent));
                *self.exit.lock().unwrap() = Some(ProcessExit::new(code == 0, Some(code)));
                self.reply = Some(Cursor::new(reply));
            }
            self.reply.as_mut().expect("composed").read(buffer)
        }
    }

    #[derive(Clone)]
    pub(crate) struct FakeAdapter {
        account: Arc<FakeAccount>,
        /// The control socket the master publishes, which readiness then registers.
        filesystem: Arc<RecordingFilesystem>,
    }
    pub(crate) struct FakeProcess(Arc<Mutex<Option<ProcessExit>>>);
    impl SshProcessAdapter for FakeAdapter {
        type Process = FakeProcess;
        fn spawn(
            &self,
            request: SshProcessSpawnRequest,
        ) -> Result<SpawnedSshProcess<Self::Process>, SshProcessMechanismError> {
            let arguments: Vec<_> = request
                .arguments()
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect();
            let exit = Arc::new(Mutex::new(None));
            let reader = |bytes: Vec<u8>| Box::new(Cursor::new(bytes)) as Box<dyn Read + Send>;
            let (stdin, stdout) = if arguments.iter().any(|argument| argument == "-G") {
                *exit.lock().unwrap() = Some(ProcessExit::successful());
                (None, Some(reader(self.account.configuration.clone())))
            } else if arguments.iter().any(|argument| argument == "-N") {
                // The master publishes its control socket and stays up until the backend ends it.
                if let Some(socket) = arguments.windows(2).find(|pair| pair[0] == "-S") {
                    self.filesystem
                        .create_socket(std::path::Path::new(&socket[1]));
                }
                (None, Some(reader(Vec::new())))
            } else if arguments.iter().any(|argument| argument == "-O") {
                *exit.lock().unwrap() = Some(ProcessExit::successful());
                (None, Some(reader(Vec::new())))
            } else {
                // The account answers once the client has written its whole request, so the
                // reply can depend on what was asked rather than on the command name alone.
                let (complete, sent) = std::sync::mpsc::channel();
                (
                    Some(Box::new(FakeInput {
                        sent: Vec::new(),
                        complete: Some(complete),
                    }) as Box<dyn Write + Send>),
                    Some(Box::new(FakeReply {
                        account: Arc::clone(&self.account),
                        command: arguments.last().cloned().unwrap_or_default(),
                        sent,
                        exit: Arc::clone(&exit),
                        reply: None,
                    }) as Box<dyn Read + Send>),
                )
            };
            let pipes = SshProcessPipes::new(
                (request.stdin() == SshProcessStdio::Piped)
                    .then(|| stdin.unwrap_or_else(|| Box::new(sink()) as Box<dyn Write + Send>)),
                (request.stdout() == SshProcessStdio::Piped)
                    .then(|| stdout.unwrap_or_else(|| reader(Vec::new()))),
                (request.stderr() == SshProcessStdio::Piped).then(|| reader(Vec::new())),
            );
            Ok(SpawnedSshProcess::new(FakeProcess(exit), pipes))
        }
        fn try_status(
            &self,
            process: &mut Self::Process,
        ) -> Result<Option<ProcessExit>, SshProcessMechanismError> {
            Ok(*process.0.lock().unwrap())
        }
        fn signal(
            &self,
            _: &mut Self::Process,
            _: ProcessSignal,
        ) -> Result<(), SshProcessMechanismError> {
            Ok(())
        }
        fn reap(&self, _: Self::Process) -> Result<(), SshProcessMechanismError> {
            Ok(())
        }
    }

    /// An askpass factory that records every attempt. Quiet continuation must never reach it.
    struct CountingAskPass(Arc<AtomicUsize>);
    struct NoPrompt;
    impl AskPassLease for NoPrompt {
        fn entries(&self) -> Vec<(&'static str, &std::ffi::OsStr)> {
            Vec::new()
        }
        fn cancel(&self) {}
        fn capability(&self) -> &[u8] {
            b"fixture"
        }
    }
    impl AskPassAttemptFactory for CountingAskPass {
        fn start_attempt(&self, _: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(AskPassAttempt {
                lease: AskPassBrokerLease::new(Arc::new(NoPrompt)),
                observation: AskPassAttemptObservation::default(),
            })
        }
    }

    /// The locator and transfer scripts as compact markers, so a test reads exactly what the
    /// client asked of the approved installation.
    struct FakeBootstrap;
    impl SshServerBootstrap for FakeBootstrap {
        fn key_public(&self, _: &std::path::Path) -> Option<super::super::command::SshCommandSpec> {
            None
        }
        fn locator(&self, identity: &str, action: &str, _: bool) -> String {
            format!("{action} {identity}")
        }
        fn install(&self, identity: &str, _: usize, _: &[u8], digest: &str, _: bool) -> String {
            format!("ssh-install {identity} {digest}")
        }
        fn stage(&self, identity: &str, _: usize, _: &[u8], digest: &str) -> String {
            format!("ssh-stage {identity} {digest}")
        }
    }

    /// A backend over one fake account, with the prompt counter quiet work must leave at zero.
    pub(crate) fn backend(
        executor: BackgroundExecutor,
        account: Arc<FakeAccount>,
        artifacts: SshServerArtifacts,
    ) -> (NativeSshConnectBackend<FakeAdapter>, Arc<AtomicUsize>) {
        let filesystem = Arc::new(RecordingFilesystem::default());
        let paths = crate::platform::testing::resolve_app_paths(
            &crate::platform::app_directories::AppDirectoryEnvironment {
                home: Some("/fixture/ssh".into()),
                xdg_runtime_dir: Some("/fixture/ssh/runtime".into()),
                ..Default::default()
            },
            Some("/fixture/ssh/temporary".into()),
            103,
            filesystem.clone(),
        )
        .unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let backend = NativeSshConnectBackend::new(NativeSshServerConfig {
            paths: Arc::new(paths),
            filesystem: Arc::new(EmptyHostConfigFilesystem),
            bootstrap: Arc::new(FakeBootstrap),
            home: "/fixture/ssh".into(),
            startup: StartupSshEnvironment::default(),
            executable: OpenSshExecutable::for_test(),
            adapter: FakeAdapter {
                account,
                filesystem: filesystem.clone(),
            },
            socket_probe: Arc::new(RecordingControlSocketProbe(filesystem)),
            askpass: Arc::new(CountingAskPass(prompts.clone())),
            executor,
            artifacts,
        });
        (backend, prompts)
    }

    /// The discovery one installed, running server reports.
    pub(crate) fn discovery(server: [u8; 32]) -> AccountDiscovery {
        AccountDiscovery {
            schema: 1,
            account: 501,
            build: identity().into(),
            server: Some(server),
            installed: true,
            enabled: true,
            running: true,
            stopped: false,
            protocol_majors: vec![1],
            capabilities: vec!["workspace_policy".into()],
            target: "x86_64-unknown-linux-gnu".into(),
            version: "0.1.0".into(),
            commit: "a".repeat(40),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_discovery_refuses_wrong_build_and_unsupported_capability() {
        let mut discovery = AccountDiscovery {
            schema: 1,
            account: 501,
            build: identity().into(),
            server: Some([1; 32]),
            installed: true,
            enabled: true,
            running: true,
            stopped: false,
            protocol_majors: vec![1],
            capabilities: vec!["workspace_policy".into()],
            target: "x86_64-unknown-linux-gnu".into(),
            version: "0.1.0".into(),
            commit: "a".repeat(40),
        };
        discovery.build = "foreign".into();
        assert!(matches!(
            parse_discovery(&serde_json::to_vec(&discovery).unwrap()),
            Err(SshConnectFailure::Changed(ContinuityChange::BuildChanged))
        ));
        discovery.build = identity().into();
        discovery.capabilities.clear();
        assert!(matches!(
            parse_discovery(&serde_json::to_vec(&discovery).unwrap()),
            Err(SshConnectFailure::Incompatible(
                IncompatibilityKind::Capability
            ))
        ));
    }

    /// Check Status refuses what Connect refuses: a server without protocol major 1 or a
    /// required capability reads as incompatible, never as available.
    #[test]
    fn check_status_calls_a_server_connect_refuses_incompatible() {
        let compatible = AccountDiscovery {
            schema: 1,
            account: 501,
            build: identity().into(),
            server: Some([1; 32]),
            installed: true,
            enabled: true,
            running: true,
            stopped: false,
            protocol_majors: vec![1],
            capabilities: vec!["workspace_policy".into()],
            target: "x86_64-unknown-linux-gnu".into(),
            version: "0.1.0".into(),
            commit: "a".repeat(40),
        };
        let observe = |discovery: &AccountDiscovery| {
            observation(Discovery::Existing(discovery.clone()), None, [2; 32])
        };
        assert!(matches!(
            observe(&compatible),
            Ok(SshServerObservation {
                availability: ServerAvailability::Running,
                ..
            })
        ));
        let other_major = AccountDiscovery {
            protocol_majors: vec![0],
            ..compatible.clone()
        };
        assert!(matches!(
            observe(&other_major),
            Err(SshConnectFailure::Incompatible(
                IncompatibilityKind::Protocol
            ))
        ));
        let not_running_without_capability = AccountDiscovery {
            running: false,
            capabilities: Vec::new(),
            ..compatible.clone()
        };
        assert!(matches!(
            observe(&not_running_without_capability),
            Err(SshConnectFailure::Incompatible(
                IncompatibilityKind::Capability
            ))
        ));
    }

    #[test]
    fn lost_observation_after_preparation_keeps_the_effect_unconfirmed() {
        for error in [
            SshConnectFailure::Cancelled,
            SshConnectFailure::Unavailable(UnavailableKind::Network),
        ] {
            assert!(matches!(
                observe_preparation(true, || Err(error)),
                Err(SshConnectFailure::Unconfirmed(
                    UnconfirmedOperation::Connect
                ))
            ));
        }
    }

    #[test]
    fn changed_account_or_server_never_prepares_a_nonrunning_replacement() {
        let expected = SshServerContinuity {
            route: [1; 32],
            account: 501,
            build: identity().into(),
            server: [2; 32],
        };
        for (account, server) in [(502, [2; 32]), (501, [3; 32])] {
            let discovery = AccountDiscovery {
                schema: 1,
                account,
                build: identity().into(),
                server: Some(server),
                installed: true,
                enabled: true,
                running: false,
                stopped: false,
                protocol_majors: vec![1],
                capabilities: vec!["workspace_policy".into()],
                target: "x86_64-unknown-linux-gnu".into(),
                version: "0.1.0".into(),
                commit: "a".repeat(40),
            };
            let result = prepare_if_needed(discovery, Some(&expected), expected.route, || {
                panic!("replacement must not be started")
            });
            assert!(matches!(result, Err(SshConnectFailure::Changed(_))));
        }
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "../platform/unix_adapter_tests/server_process.rs"]
mod unix_adapter_tests;
