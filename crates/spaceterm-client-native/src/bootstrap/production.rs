//! Foreground production Bootstrap composition. Ordinary routes do not own this provider.

use super::{
    BootstrapAccountTransport, BootstrapNativePreflight, BootstrapOpenSsh, BootstrapOwnerDelivery,
    BootstrapOwnerExchange, NativeBootstrapAccount, NativeBootstrapEnrollment,
    SshNativeServerSetup, service_failure,
};
use crate::pairing_runtime::{NativePairingRuntime, SharedCredentials};
use spaceterm_client::{
    api::{RouteDescription, RouteParts},
    journal::{JournalController, JournalSshCapture},
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
            BootstrapLingerFailure, BootstrapLingerTransport, LingerAuthenticationPrompt,
            LingerFuture, LingerInvocation, ScopedLingerAuthentication, enable_linger_if_needed,
        },
        process::{
            BootstrapProbeFailure, BootstrapProbeFuture, BootstrapProbeLease,
            BootstrapProbeTransport,
        },
        wire::*,
    },
    native_deployment::contracts::NativeRequestId,
    transport::{MessageTransport, TransportError},
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
    fn bind_foreground_fence(&self, _valid: Arc<AtomicBool>) -> Result<(), BootstrapFailure> {
        Ok(())
    }
    fn captured_ssh(&self) -> Option<JournalSshCapture> {
        None
    }
    fn revalidate_capture(&self) -> PairingFuture<'_, Result<(), BootstrapFailure>> {
        Box::pin(async move { self.revalidate() })
    }
    fn probe(&self) -> BootstrapProbeFuture<'_, Vec<u8>>;
    fn account<'a>(
        &'a self,
        request: AccountRequest,
        artifact: Option<&'a VerifiedServerArtifact>,
    ) -> PairingFuture<'a, Result<AccountReply, BootstrapFailure>>;
    /// Returns a fresh restricted session. Its final Drop retires and reaps its
    /// exact native process even when a failed frame has already closed it.
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
struct HostOperation {
    hosts: Arc<Mutex<Vec<Arc<RetainedHost>>>>,
    host: Arc<RetainedHost>,
    release_on_drop: bool,
}
impl Drop for HostOperation {
    fn drop(&mut self) {
        if self.release_on_drop
            && let Ok(mut hosts) = self.hosts.lock()
        {
            hosts.retain(|old| !Arc::ptr_eq(old, &self.host));
        }
    }
}
/// Public production provider used by Settings and production-composition tests.
pub struct ProductionSshBootstrap {
    pairing: Arc<NativePairingRuntime>,
    credentials: SharedCredentials,
    hosts: Arc<Mutex<Vec<Arc<RetainedHost>>>>,
    factory: Arc<dyn BootstrapHostFactory>,
}
impl ProductionSshBootstrap {
    pub fn new(pairing: Arc<NativePairingRuntime>, factory: Arc<dyn BootstrapHostFactory>) -> Self {
        let credentials = pairing.shared_credentials();
        Self {
            pairing,
            credentials,
            hosts: Arc::new(Mutex::new(Vec::new())),
            factory,
        }
    }
    pub async fn inspect(
        &self,
        destination: String,
        generation: u64,
    ) -> Result<BootstrapHostFacts, BootstrapFailure> {
        self.inspect_foreground(destination, generation, None).await
    }
    async fn inspect_foreground(
        &self,
        destination: String,
        generation: u64,
        valid: Option<Arc<AtomicBool>>,
    ) -> Result<BootstrapHostFacts, BootstrapFailure> {
        if generation == 0 {
            return Err(fail(
                PairingFailure::InvalidRequest,
                BootstrapPhase::ExplicitIntent,
            ));
        }
        check_foreground(valid.as_ref())?;
        let host = self.factory.open(destination, generation).await?;
        check_foreground(valid.as_ref())?;
        let connection = Arc::new(ForegroundConnection {
            inner: host.connection,
            foreground: Mutex::new(None),
        });
        if let Some(valid) = &valid {
            connection.bind_foreground_fence(valid.clone())?;
        }
        connection.revalidate()?;
        let transport = AccountTransport(connection.clone());
        let intent = ForegroundBootstrap::confirm(connection.route(), generation)?;
        let attempt = intent.cancellation();
        let setup = SshNativeServerSetup::new(
            transport.clone(),
            NativeBootstrapAccount::new(Arc::new(transport))?,
        );
        let ssh = setup.authenticate(&connection.route(), &attempt).await?;
        let facts = setup.discover(&ssh).await?;
        facts.supported_platform()?;
        facts.prerequisites(matches!(facts.registration, BootstrapRegistration::Missing))?;
        connection.revalidate()?;
        let retained = Arc::new(RetainedHost {
            generation,
            connection,
            artifact: Mutex::new(host.artifact),
        });
        {
            let mut hosts = self.hosts.lock().map_err(|_| {
                fail(
                    PairingFailure::RecoveryRequired,
                    BootstrapPhase::ReadOnlyPreflight,
                )
            })?;
            // A sheet's counter is not a process-wide session identity. A canceled
            // completion must not remove another sheet before the adapter checks it.
            check_foreground(valid.as_ref())?;
            hosts.retain(|old| old.connection.live());
            if hosts.iter().any(|old| {
                old.generation == generation
                    && old.connection.route() == retained.connection.route()
                    && old.connection.same_scope(valid.as_ref())
            }) {
                return Err(fail(
                    PairingFailure::RouteChanged,
                    BootstrapPhase::ReadOnlyPreflight,
                ));
            }
            if hosts.len() >= 4 {
                return Err(fail(
                    PairingFailure::LimitExceeded,
                    BootstrapPhase::ReadOnlyPreflight,
                ));
            }
            hosts.push(retained.clone());
        }
        if let Some(valid) = valid
            && let Err(error) = self.watch_foreground(&retained, &valid)
        {
            self.release(&retained)?;
            return Err(error);
        }
        Ok(facts)
    }
    fn host(
        &self,
        route: BootstrapRoute,
        generation: u64,
    ) -> Result<Arc<RetainedHost>, BootstrapFailure> {
        let hosts = self.hosts.lock().map_err(|_| {
            fail(
                PairingFailure::RecoveryRequired,
                BootstrapPhase::ExplicitIntent,
            )
        })?;
        let mut matches = hosts.iter().filter(|host| {
            host.generation == generation
                && host.connection.route() == route
                && host.connection.live()
        });
        let host = matches
            .next()
            .cloned()
            .ok_or_else(|| fail(PairingFailure::RouteChanged, BootstrapPhase::ExplicitIntent))?;
        if matches.next().is_some() {
            return Err(fail(
                PairingFailure::RouteChanged,
                BootstrapPhase::ExplicitIntent,
            ));
        }
        Ok(host)
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
        let already_bound = host.connection.same_scope(Some(&valid));
        if let Err(error) = host.connection.bind_foreground_fence(valid.clone()) {
            if !host.connection.live() {
                self.release(&host)?;
            }
            return Err(error);
        }
        if !already_bound && let Err(error) = self.watch_foreground(&host, &valid) {
            self.release(&host)?;
            return Err(error);
        }
        Ok(())
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
            .retain(|old| !std::ptr::eq(old.as_ref(), host));
        Ok(())
    }
    fn watch_foreground(
        &self,
        host: &Arc<RetainedHost>,
        valid: &Arc<AtomicBool>,
    ) -> Result<(), BootstrapFailure> {
        let hosts = Arc::downgrade(&self.hosts);
        let host = Arc::downgrade(host);
        let valid = Arc::downgrade(valid);
        std::thread::Builder::new()
            .name("bootstrap-retirement".into())
            .spawn(move || {
                loop {
                    // No task owns the session while waiting for cancellation. Workers
                    // keep their own Arc until their accepted operation has cleaned up.
                    let Some(hosts) = hosts.upgrade() else {
                        return;
                    };
                    let Some(host) = host.upgrade() else {
                        return;
                    };
                    let live = valid
                        .upgrade()
                        .is_some_and(|valid| valid.load(Ordering::Acquire));
                    let Ok(mut retained) = hosts.lock() else {
                        return;
                    };
                    if !live {
                        retained.retain(|old| !Arc::ptr_eq(old, &host));
                        return;
                    }
                    let present = retained.iter().any(|old| Arc::ptr_eq(old, &host));
                    drop(retained);
                    drop(host);
                    drop(hosts);
                    if !present {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            })
            .map_err(|_| {
                fail(
                    PairingFailure::RecoveryRequired,
                    BootstrapPhase::ReadOnlyPreflight,
                )
            })?;
        Ok(())
    }
    pub async fn bootstrap(
        &self,
        intent: DeliberateBootstrap,
        facts: BootstrapHostFacts,
    ) -> Result<ProductionBootstrapCompletion, BootstrapFailure> {
        let attempt = intent.cancellation();
        let host = self.host(facts.route, attempt.generation())?;
        let _operation = HostOperation {
            hosts: self.hosts.clone(),
            host: host.clone(),
            release_on_drop: true,
        };
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
                    .chain(
                        inventory
                            .enrollment_acknowledgements
                            .iter()
                            .map(|work| &work.prepared),
                    )
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
            session: Mutex::new(None),
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
        self.pending_for(facts, generation).await
    }
    /// The sheet's cancellation scope is bound before discovery and retained
    /// only while it remains live. Generation counters are local to each sheet.
    pub async fn inspect_with_pending_foreground(
        &self,
        destination: String,
        generation: u64,
        valid: Arc<AtomicBool>,
    ) -> Result<(BootstrapHostFacts, Option<([u8; 16], [u8; 32])>), BootstrapFailure> {
        let facts = self
            .inspect_foreground(destination, generation, Some(valid.clone()))
            .await?;
        let result = self.pending_for(facts, generation).await;
        check_foreground(Some(&valid))?;
        result
    }
    async fn pending_for(
        &self,
        facts: BootstrapHostFacts,
        generation: u64,
    ) -> Result<(BootstrapHostFacts, Option<([u8; 16], [u8; 32])>), BootstrapFailure> {
        let host = self.host(facts.route, generation)?;
        let mut operation = HostOperation {
            hosts: self.hosts.clone(),
            host: host.clone(),
            release_on_drop: true,
        };
        let result = self.pending_for_host(&host, facts).await;
        if result.is_ok() {
            operation.release_on_drop = false;
        }
        result
    }
    async fn pending_for_host(
        &self,
        host: &Arc<RetainedHost>,
        facts: BootstrapHostFacts,
    ) -> Result<(BootstrapHostFacts, Option<([u8; 16], [u8; 32])>), BootstrapFailure> {
        host.connection.revalidate()?;
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
            .chain(
                inventory
                    .enrollment_acknowledgements
                    .iter()
                    .map(|work| &work.prepared),
            )
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
        let _operation = HostOperation {
            hosts: self.hosts.clone(),
            host: host.clone(),
            release_on_drop: true,
        };
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
        let _operation = HostOperation {
            hosts: self.hosts.clone(),
            host: host.clone(),
            release_on_drop: true,
        };
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
        ensure_stopped(host.connection.as_ref(), &expected).await?;
        let transport = AccountTransport(host.connection.clone());
        let probe = spaceterm_host::bootstrap::process::read_only_preflight(&transport, &transport)
            .await
            .map_err(super::probe_failure)?;
        ensure_stopped(host.connection.as_ref(), &expected).await?;
        let linger = StartLingerTransport {
            transport: transport.clone(),
            expected: expected.clone(),
        };
        let prompt = StartLingerPrompt {
            connection: host.connection.clone(),
            expected: expected.clone(),
        };
        enable_linger_if_needed(&linger, &transport, &probe, &prompt)
            .await
            .map_err(super::linger_failure)?;
        // A prompt may wait while another client changes this registration.
        // Do not publish Start against a newer or no-longer-Stopped generation.
        ensure_stopped(host.connection.as_ref(), &expected).await?;
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

fn check_foreground(valid: Option<&Arc<AtomicBool>>) -> Result<(), BootstrapFailure> {
    if valid.is_some_and(|valid| !valid.load(Ordering::Acquire)) {
        return Err(fail(
            PairingFailure::Cancelled,
            BootstrapPhase::ExplicitIntent,
        ));
    }
    Ok(())
}
struct ForegroundConnection {
    inner: Arc<dyn BootstrapRemoteConnection>,
    foreground: Mutex<Option<Arc<AtomicBool>>>,
}
impl ForegroundConnection {
    fn live(&self) -> bool {
        self.foreground
            .lock()
            .is_ok_and(|scope| check_foreground(scope.as_ref()).is_ok())
    }
    fn same_scope(&self, valid: Option<&Arc<AtomicBool>>) -> bool {
        self.foreground
            .lock()
            .is_ok_and(|scope| match (scope.as_ref(), valid) {
                (Some(original), Some(valid)) => Arc::ptr_eq(original, valid),
                (None, None) => true,
                _ => false,
            })
    }
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
    fn bind_foreground_fence(&self, valid: Arc<AtomicBool>) -> Result<(), BootstrapFailure> {
        check_foreground(Some(&valid))?;
        let mut foreground = self.foreground.lock().map_err(|_| {
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
        self.inner.bind_foreground_fence(valid.clone())?;
        *foreground = Some(valid);
        drop(foreground);
        self.revalidate()
    }
    fn captured_ssh(&self) -> Option<JournalSshCapture> {
        self.inner.captured_ssh()
    }
    fn revalidate_capture(&self) -> PairingFuture<'_, Result<(), BootstrapFailure>> {
        Box::pin(async move {
            self.revalidate()?;
            self.inner.revalidate_capture().await?;
            self.revalidate()
        })
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

async fn ensure_stopped(
    connection: &dyn BootstrapRemoteConnection,
    expected: &spaceterm_host::bootstrap::setup::BootstrapAccountRegistration,
) -> Result<(), BootstrapFailure> {
    connection.revalidate()?;
    match connection.account(AccountRequest::Inspect, None).await? {
        AccountReply::Observation(observation) if observation.facts.registration == *expected => {
            Ok(())
        }
        AccountReply::Observation(_) => Err(fail(
            PairingFailure::RouteChanged,
            BootstrapPhase::Authorized,
        )),
        AccountReply::Failure(error) => Err(service_failure(error, BootstrapPhase::Authorized)),
        _ => Err(fail(
            PairingFailure::InvalidDurableState,
            BootstrapPhase::Authorized,
        )),
    }
}
struct StartLingerPrompt {
    connection: Arc<dyn BootstrapRemoteConnection>,
    expected: spaceterm_host::bootstrap::setup::BootstrapAccountRegistration,
}
impl LingerAuthenticationPrompt for StartLingerPrompt {
    fn revalidate(&self, uid: u32) -> Result<(), BootstrapLingerFailure> {
        if uid != self.connection.route().account_uid() {
            return Err(BootstrapLingerFailure::RouteChanged);
        }
        self.connection.revalidate().map_err(super::prompt_failure)
    }
    fn respond(&self, uid: u32) -> LingerFuture<'_, Zeroizing<Vec<u8>>> {
        Box::pin(async move {
            self.revalidate(uid)?;
            ensure_stopped(self.connection.as_ref(), &self.expected)
                .await
                .map_err(super::prompt_failure)?;
            let response = self
                .connection
                .prompt()
                .await
                .map_err(super::prompt_failure)?;
            self.revalidate(uid)?;
            ensure_stopped(self.connection.as_ref(), &self.expected)
                .await
                .map_err(super::prompt_failure)?;
            Ok(response)
        })
    }
}
struct StartLingerTransport {
    transport: AccountTransport,
    expected: spaceterm_host::bootstrap::setup::BootstrapAccountRegistration,
}
impl BootstrapLingerTransport for StartLingerTransport {
    type Exchange = u32;
    fn retain<'a>(
        &'a self,
        lease: &'a dyn BootstrapProbeLease,
        invocation: &'a LingerInvocation,
    ) -> LingerFuture<'a, u32> {
        Box::pin(async move {
            ensure_stopped(self.transport.0.as_ref(), &self.expected)
                .await
                .map_err(super::prompt_failure)?;
            self.transport.retain(lease, invocation).await
        })
    }
    fn revalidate(&self, exchange: &u32) -> Result<(), BootstrapLingerFailure> {
        BootstrapLingerTransport::revalidate(&self.transport, exchange)
    }
    fn invoke<'a>(
        &'a self,
        exchange: &'a mut u32,
        invocation: &'a LingerInvocation,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> LingerFuture<'a, ()> {
        Box::pin(async move {
            ensure_stopped(self.transport.0.as_ref(), &self.expected)
                .await
                .map_err(super::prompt_failure)?;
            self.transport
                .invoke(exchange, invocation, authentication)
                .await
        })
    }
    fn observe<'a>(&'a self, exchange: &'a u32, uid: u32) -> LingerFuture<'a, BootstrapLinger> {
        self.transport.observe(exchange, uid)
    }
}

struct OwnerState {
    connection: Arc<dyn BootstrapRemoteConnection>,
    pairing: Arc<NativePairingRuntime>,
    reference: RevocationRouteReference,
    channel: Mutex<Option<(Arc<dyn MessageTransport>, bool)>>,
    session: Mutex<Option<OwnerSession>>,
    pin: Mutex<Option<spaceterm_protocol::authentication::ServerPin>>,
    route: Mutex<Option<RouteDescription>>,
}
#[derive(Clone)]
struct OwnerSession {
    server: BootstrapServer,
    request: [u8; 16],
    proposal: EnrollmentProposal,
    device_proof: [u8; 64],
    authentication_proof: [u8; 64],
    reservation: PublicEnrollmentReservation,
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
            let available = {
                let mut channel = self
                    .0
                    .channel
                    .lock()
                    .map_err(|_| PairingFailure::RecoveryRequired)?;
                channel.as_mut().and_then(|(channel, used)| {
                    if *used {
                        return None;
                    }
                    *used = true;
                    Some(channel.clone())
                })
            };
            let channel = if let Some(channel) = available {
                channel
            } else {
                // acknowledge_once begins a new hello. The old channel is already
                // encrypted and cannot interpret it as another owner handshake.
                let previous = self
                    .0
                    .channel
                    .lock()
                    .map_err(|_| PairingFailure::RecoveryRequired)?
                    .take();
                if let Some((previous, _)) = previous {
                    let closed = previous.close().await;
                    let final_owner = Arc::strong_count(&previous) == 1;
                    // A failed native frame already retired its child. Repeated
                    // close may report unavailable, but releasing our final owner
                    // still completes drop-owned cleanup before the fresh session.
                    drop(previous);
                    match closed {
                        Ok(()) => (),
                        Err(TransportError::Closed | TransportError::Unavailable)
                            if final_owner => {}
                        Err(_) => return Err(PairingFailure::RecoveryRequired),
                    }
                }
                let session = self
                    .0
                    .session
                    .lock()
                    .map_err(|_| PairingFailure::RecoveryRequired)?
                    .clone()
                    .ok_or(PairingFailure::RecoveryRequired)?;
                let delivery = OwnerExchange(self.0.clone())
                    .lookup_verified(
                        &session.server,
                        session.request,
                        &session.proposal,
                        session.device_proof,
                        session.authentication_proof,
                    )
                    .await?;
                if delivery.authorization.is_some() || delivery.reservation != session.reservation {
                    return Err(PairingFailure::IdentityMismatch);
                }
                let mut channel = self
                    .0
                    .channel
                    .lock()
                    .map_err(|_| PairingFailure::RecoveryRequired)?;
                let (channel, used) = channel.as_mut().ok_or(PairingFailure::Offline)?;
                *used = true;
                channel.clone()
            };
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
            // Includes enrollment submission. Mutable SSH configuration never
            // replaces the capture of the authenticated original session.
            self.connection
                .revalidate_capture()
                .await
                .map_err(pairing_failure)?;
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
                .clone()
                .ok_or(PairingFailure::RouteChanged)?;
            let route = RouteDescription::new(RouteParts::Ssh {
                destination: self.0.connection.destination().to_owned(),
                remote_socket,
            })
            .map_err(|_| PairingFailure::RouteChanged)?;
            let request = native_request(request).map_err(|_| PairingFailure::InvalidRequest)?;
            // Query every fallible native binding before receiving a single-use
            // bearer. Failure here leaves the original owner authorization unissued.
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
                .map_err(pairing_failure)?
            {
                AccountReply::Owner {
                    observation: binding,
                    digest,
                } if digest.bytes() != [0; 32]
                    && binding.facts.registration == observation.facts.registration
                    && binding.remote_socket == observation.remote_socket =>
                {
                    digest
                }
                AccountReply::Failure(error) => {
                    return Err(pairing_failure(service_failure(
                        error,
                        BootstrapPhase::Enrolled,
                    )));
                }
                _ => return Err(PairingFailure::IdentityMismatch),
            };
            self.0
                .connection
                .revalidate_capture()
                .await
                .map_err(pairing_failure)?;
            let channel = self.0.connection.owner_channel().await?;
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
                .map_err(|_| PairingFailure::RecoveryRequired)? = Some((channel, false));
            *self
                .0
                .route
                .lock()
                .map_err(|_| PairingFailure::RecoveryRequired)? = Some(route);
            *self
                .0
                .session
                .lock()
                .map_err(|_| PairingFailure::RecoveryRequired)? = Some(OwnerSession {
                server: *server,
                request: *request.as_bytes(),
                proposal: proposal.clone(),
                device_proof,
                authentication_proof,
                reservation: reservation.clone(),
            });
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
            self.0
                .connection
                .revalidate_capture()
                .await
                .map_err(pairing_failure)?;
            let route = self
                .0
                .route
                .lock()
                .map_err(|_| PairingFailure::RecoveryRequired)?
                .clone()
                .ok_or(PairingFailure::RouteChanged)?;
            if let Some(capture) = self.0.connection.captured_ssh() {
                self.0
                    .pairing
                    .prepare_route_with_capture(prepared, route, capture)
                    .await?;
            } else {
                // Injected nonnative boundaries may have no OpenSSH capture.
                self.0.pairing.prepare_route(prepared, route).await?;
            }
            self.0
                .connection
                .revalidate_capture()
                .await
                .map_err(pairing_failure)
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
            let (channel, _) = self
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
