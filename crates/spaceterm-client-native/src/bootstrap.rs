//! Native SSH Bootstrap composition. Remote account setup is a closed operation port;
//! the existing OpenSSH owner retains host/account trust, deadlines and child cleanup.

mod enrollment;
#[cfg(test)]
mod tests;
pub use enrollment::{BootstrapOwnerDelivery, BootstrapOwnerExchange, NativeBootstrapEnrollment};

use std::sync::Arc;

use spaceterm_client::pairing::{bootstrap::*, credentials::PairingFuture};
use spaceterm_core::pairing::{EnrollmentProposal, PairingFailure};
use spaceterm_host::{
    account_service::{LingerState, ServiceFailure},
    bootstrap::{
        linger::{
            BootstrapLingerFailure, BootstrapLingerTransport, LingerAuthenticationPrompt,
            LingerFuture, enable_linger_if_needed,
        },
        probe::ReadOnlyHostProbe,
        process::{
            BootstrapProbeFailure, BootstrapProbeLease, BootstrapProbeTransport,
            read_only_preflight,
        },
        setup::{BootstrapAccountFacts, BootstrapAccountRegistration},
    },
    startup::BuildIdentity,
};
use zeroize::Zeroizing;

pub use spaceterm_host::bootstrap::artifact::{
    ArtifactTransferFailure, receive_artifact, write_artifact,
};

/// Account setup through the retained OpenSSH owner, never an ordinary reconnect.
/// Implementations dispatch fixed setup entries as the account, not through sudo.
/// They retain each original request before publication and authenticate transferred artifacts.
pub trait BootstrapAccountTransport:
    BootstrapProbeLease + BootstrapProbeTransport + BootstrapLingerTransport + Send + Sync + 'static
{
    fn route(&self) -> BootstrapRoute;
    fn inspect(&self) -> PairingFuture<'_, Result<BootstrapAccountFacts, BootstrapFailure>>;
    fn reserve_request(&self) -> PairingFuture<'_, Result<[u8; 16], BootstrapFailure>>;
    fn install(
        &self,
        request: [u8; 16],
        artifact: VerifiedServerArtifact,
    ) -> PairingFuture<'_, Result<BootstrapAccountFacts, BootstrapFailure>>;
    fn authorize_owner<'a>(
        &'a self,
        request: [u8; 16],
        expected: &'a BootstrapAccountRegistration,
        proposal: &'a EnrollmentProposal,
    ) -> PairingFuture<'a, Result<BootstrapAccountFacts, BootstrapFailure>>;
    fn resolve(
        &self,
        request: [u8; 16],
        artifact_identity: [u8; 32],
    ) -> PairingFuture<'_, Result<BootstrapAccountFacts, BootstrapFailure>>;
}

/// The native preflight contract shared with the desktop's existing SSH composition.
pub trait BootstrapNativePreflight: BootstrapAuthorizationBoundary {
    fn inspect<'a>(
        &'a self,
        route: BootstrapRoute,
        host: &'a ReadOnlyHostProbe,
    ) -> PairingFuture<'a, Result<BootstrapHostFacts, BootstrapFailure>>;
    fn resolve(
        &self,
        request: [u8; 16],
        artifact_identity: [u8; 32],
    ) -> PairingFuture<'_, Result<BootstrapServer, BootstrapFailure>>;
}

/// Authentication through the application's existing managed OpenSSH owner.
pub trait BootstrapOpenSsh: BootstrapProbeTransport + 'static {
    fn authenticate<'a>(
        &'a self,
        route: &'a BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<OpenSshAuthenticated, BootstrapFailure>>;
}

/// Composes the existing OpenSSH owner with native account setup without desktop dependencies.
pub struct SshNativeServerSetup<T, N> {
    ssh: T,
    native: N,
}
impl<T, N> SshNativeServerSetup<T, N> {
    pub fn new(ssh: T, native: N) -> Self {
        Self { ssh, native }
    }
}
impl<T: BootstrapOpenSsh, N: BootstrapNativePreflight + 'static> NativeServerSetup
    for SshNativeServerSetup<T, N>
{
    fn authenticate<'a>(
        &'a self,
        route: &'a BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<OpenSshAuthenticated, BootstrapFailure>> {
        self.ssh.authenticate(route, attempt)
    }
    fn discover<'a>(
        &'a self,
        ssh: &'a OpenSshAuthenticated,
    ) -> PairingFuture<'a, Result<BootstrapHostFacts, BootstrapFailure>> {
        Box::pin(async move {
            let host = read_only_preflight(&self.ssh, &AuthenticatedProbeLease(ssh))
                .await
                .map_err(probe_failure)?;
            let facts = self.native.inspect(ssh.route(), &host).await?;
            ssh.revalidate()?;
            if facts.route != ssh.route()
                || facts.account_uid != host.account_uid
                || facts.os != host.os
                || facts.architecture != host.architecture
                || facts.os_version != host.os_version
                || facts.libc != host.libc
                || facts.kernel != host.kernel
                || facts.supervision != host.supervision
                || facts.linger != host.linger
            {
                return Err(failure(
                    PairingFailure::RouteChanged,
                    BootstrapPhase::ReadOnlyPreflight,
                ));
            }
            Ok(facts)
        })
    }
    fn authorize<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
        prompt: &'a dyn ScopedAuthenticationPrompt,
    ) -> PairingFuture<'a, Result<AuthorizedBootstrap, BootstrapFailure>> {
        Box::pin(async move { plan.authorize(&self.native, prompt).await })
    }
    fn resolve(
        &self,
        request: [u8; 16],
        artifact_identity: [u8; 32],
    ) -> PairingFuture<'_, Result<BootstrapServer, BootstrapFailure>> {
        self.native.resolve(request, artifact_identity)
    }
}
struct AuthenticatedProbeLease<'a>(&'a OpenSshAuthenticated);
impl BootstrapProbeLease for AuthenticatedProbeLease<'_> {
    fn revalidate(&self) -> Result<(), BootstrapProbeFailure> {
        self.0
            .revalidate()
            .map_err(|_| BootstrapProbeFailure::RouteChanged)
    }
    fn account_uid(&self) -> u32 {
        self.0.route().account_uid()
    }
}

/// Remote native provider. No saved route, retry worker or wire value can mint this account port.
pub struct NativeBootstrapAccount<T> {
    transport: Arc<T>,
}
impl<T: BootstrapAccountTransport> NativeBootstrapAccount<T> {
    pub fn new(transport: Arc<T>) -> Result<Self, BootstrapFailure> {
        recheck(transport.as_ref(), transport.route())?;
        Ok(Self { transport })
    }
}
impl<T: BootstrapAccountTransport> BootstrapNativePreflight for NativeBootstrapAccount<T> {
    fn inspect<'a>(
        &'a self,
        route: BootstrapRoute,
        host: &'a ReadOnlyHostProbe,
    ) -> PairingFuture<'a, Result<BootstrapHostFacts, BootstrapFailure>> {
        Box::pin(async move {
            recheck(self.transport.as_ref(), route)?;
            if host.account_uid != route.account_uid() {
                return Err(failure(
                    PairingFailure::RouteChanged,
                    BootstrapPhase::ReadOnlyPreflight,
                ));
            }
            let native = self.transport.inspect().await?;
            recheck(self.transport.as_ref(), route)?;
            host_facts(route, host, native)
        })
    }
    fn resolve(
        &self,
        request: [u8; 16],
        artifact_identity: [u8; 32],
    ) -> PairingFuture<'_, Result<BootstrapServer, BootstrapFailure>> {
        Box::pin(async move {
            let route = self.transport.route();
            recheck(self.transport.as_ref(), route)?;
            if request == [0; 16] || artifact_identity == [0; 32] {
                return Err(failure(
                    PairingFailure::InvalidRequest,
                    BootstrapPhase::Applied,
                ));
            }
            let native = self.transport.resolve(request, artifact_identity).await?;
            recheck(self.transport.as_ref(), route)?;
            server_facts(route, native, artifact_identity)
        })
    }
}
impl<T: BootstrapAccountTransport> BootstrapAuthorizationBoundary for NativeBootstrapAccount<T> {
    fn authorize<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
        prompt: &'a dyn ScopedAuthenticationPrompt,
    ) -> PairingFuture<'a, Result<Box<dyn BootstrapAuthorizedOperation>, BootstrapFailure>> {
        Box::pin(async move {
            let route = plan.route();
            recheck(self.transport.as_ref(), route)?;
            prompt.revalidate(plan)?;
            // Reprobe on the original OpenSSH control owner before any native request is reserved.
            let host = read_only_preflight(self.transport.as_ref(), self.transport.as_ref())
                .await
                .map_err(probe_failure)?;
            let native = self.transport.inspect().await?;
            let facts = host_facts(route, &host, native.clone())?;
            facts.supported_platform()?;
            facts.prerequisites(plan.operation() == BootstrapOperation::InstallAndStart)?;
            validate_plan(plan, &facts)?;
            let scoped = PlanLingerPrompt { plan, prompt };
            enable_linger_if_needed(
                self.transport.as_ref(),
                self.transport.as_ref(),
                &host,
                &scoped,
            )
            .await
            .map_err(linger_failure)?;
            recheck(self.transport.as_ref(), route)?;
            prompt.revalidate(plan)?;
            let request = self.transport.reserve_request().await?;
            recheck(self.transport.as_ref(), route)?;
            prompt.revalidate(plan)?;
            if request == [0; 16] {
                return Err(failure(
                    PairingFailure::InvalidDurableState,
                    BootstrapPhase::Authorized,
                ));
            }
            Ok(Box::new(NativeOperation {
                plan: plan.clone(),
                request,
                expected: native.registration,
                transport: Arc::clone(&self.transport),
            }) as Box<dyn BootstrapAuthorizedOperation>)
        })
    }
}

struct NativeOperation<T> {
    plan: BootstrapPlan,
    request: [u8; 16],
    expected: BootstrapAccountRegistration,
    transport: Arc<T>,
}
impl<T: BootstrapAccountTransport> BootstrapAuthorizedOperation for NativeOperation<T> {
    fn plan(&self) -> &BootstrapPlan {
        &self.plan
    }
    fn request_identity(&self) -> [u8; 16] {
        self.request
    }
    fn apply(
        self: Box<Self>,
        artifact: Option<VerifiedServerArtifact>,
    ) -> PairingFuture<'static, Result<BootstrapServer, BootstrapFailure>> {
        // Calling apply transfers ownership. A dropped observer cannot abandon accepted native work.
        let worker = tokio::spawn(async move {
            let route = self.plan.route();
            recheck(self.transport.as_ref(), route)?;
            let expected_artifact = self
                .plan
                .artifact_identity()
                .ok_or_else(|| failure(PairingFailure::InvalidRequest, BootstrapPhase::Applied))?;
            let native = match self.plan.operation() {
                BootstrapOperation::InstallAndStart => {
                    let artifact = artifact.ok_or_else(|| {
                        failure(PairingFailure::InvalidRequest, BootstrapPhase::Applied)
                    })?;
                    if artifact.artifact_sha256() != expected_artifact
                        || artifact.feed().document().build_identity != route.build()
                    {
                        return Err(failure(
                            PairingFailure::IdentityMismatch,
                            BootstrapPhase::Applied,
                        ));
                    }
                    self.transport.install(self.request, artifact).await?
                }
                BootstrapOperation::AuthorizeOwnerEnrollment => {
                    if artifact.is_some() {
                        return Err(failure(
                            PairingFailure::InvalidRequest,
                            BootstrapPhase::Applied,
                        ));
                    }
                    let proposal = self.plan.owner_proposal().ok_or_else(|| {
                        failure(PairingFailure::InvalidRequest, BootstrapPhase::Applied)
                    })?;
                    self.transport
                        .authorize_owner(self.request, &self.expected, proposal)
                        .await?
                }
            };
            recheck(self.transport.as_ref(), route)?;
            server_facts(route, native, expected_artifact)
        });
        Box::pin(async move {
            worker
                .await
                .map_err(|_| failure(PairingFailure::RecoveryRequired, BootstrapPhase::Applied))?
        })
    }
}

struct PlanLingerPrompt<'a> {
    plan: &'a BootstrapPlan,
    prompt: &'a dyn ScopedAuthenticationPrompt,
}
impl LingerAuthenticationPrompt for PlanLingerPrompt<'_> {
    fn revalidate(&self, uid: u32) -> Result<(), BootstrapLingerFailure> {
        if uid != self.plan.route().account_uid() || !self.plan.requires_linger_authorization() {
            return Err(BootstrapLingerFailure::RouteChanged);
        }
        self.prompt.revalidate(self.plan).map_err(prompt_failure)
    }
    fn respond(&self, uid: u32) -> LingerFuture<'_, Zeroizing<Vec<u8>>> {
        Box::pin(async move {
            self.revalidate(uid)?;
            self.prompt.respond(self.plan).await.map_err(prompt_failure)
        })
    }
}

fn validate_plan(plan: &BootstrapPlan, facts: &BootstrapHostFacts) -> Result<(), BootstrapFailure> {
    let matching = match (plan.operation(), facts.registration) {
        (BootstrapOperation::InstallAndStart, BootstrapRegistration::Missing) => {
            plan.registration_generation().is_none()
                && plan.expected_server().is_none()
                && plan.owner_proposal().is_none()
        }
        (
            BootstrapOperation::AuthorizeOwnerEnrollment,
            BootstrapRegistration::Running {
                pin,
                generation,
                artifact_identity,
                ..
            },
        ) => {
            plan.expected_server() == Some(pin)
                && plan.registration_generation() == Some(generation)
                && plan.artifact_identity() == Some(artifact_identity)
                && plan
                    .owner_proposal()
                    .is_some_and(|p| p.parts().server.bytes() == pin.identity)
        }
        _ => false,
    };
    if !matching
        || plan.requires_linger_authorization() != (facts.linger == BootstrapLinger::Disabled)
    {
        return Err(failure(
            PairingFailure::RouteChanged,
            BootstrapPhase::Planned,
        ));
    }
    Ok(())
}
fn host_facts(
    route: BootstrapRoute,
    host: &ReadOnlyHostProbe,
    native: BootstrapAccountFacts,
) -> Result<BootstrapHostFacts, BootstrapFailure> {
    check_key(route, &native)?;
    let linger = match (host.os, native.linger) {
        (BootstrapOs::MacOs, None) => BootstrapLinger::NotApplicable,
        (BootstrapOs::Linux, Some(LingerState::Enabled)) => BootstrapLinger::Enabled,
        (BootstrapOs::Linux, Some(LingerState::Disabled)) => BootstrapLinger::Disabled,
        _ => BootstrapLinger::Unavailable,
    };
    if linger != host.linger {
        return Err(failure(
            PairingFailure::RouteChanged,
            BootstrapPhase::ReadOnlyPreflight,
        ));
    }
    let registration = match native.registration {
        BootstrapAccountRegistration::Missing => BootstrapRegistration::Missing,
        BootstrapAccountRegistration::Stopped { pin, generation } => {
            BootstrapRegistration::Stopped { pin, generation }
        }
        BootstrapAccountRegistration::Running {
            pin,
            generation,
            artifact_identity,
            version,
            protocol_majors,
        } => BootstrapRegistration::Running {
            pin,
            generation,
            artifact_identity,
            version: host_version(version.as_str())?,
            protocol_majors: majors(&protocol_majors)?,
        },
    };
    Ok(BootstrapHostFacts {
        route,
        account_uid: host.account_uid,
        os: host.os,
        architecture: host.architecture,
        os_version: host.os_version,
        libc: host.libc,
        kernel: host.kernel,
        supervision: host.supervision,
        registration,
        storage: BootstrapPrerequisite::Available,
        installation: BootstrapPrerequisite::Available,
        linger,
        sudo: match host.sudo {
            spaceterm_core::bootstrap::BootstrapSudoAvailability::Available => {
                BootstrapSudo::ResponseRequired
            }
            spaceterm_core::bootstrap::BootstrapSudoAvailability::Unavailable => {
                BootstrapSudo::Unavailable
            }
        },
        authorization_agent: host.sudo
            == spaceterm_core::bootstrap::BootstrapSudoAvailability::Available,
        background_items_allowed: native.supervisor
            != spaceterm_host::account_service::SupervisorState::Suppressed,
    })
}
fn server_facts(
    route: BootstrapRoute,
    native: BootstrapAccountFacts,
    expected_artifact: [u8; 32],
) -> Result<BootstrapServer, BootstrapFailure> {
    check_key(route, &native)?;
    match native.registration {
        BootstrapAccountRegistration::Running {
            pin,
            generation,
            artifact_identity,
            protocol_majors,
            ..
        } if artifact_identity == expected_artifact => Ok(BootstrapServer {
            route,
            pin,
            registration_generation: generation,
            approved_artifact_identity: artifact_identity,
            protocol_majors: majors(&protocol_majors)?,
            running: true,
        }),
        // No compatibility is asserted for deliberately Stopped code. Callers refuse Start before negotiation.
        BootstrapAccountRegistration::Stopped { pin, generation } => Ok(BootstrapServer {
            route,
            pin,
            registration_generation: generation,
            approved_artifact_identity: expected_artifact,
            protocol_majors: [0, 0],
            running: false,
        }),
        _ => Err(failure(
            PairingFailure::IdentityMismatch,
            BootstrapPhase::Applied,
        )),
    }
}
fn majors(values: &[u16]) -> Result<[u16; 2], BootstrapFailure> {
    let values: [u16; 2] = values.try_into().map_err(|_| {
        failure(
            PairingFailure::Incompatible,
            BootstrapPhase::ReadOnlyPreflight,
        )
    })?;
    if values[0] == 0 || values[0] >= values[1] || !values.iter().any(|m| matches!(m, 1 | 2)) {
        return Err(failure(
            PairingFailure::Incompatible,
            BootstrapPhase::ReadOnlyPreflight,
        ));
    }
    Ok(values)
}
fn host_version(value: &str) -> Result<HostVersion, BootstrapFailure> {
    let mut parts = value.split(['-', '+']).next().unwrap_or("").split('.');
    let mut component = || {
        parts
            .next()
            .and_then(|part| part.parse().ok())
            .ok_or_else(|| {
                failure(
                    PairingFailure::InvalidDurableState,
                    BootstrapPhase::ReadOnlyPreflight,
                )
            })
    };
    let version = HostVersion::new(component()?, component()?, component()?);
    if parts.next().is_some() {
        return Err(failure(
            PairingFailure::InvalidDurableState,
            BootstrapPhase::ReadOnlyPreflight,
        ));
    }
    Ok(version)
}
fn check_key(
    route: BootstrapRoute,
    native: &BootstrapAccountFacts,
) -> Result<(), BootstrapFailure> {
    let build = match native.key.build_identity {
        BuildIdentity::Release => BuildChannel::Release,
        BuildIdentity::Preflight => BuildChannel::Preflight,
        BuildIdentity::Development => BuildChannel::Development,
    };
    if native.key.account_uid != route.account_uid() || build != route.build() {
        return Err(failure(
            PairingFailure::RouteChanged,
            BootstrapPhase::ReadOnlyPreflight,
        ));
    }
    Ok(())
}
fn recheck<T: BootstrapAccountTransport>(
    transport: &T,
    route: BootstrapRoute,
) -> Result<(), BootstrapFailure> {
    if transport.route() != route || transport.account_uid() != route.account_uid() {
        return Err(failure(
            PairingFailure::RouteChanged,
            BootstrapPhase::OpenSshAuthenticated,
        ));
    }
    BootstrapProbeLease::revalidate(transport).map_err(probe_failure)
}
fn failure(class: PairingFailure, phase: BootstrapPhase) -> BootstrapFailure {
    BootstrapFailure::new(class, phase)
}
fn probe_failure(value: BootstrapProbeFailure) -> BootstrapFailure {
    let class = match value {
        BootstrapProbeFailure::InvalidProbe => {
            return BootstrapFailure {
                class: BootstrapFailureClass::InvalidProbe,
                phase: BootstrapPhase::ReadOnlyPreflight,
            };
        }
        BootstrapProbeFailure::LimitExceeded => PairingFailure::LimitExceeded,
        BootstrapProbeFailure::RouteChanged => PairingFailure::RouteChanged,
        BootstrapProbeFailure::Cancelled => PairingFailure::Cancelled,
        BootstrapProbeFailure::Offline => PairingFailure::Offline,
        BootstrapProbeFailure::TimedOut => PairingFailure::TimedOut,
    };
    failure(class, BootstrapPhase::ReadOnlyPreflight)
}
fn prompt_failure(value: BootstrapFailure) -> BootstrapLingerFailure {
    match value.class {
        BootstrapFailureClass::Pairing(PairingFailure::Cancelled) => {
            BootstrapLingerFailure::Cancelled
        }
        BootstrapFailureClass::Pairing(PairingFailure::AuthorizationDenied) => {
            BootstrapLingerFailure::Denied
        }
        BootstrapFailureClass::Pairing(PairingFailure::RouteChanged) => {
            BootstrapLingerFailure::RouteChanged
        }
        BootstrapFailureClass::Pairing(PairingFailure::LimitExceeded) => {
            BootstrapLingerFailure::LimitExceeded
        }
        _ => BootstrapLingerFailure::Unavailable,
    }
}
fn linger_failure(value: BootstrapLingerFailure) -> BootstrapFailure {
    let class = match value {
        BootstrapLingerFailure::Denied => PairingFailure::AuthorizationDenied,
        BootstrapLingerFailure::Cancelled => PairingFailure::Cancelled,
        BootstrapLingerFailure::RouteChanged => PairingFailure::RouteChanged,
        BootstrapLingerFailure::LimitExceeded => PairingFailure::LimitExceeded,
        BootstrapLingerFailure::InvalidResponse => PairingFailure::InvalidRequest,
        BootstrapLingerFailure::TimedOut => PairingFailure::TimedOut,
        BootstrapLingerFailure::Unavailable => PairingFailure::AuthorizationUnavailable,
        BootstrapLingerFailure::StateUnavailable => {
            return BootstrapFailure {
                class: BootstrapFailureClass::LingerUnavailable,
                phase: BootstrapPhase::Authorized,
            };
        }
    };
    failure(class, BootstrapPhase::Authorized)
}

/// Native account errors retain their classifications, not raw process output or paths.
pub fn service_failure(value: ServiceFailure, phase: BootstrapPhase) -> BootstrapFailure {
    let class = match value {
        ServiceFailure::StorageLocked => {
            return BootstrapFailure {
                class: BootstrapFailureClass::StorageLocked,
                phase,
            };
        }
        ServiceFailure::LoginSessionUnavailable => {
            return BootstrapFailure {
                class: BootstrapFailureClass::LoginSessionUnavailable,
                phase,
            };
        }
        ServiceFailure::Suppressed => {
            return BootstrapFailure {
                class: BootstrapFailureClass::BackgroundItemsSuppressed,
                phase,
            };
        }
        ServiceFailure::AuthorizationDenied => PairingFailure::AuthorizationDenied,
        ServiceFailure::AuthorizationCancelled => PairingFailure::Cancelled,
        ServiceFailure::AuthorizationUnavailable => PairingFailure::AuthorizationUnavailable,
        ServiceFailure::IdentityMismatch | ServiceFailure::StaleAssessment => {
            PairingFailure::RouteChanged
        }
        ServiceFailure::ArtifactRejected => PairingFailure::IdentityMismatch,
        ServiceFailure::UnsupportedHost => PairingFailure::UnsupportedTarget,
        ServiceFailure::SupervisorUnavailable => PairingFailure::ServiceManagerUnavailable,
        ServiceFailure::LimitExceeded => PairingFailure::LimitExceeded,
        ServiceFailure::Pending
        | ServiceFailure::Indeterminate
        | ServiceFailure::CleanupUnresolved => PairingFailure::RecoveryRequired,
        ServiceFailure::ConfirmationRequired => PairingFailure::ExplicitStartRequired,
        ServiceFailure::PairingRequired => PairingFailure::AuthorizationRequired,
        ServiceFailure::RequestConflict => PairingFailure::RequestIdentityReused,
        ServiceFailure::UnknownRequest | ServiceFailure::RequestRetired => {
            PairingFailure::InvalidRequest
        }
        ServiceFailure::Missing | ServiceFailure::StorageUnavailable => {
            PairingFailure::StorageUnavailable
        }
        ServiceFailure::UnsafeStorage
        | ServiceFailure::InvalidRecord
        | ServiceFailure::UnsupportedSchema
        | ServiceFailure::IncompatibleStorage => PairingFailure::InvalidDurableState,
        ServiceFailure::RootAccount => PairingFailure::InvalidRequest,
    };
    failure(class, phase)
}
