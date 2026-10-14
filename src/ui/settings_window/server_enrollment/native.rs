//! Foreground enrollment shares the application's native pairing runtime and registry.
//! Native invitation delivery and SSH effects remain constructor-injected capabilities.

mod bootstrap;
mod direct;

use std::{rc::Rc, sync::Arc};

use gpui::{App, Task};
use spaceterm_client::{
    api::RouteDescription,
    pairing::{
        bootstrap::{BootstrapHostFacts, BootstrapOutcome, DeliberateBootstrap},
        credentials::{PairingFuture, PreparedCredential},
        enrollment::EnrollmentAttemptRequest,
        journal::{JournalPairingStore, PreparedEnrollment, SavedPairing},
    },
    registry::{ClientRegistry, ServerHandle},
};
use spaceterm_client_native::pairing_runtime::NativePairingRuntime;
use spaceterm_core::pairing::PairingFailure;
use spaceterm_protocol::pairing_adapter::EnrollmentAuthorization;
use zeroize::Zeroizing;

use super::{
    BootstrapRequest, DirectApproval, DirectCandidate, DurableEnrollment, EnrollmentAttempt,
    EnrollmentDiscovery, EnrollmentFailure, EnrollmentProvider, EnrollmentTarget, SshCandidate,
};

/// Transient direct invitation delivery, already bound to the prepared native public keys.
/// A preparation adapter may reserve client credentials, but must not enroll or start a server.
pub(crate) struct DirectPreparation {
    pub(crate) prepared: PreparedEnrollment,
    pub(crate) device: PreparedCredential,
    pub(crate) authorization: Option<EnrollmentAuthorization>,
    pub(crate) route: RouteDescription,
    pub(crate) majors: Vec<u32>,
}

pub(crate) trait DirectInvitationPreparation: Send + Sync {
    fn prepare_claim<'a>(
        &'a self,
        _address: String,
        _descriptor: Zeroizing<String>,
        _compared_fingerprint: String,
        _attempt: EnrollmentAttempt,
        _pairing: &'a NativePairingRuntime,
    ) -> PairingFuture<'a, Result<String, EnrollmentFailure>> {
        Box::pin(async { Err(EnrollmentFailure::ProviderUnavailable) })
    }
    fn prepare<'a>(
        &'a self,
        address: String,
        invitation: Zeroizing<String>,
        attempt: EnrollmentAttempt,
        pairing: &'a NativePairingRuntime,
    ) -> PairingFuture<'a, Result<DirectPreparation, EnrollmentFailure>>;
}

pub(crate) struct BootstrapCompletion {
    pub(crate) prepared: PreparedEnrollment,
    pub(crate) saved: SavedPairing,
    pub(crate) outcome: BootstrapOutcome,
}

/// This capability is installed only with the production native Bootstrap providers.
/// Ordinary connection restoration has no reference to it.
pub(crate) trait ForegroundBootstrapServices: Send + Sync {
    fn inspect(
        &self,
        host: String,
        attempt: EnrollmentAttempt,
    ) -> PairingFuture<'_, Result<(BootstrapHostFacts, Option<BootstrapRequest>), EnrollmentFailure>>;
    fn bootstrap(
        &self,
        intent: DeliberateBootstrap,
        candidate: SshCandidate,
    ) -> PairingFuture<'_, Result<BootstrapCompletion, EnrollmentFailure>>;
    fn resolve(
        &self,
        candidate: SshCandidate,
        request: BootstrapRequest,
    ) -> PairingFuture<'_, Result<BootstrapCompletion, EnrollmentFailure>>;
    fn start(&self, candidate: SshCandidate) -> PairingFuture<'_, Result<(), EnrollmentFailure>>;
}

/// Recovered native services supplied once, not recreated when a Settings window opens.
#[derive(Clone)]
pub(crate) struct NativeEnrollmentServices {
    pub(crate) pairing: Arc<NativePairingRuntime>,
    pub(crate) direct: Arc<dyn DirectInvitationPreparation>,
    pub(crate) bootstrap: Option<Arc<dyn ForegroundBootstrapServices>>,
    pub(crate) executor: tokio::runtime::Handle,
    pub(crate) local: Option<spaceterm_client_native::NativeClientProvider>,
}

/// Production enrollment uses the exact ordinary route owner and journal. The
/// foreground provider is installed even when no server has been paired yet.
pub(crate) fn production(
    journal: Arc<spaceterm_client::journal::JournalController>,
    routes: Arc<spaceterm_client_native::route::NativeRouteConnector>,
    invalidator: Arc<dyn spaceterm_client::pending::LocalUseInvalidator>,
    local: Option<spaceterm_client_native::NativeClientProvider>,
    executor: tokio::runtime::Handle,
) -> NativeEnrollmentServices {
    let pairing = Arc::new(NativePairingRuntime::with_shared_routes(
        journal,
        routes.clone(),
        invalidator,
        spaceterm_protocol::authentication::HandshakeConfig::default(),
    ));
    let build = if cfg!(spaceterm_packaged) && cfg!(spaceterm_release) {
        spaceterm_client::pairing::bootstrap::BuildChannel::Release
    } else if cfg!(spaceterm_packaged) {
        spaceterm_client::pairing::bootstrap::BuildChannel::Preflight
    } else {
        spaceterm_client::pairing::bootstrap::BuildChannel::Development
    };
    let factory =
        Arc::new(spaceterm_client_native::route::NativeBootstrapHostFactory::new(routes, build));
    let bootstrap = spaceterm_client_native::bootstrap::production::ProductionSshBootstrap::new(
        pairing.clone(),
        factory,
    );
    NativeEnrollmentServices {
        pairing,
        direct: Arc::new(direct::NativeDirectInvitation),
        bootstrap: Some(Arc::new(bootstrap::NativeForegroundBootstrap(bootstrap))),
        executor,
        local,
    }
}

pub(crate) trait EnrollmentPublication {
    /// The caller has revalidated the exact durable binding synchronously.
    fn publish(
        &self,
        enrollment: &DurableEnrollment,
        handle: ServerHandle,
        cx: &mut App,
    ) -> Result<(), EnrollmentFailure>;
}

pub(crate) struct NativeEnrollmentProvider {
    services: Arc<NativeEnrollmentServices>,
    registry: ClientRegistry,
    publication: Rc<dyn EnrollmentPublication>,
}

impl NativeEnrollmentProvider {
    pub(crate) fn new(
        services: NativeEnrollmentServices,
        registry: ClientRegistry,
        publication: Rc<dyn EnrollmentPublication>,
    ) -> Self {
        Self {
            services: Arc::new(services),
            registry,
            publication,
        }
    }

    fn task<T: Send + 'static>(
        &self,
        cx: &mut App,
        future: impl std::future::Future<Output = Result<T, EnrollmentFailure>> + Send + 'static,
    ) -> Task<Result<T, EnrollmentFailure>> {
        let task = self.services.executor.spawn(future);
        // Dropping a sheet drops only its observer. Native work retains its own cancellation
        // fences and cleanup; it cannot publish or connect through this detached JoinHandle.
        cx.spawn(async move |_| {
            task.await
                .map_err(|_| EnrollmentFailure::ProviderUnavailable)?
        })
    }
}

async fn prepare_publication(
    services: &NativeEnrollmentServices,
    registry: &ClientRegistry,
    mut enrollment: DurableEnrollment,
) -> Result<DurableEnrollment, EnrollmentFailure> {
    enrollment.attempt.validate()?;
    let ordinary = services
        .pairing
        .restore(
            enrollment.saved.key(),
            enrollment.saved.client().parts().client.id,
        )
        .await?;
    enrollment.attempt.validate()?;
    let binding = ordinary
        .binding()
        .binding()
        .map_err(|_| PairingFailure::InvalidDurableState)?;
    // Retire only the exact suppressed incarnation. A healthy old binding
    // remains an error rather than permission to replace its connection actor.
    let state = services.pairing.journal().snapshot().map_err(classify)?;
    // Saving the new server binding can replace its old saved row. Durable
    // tombstones still identify the exact registry actor that must be joined.
    for old in state.tombstones() {
        if old.parts().server == binding.target().server() {
            enrollment.attempt.validate()?;
            registry.remove_suppressed(old).await.map_err(classify)?;
        }
    }
    for old in state.bindings() {
        let old = old.binding().map_err(classify)?;
        if old.target().server() == binding.target().server()
            && old != binding
            && !services
                .pairing
                .journal()
                .ordinary_use_allowed(&old)
                .map_err(classify)?
        {
            enrollment.attempt.validate()?;
            registry
                .remove_suppressed(&old.target().local_pairing().map_err(classify)?)
                .await
                .map_err(classify)?;
        }
    }
    enrollment.attempt.validate()?;
    let handle = registry.register(binding.clone()).await.map_err(classify)?;
    enrollment.attempt.validate()?;
    enrollment.settings = Some(
        crate::ui::settings_window::server_runtime::SettingsServices::for_enrollment(
            &binding,
            &handle,
            services.pairing.journal(),
            services.local.as_ref(),
        )
        .map_err(classify)?,
    );
    enrollment.handle = Some(handle);
    enrollment.attempt.validate()?;
    Ok(enrollment)
}

fn classify(error: spaceterm_client::failure::ClientFailure) -> EnrollmentFailure {
    use spaceterm_client::failure::ClientFailureClass as C;
    match error.class() {
        C::AuthorizationDenied | C::StaleAuthority => PairingFailure::PairingRevoked.into(),
        C::StorageUnavailable => PairingFailure::StorageUnavailable.into(),
        C::RecoveryRequired => PairingFailure::RecoveryRequired.into(),
        C::ResourceLimit => PairingFailure::LimitExceeded.into(),
        C::Cancelled => PairingFailure::Cancelled.into(),
        _ => PairingFailure::InvalidDurableState.into(),
    }
}

impl EnrollmentProvider for NativeEnrollmentProvider {
    fn prepare_claim(
        &self,
        address: String,
        descriptor: Zeroizing<String>,
        compared_fingerprint: String,
        attempt: EnrollmentAttempt,
        cx: &mut App,
    ) -> Task<Result<String, EnrollmentFailure>> {
        let services = self.services.clone();
        self.task(cx, async move {
            attempt.validate()?;
            let claim = services
                .direct
                .prepare_claim(
                    address,
                    descriptor,
                    compared_fingerprint,
                    attempt.clone(),
                    &services.pairing,
                )
                .await?;
            attempt.validate()?;
            Ok(claim)
        })
    }
    fn inspect(
        &self,
        target: EnrollmentTarget,
        attempt: EnrollmentAttempt,
        cx: &mut App,
    ) -> Task<Result<EnrollmentDiscovery, EnrollmentFailure>> {
        let services = self.services.clone();
        self.task(cx, async move {
            attempt.validate()?;
            match target {
                EnrollmentTarget::Direct {
                    address,
                    invitation,
                } => {
                    if address.is_empty()
                        || address.len() > 2048
                        || address.chars().any(char::is_control)
                        || invitation.is_empty()
                        || invitation.len() > 65_536
                    {
                        return Err(PairingFailure::InvalidRequest.into());
                    }
                    let preparation = services
                        .direct
                        .prepare(address, invitation, attempt.clone(), &services.pairing)
                        .await?;
                    attempt.validate()?;
                    let reservation = preparation
                        .prepared
                        .reservation
                        .clone()
                        .ok_or(PairingFailure::InvalidRequest)?;
                    let mut candidate = DirectCandidate::new(
                        attempt.clone(),
                        preparation.prepared,
                        preparation.device,
                        reservation,
                        preparation.authorization,
                        preparation.majors,
                    )?;
                    // Journal route binding and SSH capture are local reservation effects only.
                    services
                        .pairing
                        .prepare_route(&candidate.prepared, preparation.route.clone())
                        .await?;
                    attempt.validate()?;
                    candidate.native_route = Some(preparation.route);
                    Ok(EnrollmentDiscovery::Direct(Box::new(candidate)))
                }
                EnrollmentTarget::Ssh { host } => {
                    if host.is_empty() || host.len() > 2048 || host.chars().any(char::is_control) {
                        return Err(PairingFailure::InvalidRequest.into());
                    }
                    let bootstrap = services
                        .bootstrap
                        .as_ref()
                        .ok_or(EnrollmentFailure::ProviderUnavailable)?;
                    let (facts, pending) = bootstrap.inspect(host, attempt.clone()).await?;
                    attempt.validate()?;
                    let mut candidate = SshCandidate::new(attempt, facts)?;
                    if let Some(pending) = pending {
                        candidate = candidate.retain_request(pending)?;
                    }
                    Ok(EnrollmentDiscovery::Ssh(Box::new(candidate)))
                }
            }
        })
    }

    fn enroll_direct(
        &self,
        approval: DirectApproval,
        cx: &mut App,
    ) -> Task<Result<DurableEnrollment, EnrollmentFailure>> {
        let services = self.services.clone();
        let registry = self.registry.clone();
        self.task(cx, async move {
            approval.attempt.validate()?;
            let route = approval.native_route.ok_or(PairingFailure::RouteChanged)?;
            let saved = services
                .pairing
                .enroll(
                    EnrollmentAttemptRequest {
                        verified: &approval.verified,
                        prepared: &approval.prepared,
                        device: &approval.device,
                        reservation: approval.reservation,
                        authorization: approval.authorization.as_ref(),
                        majors: approval.majors,
                    },
                    route,
                )
                .await?;
            approval.attempt.validate()?;
            let store = JournalPairingStore::new(services.pairing.journal().clone());
            let enrollment =
                DurableEnrollment::verify(approval.attempt, &store, approval.prepared, saved)
                    .await?;
            prepare_publication(&services, &registry, enrollment).await
        })
    }

    fn bootstrap(
        &self,
        intent: DeliberateBootstrap,
        candidate: SshCandidate,
        cx: &mut App,
    ) -> Task<Result<DurableEnrollment, EnrollmentFailure>> {
        let services = self.services.clone();
        let registry = self.registry.clone();
        self.task(cx, async move {
            candidate.attempt.validate()?;
            if candidate.pending.is_some() {
                return Err(PairingFailure::RecoveryRequired.into());
            }
            let confirmed = candidate
                .confirmation
                .lock()
                .map_err(|_| PairingFailure::InvalidDurableState)?
                .clone();
            let supplied = intent.cancellation();
            if confirmed.as_ref() != Some(&supplied)
                || supplied.generation() != candidate.attempt.generation()
            {
                return Err(PairingFailure::IdentityMismatch.into());
            }
            supplied.validate(super::BootstrapPhase::ExplicitIntent)?;
            if !matches!(
                candidate.facts.registration,
                super::BootstrapRegistration::Running { .. }
                    | super::BootstrapRegistration::Missing
            ) {
                return Err(PairingFailure::ExplicitStartRequired.into());
            }
            let bootstrap = services
                .bootstrap
                .as_ref()
                .ok_or(EnrollmentFailure::ProviderUnavailable)?;
            let completion = bootstrap.bootstrap(intent, candidate.clone()).await?;
            verify_bootstrap(&services, &registry, candidate, completion).await
        })
    }

    fn resolve_bootstrap(
        &self,
        candidate: SshCandidate,
        request: BootstrapRequest,
        cx: &mut App,
    ) -> Task<Result<DurableEnrollment, EnrollmentFailure>> {
        let services = self.services.clone();
        let registry = self.registry.clone();
        self.task(cx, async move {
            candidate.attempt.validate()?;
            if candidate.pending != Some(request) {
                return Err(PairingFailure::IdentityMismatch.into());
            }
            let bootstrap = services
                .bootstrap
                .as_ref()
                .ok_or(EnrollmentFailure::ProviderUnavailable)?;
            let completion = bootstrap.resolve(candidate.clone(), request).await?;
            verify_bootstrap(&services, &registry, candidate, completion).await
        })
    }

    fn start(&self, candidate: SshCandidate, cx: &mut App) -> Task<Result<(), EnrollmentFailure>> {
        let services = self.services.clone();
        self.task(cx, async move {
            candidate.attempt.validate()?;
            if candidate.pending.is_some()
                || !matches!(
                    candidate.facts.registration,
                    super::BootstrapRegistration::Stopped { .. }
                )
            {
                return Err(PairingFailure::ExplicitStartRequired.into());
            }
            let bootstrap = services
                .bootstrap
                .as_ref()
                .ok_or(EnrollmentFailure::ProviderUnavailable)?;
            bootstrap.start(candidate.clone()).await?;
            candidate.attempt.validate()
        })
    }

    fn publish(
        &self,
        mut enrollment: DurableEnrollment,
        cx: &mut App,
    ) -> Result<(), EnrollmentFailure> {
        enrollment.attempt.validate()?;
        let state = self
            .services
            .pairing
            .journal()
            .snapshot()
            .map_err(classify)?;
        let handle = enrollment
            .handle
            .take()
            .ok_or(PairingFailure::InvalidDurableState)?;
        let saved = state
            .bindings()
            .iter()
            .find(|saved| {
                saved.proof() == enrollment.saved.receipt()
                    && saved.binding().is_ok_and(|binding| {
                        binding.target().server() == handle.identity()
                            && binding.target().client()
                                == enrollment.saved.client().parts().client.id
                            && binding.target().local_pairing().as_ref()
                                == Ok(enrollment.saved.key())
                    })
            })
            .ok_or(PairingFailure::InvalidDurableState)?;
        let binding = saved.binding().map_err(classify)?;
        if !self
            .services
            .pairing
            .journal()
            .ordinary_use_allowed(&binding)
            .map_err(classify)?
        {
            return Err(PairingFailure::PairingRevoked.into());
        }
        self.publication.publish(&enrollment, handle.clone(), cx)?;
        // Publication is the final foreground admission. Inspection, receipt recovery and
        // canceled attempts never request an ordinary connection.
        self.services.executor.spawn(async move {
            let _ = handle.connect().await;
        });
        Ok(())
    }
}

async fn verify_bootstrap(
    services: &NativeEnrollmentServices,
    registry: &ClientRegistry,
    candidate: SshCandidate,
    completion: BootstrapCompletion,
) -> Result<DurableEnrollment, EnrollmentFailure> {
    candidate.attempt.validate()?;
    if !completion.outcome.server.running {
        return Err(PairingFailure::ExplicitStartRequired.into());
    }
    if completion.outcome.server.registration_generation == 0
        || completion.outcome.server.approved_artifact_identity == [0; 32]
    {
        return Err(PairingFailure::InvalidDurableState.into());
    }
    if !completion
        .outcome
        .server
        .protocol_majors
        .iter()
        .any(|major| matches!(major, 1 | 2))
    {
        return Err(PairingFailure::Incompatible.into());
    }
    if completion.outcome.server.route != candidate.facts.route
        || candidate.pending.is_some_and(|pending| {
            pending.artifact != completion.outcome.server.approved_artifact_identity
        })
    {
        return Err(PairingFailure::RouteChanged.into());
    }
    match candidate.facts.registration {
        super::BootstrapRegistration::Running {
            pin,
            generation,
            artifact_identity,
            ..
        } => {
            if pin != completion.outcome.server.pin
                || generation != completion.outcome.server.registration_generation
                || artifact_identity != completion.outcome.server.approved_artifact_identity
                || !(completion.outcome.reused || candidate.pending.is_some())
            {
                return Err(PairingFailure::IdentityMismatch.into());
            }
        }
        super::BootstrapRegistration::Missing
            if !completion.outcome.reused || candidate.pending.is_some() => {}
        _ => return Err(PairingFailure::IdentityMismatch.into()),
    }
    let store = JournalPairingStore::new(services.pairing.journal().clone());
    let enrollment = DurableEnrollment::verify_bootstrap(
        candidate.attempt,
        &store,
        completion.prepared,
        completion.saved,
        completion.outcome,
    )
    .await?;
    prepare_publication(services, registry, enrollment).await
}

#[cfg(test)]
pub(super) mod tests;
