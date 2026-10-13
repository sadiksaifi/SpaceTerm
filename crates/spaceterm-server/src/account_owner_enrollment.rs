//! Deliberate owner requests use the running actor or an exclusive stopped actor.
//! Neither path grants ordinary access from account IPC reachability.

use std::sync::Arc;

use crate::{
    account_startup::AccountOwnerEnrollment,
    adapters::{core::CorePorts, store::production_services},
    api::{ServerConfig, ServerHandle, ServicePhase, StartupAuthorityPort},
    contracts::*,
    failure::failure,
    pairing::{
        management::AccountOwnerEnrollmentPort,
        service::{PairingExecution, PairingService},
    },
    service::RecoveredServer,
    shutdown::ShutdownReport,
};
use spaceterm_core::{
    identity::ServerId, operation_ledger::canonical::CanonicalCodec, organization::ServerDirectory,
    pairing::PairingFailure,
};
use spaceterm_host::{
    account_service::{ServiceSupervisor, StoppedOwnerAdmission},
    native_deployment::contracts::NativeRequestId,
    organization_store::{SqliteOrganizationStore, StoreIdentity, StoreLimits, migrations, schema},
    server_keys::ServerKeyStore,
    startup::DesiredServerState,
    storage::Database,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{PairingLookup, PairingRecord},
    pairing_adapter::{
        EnrollmentConnection, EnrollmentServer, PublicEnrollmentReservation, VerifiedDeviceClaim,
        enrollment_proposal_digest, reservation_to_wire,
    },
};

/// Native cleanup and external-work providers are reused for real recovery.
/// Runtime and ordinary authentication are intentionally unavailable in this path.
pub struct StoppedOwnerProviders {
    pub config: ServerConfig,
    pub host_management: Arc<dyn HostManagementPort>,
    pub external_work: Arc<dyn ExternalWorkPort>,
    pub clock: Arc<dyn ServerClock>,
}

/// Composition installs this constructor only at its deliberate native boundary.
/// A running executor owns no database, listener, runtime or service lifetime.
/// A stopped executor retains its instance lease through the actor's joined close.
pub struct AccountOwnerRequestExecutor {
    owner: Arc<AccountOwnerEnrollment>,
    service: PairingService,
    handle: ServerHandle,
    credentials: Arc<spaceterm_host::server_keys::ServerIdentity>,
    stopped: Option<RecoveredServer>,
    admission: Option<Arc<StoppedOwnerAdmission>>,
}
impl AccountOwnerRequestExecutor {
    pub fn running(
        supervisor: Arc<ServiceSupervisor>,
        handle: ServerHandle,
    ) -> Result<Self, PairingFailure> {
        let registration = supervisor
            .discover()
            .map_err(crate::account_startup::owner_failure)?
            .ok_or(PairingFailure::AuthorizationRequired)?;
        let pin = registration.server_pin();
        if registration.desired_state() != DesiredServerState::Running
            || handle.snapshot().state().server.bytes() != pin.identity
            || !matches!(handle.phase(), ServicePhase::Ready | ServicePhase::Serving)
        {
            return Err(PairingFailure::IdentityMismatch);
        }
        let identity = ServerKeyStore::open(&registration)
            .and_then(|keys| keys.load_identity(pin))
            .map_err(|error| crate::account_startup::owner_failure(error.into()))?;
        let owner = Arc::new(AccountOwnerEnrollment::new(supervisor));
        let service = PairingService::new(
            Arc::new(handle.clone()),
            identity.clone(),
            Arc::new(CanonicalCodec),
            owner.clone(),
            pin,
        )?;
        Ok(Self {
            owner,
            service,
            handle,
            credentials: identity,
            stopped: None,
            admission: None,
        })
    }

    /// Opens only established storage after retained owner authority and exclusive
    /// stopped admission. No service manager Start or transport bind is performed.
    pub async fn stopped(
        supervisor: Arc<ServiceSupervisor>,
        home: ServerDirectory,
        request: NativeRequestId,
        claim: &VerifiedDeviceClaim,
        mut providers: StoppedOwnerProviders,
    ) -> Result<Self, PairingFailure> {
        let owner = Arc::new(AccountOwnerEnrollment::new(supervisor.clone()));
        let digest = owner.record_authorization(request, claim.proposal())?;
        let server = claim.proposal().parts().server;
        let claim_digest = enrollment_proposal_digest(claim.proposal())
            .map_err(|_| PairingFailure::InvalidRequest)?;
        let admission = Arc::new(
            supervisor
                .admit_stopped_owner(request, digest, server, claim_digest)
                .map_err(crate::account_startup::owner_failure)?,
        );
        let registration = admission.registration();
        let pin = registration.server_pin();
        let credentials = ServerKeyStore::open(registration)
            .and_then(|keys| keys.load_identity(pin))
            .map_err(|error| crate::account_startup::owner_failure(error.into()))?;
        let identity = StoreIdentity {
            server,
            account: registration.key(),
            server_public_key: pin.public_key,
        };
        let services =
            production_services(identity, home.clone()).map_err(pairing_store_failure)?;
        let database = Database::open_existing_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .map_err(pairing_storage_failure)?;
        let store = SqliteOrganizationStore::open_existing(
            database,
            identity,
            StoreLimits::production(),
            services,
        )
        .map_err(pairing_store_failure)?;
        providers.config.host.authority = Arc::new(StoppedAuthority(admission.clone()));
        providers.config.core = CorePorts::production(identity, home, providers.clock.clone())
            .map_err(crate::service::pairing_actor::pairing_failure)?;
        let stopped = RecoveredServer::recover(
            providers.config,
            ServerDependencies {
                store: Box::new(StoppedStore {
                    store,
                    _admission: admission.clone(),
                }),
                credentials: credentials.clone(),
                pairing: Arc::new(NoOrdinaryAccess),
                runtime: Arc::new(NoRuntime),
                host_management: providers.host_management,
                external_work: providers.external_work,
                clock: providers.clock,
            },
        )
        .await
        .map_err(crate::service::pairing_actor::pairing_failure)?;
        let handle = stopped.handle();
        let stopped_owner = Arc::new(StoppedOwnerPort {
            owner: owner.clone(),
            admission: admission.clone(),
        });
        let service = PairingService::new(
            Arc::new(handle.clone()),
            credentials.clone(),
            Arc::new(CanonicalCodec),
            stopped_owner,
            pin,
        )?;
        Ok(Self {
            owner,
            service,
            handle,
            credentials,
            stopped: Some(stopped),
            admission: Some(admission),
        })
    }

    /// Preserves the original host ID and exact verified claim. Validation runs
    /// again inside PairingService's serialized actor preparation and consumption.
    pub async fn issue(
        &self,
        request: NativeRequestId,
        claim: VerifiedDeviceClaim,
    ) -> Result<PairingExecution, PairingFailure> {
        if let Some(admission) = &self.admission {
            if request != admission.request() {
                return Err(PairingFailure::RequestIdentityReused);
            }
            admission
                .revalidate()
                .map_err(crate::account_startup::owner_failure)?;
        }
        let digest = self.owner.record_authorization(request, claim.proposal())?;
        self.service
            .issue_owner_authorization(request, digest, claim)
            .await
    }

    pub(crate) fn handle(&self) -> ServerHandle {
        self.handle.clone()
    }

    pub(crate) fn retained_reservation(
        &self,
        ticket: &spaceterm_core::pairing::EnrollmentTicketId,
    ) -> Result<spaceterm_core::pairing::EnrollmentReservation, PairingFailure> {
        let snapshot = self
            .handle
            .pairing_snapshot()
            .map_err(|_| PairingFailure::RecoveryRequired)?;
        snapshot.reservation(ticket).cloned()
    }

    /// Public comparison data for the protocol's proof-of-possession handshake.
    pub fn enrollment_reservation(
        &self,
        ticket: &spaceterm_core::pairing::EnrollmentTicketId,
    ) -> Result<PublicEnrollmentReservation, ProtocolError> {
        let state = self
            .handle
            .pairing_snapshot()
            .map_err(|_| ProtocolError::PairingDenied)?;
        let reservation = state
            .reservation(ticket)
            .map_err(|_| ProtocolError::PairingDenied)?;
        PublicEnrollmentReservation::from_wire(&reservation_to_wire(reservation))
    }

    /// Negotiates only the enrollment purpose, with the established server key.
    pub fn accept_enrollment(
        &self,
        hello: &[u8],
    ) -> Result<(OwnerEnrollmentHandshake, Vec<u8>), ProtocolError> {
        let lookup =
            crate::pairing::protocol_lookup::PairingProtocolLookup::new(self.handle.clone());
        let (server, response) = EnrollmentServer::accept(
            self.credentials.pin().identity,
            spaceterm_protocol::negotiation::SUPPORTED_MAJORS.to_vec(),
            self.credentials.clone(),
            &lookup,
            hello,
        )?;
        Ok((OwnerEnrollmentHandshake { server, lookup }, response))
    }

    /// Only a verified enrollment-purpose channel can consume the reservation.
    pub async fn enrollment_frame(
        &self,
        connection: &mut EnrollmentConnection,
        frame: &[u8],
    ) -> Result<Vec<u8>, ServerFailure> {
        self.service.enrollment_frame(connection, frame).await
    }

    /// Joins the stopped actor and store worker before releasing exclusive custody.
    /// Closing a running executor does not stop or restart its server.
    pub async fn close(mut self) -> Result<Option<ShutdownReport>, ServerFailure> {
        match self.stopped.take() {
            Some(server) => server.close_owner_enrollment().await.map(Some),
            None => Ok(None),
        }
    }
}

struct StoppedAuthority(Arc<StoppedOwnerAdmission>);
impl StartupAuthorityPort for StoppedAuthority {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure> {
        self.0
            .revalidate()
            .map_err(|_| failure(ServerFailureClass::NativeUnavailable))?;
        Ok(ServerId::new(self.0.registration().server_pin().identity))
    }
    fn validate_credentials(
        &self,
        credentials: &dyn ServerCredentialPort,
        server: ServerId,
    ) -> Result<(), ServerFailure> {
        let pin = self.0.registration().server_pin();
        if server.bytes() != pin.identity || credentials.public_key() != pin.public_key {
            return Err(failure(ServerFailureClass::Authentication));
        }
        self.validate_startup().map(|_| ())
    }
}
struct NoOrdinaryAccess;
impl PairingLookup for NoOrdinaryAccess {
    fn lookup(
        &self,
        _: [u8; 16],
        _: [u8; 16],
        _: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        Err(ProtocolError::PairingDenied)
    }
}
impl PairingAuthorizationPort for NoOrdinaryAccess {}
struct NoRuntime;
impl RuntimeFactoryPort for NoRuntime {
    fn start<'a>(
        &'a self,
        _: &'a spaceterm_terminal::runtime_contracts::RuntimeStartRequest,
    ) -> ServerFuture<'a, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>> {
        Box::pin(async { Err(failure(ServerFailureClass::NativeUnavailable)) })
    }
}

/// A proof-of-possession handshake cannot construct an ordinary principal.
pub struct OwnerEnrollmentHandshake {
    server: EnrollmentServer,
    lookup: crate::pairing::protocol_lookup::PairingProtocolLookup<ServerHandle>,
}
impl OwnerEnrollmentHandshake {
    pub fn receive_proof(
        self,
        proof: &[u8],
    ) -> Result<(EnrollmentConnection, Vec<u8>), ProtocolError> {
        self.server.receive_proof(&self.lookup, proof)
    }
}

// Retain instance custody even if a recover future is cancelled while its store
// worker is still draining a started physical commit.
struct StoppedStore {
    store: SqliteOrganizationStore,
    _admission: Arc<StoppedOwnerAdmission>,
}
impl spaceterm_core::storage::OrganizationStore for StoppedStore {
    fn recover(
        &mut self,
    ) -> Result<spaceterm_core::storage::RecoveredState, spaceterm_core::storage::StoreFailure>
    {
        self.store.recover()
    }
    fn commit(
        &mut self,
        transaction: &spaceterm_core::storage::DurableTransaction,
    ) -> spaceterm_core::storage::CommitDisposition {
        self.store.commit(transaction)
    }
}

struct StoppedOwnerPort {
    owner: Arc<AccountOwnerEnrollment>,
    admission: Arc<StoppedOwnerAdmission>,
}
impl AccountOwnerEnrollmentPort for StoppedOwnerPort {
    fn revalidate_owner_issuance(
        &self,
        request: &NativeRequestId,
        digest: spaceterm_core::operations::RequestDigest,
        server: ServerId,
        claim: spaceterm_core::operations::RequestDigest,
    ) -> Result<(), PairingFailure> {
        if *request != self.admission.request() {
            return Err(PairingFailure::RequestIdentityReused);
        }
        self.admission
            .revalidate()
            .map_err(crate::account_startup::owner_failure)?;
        self.owner
            .revalidate_owner_issuance(request, digest, server, claim)
    }
    fn validate_owner_request(
        &self,
        request: &NativeRequestId,
        digest: spaceterm_core::operations::RequestDigest,
        server: ServerId,
        proposal: &spaceterm_core::pairing::EnrollmentProposal,
    ) -> Result<(), PairingFailure> {
        if *request != self.admission.request() {
            return Err(PairingFailure::RequestIdentityReused);
        }
        self.admission
            .revalidate()
            .map_err(crate::account_startup::owner_failure)?;
        self.owner
            .validate_owner_request(request, digest, server, proposal)
    }
}

fn pairing_storage_failure(error: spaceterm_host::storage::StorageFailure) -> PairingFailure {
    use spaceterm_host::storage::StorageFailure;
    match error {
        StorageFailure::Unavailable | StorageFailure::Io => PairingFailure::StorageUnavailable,
        StorageFailure::UnsafeStorage => PairingFailure::UnsafeStorage,
        StorageFailure::UnknownSchema { .. } => PairingFailure::UnknownSchema,
        StorageFailure::InvalidState => PairingFailure::InvalidDurableState,
        StorageFailure::RecoveryRequired => PairingFailure::RecoveryRequired,
    }
}
fn pairing_store_failure(error: spaceterm_core::storage::StoreFailure) -> PairingFailure {
    use spaceterm_core::storage::StoreFailure;
    match error {
        StoreFailure::Locked | StoreFailure::Unavailable | StoreFailure::Io => {
            PairingFailure::StorageUnavailable
        }
        StoreFailure::UnsafeStorage => PairingFailure::UnsafeStorage,
        StoreFailure::UnknownSchema => PairingFailure::UnknownSchema,
        StoreFailure::IdentityMismatch => PairingFailure::IdentityMismatch,
        StoreFailure::Exhausted => PairingFailure::LimitExceeded,
        StoreFailure::InvalidTransaction => PairingFailure::InvalidRequest,
        StoreFailure::InvalidDurableState => PairingFailure::InvalidDurableState,
        StoreFailure::RevisionConflict | StoreFailure::RecoveryRequired => {
            PairingFailure::RecoveryRequired
        }
    }
}
