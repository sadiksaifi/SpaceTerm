//! Concrete listener-free paired maintenance of an established stopped account
//! server. SSH reachability and owner enrollment supply no authority here.

use std::sync::Arc;

use spaceterm_core::{
    authority::{ConnectionGeneration, FieldVersion},
    identity::ServerId,
    organization::ServerDirectory,
    pairing::policy::{PairingPolicy, PairingPrincipal, PairingSnapshot},
    storage::{OrganizationStore, StoreFailure},
};
use spaceterm_distribution::VerifiedAccountServerArtifact;
use spaceterm_host::{
    account_service::{
        STOPPED_NATIVE_WIRE_LIMIT, ServiceFailure, ServiceProgress, ServiceSupervisor,
        StoppedNativeAdmission, StoppedNativeRequest, TrustedAccountFacts,
    },
    organization_store::{SqliteOrganizationStore, StoreIdentity, StoreLimits, migrations, schema},
    server_keys::{ServerIdentity, ServerKeyStore},
    startup::AccountBuildKey,
    storage::{Database, StorageFailure},
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{
        Established, HandshakeConfig, PairingLookup, PairingRecord, Principal, ServerHandshake,
        ServerPin,
    },
    codec::Payload,
    connection::AuthenticatedConnection,
    wire::{common, v1, v2},
};

use crate::{
    adapters::store::production_services,
    api::VerifiedPrincipal,
    pairing::protocol_lookup::{PairingProtocolLookup, PairingSnapshotPort},
};

pub mod producer;

pub use spaceterm_host::account_service::{StoppedNativeCall, StoppedNativeReply};

/// Exact saved client and selected native registration comparisons. Constructing
/// this value grants no authority; the ordinary pinned handshake proves possession.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoppedNativeTarget {
    pub key: AccountBuildKey,
    pub pin: ServerPin,
    pub principal: Principal,
}

/// Established SQLite and native custody live together until original admission.
/// Field order closes SQLite before releasing its immutable instance lease.
struct FrozenStopped {
    store: SqliteOrganizationStore,
    admission: StoppedNativeAdmission,
}
impl FrozenStopped {
    fn pairings(&mut self, expected: Principal) -> Result<FrozenPairings, ServiceFailure> {
        let recovered = self.store.recover().map_err(store_failure)?;
        if recovered.organization.server.bytes() != expected.server {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let recovery = recovered.pairing.ok_or(ServiceFailure::PairingRequired)?;
        let snapshot = PairingSnapshot::new(
            recovered.organization.server,
            recovered.last_epoch,
            recovered.store_revision,
            recovered.high_water,
            recovery,
        )
        .map_err(|_| ServiceFailure::InvalidRecord)?;
        Ok(FrozenPairings { snapshot, expected })
    }
}

struct FrozenPairings {
    snapshot: PairingSnapshot,
    expected: Principal,
}
struct SnapshotSource(PairingSnapshot);
impl PairingSnapshotPort for SnapshotSource {
    fn snapshot(&self) -> Result<PairingSnapshot, ProtocolError> {
        Ok(self.0.clone())
    }
}
impl PairingLookup for FrozenPairings {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        if server != self.expected.server
            || pairing != self.expected.pairing
            || client != self.expected.client
        {
            return Err(ProtocolError::PairingDenied);
        }
        let record = PairingProtocolLookup::new(SnapshotSource(self.snapshot.clone()))
            .lookup(server, pairing, client)?;
        if record.principal != self.expected {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(record)
    }
}
impl FrozenPairings {
    fn authorize(
        &self,
        principal: VerifiedPrincipal,
    ) -> Result<(PairingPrincipal, FieldVersion), ServiceFailure> {
        if principal.server() != self.snapshot.server()
            || principal.epoch() != self.snapshot.epoch()
        {
            return Err(ServiceFailure::PairingRequired);
        }
        let authorized = PairingPolicy::default()
            .authorize_client(&self.snapshot, principal.pairing(), principal.client())
            .map_err(|_| ServiceFailure::PairingRequired)?;
        if authorized.device() != principal.device()
            || authorized.server().bytes() != self.expected.server
            || authorized.device().bytes() != self.expected.device
        {
            return Err(ServiceFailure::PairingRequired);
        }
        let version = self
            .snapshot
            .authorization(authorized.pairing())
            .map_err(|_| ServiceFailure::PairingRequired)?
            .parts()
            .authorization_version;
        if version.get() == 0 {
            return Err(ServiceFailure::PairingRequired);
        }
        Ok((authorized, version))
    }
}

/// Opens only existing native keys and canonical SQLite after confirmed exclusive
/// stopped admission. It creates no actor, listener, runtime Session or enrollment.
pub struct AccountNativeMaintenanceExecutor {
    frozen: FrozenStopped,
    supervisor: Arc<ServiceSupervisor>,
    target: StoppedNativeTarget,
    original: StoppedNativeRequest,
    credentials: Arc<ServerIdentity>,
    artifact: Option<VerifiedAccountServerArtifact>,
    authentication_deadline: tokio::time::Instant,
}
impl AccountNativeMaintenanceExecutor {
    /// Native composition supplies the account home, never a remote path. Stage
    /// takes the existing signed artifact type; host admission independently checks
    /// its compiled publisher trust before admitting artifact effects.
    pub async fn open(
        supervisor: Arc<ServiceSupervisor>,
        home: ServerDirectory,
        target: StoppedNativeTarget,
        original: StoppedNativeRequest,
        artifact: Option<VerifiedAccountServerArtifact>,
    ) -> Result<Self, ServiceFailure> {
        let account = TrustedAccountFacts::current()?;
        if supervisor.key() != target.key
            || target.key.account_uid != account.uid()
            || target.principal.server != target.pin.identity
            || target.principal.device == [0; 16]
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let admission = supervisor.admit_stopped_native(original).await?;
        let registration = admission.registration();
        if registration.key() != target.key
            || registration.server_pin() != target.pin
            || home.server() != ServerId::new(target.pin.identity)
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let credentials = ServerKeyStore::open(registration)
            .and_then(|keys| keys.load_identity(target.pin))
            .map_err(ServiceFailure::from)?;
        let identity = StoreIdentity {
            server: ServerId::new(target.pin.identity),
            account: target.key,
            server_public_key: target.pin.public_key,
        };
        let services = production_services(identity, home).map_err(store_failure)?;
        let database = Database::open_frozen_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .map_err(storage_failure)?;
        let store = SqliteOrganizationStore::open_existing(
            database,
            identity,
            StoreLimits::production(),
            services,
        )
        .map_err(store_failure)?;
        let mut frozen = FrozenStopped { store, admission };
        // Recover through the canonical production validator, including signed
        // receipt and Client stream bindings. A hand-built lookup is not a fallback.
        frozen.pairings(target.principal)?;
        frozen.admission.revalidate().await?;
        Ok(Self {
            frozen,
            supervisor,
            target,
            original,
            credentials,
            artifact,
            authentication_deadline: tokio::time::Instant::now()
                + crate::connection::HANDSHAKE_DEADLINE,
        })
    }
    fn check_authentication_deadline(&self) -> Result<(), ServiceFailure> {
        if tokio::time::Instant::now() >= self.authentication_deadline {
            return Err(ServiceFailure::PairingRequired);
        }
        Ok(())
    }
    /// Deadline the bounded transport producer applies to receiving the original
    /// hello and device proof. It is never refreshed between handshake messages.
    pub fn authentication_deadline(&self) -> tokio::time::Instant {
        self.authentication_deadline
    }
    /// Exchanges an ordinary-purpose ServerAuth hello. The caller forwards these
    /// exact bytes to the initiating Client's own pinned native signer.
    pub async fn accept_hello(
        mut self,
        hello: &[u8],
    ) -> Result<(StoppedNativeHandshake, Vec<u8>), ServiceFailure> {
        self.check_authentication_deadline()?;
        tokio::time::timeout_at(
            self.authentication_deadline,
            self.frozen.admission.revalidate(),
        )
        .await
        .map_err(|_| ServiceFailure::PairingRequired)??;
        let pairings = self.frozen.pairings(self.target.principal)?;
        let (server, reply) = ServerHandshake::accept(
            self.target.pin.identity,
            pairings.snapshot.epoch().get(),
            HandshakeConfig {
                capabilities: vec![
                    spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
                    spaceterm_protocol::negotiation::CAPABILITY_ACCOUNT_RUNTIME,
                    spaceterm_protocol::negotiation::CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
                ],
                ..HandshakeConfig::default()
            },
            self.credentials.clone(),
            &pairings,
            hello,
        )
        .map_err(protocol_failure)?;
        self.check_authentication_deadline()?;
        Ok((
            StoppedNativeHandshake {
                executor: self,
                server,
            },
            reply,
        ))
    }
}

/// Keeps immutable native custody and SQLite throughout the real proof exchange.
pub struct StoppedNativeHandshake {
    executor: AccountNativeMaintenanceExecutor,
    server: ServerHandshake,
}
impl StoppedNativeHandshake {
    /// Original deadline, shared with the hello exchange and never refreshed.
    pub fn authentication_deadline(&self) -> tokio::time::Instant {
        self.executor.authentication_deadline()
    }
    /// Verifies the initiating device proof against current durable authorization.
    /// Restricted revocation/status and enrollment channels cannot enter this state.
    pub async fn receive_proof(
        mut self,
        proof: &[u8],
    ) -> Result<(StoppedNativeSession, Vec<u8>), ServiceFailure> {
        self.executor.check_authentication_deadline()?;
        tokio::time::timeout_at(
            self.executor.authentication_deadline,
            self.executor.frozen.admission.revalidate(),
        )
        .await
        .map_err(|_| ServiceFailure::PairingRequired)??;
        let pairings = self
            .executor
            .frozen
            .pairings(self.executor.target.principal)?;
        let (established, ready) = self
            .server
            .receive_device(&pairings, proof)
            .map_err(protocol_failure)?;
        let Established::Ordinary(connection) = established else {
            return Err(ServiceFailure::PairingRequired);
        };
        if connection.principal() != self.executor.target.principal
            || !connection
                .capabilities()
                .contains(&spaceterm_protocol::negotiation::CAPABILITY_ACCOUNT_RUNTIME)
            || !connection
                .capabilities()
                .contains(&spaceterm_protocol::negotiation::CAPABILITY_REMOTE_NATIVE_LIFECYCLE)
        {
            return Err(ServiceFailure::PairingRequired);
        }
        let principal =
            VerifiedPrincipal::from_authenticated(&connection, ConnectionGeneration::new(1))
                .map_err(|_| ServiceFailure::PairingRequired)?;
        self.executor.check_authentication_deadline()?;
        let current = self
            .executor
            .frozen
            .pairings(self.executor.target.principal)?;
        current.authorize(principal)?;
        tokio::time::timeout_at(
            self.executor.authentication_deadline,
            self.executor.frozen.admission.revalidate(),
        )
        .await
        .map_err(|_| ServiceFailure::PairingRequired)??;
        self.executor.check_authentication_deadline()?;
        let executor = self.executor;
        Ok((
            StoppedNativeSession {
                frozen: Some(executor.frozen),
                supervisor: executor.supervisor,
                target: executor.target,
                original: executor.original,
                credentials: executor.credentials,
                artifact: executor.artifact,
                connection,
                principal,
                completion: None,
            },
            ready,
        ))
    }
}

/// An ordinary authenticated channel is owned here, not supplied by a wire caller.
/// Assess-only disposal is safe. An admitted operation transfers to an owned native
/// worker after the frozen database and instance lease have both been released.
pub struct StoppedNativeSession {
    frozen: Option<FrozenStopped>,
    supervisor: Arc<ServiceSupervisor>,
    target: StoppedNativeTarget,
    original: StoppedNativeRequest,
    credentials: Arc<ServerIdentity>,
    artifact: Option<VerifiedAccountServerArtifact>,
    connection: AuthenticatedConnection,
    principal: VerifiedPrincipal,
    completion: Option<tokio::task::JoinHandle<Result<ServiceProgress, ServiceFailure>>>,
}
impl StoppedNativeSession {
    /// Accepts only the closed stopped maintenance grammar in a signed ordinary
    /// AccountRuntimeRequest. Every decoded value is comparison data, not authority.
    pub async fn frame(&mut self, bytes: &[u8]) -> Result<Vec<u8>, ServiceFailure> {
        let payload = self.connection.decode(bytes).map_err(protocol_failure)?;
        let message = match &payload {
            Payload::Major1(message) => match &message.kind {
                Some(v1::message::Kind::AccountRuntimeRequest(message)) => message,
                _ => return Err(ServiceFailure::RequestConflict),
            },
            Payload::Major2(message) => match &message.kind {
                Some(v2::message::Kind::AccountRuntimeRequest(message)) => message,
                _ => return Err(ServiceFailure::RequestConflict),
            },
        };
        if message.request == 0
            || message.request_generation == 0
            || message.body.len() > STOPPED_NATIVE_WIRE_LIMIT
        {
            return Err(ServiceFailure::LimitExceeded);
        }
        let call: StoppedNativeCall =
            serde_json::from_slice(&message.body).map_err(|_| ServiceFailure::InvalidRecord)?;
        let reply = tokio::time::timeout(crate::connection::HANDSHAKE_DEADLINE, self.call(call))
            .await
            .map_err(|_| ServiceFailure::Pending)?
            .unwrap_or_else(StoppedNativeReply::Failed);
        let body = serde_json::to_vec(&reply).map_err(|_| ServiceFailure::InvalidRecord)?;
        if body.len() > STOPPED_NATIVE_WIRE_LIMIT {
            return Err(ServiceFailure::LimitExceeded);
        }
        let response = common::AccountRuntimeMessage {
            request: message.request,
            request_generation: message.request_generation,
            body,
        };
        let response = match payload {
            Payload::Major1(_) => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeReply(response)),
            }),
            Payload::Major2(_) => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeReply(response)),
            }),
        };
        self.connection.encode(&response).map_err(protocol_failure)
    }
    async fn call(
        &mut self,
        call: StoppedNativeCall,
    ) -> Result<StoppedNativeReply, ServiceFailure> {
        let frozen = self
            .frozen
            .as_mut()
            .ok_or(ServiceFailure::RequestConflict)?;
        frozen.admission.revalidate().await?;
        let pairings = frozen.pairings(self.target.principal)?;
        let (principal, version) = pairings.authorize(self.principal)?;
        match call {
            StoppedNativeCall::Assess(original) => {
                if original != self.original {
                    return Err(ServiceFailure::RequestConflict);
                }
                frozen
                    .admission
                    .assess(
                        principal,
                        version,
                        pairings.snapshot.epoch(),
                        self.credentials.clone(),
                        self.artifact.as_ref(),
                    )
                    .await
                    .map(StoppedNativeReply::Assessment)
            }
            StoppedNativeCall::Admit(reference) => {
                if reference.original != self.original {
                    return Err(ServiceFailure::RequestConflict);
                }
                let acceptance = frozen
                    .admission
                    .accept(&reference, principal, version, pairings.snapshot.epoch())
                    .await;
                match acceptance {
                    Ok(accepted) => {
                        self.retain_completion();
                        Ok(StoppedNativeReply::Accepted(accepted))
                    }
                    Err(ServiceFailure::Indeterminate) => {
                        // Publication may already contain Accepted. The producer
                        // must drain exact-original recovery even when no success
                        // reply can be sent and the SSH stream immediately ends.
                        self.retain_completion();
                        Err(ServiceFailure::Indeterminate)
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }
    fn retain_completion(&mut self) {
        // SQLite closes before the lifecycle and instance leases. Exact original
        // recovery then observes the retained outcome, never promotes Reserved,
        // and lets an accepted Start acquire normal startup custody.
        drop(self.frozen.take());
        let supervisor = self.supervisor.clone();
        let original = self.original;
        let artifact = self.artifact.take();
        self.completion = Some(tokio::spawn(async move {
            supervisor
                .resolve_stopped_native_with_artifact_exact(original, artifact.as_ref())
                .await
        }));
    }
    /// Includes uncertain Accepted publication. The producer must join this owned
    /// recovery/completion before exiting on EOF or any reply encoding/write error.
    pub fn needs_completion(&self) -> bool {
        self.completion.is_some()
    }
    /// Compatibility query for producers. True also means publication uncertainty
    /// must be drained; it is not a claim that admission was durably observed.
    pub fn is_admitted(&self) -> bool {
        self.needs_completion()
    }
    /// Joins the exact original native worker. An Assess-only channel cannot use
    /// this method to accept a request, launch code or refresh its expiry.
    pub async fn finish(mut self) -> Result<ServiceProgress, ServiceFailure> {
        let worker = self
            .completion
            .take()
            .ok_or(ServiceFailure::PairingRequired)?;
        worker.await.map_err(|_| ServiceFailure::Indeterminate)?
    }
}

fn protocol_failure(_: ProtocolError) -> ServiceFailure {
    ServiceFailure::PairingRequired
}
fn storage_failure(error: StorageFailure) -> ServiceFailure {
    match error {
        StorageFailure::Unavailable | StorageFailure::Io => ServiceFailure::StorageUnavailable,
        StorageFailure::UnsafeStorage => ServiceFailure::UnsafeStorage,
        StorageFailure::UnknownSchema { .. } => ServiceFailure::UnsupportedSchema,
        StorageFailure::InvalidState => ServiceFailure::InvalidRecord,
        StorageFailure::RecoveryRequired => ServiceFailure::CleanupUnresolved,
    }
}
fn store_failure(error: StoreFailure) -> ServiceFailure {
    match error {
        StoreFailure::Locked => ServiceFailure::StorageLocked,
        StoreFailure::Unavailable | StoreFailure::Io => ServiceFailure::StorageUnavailable,
        StoreFailure::UnsafeStorage => ServiceFailure::UnsafeStorage,
        StoreFailure::UnknownSchema => ServiceFailure::UnsupportedSchema,
        StoreFailure::IdentityMismatch => ServiceFailure::IdentityMismatch,
        StoreFailure::Exhausted => ServiceFailure::LimitExceeded,
        StoreFailure::InvalidTransaction | StoreFailure::InvalidDurableState => {
            ServiceFailure::InvalidRecord
        }
        StoreFailure::RevisionConflict | StoreFailure::RecoveryRequired => {
            ServiceFailure::CleanupUnresolved
        }
    }
}
