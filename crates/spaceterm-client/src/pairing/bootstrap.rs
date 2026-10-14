//! Foreground SSH Bootstrap. Reconnect and restricted retries never receive its intent.

use super::credentials::PairingFuture;
use spaceterm_core::pairing::{EnrollmentProposal, PairingFailure};
pub use spaceterm_distribution::server_feed::BuildChannel;
use spaceterm_distribution::server_feed::ServerPlatform;
use spaceterm_protocol::authentication::ServerPin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use zeroize::Zeroizing;

// Distribution verification is the sole issuer. Bootstrap cannot bless raw archive bytes.
pub use spaceterm_distribution::server_feed::VerifiedAccountServerArtifact as VerifiedServerArtifact;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapPhase {
    ExplicitIntent,
    OpenSshAuthenticated,
    ReadOnlyPreflight,
    Planned,
    Authorized,
    Applied,
    Enrolled,
    Connected,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapFailureClass {
    Pairing(PairingFailure),
    BootstrapTrustUnavailable,
    MissingAuthorizationAgent,
    BackgroundItemsSuppressed,
    InvalidProbe,
    LoginSessionUnavailable,
    StorageLocked,
    LingerUnavailable,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("SSH Bootstrap failed: {class:?} during {phase:?}")]
pub struct BootstrapFailure {
    pub class: BootstrapFailureClass,
    pub phase: BootstrapPhase,
}
impl BootstrapFailure {
    pub const fn new(class: PairingFailure, phase: BootstrapPhase) -> Self {
        Self {
            class: BootstrapFailureClass::Pairing(class),
            phase,
        }
    }
}

/// Captured effective OpenSSH host, account and configuration bindings, not an alias to reinterpret.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct BootstrapRoute {
    host: [u8; 32],
    account: [u8; 32],
    configuration: [u8; 32],
    account_uid: u32,
    build: BuildChannel,
}
impl BootstrapRoute {
    pub fn capture(
        host: [u8; 32],
        account: [u8; 32],
        configuration: [u8; 32],
        account_uid: u32,
        build: BuildChannel,
    ) -> Result<Self, BootstrapFailure> {
        if host == [0; 32]
            || account == [0; 32]
            || configuration == [0; 32]
            || matches!(account_uid, 0 | u32::MAX)
        {
            return Err(BootstrapFailure::new(
                PairingFailure::InvalidRequest,
                BootstrapPhase::ExplicitIntent,
            ));
        }
        Ok(Self {
            host,
            account,
            configuration,
            account_uid,
            build,
        })
    }
    pub fn account_uid(self) -> u32 {
        self.account_uid
    }
    pub fn build(self) -> BuildChannel {
        self.build
    }
    pub fn host_binding(self) -> [u8; 32] {
        self.host
    }
    pub fn account_binding(self) -> [u8; 32] {
        self.account
    }
    pub fn configuration_binding(self) -> [u8; 32] {
        self.configuration
    }
}

/// Only the foreground action issues this nonserializable, nonclone capability.
///
/// ```compile_fail
/// use spaceterm_client::pairing::bootstrap::DeliberateBootstrap;
/// fn retained<T: serde::de::DeserializeOwned>() {}
/// retained::<DeliberateBootstrap>();
/// ```
pub struct DeliberateBootstrap {
    route: BootstrapRoute,
    attempt: BootstrapAttempt,
    observed: Option<BootstrapHostFacts>,
}
/// The explicit UI/CLI action boundary, never part of a reconnect or retry port.
pub struct ForegroundBootstrap;
impl ForegroundBootstrap {
    /// Binds a delayed foreground confirmation to the exact read-only account observation.
    /// A changed registration, pin, route or prerequisite requires a fresh user comparison.
    pub fn confirm_observed(
        facts: BootstrapHostFacts,
        generation: u64,
    ) -> Result<DeliberateBootstrap, BootstrapFailure> {
        if facts.account_uid != facts.route.account_uid() {
            return Err(BootstrapFailure::new(
                PairingFailure::RouteChanged,
                BootstrapPhase::ExplicitIntent,
            ));
        }
        let mut intent = Self::confirm(facts.route, generation)?;
        intent.observed = Some(facts);
        Ok(intent)
    }

    pub fn confirm(
        route: BootstrapRoute,
        generation: u64,
    ) -> Result<DeliberateBootstrap, BootstrapFailure> {
        if generation == 0 {
            return Err(BootstrapFailure::new(
                PairingFailure::InvalidRequest,
                BootstrapPhase::ExplicitIntent,
            ));
        }
        Ok(DeliberateBootstrap {
            route,
            observed: None,
            attempt: BootstrapAttempt {
                generation,
                valid: Arc::new(AtomicBool::new(true)),
                cancelled: Arc::new(tokio::sync::Notify::new()),
            },
        })
    }
}
impl DeliberateBootstrap {
    pub fn cancellation(&self) -> BootstrapAttempt {
        self.attempt.clone()
    }
}
#[derive(Clone)]
pub struct BootstrapAttempt {
    generation: u64,
    valid: Arc<AtomicBool>,
    cancelled: Arc<tokio::sync::Notify>,
}
impl PartialEq for BootstrapAttempt {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && Arc::ptr_eq(&self.valid, &other.valid)
    }
}
impl Eq for BootstrapAttempt {}
impl BootstrapAttempt {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn cancel(&self) {
        self.valid.store(false, Ordering::Release);
        self.cancelled.notify_waiters();
    }
    async fn cancelled(&self) {
        loop {
            let notified = self.cancelled.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.valid.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
    pub fn validate(&self, phase: BootstrapPhase) -> Result<(), BootstrapFailure> {
        if self.valid.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(BootstrapFailure::new(PairingFailure::Cancelled, phase))
        }
    }
}

/// The existing OpenSSH owner implements this lease. Drop enqueues exact process-group reaping.
/// Publication transfers the lease to the native-owned worker, not to the observing client.
pub trait BootstrapSshLease: Send + Sync {
    fn revalidate(&self, route: BootstrapRoute) -> Result<(), BootstrapFailure>;
}
pub struct OpenSshAuthenticated {
    route: BootstrapRoute,
    lease: Box<dyn BootstrapSshLease>,
}
impl OpenSshAuthenticated {
    /// Called only at the OpenSSH authentication boundary after host/account verification.
    pub fn retain(
        route: BootstrapRoute,
        lease: Box<dyn BootstrapSshLease>,
    ) -> Result<Self, BootstrapFailure> {
        lease.revalidate(route)?;
        Ok(Self { route, lease })
    }
    pub fn route(&self) -> BootstrapRoute {
        self.route
    }
    pub fn revalidate(&self) -> Result<(), BootstrapFailure> {
        self.lease.revalidate(self.route)
    }
}

pub use spaceterm_core::bootstrap::{
    BootstrapArchitecture, BootstrapLibc, BootstrapLinger, BootstrapOs, BootstrapSupervision,
    HostVersion,
};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapSudo {
    NonInteractive,
    ResponseRequired,
    Denied,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapPrerequisite {
    Available,
    Locked,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapRegistration {
    Missing,
    Running {
        pin: ServerPin,
        generation: u64,
        artifact_identity: [u8; 32],
        version: HostVersion,
        protocol_majors: [u16; 2],
    },
    Stopped {
        pin: ServerPin,
        generation: u64,
    },
    Locked,
    Unavailable,
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct BootstrapHostFacts {
    pub route: BootstrapRoute,
    pub account_uid: u32,
    pub os: BootstrapOs,
    pub architecture: BootstrapArchitecture,
    pub os_version: HostVersion,
    pub libc: BootstrapLibc,
    pub kernel: HostVersion,
    pub supervision: BootstrapSupervision,
    pub registration: BootstrapRegistration,
    pub storage: BootstrapPrerequisite,
    pub installation: BootstrapPrerequisite,
    pub linger: BootstrapLinger,
    pub sudo: BootstrapSudo,
    pub authorization_agent: bool,
    pub background_items_allowed: bool,
}
impl BootstrapHostFacts {
    pub fn supported_platform(&self) -> Result<ServerPlatform, BootstrapFailure> {
        let phase = BootstrapPhase::ReadOnlyPreflight;
        let unsupported = || BootstrapFailure::new(PairingFailure::UnsupportedTarget, phase);
        let platform = match (self.os, self.architecture) {
            (BootstrapOs::MacOs, BootstrapArchitecture::Arm64)
                if self.os_version.at_least(26, 0) =>
            {
                ServerPlatform::DarwinArm64
            }
            (BootstrapOs::Linux, BootstrapArchitecture::X86_64)
                if self.kernel.at_least(5, 15)
                    && matches!(self.libc, BootstrapLibc::Glibc(v) if v.at_least(2, 35)) =>
            {
                ServerPlatform::LinuxX86_64
            }
            _ => return Err(unsupported()),
        };
        if matches!(
            self.supervision,
            BootstrapSupervision::LaunchAgent { gui_domain: false }
        ) {
            return Err(BootstrapFailure {
                class: BootstrapFailureClass::LoginSessionUnavailable,
                phase,
            });
        }
        if !matches!(
            (platform, self.supervision),
            (
                ServerPlatform::DarwinArm64,
                BootstrapSupervision::LaunchAgent { gui_domain: true }
            ) | (
                ServerPlatform::LinuxX86_64,
                BootstrapSupervision::SystemdUser {
                    version: 249..,
                    reachable: true
                }
            )
        ) {
            return Err(BootstrapFailure::new(
                PairingFailure::ServiceManagerUnavailable,
                phase,
            ));
        }
        Ok(platform)
    }
    /// Read-only checks shared by foreground preview and final bootstrap admission.
    pub fn prerequisites(&self, install: bool) -> Result<(), BootstrapFailure> {
        let phase = BootstrapPhase::ReadOnlyPreflight;
        for state in [Some(self.storage), install.then_some(self.installation)]
            .into_iter()
            .flatten()
        {
            match state {
                BootstrapPrerequisite::Available => (),
                BootstrapPrerequisite::Locked => {
                    return Err(BootstrapFailure {
                        class: BootstrapFailureClass::StorageLocked,
                        phase,
                    });
                }
                BootstrapPrerequisite::Unavailable => {
                    return Err(BootstrapFailure::new(
                        PairingFailure::StorageUnavailable,
                        phase,
                    ));
                }
            }
        }
        if !self.background_items_allowed {
            return Err(BootstrapFailure {
                class: BootstrapFailureClass::BackgroundItemsSuppressed,
                phase,
            });
        }
        match (self.os, self.linger) {
            (BootstrapOs::Linux, BootstrapLinger::Enabled)
            | (BootstrapOs::MacOs, BootstrapLinger::NotApplicable) => return Ok(()),
            (BootstrapOs::Linux, BootstrapLinger::Disabled) => (),
            _ => {
                return Err(BootstrapFailure {
                    class: BootstrapFailureClass::LingerUnavailable,
                    phase,
                });
            }
        }
        if !self.authorization_agent {
            return Err(BootstrapFailure {
                class: BootstrapFailureClass::MissingAuthorizationAgent,
                phase,
            });
        }
        match self.sudo {
            BootstrapSudo::NonInteractive | BootstrapSudo::ResponseRequired => Ok(()),
            BootstrapSudo::Denied => Err(BootstrapFailure::new(
                PairingFailure::AuthorizationDenied,
                phase,
            )),
            BootstrapSudo::Unavailable => Err(BootstrapFailure::new(
                PairingFailure::AuthorizationUnavailable,
                phase,
            )),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapOperation {
    InstallAndStart,
    AuthorizeOwnerEnrollment,
}
/// Fixed operations and comparisons only. Native account/build authority stays with the provider.
#[derive(Clone, Eq, PartialEq)]
pub struct BootstrapPlan {
    route: BootstrapRoute,
    linger_required: bool,
    operation: BootstrapOperation,
    registration_generation: Option<u64>,
    artifact: Option<[u8; 32]>,
    expected_server: Option<ServerPin>,
    attempt: BootstrapAttempt,
    owner_proposal: Option<EnrollmentProposal>,
}
impl BootstrapPlan {
    pub fn route(&self) -> BootstrapRoute {
        self.route
    }
    /// Only a disabled Linux linger setting admits a sudo response.
    pub fn requires_linger_authorization(&self) -> bool {
        self.linger_required
    }
    pub fn operation(&self) -> BootstrapOperation {
        self.operation
    }
    pub fn registration_generation(&self) -> Option<u64> {
        self.registration_generation
    }
    pub fn artifact_identity(&self) -> Option<[u8; 32]> {
        self.artifact
    }
    pub fn expected_server(&self) -> Option<ServerPin> {
        self.expected_server
    }
    pub fn owner_proposal(&self) -> Option<&EnrollmentProposal> {
        self.owner_proposal.as_ref()
    }
    pub fn attempt_generation(&self) -> u64 {
        self.attempt.generation()
    }
    /// Equal numeric generations from separate foreground actions are not the same attempt.
    pub fn belongs_to_attempt(&self, attempt: &BootstrapAttempt) -> bool {
        self.attempt == *attempt
    }
    /// The native authorization boundary validates the exact plan and returns a journal-owned operation.
    pub async fn authorize(
        &self,
        boundary: &dyn BootstrapAuthorizationBoundary,
        prompt: &dyn ScopedAuthenticationPrompt,
    ) -> Result<AuthorizedBootstrap, BootstrapFailure> {
        self.attempt.validate(BootstrapPhase::Planned)?;
        match (
            self.operation,
            self.owner_proposal.as_ref(),
            self.expected_server,
        ) {
            (BootstrapOperation::InstallAndStart, None, None) => (),
            (BootstrapOperation::AuthorizeOwnerEnrollment, Some(proposal), Some(pin)) => {
                validate_proposal(proposal, pin)?
            }
            _ => {
                return Err(BootstrapFailure::new(
                    PairingFailure::InvalidRequest,
                    BootstrapPhase::Planned,
                ));
            }
        }
        let scoped = LingerOnlyPrompt {
            plan: self,
            inner: prompt,
            attempts: AtomicU8::new(0),
        };
        let operation = boundary.authorize(self, &scoped).await?;
        self.attempt.validate(BootstrapPhase::Authorized)?;
        if operation.plan() != self {
            return Err(BootstrapFailure::new(
                PairingFailure::IdentityMismatch,
                BootstrapPhase::Authorized,
            ));
        }
        Ok(AuthorizedBootstrap {
            plan: self.clone(),
            operation,
        })
    }
}
/// Obscured response channel. The native scoped sudo exchange is its only consumer.
pub trait ScopedAuthenticationPrompt: Send + Sync {
    fn revalidate(&self, plan: &BootstrapPlan) -> Result<(), BootstrapFailure>;
    fn respond<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
    ) -> PairingFuture<'a, Result<Zeroizing<Vec<u8>>, BootstrapFailure>>;
}
/// An account operation cannot turn install or owner issuance into a password prompt.
struct LingerOnlyPrompt<'a> {
    plan: &'a BootstrapPlan,
    inner: &'a dyn ScopedAuthenticationPrompt,
    attempts: AtomicU8,
}
impl ScopedAuthenticationPrompt for LingerOnlyPrompt<'_> {
    fn revalidate(&self, plan: &BootstrapPlan) -> Result<(), BootstrapFailure> {
        if plan != self.plan {
            return Err(BootstrapFailure::new(
                PairingFailure::IdentityMismatch,
                BootstrapPhase::Authorized,
            ));
        }
        plan.attempt.validate(BootstrapPhase::Authorized)?;
        self.inner.revalidate(plan)
    }
    fn respond<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
    ) -> PairingFuture<'a, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        Box::pin(async move {
            self.revalidate(plan)?;
            if !plan.linger_required {
                return Err(BootstrapFailure::new(
                    PairingFailure::AuthorizationDenied,
                    BootstrapPhase::Authorized,
                ));
            }
            self.attempts
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    (count < 3).then_some(count + 1)
                })
                .map_err(|_| {
                    BootstrapFailure::new(PairingFailure::LimitExceeded, BootstrapPhase::Authorized)
                })?;
            let response = self.inner.respond(plan).await?;
            self.revalidate(plan)?;
            if response.is_empty() || response.len() > 16 * 1024 {
                return Err(BootstrapFailure::new(
                    PairingFailure::LimitExceeded,
                    BootstrapPhase::Authorized,
                ));
            }
            if response.iter().any(|byte| *byte < 0x20 || *byte == 0x7f) {
                return Err(BootstrapFailure::new(
                    PairingFailure::InvalidRequest,
                    BootstrapPhase::Authorized,
                ));
            }
            Ok(response)
        })
    }
}

pub trait BootstrapAuthorizationBoundary: Send + Sync {
    fn authorize<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
        prompt: &'a dyn ScopedAuthenticationPrompt,
    ) -> PairingFuture<'a, Result<Box<dyn BootstrapAuthorizedOperation>, BootstrapFailure>>;
}
/// Implemented by the account service boundary. All server code executes as the account.
/// Its original durable request owns publication and outcome resolution once apply is called.
pub trait BootstrapAuthorizedOperation: Send + Sync {
    fn plan(&self) -> &BootstrapPlan;
    fn request_identity(&self) -> [u8; 16];
    fn apply(
        self: Box<Self>,
        artifact: Option<VerifiedServerArtifact>,
    ) -> PairingFuture<'static, Result<BootstrapServer, BootstrapFailure>>;
}
pub struct AuthorizedBootstrap {
    plan: BootstrapPlan,
    operation: Box<dyn BootstrapAuthorizedOperation>,
}
impl AuthorizedBootstrap {
    pub fn plan(&self) -> &BootstrapPlan {
        &self.plan
    }
    pub fn request_identity(&self) -> [u8; 16] {
        self.operation.request_identity()
    }
    pub fn apply(
        self,
        artifact: Option<VerifiedServerArtifact>,
    ) -> PairingFuture<'static, Result<BootstrapServer, BootstrapFailure>> {
        if let Err(failure) = self.plan.attempt.validate(BootstrapPhase::Authorized) {
            return Box::pin(async move { Err(failure) });
        }
        let valid = match self.plan.operation {
            BootstrapOperation::InstallAndStart => artifact.as_ref().is_some_and(|artifact| {
                Some(artifact.artifact_sha256()) == self.plan.artifact
                    && artifact.feed().document().build_identity == self.plan.route.build()
            }),
            BootstrapOperation::AuthorizeOwnerEnrollment => artifact.is_none(),
        };
        if !valid {
            return Box::pin(async {
                Err(BootstrapFailure::new(
                    PairingFailure::IdentityMismatch,
                    BootstrapPhase::Authorized,
                ))
            });
        }
        self.operation.apply(artifact)
    }
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct BootstrapServer {
    pub route: BootstrapRoute,
    pub pin: ServerPin,
    pub registration_generation: u64,
    pub approved_artifact_identity: [u8; 32],
    pub protocol_majors: [u16; 2],
    pub running: bool,
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct BootstrapOutcome {
    pub server: BootstrapServer,
    pub reused: bool,
    pub phase: BootstrapPhase,
}

pub trait NativeServerSetup: Send + Sync + 'static {
    fn authenticate<'a>(
        &'a self,
        route: &'a BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<OpenSshAuthenticated, BootstrapFailure>>;
    fn discover<'a>(
        &'a self,
        ssh: &'a OpenSshAuthenticated,
    ) -> PairingFuture<'a, Result<BootstrapHostFacts, BootstrapFailure>>;
    fn authorize<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
        prompt: &'a dyn ScopedAuthenticationPrompt,
    ) -> PairingFuture<'a, Result<AuthorizedBootstrap, BootstrapFailure>>;
    fn apply(
        &self,
        authorized: AuthorizedBootstrap,
        artifact: Option<VerifiedServerArtifact>,
    ) -> PairingFuture<'static, Result<BootstrapServer, BootstrapFailure>> {
        authorized.apply(artifact)
    }
    /// Resolution reads the original request and exact artifact on the retained SSH route.
    /// It never selects a fallback host, installs, starts, or issues another owner request.
    fn resolve(
        &self,
        request: [u8; 16],
        artifact_identity: [u8; 32],
    ) -> PairingFuture<'_, Result<BootstrapServer, BootstrapFailure>>;
}
/// Owner issuance uses the account service's account-owner entry point and native request identity.
/// No invented ordinary PairingId, ClientId or operation can stand in for that authority.
pub trait BootstrapEnrollment: Send + Sync {
    /// Checks local native credentials and journal admission before remote installation can begin.
    fn preflight<'a>(
        &'a self,
        route: BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>>;
    /// Prepares the exact candidate/device claim before owner authorization, through native credentials and the one journal.
    fn prepare_owner<'a>(
        &'a self,
        server: &'a BootstrapServer,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<EnrollmentProposal, BootstrapFailure>>;
    /// Success requires a receipt verified against the original claim and durable credential activation.
    /// Retry and restart read the issued owner record; they never issue replacement authority.
    fn enroll_owner<'a>(
        &'a self,
        server: &'a BootstrapServer,
        request: [u8; 16],
        proposal: &'a EnrollmentProposal,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>>;
    fn connect<'a>(
        &'a self,
        server: &'a BootstrapServer,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>>;
}

pub struct BootstrapCoordinator<H, E, P> {
    setup: Arc<H>,
    enrollment: E,
    prompt: P,
    admission: Arc<tokio::sync::Semaphore>,
}
impl<H: NativeServerSetup, E: BootstrapEnrollment, P: ScopedAuthenticationPrompt>
    BootstrapCoordinator<H, E, P>
{
    pub fn new(setup: Arc<H>, enrollment: E, prompt: P) -> Self {
        Self {
            setup,
            enrollment,
            prompt,
            admission: Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }
    pub async fn bootstrap(
        &self,
        intent: DeliberateBootstrap,
        artifact: Option<VerifiedServerArtifact>,
    ) -> Result<BootstrapOutcome, BootstrapFailure> {
        tokio::select! {
            biased;
            _ = intent.attempt.cancelled() => Err(BootstrapFailure::new(PairingFailure::Cancelled, BootstrapPhase::ExplicitIntent)),
            result = self.bootstrap_inner(&intent, artifact) => result,
        }
    }
    async fn bootstrap_inner(
        &self,
        intent: &DeliberateBootstrap,
        artifact: Option<VerifiedServerArtifact>,
    ) -> Result<BootstrapOutcome, BootstrapFailure> {
        let permit = self.admission.clone().try_acquire_owned().map_err(|_| {
            BootstrapFailure::new(
                PairingFailure::LimitExceeded,
                BootstrapPhase::ExplicitIntent,
            )
        })?;
        intent.attempt.validate(BootstrapPhase::ExplicitIntent)?;
        let ssh = self
            .setup
            .authenticate(&intent.route, &intent.attempt)
            .await?;
        if ssh.route() != intent.route {
            return Err(BootstrapFailure::new(
                PairingFailure::RouteChanged,
                BootstrapPhase::OpenSshAuthenticated,
            ));
        }
        ssh.revalidate()?;
        intent
            .attempt
            .validate(BootstrapPhase::OpenSshAuthenticated)?;
        let facts = self.setup.discover(&ssh).await?;
        ssh.revalidate()?;
        intent.attempt.validate(BootstrapPhase::ReadOnlyPreflight)?;
        if facts.route != intent.route
            || facts.account_uid != intent.route.account_uid()
            || intent
                .observed
                .as_ref()
                .is_some_and(|observed| observed != &facts)
        {
            return Err(BootstrapFailure::new(
                PairingFailure::RouteChanged,
                BootstrapPhase::ReadOnlyPreflight,
            ));
        }
        let (operation, generation, expected_server, artifact_identity, reused) =
            match facts.registration {
                BootstrapRegistration::Locked => {
                    return Err(BootstrapFailure {
                        class: BootstrapFailureClass::StorageLocked,
                        phase: BootstrapPhase::ReadOnlyPreflight,
                    });
                }
                BootstrapRegistration::Unavailable => {
                    return Err(BootstrapFailure::new(
                        PairingFailure::StorageUnavailable,
                        BootstrapPhase::ReadOnlyPreflight,
                    ));
                }
                BootstrapRegistration::Stopped { .. } => {
                    return Err(BootstrapFailure::new(
                        PairingFailure::ExplicitStartRequired,
                        BootstrapPhase::ReadOnlyPreflight,
                    ));
                }
                BootstrapRegistration::Running {
                    pin,
                    generation,
                    artifact_identity,
                    protocol_majors,
                    ..
                } => {
                    facts.supported_platform()?;
                    if !valid_pin(pin) || generation == 0 || artifact_identity == [0; 32] {
                        return Err(BootstrapFailure::new(
                            PairingFailure::InvalidDurableState,
                            BootstrapPhase::ReadOnlyPreflight,
                        ));
                    }
                    if !compatible(protocol_majors) {
                        return Err(BootstrapFailure::new(
                            PairingFailure::Incompatible,
                            BootstrapPhase::ReadOnlyPreflight,
                        ));
                    }
                    (
                        BootstrapOperation::AuthorizeOwnerEnrollment,
                        Some(generation),
                        Some(pin),
                        Some(artifact_identity),
                        true,
                    )
                }
                BootstrapRegistration::Missing => {
                    let platform = facts.supported_platform()?;
                    let verified = artifact.as_ref().ok_or(BootstrapFailure {
                        class: BootstrapFailureClass::BootstrapTrustUnavailable,
                        phase: BootstrapPhase::Planned,
                    })?;
                    let feed = verified.feed().document();
                    if feed.platform != platform || feed.build_identity != intent.route.build() {
                        return Err(BootstrapFailure::new(
                            PairingFailure::IdentityMismatch,
                            BootstrapPhase::Planned,
                        ));
                    }
                    if !feed
                        .protocol_majors
                        .iter()
                        .any(|major| matches!(major, 1 | 2))
                    {
                        return Err(BootstrapFailure::new(
                            PairingFailure::Incompatible,
                            BootstrapPhase::Planned,
                        ));
                    }
                    (
                        BootstrapOperation::InstallAndStart,
                        None,
                        None,
                        Some(feed.archive.sha256),
                        false,
                    )
                }
            };
        facts.prerequisites(!reused)?;
        self.enrollment
            .preflight(intent.route, &intent.attempt)
            .await?;
        intent.attempt.validate(BootstrapPhase::ReadOnlyPreflight)?;
        ssh.revalidate()?;
        let proposal = if let BootstrapRegistration::Running {
            pin,
            generation,
            artifact_identity,
            protocol_majors,
            ..
        } = facts.registration
        {
            let server = BootstrapServer {
                route: intent.route,
                pin,
                registration_generation: generation,
                approved_artifact_identity: artifact_identity,
                protocol_majors,
                running: true,
            };
            let proposal = self
                .enrollment
                .prepare_owner(&server, &intent.attempt)
                .await?;
            validate_proposal(&proposal, pin)?;
            Some(proposal)
        } else {
            None
        };
        let plan = BootstrapPlan {
            owner_proposal: proposal.clone(),
            route: intent.route,
            linger_required: facts.linger == BootstrapLinger::Disabled,
            operation,
            registration_generation: generation,
            artifact: artifact_identity,
            expected_server,
            attempt: intent.attempt.clone(),
        };
        intent.attempt.validate(BootstrapPhase::Planned)?;
        ssh.revalidate()?;
        self.prompt.revalidate(&plan)?;
        let authorized = self.setup.authorize(&plan, &self.prompt).await?;
        intent.attempt.validate(BootstrapPhase::Authorized)?;
        ssh.revalidate()?;
        self.prompt.revalidate(&plan)?;
        if authorized.plan() != &plan || authorized.request_identity() == [0; 16] {
            return Err(BootstrapFailure::new(
                PairingFailure::IdentityMismatch,
                BootstrapPhase::Authorized,
            ));
        }
        let request = authorized.request_identity();
        // Initial installation and automatic original-request recovery share one deadline.
        // The admitted apply worker retains ownership even after foreground observation expires.
        let startup_deadline =
            (!reused).then(|| tokio::time::Instant::now() + std::time::Duration::from_secs(15));
        let unresolved =
            BootstrapFailure::new(PairingFailure::RecoveryRequired, BootstrapPhase::Applied);
        // apply transfers ownership to the native journal. Dropping observation never cancels it.
        let apply = self
            .setup
            .apply(authorized, if reused { None } else { artifact });
        let (send, receive) = tokio::sync::oneshot::channel();
        let setup = Arc::clone(&self.setup);
        let expected_artifact = plan.artifact.ok_or_else(|| {
            BootstrapFailure::new(PairingFailure::InvalidRequest, BootstrapPhase::Planned)
        })?;
        tokio::spawn(async move {
            let result = match apply.await {
                Err(failure) if outcome_unresolved(failure) => match startup_deadline {
                    Some(deadline) if tokio::time::Instant::now() >= deadline => Err(unresolved),
                    Some(deadline) => {
                        tokio::time::timeout_at(deadline, setup.resolve(request, expected_artifact))
                            .await
                            .unwrap_or(Err(unresolved))
                    }
                    None => setup.resolve(request, expected_artifact).await,
                },
                result => result,
            };
            let _ = send.send((result, ssh, permit));
        });
        let received = match startup_deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, receive)
                .await
                .map_err(|_| unresolved)?,
            None => receive.await,
        };
        let (server, ssh, permit) = received.map_err(|_| unresolved)?;
        // A ready channel can win timeout_at's poll after scheduling or reply latency.
        if startup_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Err(unresolved);
        }
        let server = server?;
        ssh.revalidate()?;
        intent.attempt.validate(BootstrapPhase::Applied)?;
        if server.route != intent.route
            || !valid_pin(server.pin)
            || server.registration_generation == 0
            || server.approved_artifact_identity == [0; 32]
            || Some(server.approved_artifact_identity) != artifact_identity
            || expected_server.is_some_and(|pin| pin != server.pin)
            || generation.is_some_and(|value| value != server.registration_generation)
        {
            return Err(BootstrapFailure::new(
                PairingFailure::IdentityMismatch,
                BootstrapPhase::Applied,
            ));
        }
        if !server.running {
            return Err(BootstrapFailure::new(
                PairingFailure::ExplicitStartRequired,
                BootstrapPhase::Applied,
            ));
        }
        if !compatible(server.protocol_majors) {
            return Err(BootstrapFailure::new(
                PairingFailure::Incompatible,
                BootstrapPhase::Applied,
            ));
        }
        // Install authority cannot issue an enrollment ticket. New installation needs its own
        // claim-bound owner request after the committed Server identity is known.
        let (request, proposal, ssh, _permit) = if let Some(proposal) = proposal {
            (request, proposal, ssh, permit)
        } else {
            let proposal = self
                .enrollment
                .prepare_owner(&server, &intent.attempt)
                .await?;
            validate_proposal(&proposal, server.pin)?;
            let owner_plan = BootstrapPlan {
                route: intent.route,
                linger_required: false,
                operation: BootstrapOperation::AuthorizeOwnerEnrollment,
                registration_generation: Some(server.registration_generation),
                artifact: Some(server.approved_artifact_identity),
                expected_server: Some(server.pin),
                attempt: intent.attempt.clone(),
                owner_proposal: Some(proposal.clone()),
            };
            intent.attempt.validate(BootstrapPhase::Planned)?;
            ssh.revalidate()?;
            self.prompt.revalidate(&owner_plan)?;
            let authorized = self.setup.authorize(&owner_plan, &self.prompt).await?;
            intent.attempt.validate(BootstrapPhase::Authorized)?;
            ssh.revalidate()?;
            self.prompt.revalidate(&owner_plan)?;
            if authorized.plan() != &owner_plan || authorized.request_identity() == [0; 16] {
                return Err(BootstrapFailure::new(
                    PairingFailure::IdentityMismatch,
                    BootstrapPhase::Authorized,
                ));
            }
            let request = authorized.request_identity();
            let apply = self.setup.apply(authorized, None);
            let (send, receive) = tokio::sync::oneshot::channel();
            let setup = Arc::clone(&self.setup);
            let expected_artifact = server.approved_artifact_identity;
            tokio::spawn(async move {
                let result = match apply.await {
                    Err(failure) if outcome_unresolved(failure) => {
                        setup.resolve(request, expected_artifact).await
                    }
                    result => result,
                };
                let _ = send.send((result, ssh, permit));
            });
            let (resolved, ssh, permit) = receive.await.map_err(|_| {
                BootstrapFailure::new(PairingFailure::RecoveryRequired, BootstrapPhase::Applied)
            })?;
            let resolved = resolved?;
            intent.attempt.validate(BootstrapPhase::Applied)?;
            ssh.revalidate()?;
            if !resolved.running {
                return Err(BootstrapFailure::new(
                    PairingFailure::ExplicitStartRequired,
                    BootstrapPhase::Applied,
                ));
            }
            if resolved != server {
                return Err(BootstrapFailure::new(
                    PairingFailure::IdentityMismatch,
                    BootstrapPhase::Applied,
                ));
            }
            (request, proposal, ssh, permit)
        };
        self.enrollment
            .enroll_owner(&server, request, &proposal, &intent.attempt)
            .await?;
        intent.attempt.validate(BootstrapPhase::Enrolled)?;
        ssh.revalidate()?;
        self.enrollment.connect(&server, &intent.attempt).await?;
        intent.attempt.validate(BootstrapPhase::Connected)?;
        ssh.revalidate()?;
        Ok(BootstrapOutcome {
            server,
            reused,
            phase: BootstrapPhase::Connected,
        })
    }
}
impl Drop for DeliberateBootstrap {
    fn drop(&mut self) {
        self.attempt.cancel();
    }
}
fn valid_pin(pin: ServerPin) -> bool {
    pin.identity != [0; 16] && pin.public_key != [0; 32]
}
fn compatible(majors: [u16; 2]) -> bool {
    majors[0] != 0 && majors[0] < majors[1] && majors.iter().any(|major| matches!(major, 1 | 2))
}

fn validate_proposal(
    proposal: &EnrollmentProposal,
    pin: ServerPin,
) -> Result<(), BootstrapFailure> {
    if proposal.validate().is_err() || proposal.parts().server.bytes() != pin.identity {
        return Err(BootstrapFailure::new(
            PairingFailure::IdentityMismatch,
            BootstrapPhase::Planned,
        ));
    }
    Ok(())
}

fn outcome_unresolved(failure: BootstrapFailure) -> bool {
    matches!(
        failure.class,
        BootstrapFailureClass::Pairing(
            PairingFailure::RecoveryRequired | PairingFailure::Offline | PairingFailure::TimedOut
        )
    )
}
