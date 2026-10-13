//! Pairing composition over the caller's one journal controller. Construction
//! neither discovers a server nor creates credentials, enrolls, starts or repairs.

use crate::{
    credentials::adapter::DeviceCredentials, restricted_routes::NativePairingConnector,
    route::NativeRouteConnector,
};
use spaceterm_client::{
    api::RouteDescription,
    failure::{ClientFailure, ClientFailureClass},
    journal::{ClientJournalChange, ClientJournalRevision, JournalController, SavedBinding},
    pairing::{
        credentials::{
            CredentialCreationReservation, CredentialDeletion, CredentialScope, CredentialUse,
            NativeDeviceCredentials, OrdinaryCredentialAuthority, PairingFuture, PreparedCredential,
        },
        enrollment::{
            EnrollmentAttemptRequest, EnrollmentConnector, EnrollmentDriver, PairingTransport,
            save_binding,
        },
        journal::{
            JournalPairingStore, LocalPairingStore, PairingJournalSnapshot, PreparedEnrollment,
            SavedPairing, prepare_native_credential, recover_native_credential,
        },
        revocation::{ConfirmedSelfUnpair, LocalRemoval, PairingController, RevocationDriver},
    },
    pending::{LocalUseInvalidator, PendingRuntime, PendingTasks},
};
use spaceterm_core::{
    identity::ClientId,
    operation_ledger::canonical::CanonicalCodec,
    operations::{OperationId, OperationSequence},
    pairing::{
        LocalPairingGeneration, LocalPairingKey, PairingFailure, PendingRevocationTask,
        RevocationRouteReference, SignedPairingReceipt,
    },
};
use spaceterm_protocol::authentication::{HandshakeConfig, ServerPin, SigningKey};
use std::sync::Arc;
use tokio::sync::watch;

type Store = JournalPairingStore<JournalController>;
type Enrollment = EnrollmentDriver<Store, SharedCredentials, SharedConnector>;
type Restricted = RevocationDriver<SharedCredentials, SharedConnector>;

/// Desktop, CLI, companion and Bootstrap pass their own exact saved Client ID.
/// This runtime never chooses the first pairing or borrows another consumer's ID.
pub struct NativePairingRuntime {
    journal: Arc<JournalController>,
    credentials: SharedCredentials,
    enrollment: Arc<Enrollment>,
    restricted: Arc<Restricted>,
    native_connector: Option<Arc<NativePairingConnector>>,
    pending: PendingTasks,
    invalidator: Arc<dyn LocalUseInvalidator>,
    majors: Vec<u32>,
}

/// A durable binding and its process-local ordinary credential fence. A caller
/// registers this exact binding; no candidate or historical receipt grants it.
pub struct OrdinaryPairing {
    binding: SavedBinding,
    authority: OrdinaryCredentialAuthority,
    credential: PreparedCredential,
    credentials: SharedCredentials,
}
impl OrdinaryPairing {
    pub fn binding(&self) -> &SavedBinding {
        &self.binding
    }
    pub fn authority(&self) -> &OrdinaryCredentialAuthority {
        &self.authority
    }
    pub async fn signing_key(&self) -> Result<Arc<dyn SigningKey>, PairingFailure> {
        self.credentials
            .signing_key(&self.credential, &self.authority.lease())
            .await
    }
}

/// The revision and exact Client's self-revocation stream are captured together.
/// Confirming an obsolete assessment cannot remove another incarnation.
pub struct UnpairAssessment {
    revision: u64,
    saved: SavedPairing,
}
impl UnpairAssessment {
    pub fn pairing(&self) -> &LocalPairingKey {
        self.saved.key()
    }
    pub fn client(&self) -> ClientId {
        self.saved.client().parts().client.id
    }
}

impl NativePairingRuntime {
    /// Real native credentials and restricted routes, with no test adapters or
    /// plaintext fallback. Both connectors share this exact journal controller.
    pub fn native(
        journal: Arc<JournalController>,
        routes: NativeRouteConnector,
        invalidator: Arc<dyn LocalUseInvalidator>,
        config: HandshakeConfig,
    ) -> Self {
        let connector = Arc::new(NativePairingConnector::new(journal.clone(), routes));
        let mut runtime = Self::with_adapters(
            journal,
            Arc::new(DeviceCredentials::native()),
            connector.clone(),
            connector.clone(),
            invalidator,
            config,
        );
        runtime.native_connector = Some(connector);
        runtime
    }

    /// Injection is confined to native credential and transport boundaries.
    /// The portable journal, receipts, enrollment driver and scheduler stay real.
    pub fn with_adapters(
        journal: Arc<JournalController>,
        credentials: Arc<dyn NativeDeviceCredentials>,
        enrollment: Arc<dyn EnrollmentConnector>,
        restricted: Arc<dyn spaceterm_client::pairing::revocation::RestrictedConnector>,
        invalidator: Arc<dyn LocalUseInvalidator>,
        config: HandshakeConfig,
    ) -> Self {
        let credentials = SharedCredentials(credentials);
        let connector = SharedConnector { enrollment, restricted };
        Self {
            enrollment: Arc::new(EnrollmentDriver::new(
                JournalPairingStore::new(journal.clone()),
                credentials.clone(),
                connector.clone(),
            )),
            restricted: Arc::new(RevocationDriver::new(
                credentials.clone(),
                connector,
                Arc::new(CanonicalCodec),
                config.clone(),
            )),
            pending: PendingTasks::new(journal.clone(), invalidator.clone()),
            journal,
            credentials,
            native_connector: None,
            invalidator,
            majors: config.majors,
        }
    }

    pub fn journal(&self) -> &Arc<JournalController> {
        &self.journal
    }
    /// Revisions cover preparation, activation, suppression, pending phases and
    /// dismissal. Consumers read inventory after each notification.
    pub fn changes(&self) -> watch::Receiver<ClientJournalRevision> {
        self.journal.revisions()
    }
    pub async fn inventory(&self) -> Result<PairingJournalSnapshot, PairingFailure> {
        self.store().recover().await
    }
    fn store(&self) -> Store {
        JournalPairingStore::new(self.journal.clone())
    }

    /// The durable reservation precedes insertion into Keychain/Secret Service.
    pub async fn prepare_credential(
        &self,
        scope: CredentialScope,
    ) -> Result<(LocalPairingGeneration, PreparedCredential), PairingFailure> {
        prepare_native_credential(&self.store(), &self.credentials, scope).await
    }
    /// Reopening never creates a replacement key for an absent or locked item.
    pub async fn recover_credential(
        &self,
        generation: LocalPairingGeneration,
    ) -> Result<Option<PreparedCredential>, PairingFailure> {
        recover_native_credential(&self.store(), &self.credentials, generation).await
    }

    /// The caller selects a local route explicitly. A remote ticket's opaque
    /// reference cannot choose an IPC path, SSH alias or WSS endpoint by itself.
    pub async fn prepare_route(
        &self,
        prepared: &PreparedEnrollment,
        route: RouteDescription,
    ) -> Result<(), PairingFailure> {
        prepared.validate()?;
        let state = self.journal.snapshot().map_err(classify)?;
        let slot = state
            .pairing()
            .slot(prepared.credential.scope.reference())
            .ok_or(PairingFailure::CredentialIntegrity)?;
        if slot.generation != prepared.local_pairing.parts().generation
            || slot.server != Some(prepared.local_pairing.parts().server)
            || slot.public_key != Some(prepared.credential.public_key)
        {
            return Err(PairingFailure::CredentialIntegrity);
        }
        if slot.route_reference == Some(prepared.route) && slot.route.as_ref() == Some(&route) {
            // A retained enrollment has already consumed its preparation slot.
            // Its exact route is reusable, never writable as a new selection.
            return Ok(());
        }
        self.journal
            .transact(vec![ClientJournalChange::BindCredentialRoute {
                reference: prepared.credential.scope.reference(),
                route_reference: prepared.route,
                route,
            }])
            .await
            .map_err(classify)?;
        if let Some(connector) = &self.native_connector {
            connector.capture_route(prepared.credential.scope.reference()).await?;
        }
        Ok(())
    }

    /// Only explicit foreground verification and an original reservation can
    /// reach enrollment. Owner/Bootstrap issuance is supplied by its deliberate
    /// host provider; construction and ordinary restoration never issue tickets.
    pub async fn enroll(
        &self,
        request: EnrollmentAttemptRequest<'_>,
        route: RouteDescription,
    ) -> Result<SavedPairing, PairingFailure> {
        let key = request.prepared.local_pairing.clone();
        self.prepare_route(request.prepared, route).await?;
        let saved = self.enrollment.enroll(request).await?;
        // The driver retains lost ack work. Registration waits until that work
        // has settled instead of treating a successful enrollment reply as ready.
        self.require_enrollment_acknowledged(&key).await?;
        Ok(saved)
    }

    pub async fn acknowledge_enrollment_once(
        &self,
        operation: OperationId,
    ) -> Result<(), PairingFailure> {
        self.enrollment.acknowledge_once(operation, self.majors.clone()).await
    }

    /// A server-issued additional identity is verified and retained before the
    /// originating ordinary worker acknowledges its tracked operation outcome.
    /// This method does not submit another request or invent a Client identity.
    pub async fn save_additional_client(
        &self,
        prepared: &PreparedEnrollment,
        receipt: SignedPairingReceipt,
    ) -> Result<SavedPairing, PairingFailure> {
        save_binding(&self.store(), prepared, receipt, true).await
    }

    async fn require_enrollment_acknowledged(
        &self,
        key: &LocalPairingKey,
    ) -> Result<(), PairingFailure> {
        let inventory = self.inventory().await?;
        if inventory.enrollment_acknowledgements.iter().any(|work| &work.prepared.local_pairing == key) {
            return Err(PairingFailure::RecoveryRequired);
        }
        Ok(())
    }

    /// Read-only exact saved-binding restoration. Suppression and the live
    /// fence are checked before any native credential access or registration.
    pub async fn restore(
        &self,
        key: &LocalPairingKey,
        client: ClientId,
    ) -> Result<OrdinaryPairing, PairingFailure> {
        let inventory = self.inventory().await?;
        if inventory.is_suppressed(key) {
            return Err(PairingFailure::PairingRevoked);
        }
        self.require_enrollment_acknowledged(key).await?;
        let saved = inventory.saved.iter().find(|saved| saved.key() == key && saved.client().parts().client.id == client)
            .ok_or(PairingFailure::ClientNotBound)?;
        let state = self.journal.snapshot().map_err(classify)?;
        let binding = state.bindings().iter().find(|binding| {
            binding.target().client == client
                && binding.target().local_pairing().as_ref() == Ok(key)
                && binding.proof() == saved.receipt()
        }).cloned().ok_or(PairingFailure::InvalidDurableState)?;
        let authority = self.store().ordinary_authority(key).await?;
        Ok(OrdinaryPairing {
            binding,
            authority,
            credential: saved.credential().clone(),
            credentials: self.credentials.clone(),
        })
    }

    pub async fn assess_unpair(
        &self,
        key: &LocalPairingKey,
        client: ClientId,
    ) -> Result<UnpairAssessment, PairingFailure> {
        let inventory = self.inventory().await?;
        if inventory.is_suppressed(key) {
            return Err(PairingFailure::PairingRevoked);
        }
        let saved = inventory.saved.iter().find(|saved| saved.key() == key && saved.client().parts().client.id == client)
            .cloned().ok_or(PairingFailure::ClientNotBound)?;
        Ok(UnpairAssessment { revision: inventory.revision, saved })
    }

    pub async fn self_unpair(
        &self,
        assessment: UnpairAssessment,
    ) -> Result<LocalRemoval, PairingFailure> {
        let inventory = self.inventory().await?;
        if inventory.revision != assessment.revision || !inventory.saved.contains(&assessment.saved) {
            return Err(PairingFailure::StaleAssessment);
        }
        let key = assessment.saved.key().clone();
        let authority = self.store().ordinary_authority(&key).await?;
        let operation = OperationId::new(
            assessment.saved.client().parts().self_revocation,
            OperationSequence::new(1).map_err(|_| PairingFailure::InvalidRequest)?,
        );
        self.invalidator.invalidate_pairing(&key);
        let result = PairingController::new(self.store(), Arc::new(CanonicalCodec))
            .self_unpair(&authority, ConfirmedSelfUnpair::confirm(key.clone(), operation)?)
            .await?;
        self.invalidator.remove_pairing(&key).await.map_err(classify)?;
        Ok(result)
    }

    pub async fn dismiss_notice(&self, operation: OperationId) -> Result<(), PairingFailure> {
        self.pending.dismiss_revocation(operation).await.map_err(classify)
    }
    /// One attempt through the same controller. No new scheduler or retry clock.
    pub async fn retry_revocation_once(&self, operation: OperationId) -> Result<(), PairingFailure> {
        self.restricted.run_once(&self.store(), operation).await
    }
    /// The controller's lifetime claim rejects every duplicate scheduler, even
    /// when another runtime facade shares the physical journal.
    pub fn start_scheduler(&self) -> Result<PendingRuntime, ClientFailure> {
        self.pending.start_pairing_scheduler(self.restricted.clone())
    }
}

#[derive(Clone)]
struct SharedCredentials(Arc<dyn NativeDeviceCredentials>);
impl NativeDeviceCredentials for SharedCredentials {
    fn create(&self, reservation: CredentialCreationReservation) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        self.0.create(reservation)
    }
    fn recover_preparation<'a>(&'a self, scope: &'a CredentialScope) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        self.0.recover_preparation(scope)
    }
    fn signing_key<'a>(&'a self, credential: &'a PreparedCredential, authorization: &'a CredentialUse) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        self.0.signing_key(credential, authorization)
    }
    fn delete_pairing_key<'a>(&'a self, credential: &'a PreparedCredential) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        self.0.delete_pairing_key(credential)
    }
}
#[derive(Clone)]
struct SharedConnector {
    enrollment: Arc<dyn EnrollmentConnector>,
    restricted: Arc<dyn spaceterm_client::pairing::revocation::RestrictedConnector>,
}
impl EnrollmentConnector for SharedConnector {
    fn open<'a>(&'a self, pin: ServerPin, route: RevocationRouteReference) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        self.enrollment.open(pin, route)
    }
}
impl spaceterm_client::pairing::revocation::RestrictedConnector for SharedConnector {
    fn open<'a>(&'a self, task: &'a PendingRevocationTask) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        self.restricted.open(task)
    }
}

fn classify(failure: ClientFailure) -> PairingFailure {
    match failure.class() {
        ClientFailureClass::Unavailable => PairingFailure::Offline,
        ClientFailureClass::AuthorizationDenied => PairingFailure::AuthorizationDenied,
        ClientFailureClass::PinMismatch => PairingFailure::IdentityMismatch,
        ClientFailureClass::Incompatible => PairingFailure::Incompatible,
        ClientFailureClass::StaleAuthority => PairingFailure::StaleAssessment,
        ClientFailureClass::ResourceLimit | ClientFailureClass::CounterExhausted => PairingFailure::LimitExceeded,
        ClientFailureClass::StorageUnavailable => PairingFailure::StorageUnavailable,
        ClientFailureClass::UnsafeStorage | ClientFailureClass::UnsafeEndpoint => PairingFailure::UnsafeStorage,
        ClientFailureClass::UnknownSchema => PairingFailure::UnknownSchema,
        ClientFailureClass::RecoveryRequired => PairingFailure::RecoveryRequired,
        ClientFailureClass::Cancelled => PairingFailure::Cancelled,
        _ => PairingFailure::InvalidDurableState,
    }
}
