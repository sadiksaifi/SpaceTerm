//! Foreground production Bootstrap composition. Ordinary routes do not own this provider.

use super::{
    BootstrapAccountTransport, BootstrapNativePreflight, BootstrapOpenSsh, BootstrapOwnerDelivery,
    BootstrapOwnerExchange, NativeBootstrapAccount, NativeBootstrapEnrollment,
    SshNativeServerSetup, service_failure,
};
use crate::pairing_runtime::{NativePairingRuntime, SharedCredentials};
use spaceterm_client::{
    api::{RouteDescription, RouteParts},
    journal::JournalController,
    pairing::{
        bootstrap::*,
        credentials::{CredentialNamespace, PairingFuture},
        enrollment::{EnrollmentConnector, PairingTransport},
        journal::{PreparedEnrollment, SavedPairing},
    },
};
use spaceterm_core::pairing::{EnrollmentProposal, PairingFailure, RevocationRouteReference};
use spaceterm_host::{
    bootstrap::{
        command::{MAX_PROBE_BYTES, read_only_probe_script},
        linger::{
            BootstrapLingerFailure, BootstrapLingerTransport, LingerFuture, LingerInvocation,
            ScopedLingerAuthentication,
        },
        process::{
            BootstrapProbeFailure, BootstrapProbeFuture, BootstrapProbeLease,
            BootstrapProbeTransport,
        },
        wire::*,
    },
    native_deployment::contracts::NativeRequestId,
    transport::MessageTransport,
};
use spaceterm_protocol::pairing_adapter::{EnrollmentAuthorization, PublicEnrollmentReservation};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use zeroize::Zeroizing;

/// External SSH process boundary. Only fixed probes and closed framed operations are accepted.
/// The production adapter keeps an authenticated multiplex owner and never falls back to another host.
pub trait BootstrapRemoteConnection: Send + Sync {
    fn route(&self) -> BootstrapRoute;
    fn destination(&self) -> &str;
    fn revalidate(&self) -> Result<(), BootstrapFailure>;
    fn probe(&self) -> BootstrapProbeFuture<'_, Vec<u8>>;
    fn account<'a>(
        &'a self,
        request: AccountRequest,
        artifact: Option<&'a VerifiedServerArtifact>,
    ) -> PairingFuture<'a, Result<AccountReply, BootstrapFailure>>;
    fn owner_channel(&self)
    -> PairingFuture<'_, Result<Arc<dyn MessageTransport>, PairingFailure>>;
    fn prompt(&self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, BootstrapFailure>>;
    fn enable_linger<'a>(
        &'a self,
        invocation: &'a LingerInvocation,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> LingerFuture<'a, ()>;
}

pub struct BootstrapHostConnection {
    pub connection: Arc<dyn BootstrapRemoteConnection>,
    /// Exact signed artifact established by the production embedded-key download adapter.
    pub artifact: Option<VerifiedServerArtifact>,
}
/// Replaces the external SSH and byte-transfer boundary, not the portable Bootstrap engine.
pub trait BootstrapHostFactory: Send + Sync {
    fn open(
        &self,
        destination: String,
        generation: u64,
    ) -> PairingFuture<'_, Result<BootstrapHostConnection, BootstrapFailure>>;
}

pub struct ProductionBootstrapCompletion {
    pub prepared: PreparedEnrollment,
    pub saved: SavedPairing,
    pub outcome: BootstrapOutcome,
}
struct RetainedHost {
    generation: u64,
    connection: Arc<ForegroundConnection>,
    artifact: Mutex<Option<VerifiedServerArtifact>>,
}
/// Public production provider used by Settings and production-composition tests.
pub struct ProductionSshBootstrap {
    pairing: Arc<NativePairingRuntime>,
    credentials: SharedCredentials,
    hosts: Mutex<Vec<Arc<RetainedHost>>>,
    factory: Arc<dyn BootstrapHostFactory>,
}
impl ProductionSshBootstrap {
    pub fn new(pairing: Arc<NativePairingRuntime>, factory: Arc<dyn BootstrapHostFactory>) -> Self {
        let credentials = pairing.shared_credentials();
        Self {
            pairing,
            credentials,
            hosts: Mutex::new(Vec::new()),
            factory,
        }
    }
    pub async fn inspect(
        &self,
        destination: String,
        generation: u64,
    ) -> Result<BootstrapHostFacts, BootstrapFailure> {
        if generation == 0 {
            return Err(fail(
                PairingFailure::InvalidRequest,
                BootstrapPhase::ExplicitIntent,
            ));
        }
        let host = self.factory.open(destination, generation).await?;
        host.connection.revalidate()?;
        let transport = AccountTransport(host.connection.clone());
        let intent = ForegroundBootstrap::confirm(host.connection.route(), generation)?;
        let attempt = intent.cancellation();
        let setup = SshNativeServerSetup::new(
            transport.clone(),
            NativeBootstrapAccount::new(Arc::new(transport))?,
        );
        let ssh = setup
            .authenticate(&host.connection.route(), &attempt)
            .await?;
        let facts = setup.discover(&ssh).await?;
        facts.supported_platform()?;
        facts.prerequisites(matches!(facts.registration, BootstrapRegistration::Missing))?;
        let retained = Arc::new(RetainedHost {
            generation,
            connection: Arc::new(ForegroundConnection {
                inner: host.connection,
                foreground: Mutex::new(None),
            }),
            artifact: Mutex::new(host.artifact),
        });
        let mut hosts = self.hosts.lock().map_err(|_| {
            fail(
                PairingFailure::RecoveryRequired,
                BootstrapPhase::ReadOnlyPreflight,
            )
        })?;
        hosts.retain(|old| old.generation != generation);
        if hosts.len() >= 4 {
            // Retire the oldest foreground session. Its candidate now fails the
            // exact generation lookup instead of exhausting Bootstrap forever.
            hosts.remove(0);
        }
        hosts.push(retained);
        Ok(facts)
    }
    fn host(
        &self,
        route: BootstrapRoute,
        generation: u64,
    ) -> Result<Arc<RetainedHost>, BootstrapFailure> {
        self.hosts
            .lock()
            .map_err(|_| {
                fail(
                    PairingFailure::RecoveryRequired,
                    BootstrapPhase::ExplicitIntent,
                )
            })?
            .iter()
            .find(|host| host.generation == generation && host.connection.route() == route)
            .cloned()
            .ok_or_else(|| fail(PairingFailure::RouteChanged, BootstrapPhase::ExplicitIntent))
    }
    /// Binds the native sheet's live cancellation fence to this exact retained session.
    /// All subsequent remote effects and enrollment frames revalidate the fence.
    pub fn bind_foreground(
        &self,
        route: BootstrapRoute,
        generation: u64,
        valid: Arc<AtomicBool>,
    ) -> Result<(), BootstrapFailure> {
        let host = self.host(route, generation)?;
        if !valid.load(Ordering::Acquire) {
            return Err(fail(
                PairingFailure::Cancelled,
                BootstrapPhase::ExplicitIntent,
            ));
        }
        let mut foreground = host.connection.foreground.lock().map_err(|_| {
            fail(
                PairingFailure::RecoveryRequired,
                BootstrapPhase::ExplicitIntent,
            )
        })?;
        if foreground
            .as_ref()
            .is_some_and(|original| !Arc::ptr_eq(original, &valid))
        {
            return Err(fail(
                PairingFailure::RouteChanged,
                BootstrapPhase::ExplicitIntent,
            ));
        }
        *foreground = Some(valid);
        drop(foreground);
        host.connection.revalidate()
    }
    /// Retires a canceled inspection without disturbing any accepted native worker.
    pub fn abandon(&self, route: BootstrapRoute, generation: u64) -> Result<(), BootstrapFailure> {
        let host = self.host(route, generation)?;
        self.release(&host)
    }
    fn release(&self, host: &RetainedHost) -> Result<(), BootstrapFailure> {
        self.hosts
            .lock()
            .map_err(|_| fail(PairingFailure::RecoveryRequired, BootstrapPhase::Connected))?
            .retain(|old| {
                old.generation != host.generation
                    || old.connection.route() != host.connection.route()
            });
        Ok(())
    }
    pub async fn bootstrap(
        &self,
        intent: DeliberateBootstrap,
        facts: BootstrapHostFacts,
    ) -> Result<ProductionBootstrapCompletion, BootstrapFailure> {
        let attempt = intent.cancellation();
        let host = self.host(facts.route, attempt.generation())?;
        host.connection.revalidate()?;
        let artifact = if matches!(facts.registration, BootstrapRegistration::Missing) {
            host.artifact
                .lock()
                .map_err(|_| fail(PairingFailure::RecoveryRequired, BootstrapPhase::Planned))?
                .take()
        } else {
            None
        };
        let transport = AccountTransport(host.connection.clone());
        let setup = Arc::new(SshNativeServerSetup::new(
            transport.clone(),
            NativeBootstrapAccount::new(Arc::new(transport))?,
        ));
        let enrollment = self.enrollment_for(&host, facts).await?;
        let coordinator = BootstrapCoordinator::new(
            setup,
            SharedEnrollment(enrollment.clone()),
            Prompt(host.connection.clone()),
        );
        let outcome = coordinator.bootstrap(intent, artifact).await?;
        let (prepared, saved) = enrollment
            .completed()
            .map_err(|e| fail(e, BootstrapPhase::Enrolled))?
            .ok_or_else(|| fail(PairingFailure::RecoveryRequired, BootstrapPhase::Enrolled))?;
        self.release(&host)?;
        Ok(ProductionBootstrapCompletion {
            prepared,
            saved,
            outcome,
        })
    }
    async fn enrollment_for(
        &self,
        host: &Arc<RetainedHost>,
        facts: BootstrapHostFacts,
    ) -> Result<Arc<Enrollment>, BootstrapFailure> {
        let mut reference = [0; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut reference).map_err(
            |_| {
                fail(
                    PairingFailure::CredentialUnavailable,
                    BootstrapPhase::Planned,
                )
            },
        )?;
        let reference = match facts.registration {
            BootstrapRegistration::Running { pin, .. } => {
                let inventory = self
                    .pairing
                    .inventory()
                    .await
                    .map_err(|e| fail(e, BootstrapPhase::Planned))?;
                inventory
                    .prepared
                    .iter()
                    .filter(|prepared| {
                        prepared.pin == pin && !inventory.is_suppressed(&prepared.local_pairing)
                    })
                    .map(|prepared| prepared.route)
                    .next()
                    .unwrap_or(RevocationRouteReference::new(reference))
            }
            _ => RevocationRouteReference::new(reference),
        };
        let shared = Arc::new(OwnerState {
            connection: host.connection.clone(),
            pairing: self.pairing.clone(),
            reference,
            channel: Mutex::new(None),
            pin: Mutex::new(None),
            route: Mutex::new(None),
        });
        let namespace = CredentialNamespace::new(
            match host.connection.route().build() {
                BuildChannel::Release => "app.spaceterm.desktop",
                BuildChannel::Preflight => "app.spaceterm.preflight.desktop",
                BuildChannel::Development => "dev.spaceterm.desktop",
            }
            .to_owned(),
        )
        .map_err(|e| fail(e, BootstrapPhase::Planned))?;
        let enrollment = Arc::new(
            NativeBootstrapEnrollment::new(
                self.pairing.journal().clone(),
                self.credentials.clone(),
                OwnerConnector(shared.clone()),
                OwnerExchange(shared),
                namespace,
                host.connection.route(),
                reference,
            )
            .map_err(|e| fail(e, BootstrapPhase::Planned))?,
        );
        Ok(enrollment)
    }
    /// Reads pending original owner metadata without reenrolling or issuing new native authority.
    pub async fn inspect_with_pending(
        &self,
        destination: String,
        generation: u64,
    ) -> Result<(BootstrapHostFacts, Option<([u8; 16], [u8; 32])>), BootstrapFailure> {
        let facts = self.inspect(destination, generation).await?;
        let BootstrapRegistration::Running {
            pin,
            artifact_identity,
            ..
        } = facts.registration
        else {
            return Ok((facts, None));
        };
        let inventory = self
            .pairing
            .inventory()
            .await
            .map_err(|e| fail(e, BootstrapPhase::ReadOnlyPreflight))?;
        let prepared = inventory
            .prepared
            .iter()
            .filter(|prepared| {
                prepared.pin == pin && !inventory.is_suppressed(&prepared.local_pairing)
            })
            .collect::<Vec<_>>();
        if prepared.len() > 1 {
            return Err(fail(
                PairingFailure::RecoveryRequired,
                BootstrapPhase::ReadOnlyPreflight,
            ));
        }
        let Some(prepared) = prepared.first() else {
            return Ok((facts, None));
        };
        let host = self.host(facts.route, generation)?;
        match host
            .connection
            .account(
                AccountRequest::RetainedOwner {
                    keys: prepared.keys.clone(),
                },
                None,
            )
            .await?
        {
            AccountReply::RetainedOwner(Some((request, proposal)))
                if proposal.parts().keys == prepared.keys =>
            {
                Ok((facts, Some((*request.as_bytes(), artifact_identity))))
            }
            AccountReply::RetainedOwner(None) => Err(fail(
                PairingFailure::RecoveryRequired,
                BootstrapPhase::ReadOnlyPreflight,
            )),
            AccountReply::Failure(error) => {
                Err(service_failure(error, BootstrapPhase::ReadOnlyPreflight))
            }
            _ => Err(fail(
                PairingFailure::IdentityMismatch,
                BootstrapPhase::ReadOnlyPreflight,
            )),
        }
    }
    /// Resolves only an existing public owner request. It allocates no replacement and never Starts.
    pub async fn resolve(
        &self,
        facts: BootstrapHostFacts,
        generation: u64,
        request: [u8; 16],
        artifact_identity: [u8; 32],
    ) -> Result<ProductionBootstrapCompletion, BootstrapFailure> {
        let host = self.host(facts.route, generation)?;
        let transport = AccountTransport(host.connection.clone());
        let native = NativeBootstrapAccount::new(Arc::new(transport))?;
        let server = BootstrapNativePreflight::resolve(&native, request, artifact_identity).await?;
        if !server.running {
            return Err(fail(
                PairingFailure::ExplicitStartRequired,
                BootstrapPhase::Applied,
            ));
        }
        if server.route != facts.route
            || !matches!(facts.registration, BootstrapRegistration::Running { pin, artifact_identity, .. }
                if pin == server.pin && artifact_identity == server.approved_artifact_identity)
        {
            return Err(fail(
                PairingFailure::IdentityMismatch,
                BootstrapPhase::Applied,
            ));
        }
        let intent = ForegroundBootstrap::confirm_observed(facts, generation)?;
        let attempt = intent.cancellation();
        let enrollment = self.enrollment_for(&host, facts).await?;
        let proposal = enrollment.prepare_owner(&server, &attempt).await?;
        match host
            .connection
            .account(
                AccountRequest::RetainedOwner {
                    keys: proposal.parts().keys.clone(),
                },
                None,
            )
            .await?
        {
            AccountReply::RetainedOwner(Some((original, claim)))
                if original.as_bytes() == &request && claim == proposal => {}
            AccountReply::Failure(error) => {
                return Err(service_failure(error, BootstrapPhase::Applied));
            }
            _ => {
                return Err(fail(
                    PairingFailure::IdentityMismatch,
                    BootstrapPhase::Applied,
                ));
            }
        }
        enrollment
            .enroll_owner(&server, request, &proposal, &attempt)
            .await?;
        enrollment.connect(&server, &attempt).await?;
        let (prepared, saved) = enrollment
            .completed()
            .map_err(|e| fail(e, BootstrapPhase::Enrolled))?
            .ok_or_else(|| fail(PairingFailure::RecoveryRequired, BootstrapPhase::Enrolled))?;
        self.release(&host)?;
        Ok(ProductionBootstrapCompletion {
            prepared,
            saved,
            outcome: BootstrapOutcome {
                server,
                reused: true,
                phase: BootstrapPhase::Connected,
            },
        })
    }
    /// A separately foreground-authorized Start. Bootstrap and reconnect never invoke it.
    pub async fn start(
        &self,
        facts: BootstrapHostFacts,
        generation: u64,
    ) -> Result<(), BootstrapFailure> {
        let host = self.host(facts.route, generation)?;
        let expected = match facts.registration {
            BootstrapRegistration::Stopped { pin, generation } => {
                spaceterm_host::bootstrap::setup::BootstrapAccountRegistration::Stopped {
                    pin,
                    generation,
                }
            }
            _ => {
                return Err(fail(
                    PairingFailure::ExplicitStartRequired,
                    BootstrapPhase::ExplicitIntent,
                ));
            }
        };
        host.connection.revalidate()?;
        match host
            .connection
            .account(AccountRequest::Start { expected }, None)
            .await?
        {
            AccountReply::Observation(observation)
                if matches!(
                    observation.facts.registration,
                    spaceterm_host::bootstrap::setup::BootstrapAccountRegistration::Running { pin, .. }
                    if matches!(facts.registration, BootstrapRegistration::Stopped { pin: original, .. } if pin == original)
                ) =>
            {
                Ok(())
            }
            AccountReply::Failure(error) => Err(service_failure(error, BootstrapPhase::Applied)),
            _ => Err(fail(
                PairingFailure::RecoveryRequired,
                BootstrapPhase::Applied,
            )),
        }
    }
}

struct ForegroundConnection {
    inner: Arc<dyn BootstrapRemoteConnection>,
    foreground: Mutex<Option<Arc<AtomicBool>>>,
}
impl BootstrapRemoteConnection for ForegroundConnection {
    fn route(&self) -> BootstrapRoute {
        self.inner.route()
    }
    fn destination(&self) -> &str {
        self.inner.destination()
    }
    fn revalidate(&self) -> Result<(), BootstrapFailure> {
        if self
            .foreground
            .lock()
            .map_err(|_| {
                fail(
                    PairingFailure::RecoveryRequired,
                    BootstrapPhase::ExplicitIntent,
                )
            })?
            .as_ref()
            .is_some_and(|valid| !valid.load(Ordering::Acquire))
        {
            return Err(fail(
                PairingFailure::Cancelled,
                BootstrapPhase::ExplicitIntent,
            ));
        }
        self.inner.revalidate()
    }
    fn probe(&self) -> BootstrapProbeFuture<'_, Vec<u8>> {
        Box::pin(async move {
            self.revalidate()
                .map_err(|_| BootstrapProbeFailure::RouteChanged)?;
            let result = self.inner.probe().await;
            self.revalidate()
                .map_err(|_| BootstrapProbeFailure::RouteChanged)?;
            result
        })
    }
    fn account<'a>(
        &'a self,
        request: AccountRequest,
        artifact: Option<&'a VerifiedServerArtifact>,
    ) -> PairingFuture<'a, Result<AccountReply, BootstrapFailure>> {
        Box::pin(async move {
            self.revalidate()?;
            let result = self.inner.account(request, artifact).await;
            self.revalidate()?;
            result
        })
    }
    fn owner_channel(
        &self,
    ) -> PairingFuture<'_, Result<Arc<dyn MessageTransport>, PairingFailure>> {
        Box::pin(async move {
            self.revalidate().map_err(pairing_failure)?;
            let result = self.inner.owner_channel().await;
            self.revalidate().map_err(pairing_failure)?;
            result
        })
    }
    fn prompt(&self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        Box::pin(async move {
            self.revalidate()?;
            let result = self.inner.prompt().await;
            self.revalidate()?;
            result
        })
    }
    fn enable_linger<'a>(
        &'a self,
        invocation: &'a LingerInvocation,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> LingerFuture<'a, ()> {
        Box::pin(async move {
            self.revalidate()
                .map_err(|_| BootstrapLingerFailure::Cancelled)?;
            let result = self.inner.enable_linger(invocation, authentication).await;
            self.revalidate()
                .map_err(|_| BootstrapLingerFailure::Cancelled)?;
            result
        })
    }
}
fn pairing_failure(failure: BootstrapFailure) -> PairingFailure {
    match failure.class {
        BootstrapFailureClass::Pairing(class) => class,
        _ => PairingFailure::RecoveryRequired,
    }
}

#[derive(Clone)]
struct AccountTransport(Arc<dyn BootstrapRemoteConnection>);
impl BootstrapProbeLease for AccountTransport {
    fn revalidate(&self) -> Result<(), BootstrapProbeFailure> {
        self.0
            .revalidate()
            .map_err(|_| BootstrapProbeFailure::RouteChanged)
    }
    fn account_uid(&self) -> u32 {
        self.0.route().account_uid()
    }
}
impl BootstrapProbeTransport for AccountTransport {
    fn run_read_only<'a>(
        &'a self,
        lease: &'a dyn BootstrapProbeLease,
        script: &'static [u8],
        maximum: usize,
    ) -> BootstrapProbeFuture<'a, Vec<u8>> {
        Box::pin(async move {
            lease.revalidate()?;
            if script != read_only_probe_script() || maximum != MAX_PROBE_BYTES {
                return Err(BootstrapProbeFailure::InvalidProbe);
            }
            let bytes = self.0.probe().await?;
            lease.revalidate()?;
            if bytes.len() > maximum {
                return Err(BootstrapProbeFailure::LimitExceeded);
            }
            Ok(bytes)
        })
    }
}
struct Lease(Arc<dyn BootstrapRemoteConnection>);
impl BootstrapSshLease for Lease {
    fn revalidate(&self, route: BootstrapRoute) -> Result<(), BootstrapFailure> {
        if route != self.0.route() {
            return Err(fail(
                PairingFailure::RouteChanged,
                BootstrapPhase::OpenSshAuthenticated,
            ));
        }
        self.0.revalidate()
    }
}
impl BootstrapOpenSsh for AccountTransport {
    fn authenticate<'a>(
        &'a self,
        route: &'a BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<OpenSshAuthenticated, BootstrapFailure>> {
        Box::pin(async move {
            attempt.validate(BootstrapPhase::OpenSshAuthenticated)?;
            OpenSshAuthenticated::retain(*route, Box::new(Lease(self.0.clone())))
        })
    }
}
impl BootstrapLingerTransport for AccountTransport {
    type Exchange = u32;
    fn retain<'a>(
        &'a self,
        lease: &'a dyn BootstrapProbeLease,
        invocation: &'a LingerInvocation,
    ) -> LingerFuture<'a, u32> {
        Box::pin(async move {
            lease
                .revalidate()
                .map_err(|_| BootstrapLingerFailure::RouteChanged)?;
            Ok(invocation.account_uid())
        })
    }
    fn revalidate(&self, uid: &u32) -> Result<(), BootstrapLingerFailure> {
        if *uid != self.0.route().account_uid() {
            return Err(BootstrapLingerFailure::RouteChanged);
        }
        self.0
            .revalidate()
            .map_err(|_| BootstrapLingerFailure::RouteChanged)
    }
    fn invoke<'a>(
        &'a self,
        exchange: &'a mut u32,
        invocation: &'a LingerInvocation,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> LingerFuture<'a, ()> {
        Box::pin(async move {
            BootstrapLingerTransport::revalidate(self, exchange)?;
            self.0.enable_linger(invocation, authentication).await
        })
    }
    fn observe<'a>(&'a self, exchange: &'a u32, uid: u32) -> LingerFuture<'a, BootstrapLinger> {
        Box::pin(async move {
            BootstrapLingerTransport::revalidate(self, exchange)?;
            if *exchange != uid {
                return Err(BootstrapLingerFailure::RouteChanged);
            }
            let bytes = self
                .0
                .probe()
                .await
                .map_err(|_| BootstrapLingerFailure::StateUnavailable)?;
            spaceterm_host::bootstrap::probe::ReadOnlyHostProbe::parse(&bytes, uid)
                .map(|host| host.linger)
                .map_err(|_| BootstrapLingerFailure::StateUnavailable)
        })
    }
}
impl BootstrapAccountTransport for AccountTransport {
    fn route(&self) -> BootstrapRoute {
        self.0.route()
    }
    fn inspect(
        &self,
    ) -> PairingFuture<
        '_,
        Result<spaceterm_host::bootstrap::setup::BootstrapAccountFacts, BootstrapFailure>,
    > {
        Box::pin(async move { observation(self.0.account(AccountRequest::Inspect, None).await?) })
    }
    fn reserve_request(&self) -> PairingFuture<'_, Result<[u8; 16], BootstrapFailure>> {
        Box::pin(async move { reserved(self.0.account(AccountRequest::Reserve, None).await?) })
    }
    fn reserve_owner_request<'a>(
        &'a self,
        proposal: &'a EnrollmentProposal,
    ) -> PairingFuture<'a, Result<[u8; 16], BootstrapFailure>> {
        Box::pin(async move {
            reserved(
                self.0
                    .account(
                        AccountRequest::ReserveOwner {
                            proposal: proposal.clone(),
                        },
                        None,
                    )
                    .await?,
            )
        })
    }
    fn install(
        &self,
        request: [u8; 16],
        artifact: VerifiedServerArtifact,
    ) -> PairingFuture<
        '_,
        Result<spaceterm_host::bootstrap::setup::BootstrapAccountFacts, BootstrapFailure>,
    > {
        Box::pin(async move {
            let transfer_bytes = 32
                + artifact.feed().bytes().len() as u64
                + artifact.feed().signature().len() as u64
                + artifact.bytes().len() as u64;
            let request = native_request(request)?;
            observation(
                self.0
                    .account(
                        AccountRequest::Install {
                            request,
                            artifact: artifact.artifact_sha256(),
                            transfer_bytes,
                        },
                        Some(&artifact),
                    )
                    .await?,
            )
        })
    }
    fn authorize_owner<'a>(
        &'a self,
        request: [u8; 16],
        expected: &'a spaceterm_host::bootstrap::setup::BootstrapAccountRegistration,
        proposal: &'a EnrollmentProposal,
    ) -> PairingFuture<
        'a,
        Result<spaceterm_host::bootstrap::setup::BootstrapAccountFacts, BootstrapFailure>,
    > {
        Box::pin(async move {
            observation(
                self.0
                    .account(
                        AccountRequest::AuthorizeOwner {
                            request: native_request(request)?,
                            expected: expected.clone(),
                            proposal: proposal.clone(),
                        },
                        None,
                    )
                    .await?,
            )
        })
    }
    fn resolve(
        &self,
        request: [u8; 16],
        artifact_identity: [u8; 32],
    ) -> PairingFuture<
        '_,
        Result<spaceterm_host::bootstrap::setup::BootstrapAccountFacts, BootstrapFailure>,
    > {
        Box::pin(async move {
            observation(
                self.0
                    .account(
                        AccountRequest::Resolve {
                            request: native_request(request)?,
                            artifact: artifact_identity,
                        },
                        None,
                    )
                    .await?,
            )
        })
    }
}
fn native_request(bytes: [u8; 16]) -> Result<NativeRequestId, BootstrapFailure> {
    NativeRequestId::new(bytes)
        .map_err(|_| fail(PairingFailure::InvalidRequest, BootstrapPhase::Applied))
}
fn reserved(reply: AccountReply) -> Result<[u8; 16], BootstrapFailure> {
    match reply {
        AccountReply::Reserved(request) => Ok(*request.as_bytes()),
        AccountReply::Failure(error) => Err(service_failure(error, BootstrapPhase::Authorized)),
        _ => Err(fail(
            PairingFailure::InvalidDurableState,
            BootstrapPhase::Authorized,
        )),
    }
}
fn observation(
    reply: AccountReply,
) -> Result<spaceterm_host::bootstrap::setup::BootstrapAccountFacts, BootstrapFailure> {
    match reply {
        AccountReply::Observation(observation) | AccountReply::Owner { observation, .. } => {
            Ok(observation.facts)
        }
        AccountReply::Failure(error) => {
            Err(service_failure(error, BootstrapPhase::ReadOnlyPreflight))
        }
        _ => Err(fail(
            PairingFailure::InvalidDurableState,
            BootstrapPhase::ReadOnlyPreflight,
        )),
    }
}
struct Prompt(Arc<dyn BootstrapRemoteConnection>);
impl ScopedAuthenticationPrompt for Prompt {
    fn revalidate(&self, plan: &BootstrapPlan) -> Result<(), BootstrapFailure> {
        if plan.route() != self.0.route() {
            return Err(fail(
                PairingFailure::RouteChanged,
                BootstrapPhase::Authorized,
            ));
        }
        self.0.revalidate()
    }
    fn respond<'a>(
        &'a self,
        plan: &'a BootstrapPlan,
    ) -> PairingFuture<'a, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        Box::pin(async move {
            self.revalidate(plan)?;
            self.0.prompt().await
        })
    }
}

struct OwnerState {
    connection: Arc<dyn BootstrapRemoteConnection>,
    pairing: Arc<NativePairingRuntime>,
    reference: RevocationRouteReference,
    channel: Mutex<Option<Arc<dyn MessageTransport>>>,
    pin: Mutex<Option<spaceterm_protocol::authentication::ServerPin>>,
    route: Mutex<Option<RouteDescription>>,
}
#[derive(Clone)]
struct OwnerConnector(Arc<OwnerState>);
impl EnrollmentConnector for OwnerConnector {
    fn open<'a>(
        &'a self,
        pin: spaceterm_protocol::authentication::ServerPin,
        reference: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async move {
            if reference != self.0.reference
                || *self
                    .0
                    .pin
                    .lock()
                    .map_err(|_| PairingFailure::RecoveryRequired)?
                    != Some(pin)
            {
                return Err(PairingFailure::IdentityMismatch);
            }
            self.0
                .connection
                .revalidate()
                .map_err(|_| PairingFailure::RouteChanged)?;
            let channel = self
                .0
                .channel
                .lock()
                .map_err(|_| PairingFailure::RecoveryRequired)?
                .clone()
                .ok_or(PairingFailure::Offline)?;
            Ok(Box::new(OwnerTransport {
                channel,
                connection: self.0.connection.clone(),
            }) as Box<dyn PairingTransport>)
        })
    }
}
struct OwnerTransport {
    channel: Arc<dyn MessageTransport>,
    connection: Arc<dyn BootstrapRemoteConnection>,
}
impl PairingTransport for OwnerTransport {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            self.connection.revalidate().map_err(pairing_failure)?;
            self.channel
                .send(bytes)
                .await
                .map_err(|_| PairingFailure::Offline)?;
            self.connection.revalidate().map_err(pairing_failure)
        })
    }
    fn receive(&mut self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, PairingFailure>> {
        Box::pin(async move {
            self.connection.revalidate().map_err(pairing_failure)?;
            let bytes = self
                .channel
                .recv()
                .await
                .map_err(|_| PairingFailure::Offline)?
                .filter(|b| !b.is_empty() && b.len() <= FRAME_LIMIT)
                .map(Zeroizing::new)
                .ok_or(PairingFailure::Offline)?;
            self.connection.revalidate().map_err(pairing_failure)?;
            Ok(bytes)
        })
    }
}
struct OwnerExchange(Arc<OwnerState>);
impl BootstrapOwnerExchange for OwnerExchange {
    fn lookup<'a>(
        &'a self,
        _: &'a BootstrapServer,
        _: [u8; 16],
        _: &'a EnrollmentProposal,
    ) -> PairingFuture<'a, Result<BootstrapOwnerDelivery, PairingFailure>> {
        Box::pin(async { Err(PairingFailure::AuthorizationDenied) })
    }
    fn lookup_verified<'a>(
        &'a self,
        server: &'a BootstrapServer,
        request: [u8; 16],
        proposal: &'a EnrollmentProposal,
        device_proof: [u8; 64],
        authentication_proof: [u8; 64],
    ) -> PairingFuture<'a, Result<BootstrapOwnerDelivery, PairingFailure>> {
        Box::pin(async move {
            self.0
                .connection
                .revalidate()
                .map_err(|_| PairingFailure::RouteChanged)?;
            let reply = self
                .0
                .connection
                .account(AccountRequest::Inspect, None)
                .await
                .map_err(|_| PairingFailure::RecoveryRequired)?;
            let AccountReply::Observation(observation) = reply else {
                return Err(PairingFailure::RecoveryRequired);
            };
            if !matches!(observation.facts.registration, spaceterm_host::bootstrap::setup::BootstrapAccountRegistration::Running { pin, generation, artifact_identity, .. }
                if pin == server.pin && generation == server.registration_generation && artifact_identity == server.approved_artifact_identity)
            {
                return Err(PairingFailure::IdentityMismatch);
            }
            let remote_socket = observation
                .remote_socket
                .ok_or(PairingFailure::RouteChanged)?;
            let route = RouteDescription::new(RouteParts::Ssh {
                destination: self.0.connection.destination().to_owned(),
                remote_socket,
            })
            .map_err(|_| PairingFailure::RouteChanged)?;
            let channel = self.0.connection.owner_channel().await?;
            let request = native_request(request).map_err(|_| PairingFailure::InvalidRequest)?;
            let initial = serde_json::to_vec(&serde_json::json!({"request":request, "proposal":proposal, "device_proof":device_proof.to_vec(), "authentication_proof":authentication_proof.to_vec()})).map_err(|_| PairingFailure::InvalidRequest)?;
            self.0.connection.revalidate().map_err(pairing_failure)?;
            channel
                .send(&initial)
                .await
                .map_err(|_| PairingFailure::RecoveryRequired)?;
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Reply {
                schema: u16,
                disposition: String,
                outcome: spaceterm_core::pairing::PairingOutcome,
            }
            let reply: Reply = serde_json::from_slice(
                &channel
                    .recv()
                    .await
                    .map_err(|_| PairingFailure::RecoveryRequired)?
                    .ok_or(PairingFailure::Offline)?,
            )
            .map_err(|_| PairingFailure::InvalidDurableState)?;
            if reply.schema != 1
                || !matches!(
                    reply.outcome,
                    spaceterm_core::pairing::PairingOutcome::AuthorizationReserved { .. }
                )
            {
                return Err(PairingFailure::AuthorizationDenied);
            }
            self.0.connection.revalidate().map_err(pairing_failure)?;
            let delivery = Zeroizing::new(
                channel
                    .recv()
                    .await
                    .map_err(|_| PairingFailure::RecoveryRequired)?
                    .ok_or(PairingFailure::Offline)?,
            );
            self.0.connection.revalidate().map_err(pairing_failure)?;
            let (reservation, authorization) = match reply.disposition.as_str() {
                "authorization_delivered" => {
                    let (reservation, authorization) =
                        EnrollmentAuthorization::from_delivery(delivery)
                            .map_err(|_| PairingFailure::IdentityMismatch)?;
                    (reservation, Some(authorization))
                }
                "receipt_available" => (
                    PublicEnrollmentReservation::decode(&delivery)
                        .map_err(|_| PairingFailure::InvalidDurableState)?,
                    None,
                ),
                _ => return Err(PairingFailure::RecoveryRequired),
            };
            *self
                .0
                .pin
                .lock()
                .map_err(|_| PairingFailure::RecoveryRequired)? = Some(server.pin);
            *self
                .0
                .channel
                .lock()
                .map_err(|_| PairingFailure::RecoveryRequired)? = Some(channel);
            *self
                .0
                .route
                .lock()
                .map_err(|_| PairingFailure::RecoveryRequired)? = Some(route);
            // The native digest binds only public request/claim material.
            let digest = match self
                .0
                .connection
                .account(
                    AccountRequest::OwnerBinding {
                        request,
                        proposal: proposal.clone(),
                    },
                    None,
                )
                .await
                .map_err(|_| PairingFailure::RecoveryRequired)?
            {
                AccountReply::Owner { digest, .. } => digest,
                _ => return Err(PairingFailure::IdentityMismatch),
            };
            Ok(BootstrapOwnerDelivery {
                native_request: *request.as_bytes(),
                native_request_digest: digest,
                reservation,
                authorization,
            })
        })
    }
    fn prepare_binding<'a>(
        &'a self,
        prepared: &'a PreparedEnrollment,
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            self.0.connection.revalidate().map_err(pairing_failure)?;
            let route = self
                .0
                .route
                .lock()
                .map_err(|_| PairingFailure::RecoveryRequired)?
                .clone()
                .ok_or(PairingFailure::RouteChanged)?;
            self.0.pairing.prepare_route(prepared, route).await?;
            self.0.connection.revalidate().map_err(pairing_failure)
        })
    }
    fn connect<'a>(
        &'a self,
        server: &'a BootstrapServer,
        saved: &'a SavedPairing,
    ) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            if saved.pin() != server.pin {
                return Err(PairingFailure::IdentityMismatch);
            }
            self.0
                .connection
                .revalidate()
                .map_err(|_| PairingFailure::RouteChanged)?;
            // Final ordinary admission belongs to Settings publication, after its attempt fence.
            // Here the authenticated enrollment channel is closed only after its durable ACK.
            let channel = self
                .0
                .channel
                .lock()
                .map_err(|_| PairingFailure::RecoveryRequired)?
                .take()
                .ok_or(PairingFailure::Offline)?;
            channel
                .close()
                .await
                .map_err(|_| PairingFailure::RecoveryRequired)
        })
    }
}
type Enrollment =
    NativeBootstrapEnrollment<OwnerConnector, OwnerExchange, JournalController, SharedCredentials>;
struct SharedEnrollment(Arc<Enrollment>);
impl BootstrapEnrollment for SharedEnrollment {
    fn preflight<'a>(
        &'a self,
        route: BootstrapRoute,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        self.0.preflight(route, attempt)
    }
    fn prepare_owner<'a>(
        &'a self,
        server: &'a BootstrapServer,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<EnrollmentProposal, BootstrapFailure>> {
        self.0.prepare_owner(server, attempt)
    }
    fn enroll_owner<'a>(
        &'a self,
        server: &'a BootstrapServer,
        request: [u8; 16],
        proposal: &'a EnrollmentProposal,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        self.0.enroll_owner(server, request, proposal, attempt)
    }
    fn connect<'a>(
        &'a self,
        server: &'a BootstrapServer,
        attempt: &'a BootstrapAttempt,
    ) -> PairingFuture<'a, Result<(), BootstrapFailure>> {
        self.0.connect(server, attempt)
    }
}
fn fail(class: PairingFailure, phase: BootstrapPhase) -> BootstrapFailure {
    BootstrapFailure::new(class, phase)
}
