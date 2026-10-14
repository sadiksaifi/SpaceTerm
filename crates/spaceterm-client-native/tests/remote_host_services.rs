//! Canonical Mac retention, the real protocol worker and genuine guest native
//! lifecycle journals. Only SSH, OS-manager and runtime/authentication ports are injected.
#![cfg(any(target_os = "macos", target_os = "linux"))]

#[path = "../../spaceterm-client/tests/support/mod.rs"]
mod connection_support;
#[path = "../../spaceterm-host/tests/support/signed_installation.rs"]
mod signed_installation;

use std::{
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use connection_support::{TestKey, snapshot};
use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding, RouteDescription, RouteParts},
    failure::{ClientFailure, ClientFailureClass},
    journal::{
        ClientJournalChange, JournalController, JournalCredentialSlot, JournalLimits,
        ReceiptVerifier, SavedBinding,
    },
    limits::ClientLimits,
    management_cli::{ManagementFailure, ManagementFuture},
    pairing::{credentials::*, enrollment::*, revocation::RestrictedConnector},
    pending::LocalUseInvalidator,
    ports::*,
    registry::{ClientRegistry, ClientRuntime, ServerHandle},
};
use spaceterm_client_native::{
    journal::NativeClientJournal,
    native::{
        classify_service,
        host_services::HostServiceManagement,
        remote_host_services::{RemoteHostServices, RemoteNativeTransport, RemoteStoppedRequest},
    },
    pairing_runtime::NativePairingRuntime,
};
use spaceterm_core::{
    authority::{
        FieldVersion, GrantedControl, ServerEpoch, SessionGeneration, SessionKey, StoreRevision,
    },
    identity::*,
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    operation_ledger::canonical::CanonicalCodec,
    operations::{OperationBinding, OperationId, OperationSequence, RequestDigest},
    pairing::policy::PairingPrincipal,
    pairing::*,
    retirement::{CleanupResult, ConfirmedCleanupClass, SessionCleanupObservation},
};
use spaceterm_distribution::VerifiedAccountServerArtifact;
use spaceterm_host::{
    account_service::{
        InstallationStatus, LingerState, NativeStageAccountReply, NativeStageAccountRequest,
        RegisteredServer, RuntimeAssessment, ServiceAction, ServiceClock, ServiceFailure,
        ServiceFuture, ServiceManager, ServiceOutcome, ServiceProcesses, ServiceProgress,
        ServiceRequestState, ServiceSupervisor, ServiceTime, StoppedNativeAction,
        StoppedNativeAssessmentRef, StoppedNativeCall, StoppedNativeReply, StoppedNativeRequest,
        SupervisorState,
        remote_wire::{NativeLifecycleAccountReply, NativeLifecycleAccountRequest},
        runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply, RuntimeCustody},
        test_support::AccountServiceFixture,
    },
    native_deployment::contracts::NativeRequestId,
    server_keys::ServerKeyStore,
    startup::{AccountBuildKey, BuildIdentity, DesiredServerState},
    transport::MessageTransport,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{
        Established, HandshakeConfig, PairingLookup, PairingRecord, Principal, ServerHandshake,
        ServerPin, SigningKey,
    },
    codec::Payload,
    convert::organization,
    convert::organization::PublicationPart,
    negotiation::{
        CAPABILITY_ACCOUNT_RUNTIME, CAPABILITY_CANONICAL_DOMAIN,
        CAPABILITY_NORMALIZED_CREATION_TOPOLOGY, CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
    },
    pairing_adapter::ReceiptCodec,
    wire::{common, v1, v2},
};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidStorage))
    }
}

struct Clock(AtomicU64);
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: [1; 32],
            millis: self.0.load(Ordering::SeqCst),
        })
    }
}

struct Manager {
    running: Arc<AtomicBool>,
    effects: AtomicUsize,
}
impl ServiceManager for Manager {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async {
            Ok(if self.running.load(Ordering::SeqCst) {
                SupervisorState::Running
            } else {
                SupervisorState::Inactive
            })
        })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async { Ok(cfg!(target_os = "linux").then_some(LingerState::Enabled)) })
    }
    fn configure<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn start<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.fetch_add(1, Ordering::SeqCst);
            self.running.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.fetch_add(1, Ordering::SeqCst);
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        })
    }
    fn uninstall<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn verify_installation(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        Ok(())
    }
}
struct Processes {
    running: Arc<AtomicBool>,
}
impl ServiceProcesses for Processes {
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            let server = ServerId::new(registration.server_pin().identity);
            let epoch = ServerEpoch::new(17);
            let running = self.running.load(Ordering::SeqCst);
            Ok(RuntimeAssessment {
                server,
                epoch: running.then_some(epoch),
                generation: 4,
                token: [6; 32],
                sessions: if running {
                    [1, 2]
                        .map(|pane| SessionKey {
                            pane: PaneId::new(server, pane).unwrap(),
                            epoch,
                            generation: SessionGeneration::new(1),
                        })
                        .to_vec()
                } else {
                    vec![]
                },
            })
        })
    }
    fn revalidate<'a>(
        &'a self,
        _: &'a RegisteredServer,
        scope: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if scope.epoch == Some(ServerEpoch::new(17)) {
                Ok(())
            } else {
                Err(ServiceFailure::StaleAssessment)
            }
        })
    }
    fn abort_reservation<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn quiesce<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: NativeManagementRequestRef,
        action: NativeRetirementAction,
        scope: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            self.running.store(false, Ordering::SeqCst);
            scope
                .sessions
                .iter()
                .map(|session| {
                    SessionCleanupObservation::new(
                        *session,
                        action.reason(),
                        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
                    )
                    .map_err(|_| ServiceFailure::InvalidRecord)
                })
                .collect()
        })
    }
    fn verify_absent<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            if self.running.load(Ordering::SeqCst) {
                Err(ServiceFailure::CleanupUnresolved)
            } else {
                Ok(())
            }
        })
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: ServerEpoch,
        _: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async { Err(ServiceFailure::CleanupUnresolved) })
    }
}

struct Credentials(Arc<TestKey>);
impl NativeDeviceCredentials for Credentials {
    fn create(
        &self,
        _: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async { panic!("management must not create credentials") })
    }
    fn recover_preparation<'a>(
        &'a self,
        _: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async { Ok(None) })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            Ok(self.0.clone() as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async { panic!("management must not delete credentials") })
    }
}
struct NoEnrollment;
impl EnrollmentConnector for NoEnrollment {
    fn open<'a>(
        &'a self,
        _: ServerPin,
        _: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async { panic!("management must not enroll") })
    }
}
impl RestrictedConnector for NoEnrollment {
    fn open<'a>(
        &'a self,
        _: &'a PendingRevocationTask,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async { panic!("management must not replace a pairing") })
    }
}
struct NoLocalEffects;
impl LocalUseInvalidator for NoLocalEffects {
    fn invalidate_pairing(&self, _: &LocalPairingKey) {}
    fn remove_pairing<'a>(&'a self, _: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
        Box::pin(async { panic!("management must not remove a pairing") })
    }
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Ok(vec![])
    }
    fn invalidate_attachment(
        &self,
        _: &LocalPairingKey,
        _: spaceterm_client::view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Ok(vec![])
    }
}

type StoppedWorkers =
    Arc<AsyncMutex<Vec<tokio::task::JoinHandle<Result<ServiceProgress, ServiceFailure>>>>>;
type RevalidationHook = Arc<dyn Fn() -> ManagementFuture<'static, ()> + Send + Sync>;

struct Ssh {
    key: AccountBuildKey,
    pin: ServerPin,
    supervisor: Arc<ServiceSupervisor>,
    calls: Mutex<Vec<NativeLifecycleAccountRequest>>,
    lose_capture: AtomicBool,
    lose_ack: AtomicBool,
    lose_stage_resolve: AtomicBool,
    wrong_owner: AtomicBool,
    peer: Arc<Peer>,
    stopped_opens: Arc<AtomicUsize>,
    capture_race: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    revalidation_hook: Mutex<Option<RevalidationHook>>,
    stopped_completions: StoppedWorkers,
}
impl RemoteNativeTransport for Ssh {
    fn revalidate(&self, key: AccountBuildKey, pin: ServerPin) -> ManagementFuture<'_, ()> {
        Box::pin(async move {
            if self.wrong_owner.load(Ordering::SeqCst) || key != self.key || pin != self.pin {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            let hook = self.revalidation_hook.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook().await?;
            }
            if self.wrong_owner.load(Ordering::SeqCst) || key != self.key || pin != self.pin {
                Err(ManagementFailure::AuthorizationDenied)
            } else {
                Ok(())
            }
        })
    }
    fn account_call(
        &self,
        request: NativeLifecycleAccountRequest,
    ) -> ManagementFuture<'_, NativeLifecycleAccountReply> {
        Box::pin(async move {
            let (key, pin) = request.expected_identity();
            self.revalidate(key, pin).await?;
            let request =
                NativeLifecycleAccountRequest::decode(&request.encode().unwrap()).unwrap();
            self.calls.lock().unwrap().push(request.clone());
            // Match the genuine producer behavior using the real native owner.
            let reply = match request {
                NativeLifecycleAccountRequest::Observe { .. } => {
                    let status = self.supervisor.status().await.map_err(classify_service)?;
                    let generation = self
                        .supervisor
                        .discover()
                        .map_err(classify_service)?
                        .unwrap()
                        .generation();
                    NativeLifecycleAccountReply::Observed(
                        spaceterm_host::account_service::remote_wire::NativeLifecycleObservation {
                            status,
                            registration_generation: std::num::NonZeroU64::new(generation).unwrap(),
                        },
                    )
                }
                NativeLifecycleAccountRequest::Reserve { .. } => {
                    NativeLifecycleAccountReply::Reserved(
                        self.supervisor
                            .allocate_request()
                            .map_err(classify_service)?,
                    )
                }
                NativeLifecycleAccountRequest::Capture {
                    request, action, ..
                } => {
                    let capture = self
                        .supervisor
                        .capture_runtime_request(request, action)
                        .map_err(classify_service)?;
                    if self.lose_capture.swap(false, Ordering::SeqCst) {
                        return Err(ManagementFailure::IoUnavailable);
                    }
                    NativeLifecycleAccountReply::Captured(capture)
                }
                NativeLifecycleAccountRequest::Restore {
                    original,
                    assessment,
                    ..
                } => {
                    let retained = self
                        .supervisor
                        .restore_runtime_request(original.request)
                        .map_err(classify_service)?;
                    let reference = match self
                        .supervisor
                        .restore_runtime_lifecycle_reference(original)
                    {
                        Ok(reference) => Some(reference),
                        Err(ServiceFailure::RequestConflict) => None,
                        Err(failure) => return Err(classify_service(failure)),
                    };
                    if retained != original
                        || assessment
                            .as_ref()
                            .is_some_and(|expected| reference.as_ref() != Some(expected))
                    {
                        return Err(ManagementFailure::StaleAssessment);
                    }
                    NativeLifecycleAccountReply::Restored {
                        original: retained,
                        assessment: reference,
                    }
                }
                NativeLifecycleAccountRequest::Resolve { request, .. } => {
                    NativeLifecycleAccountReply::Progress(
                        self.supervisor
                            .resolve_runtime_lifecycle(key, pin, request)
                            .await
                            .map_err(classify_service)?,
                    )
                }
                NativeLifecycleAccountRequest::Acknowledge { progress, .. } => {
                    self.supervisor
                        .acknowledge_runtime_lifecycle_exact(&progress)
                        .map_err(classify_service)?;
                    if self.lose_ack.swap(false, Ordering::SeqCst) {
                        return Err(ManagementFailure::IoUnavailable);
                    }
                    NativeLifecycleAccountReply::Acknowledged { progress }
                }
            };
            self.revalidate(key, pin).await?;
            Ok(NativeLifecycleAccountReply::decode(&reply.encode().unwrap()).unwrap())
        })
    }
    fn stage_call<'a>(
        &'a self,
        request: NativeStageAccountRequest,
        artifact: Option<&'a VerifiedAccountServerArtifact>,
    ) -> ManagementFuture<'a, NativeStageAccountReply> {
        Box::pin(async move {
            let (key, pin) = request.expected_identity();
            self.revalidate(key, pin).await?;
            let request = NativeStageAccountRequest::decode(&request.encode().unwrap()).unwrap();
            let reply = match request {
                NativeStageAccountRequest::Capture {
                    request,
                    artifact_sha256,
                    ..
                } => {
                    let artifact = artifact.ok_or(ManagementFailure::UnsafeNativeState)?;
                    assert_eq!(artifact.artifact_sha256(), artifact_sha256);
                    let original = self
                        .supervisor
                        .capture_runtime_stage(request, artifact)
                        .map_err(classify_service)?;
                    if self.lose_capture.swap(false, Ordering::SeqCst) {
                        return Err(ManagementFailure::IoUnavailable);
                    }
                    NativeStageAccountReply::Captured(original)
                }
                NativeStageAccountRequest::Restore { request, .. } => {
                    let original = self
                        .supervisor
                        .restore_runtime_stage(request)
                        .map_err(classify_service)?;
                    let assessment = match self.supervisor.restore_runtime_stage_reference(original)
                    {
                        Ok(reference) => Some(reference),
                        Err(ServiceFailure::Pending) => None,
                        Err(failure) => return Err(classify_service(failure)),
                    };
                    let progress = match self.supervisor.inspect_install_request(request) {
                        Ok((progress, _)) => Some(progress),
                        Err(ServiceFailure::UnknownRequest) => None,
                        Err(failure) => return Err(classify_service(failure)),
                    };
                    NativeStageAccountReply::Restored {
                        original,
                        assessment,
                        progress,
                    }
                }
                NativeStageAccountRequest::Resolve { request, .. } => {
                    if self.lose_stage_resolve.swap(false, Ordering::SeqCst) {
                        return Err(ManagementFailure::IoUnavailable);
                    }
                    NativeStageAccountReply::Progress(
                        self.supervisor
                            .resolve_runtime_stage_exact(request, artifact)
                            .map_err(classify_service)?,
                    )
                }
                NativeStageAccountRequest::Acknowledge { progress, .. } => {
                    self.supervisor
                        .acknowledge_runtime_stage_exact(&progress)
                        .map_err(classify_service)?;
                    NativeStageAccountReply::Acknowledged { progress }
                }
            };
            self.revalidate(key, pin).await?;
            Ok(NativeStageAccountReply::decode(&reply.encode().unwrap()).unwrap())
        })
    }
    fn stopped_channel(
        &self,
        request: RemoteStoppedRequest,
        artifact: Option<VerifiedAccountServerArtifact>,
    ) -> ManagementFuture<'_, (StoppedNativeRequest, Arc<dyn MessageTransport>)> {
        Box::pin(async move {
            self.revalidate(request.expected_key, request.expected_pin)
                .await?;
            assert_eq!(
                request.principal.server,
                self.peer.binding.target().server().bytes()
            );
            assert_eq!(
                request.principal.client,
                local(self.peer.binding.target().client().local())
            );
            if let Some(change) = self.capture_race.lock().unwrap().take() {
                change();
            }
            let original = self
                .supervisor
                .capture_stopped_native_expected(
                    request.request,
                    request.action,
                    request.expected_key,
                    request.expected_pin,
                    request.registration_generation.get(),
                )
                .map_err(classify_service)?;
            let admission = self
                .supervisor
                .admit_stopped_native(original)
                .await
                .map_err(classify_service)?;
            let (to_guest, from_mac) = mpsc::channel(4);
            let (to_mac, from_guest) = mpsc::channel(4);
            let peer = self.peer.clone();
            let completions = self.stopped_completions.clone();
            self.stopped_opens.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                stopped_peer(
                    peer,
                    request.principal,
                    original,
                    admission,
                    artifact,
                    from_mac,
                    to_mac,
                    completions,
                )
                .await
            });
            Ok((
                original,
                Arc::new(StoppedPipe {
                    sender: Mutex::new(Some(to_guest)),
                    receiver: AsyncMutex::new(from_guest),
                }) as Arc<dyn MessageTransport>,
            ))
        })
    }
    fn restore_stopped(
        &self,
        key: AccountBuildKey,
        pin: ServerPin,
        request: NativeRequestId,
        action: StoppedNativeAction,
    ) -> ManagementFuture<'_, Option<StoppedNativeAssessmentRef>> {
        Box::pin(async move {
            self.revalidate(key, pin).await?;
            let original = match self.supervisor.restore_stopped_native(request) {
                Ok(original) => original,
                Err(ServiceFailure::RequestConflict) => return Ok(None),
                Err(failure) => return Err(classify_service(failure)),
            };
            if original.action != action {
                return Err(ManagementFailure::StaleAssessment);
            }
            match self.supervisor.restore_stopped_native_reference(original) {
                Ok(reference) => Ok(Some(reference)),
                Err(ServiceFailure::Pending) => Ok(None),
                Err(failure) => Err(classify_service(failure)),
            }
        })
    }
    fn resolve_stopped<'a>(
        &'a self,
        key: AccountBuildKey,
        pin: ServerPin,
        request: NativeRequestId,
        artifact: Option<&'a VerifiedAccountServerArtifact>,
    ) -> ManagementFuture<'a, ServiceProgress> {
        Box::pin(async move {
            self.revalidate(key, pin).await?;
            let pending = std::mem::take(&mut *self.stopped_completions.lock().await);
            for worker in pending {
                worker.await.unwrap().map_err(classify_service)?;
            }
            let original = self
                .supervisor
                .restore_stopped_native(request)
                .map_err(classify_service)?;
            if let Some(artifact) = artifact {
                if !matches!(original.action, StoppedNativeAction::Stage { artifact_sha256 } if artifact_sha256 == artifact.artifact_sha256())
                {
                    return Err(ManagementFailure::StaleAssessment);
                }
                return self
                    .supervisor
                    .execute_stopped_native(original, Some(artifact))
                    .await
                    .map_err(classify_service);
            }
            self.supervisor
                .resolve_stopped_native_exact(original)
                .await
                .map_err(classify_service)
        })
    }
    fn acknowledge_stopped(
        &self,
        key: AccountBuildKey,
        pin: ServerPin,
        progress: ServiceProgress,
    ) -> ManagementFuture<'_, ServiceProgress> {
        Box::pin(async move {
            self.revalidate(key, pin).await?;
            self.supervisor
                .acknowledge_stopped_native_exact(&progress)
                .map_err(classify_service)?;
            if self.lose_ack.swap(false, Ordering::SeqCst) {
                return Err(ManagementFailure::IoUnavailable);
            }
            Ok(progress)
        })
    }
}

struct Auth {
    principal: Principal,
    key: [u8; 32],
}
impl PairingLookup for Auth {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        if (server, pairing, client)
            != (
                self.principal.server,
                self.principal.pairing,
                self.principal.client,
            )
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(PairingRecord {
            principal: self.principal,
            device_public_key: self.key,
            active: true,
        })
    }
}
struct Writer(Mutex<Option<mpsc::Sender<Vec<u8>>>>);
impl MessageSender for Writer {
    fn send<'a>(&'a self, bytes: &'a [u8]) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            let sender = self
                .0
                .lock()
                .unwrap()
                .as_ref()
                .cloned()
                .ok_or(ClientFailure::new(ClientFailureClass::Unavailable))?;
            sender
                .send(bytes.to_vec())
                .await
                .map_err(|_| ClientFailure::new(ClientFailureClass::Unavailable))
        })
    }
    fn close(&self) -> ClientFuture<'_, ()> {
        self.0.lock().unwrap().take();
        Box::pin(async { Ok(()) })
    }
    fn abort(&self) {
        self.0.lock().unwrap().take();
    }
}
struct Reader(mpsc::Receiver<Vec<u8>>);
impl MessageReceiver for Reader {
    fn receive(&mut self) -> ClientFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.0.recv().await) })
    }
}
struct Peer {
    binding: PairedServerBinding,
    signer: Arc<dyn SigningKey>,
    client_signer: Arc<TestKey>,
    custody: Arc<RuntimeCustody>,
    principal: PairingPrincipal,
    supervisor: Arc<ServiceSupervisor>,
    processes: Arc<Processes>,
    capabilities: Vec<u32>,
    withdraw_native: AtomicBool,
    major: u32,
    opens: AtomicUsize,
    handshakes: AtomicUsize,
    publications: AtomicUsize,
    assess_calls: AtomicUsize,
    admit_calls: AtomicUsize,
    lose_assessment: AtomicBool,
    lose_admit: AtomicBool,
    lose_before_admit: AtomicBool,
}
impl Peer {
    fn advertised_capabilities(&self) -> Vec<u32> {
        let withdraw = self.withdraw_native.load(Ordering::SeqCst);
        self.capabilities
            .iter()
            .copied()
            .filter(|capability| !withdraw || *capability != CAPABILITY_REMOTE_NATIVE_LIFECYCLE)
            .collect()
    }
}
struct Connector(Arc<Peer>);
impl RouteConnector for Connector {
    fn open<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, ConnectedRoute> {
        Box::pin(async move {
            assert_eq!(target, self.0.binding.target());
            assert_eq!(purpose, ConnectionPurpose::Ordinary);
            let connection = spaceterm_core::authority::ConnectionGeneration::new(
                self.0.opens.fetch_add(1, Ordering::SeqCst) as u64 + 1,
            );
            let (to_server, input) = mpsc::channel(16);
            let (output, from_server) = mpsc::channel(16);
            let peer = self.0.clone();
            tokio::spawn(async move { serve(peer, connection, input, output).await });
            Ok(ConnectedRoute {
                sender: Arc::new(Writer(Mutex::new(Some(to_server)))),
                receiver: Box::new(Reader(from_server)),
                peer: PeerFacts {
                    route: RouteKind::SecureWebSocket,
                    local_account_verified: false,
                    pinned_server_key: Some(target.pin().public_key),
                },
            })
        })
    }
}
struct ClientAuth(Arc<TestKey>);
impl CredentialSource for ClientAuth {
    fn signing_access<'a>(
        &'a self,
        _: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, SigningAccess> {
        Box::pin(async move {
            assert_eq!(purpose, ConnectionPurpose::Ordinary);
            Ok(SigningAccess(self.0.clone()))
        })
    }
}
struct Decoder;
impl OrganizationDecoder for Decoder {
    fn decode(&self, payload: Payload) -> Result<OrganizationPublication, ClientFailure> {
        let value = match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::OrganizationSnapshot(value)),
            }) => organization::v1::decode_snapshot_part(&value),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationSnapshot(value)),
            }) => organization::v2::decode_snapshot_part(&value),
            _ => return Err(ClientFailure::new(ClientFailureClass::InvalidPeer)),
        }
        .map_err(ClientFailure::from)?;
        let part = value.part;
        Ok(OrganizationPublication::Snapshot(Box::new(
            spaceterm_client::replica::SnapshotPart {
                stamp: spaceterm_client::replica::PublicationStamp {
                    request: part.request,
                    request_generation: part.generation,
                    transfer: part.transfer,
                    index: part.index,
                    total: part.total,
                    cursor: spaceterm_client::replica::OrganizationCursor {
                        server: value.state.server,
                        epoch: part.epoch,
                        revision: part.revision,
                    },
                },
                state: value.state,
                runtime: value.runtime,
            },
        )))
    }
}

fn local(number: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&number.to_be_bytes());
    bytes
}
async fn serve(
    peer: Arc<Peer>,
    connection: spaceterm_core::authority::ConnectionGeneration,
    mut input: mpsc::Receiver<Vec<u8>>,
    output: mpsc::Sender<Vec<u8>>,
) {
    let target = peer.binding.target();
    let principal = Principal {
        server: target.server().bytes(),
        device: target.device().bytes(),
        pairing: local(target.pairing().local()),
        client: local(target.client().local()),
    };
    let auth = Auth {
        principal,
        key: peer.client_signer.public_key(),
    };
    let Some(hello) = input.recv().await else {
        return;
    };
    let (handshake, reply) = ServerHandshake::accept(
        target.server().bytes(),
        17,
        HandshakeConfig {
            majors: vec![peer.major],
            capabilities: peer.advertised_capabilities(),
            ..HandshakeConfig::default()
        },
        peer.signer.clone(),
        &auth,
        &hello,
    )
    .unwrap();
    if output.send(reply).await.is_err() {
        return;
    }
    let Some(proof) = input.recv().await else {
        return;
    };
    let (Established::Ordinary(mut codec), ready) =
        handshake.receive_device(&auth, &proof).unwrap()
    else {
        panic!("ordinary worker required");
    };
    peer.handshakes.fetch_add(1, Ordering::SeqCst);
    if output.send(ready).await.is_err() {
        return;
    }
    while let Some(bytes) = input.recv().await {
        let payload = codec.decode(&bytes).unwrap();
        let query = match &payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::OrganizationQuery(value)),
            }) => Some((value.request, value.request_generation)),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::OrganizationQuery(value)),
            }) => Some((value.request, value.request_generation)),
            _ => None,
        };
        if let Some((request, generation)) = query {
            let snapshot = snapshot(target.server(), 17, 1);
            let part = PublicationPart {
                request,
                generation,
                transfer: 1,
                index: 0,
                total: 1,
                epoch: ServerEpoch::new(17),
                base: spaceterm_core::authority::OrganizationRevision::new(0),
                revision: snapshot.state().revision,
            };
            let response = if peer.major == 1 {
                organization::v1::snapshot_payload(&snapshot, 0..0, part)
            } else {
                organization::v2::snapshot_payload(&snapshot, 0..0, part)
            }
            .unwrap();
            if output.send(codec.encode(&response).unwrap()).await.is_err() {
                return;
            }
            peer.publications.fetch_add(1, Ordering::SeqCst);
            continue;
        }
        // Snapshot installation is not worker readiness. Complete the exact
        // authenticated reconciliation before accepting native runtime calls.
        macro_rules! acknowledge_reconciliation {
            ($query:expr, $major:ident, $wire:ident) => {{
                let query = $query;
                assert_ne!(query.request, 0);
                assert_eq!(query.request_generation, query.request);
                assert_eq!(
                    query.server.as_ref().unwrap().value,
                    target.server().bytes()
                );
                assert_eq!(query.epoch, 17);
                assert_eq!(query.client.as_ref().unwrap().server, query.server);
                assert_eq!(
                    query.client.as_ref().unwrap().local_id,
                    target.client().local()
                );
                assert_eq!(query.organization_revision, 1);
                assert_eq!(query.connection_generation, 0);
                assert!(!query.acknowledged);
                assert!(query.grid_limits.is_none());
                let mut reply = query.clone();
                reply.acknowledged = true;
                reply.connection_generation = connection.get();
                Some(Payload::$major($wire::Message {
                    kind: Some($wire::message::Kind::Reconciliation(reply)),
                }))
            }};
        }
        let reconciliation = match &payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::Reconciliation(query)),
            }) => acknowledge_reconciliation!(query, Major1, v1),
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::Reconciliation(query)),
            }) => acknowledge_reconciliation!(query, Major2, v2),
            _ => None,
        };
        if let Some(response) = reconciliation {
            if output.send(codec.encode(&response).unwrap()).await.is_err() {
                return;
            }
            continue;
        }
        let message = match payload {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeRequest(message)),
            })
            | Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeRequest(message)),
            }) => message,
            _ => continue,
        };
        let call: AccountRuntimeCall = serde_json::from_slice(&message.body).unwrap();
        let authority = peer
            .custody
            .native_lifecycle_authority(peer.principal, FieldVersion::new(1))
            .unwrap();
        let (reply, lost) = match call {
            AccountRuntimeCall::AssessNativeLifecycle(capture) => {
                peer.assess_calls.fetch_add(1, Ordering::SeqCst);
                let reply = peer
                    .supervisor
                    .assess_runtime_lifecycle(capture, &authority, peer.processes.as_ref())
                    .await
                    .map(AccountRuntimeReply::NativeLifecycleAssessment)
                    .unwrap_or_else(AccountRuntimeReply::Failed);
                (reply, peer.lose_assessment.swap(false, Ordering::SeqCst))
            }
            AccountRuntimeCall::AdmitNativeLifecycle(reference) => {
                peer.admit_calls.fetch_add(1, Ordering::SeqCst);
                if peer.lose_before_admit.swap(false, Ordering::SeqCst) {
                    return;
                }
                let reply = peer
                    .supervisor
                    .admit_runtime_lifecycle(reference, &authority, peer.processes.as_ref())
                    .await
                    .map(AccountRuntimeReply::NativeLifecycleAccepted)
                    .unwrap_or_else(AccountRuntimeReply::Failed);
                (reply, peer.lose_admit.swap(false, Ordering::SeqCst))
            }
            AccountRuntimeCall::AssessNativeStage(capture) => {
                peer.assess_calls.fetch_add(1, Ordering::SeqCst);
                let reply = peer
                    .supervisor
                    .assess_runtime_stage(capture, &authority, connection)
                    .map(AccountRuntimeReply::NativeStageAssessment)
                    .unwrap_or_else(AccountRuntimeReply::Failed);
                (reply, peer.lose_assessment.swap(false, Ordering::SeqCst))
            }
            AccountRuntimeCall::AdmitNativeStage(reference) => {
                peer.admit_calls.fetch_add(1, Ordering::SeqCst);
                let reply = peer
                    .supervisor
                    .prepare_runtime_stage_admission(reference, authority, connection)
                    .and_then(|admission| {
                        admission.publish(
                            peer.principal,
                            FieldVersion::new(1),
                            connection,
                            ServerEpoch::new(17),
                        )
                    })
                    .map(AccountRuntimeReply::NativeStageAccepted)
                    .unwrap_or_else(AccountRuntimeReply::Failed);
                (reply, peer.lose_admit.swap(false, Ordering::SeqCst))
            }
            _ => panic!("adapter may use only fixed native lifecycle runtime verbs"),
        };
        if lost {
            return;
        }
        let body = serde_json::to_vec(&reply).unwrap();
        let message = common::AccountRuntimeMessage {
            request: message.request,
            request_generation: message.request_generation,
            body,
        };
        let payload = if peer.major == 1 {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeReply(message)),
            })
        } else {
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeReply(message)),
            })
        };
        if output.send(codec.encode(&payload).unwrap()).await.is_err() {
            return;
        }
    }
}

struct StoppedPipe {
    sender: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    receiver: AsyncMutex<mpsc::Receiver<Vec<u8>>>,
}
impl MessageTransport for StoppedPipe {
    fn peer_facts(&self) -> spaceterm_host::transport::PeerFacts {
        spaceterm_host::transport::PeerFacts {
            route: spaceterm_host::transport::TransportRoute::PrivateIpc,
            local_uid: None,
            local_pid: None,
            pinned_server_key: None,
        }
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> spaceterm_host::transport::TransportFuture<'a, ()> {
        Box::pin(async move {
            assert!(
                !bytes.is_empty() && bytes.len() <= spaceterm_host::transport::MAX_ENVELOPE_BYTES
            );
            let sender = self
                .sender
                .lock()
                .unwrap()
                .as_ref()
                .cloned()
                .ok_or(spaceterm_host::transport::TransportError::Closed)?;
            sender
                .send(bytes.to_vec())
                .await
                .map_err(|_| spaceterm_host::transport::TransportError::Closed)
        })
    }
    fn recv(&self) -> spaceterm_host::transport::TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.receiver.lock().await.recv().await) })
    }
    fn close(&self) -> spaceterm_host::transport::TransportFuture<'_, ()> {
        self.sender.lock().unwrap().take();
        Box::pin(async { Ok(()) })
    }
}

// The external authentication/runtime test port keeps the genuine native
// stopped lease, pinned protocol handshake, signed assessment and native journal.
#[expect(
    clippy::too_many_arguments,
    reason = "the injected authentication port owns one exact original channel"
)]
async fn stopped_peer(
    peer: Arc<Peer>,
    principal: Principal,
    original: StoppedNativeRequest,
    admission: spaceterm_host::account_service::StoppedNativeAdmission,
    mut artifact: Option<VerifiedAccountServerArtifact>,
    mut input: mpsc::Receiver<Vec<u8>>,
    output: mpsc::Sender<Vec<u8>>,
    completions: StoppedWorkers,
) {
    let auth = Auth {
        principal,
        key: peer.client_signer.public_key(),
    };
    let Some(hello) = input.recv().await else {
        return;
    };
    admission.revalidate().await.unwrap();
    let (handshake, reply) = ServerHandshake::accept(
        principal.server,
        17,
        HandshakeConfig {
            majors: vec![peer.major],
            capabilities: peer.advertised_capabilities(),
            ..HandshakeConfig::default()
        },
        peer.signer.clone(),
        &auth,
        &hello,
    )
    .unwrap();
    if output.send(reply).await.is_err() {
        return;
    }
    let Some(proof) = input.recv().await else {
        return;
    };
    let (Established::Ordinary(mut codec), ready) =
        handshake.receive_device(&auth, &proof).unwrap()
    else {
        panic!("ordinary stopped authentication required");
    };
    if output.send(ready).await.is_err() {
        return;
    }
    let mut admission = Some(admission);
    while let Some(bytes) = input.recv().await {
        let message = match codec.decode(&bytes).unwrap() {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeRequest(message)),
            })
            | Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeRequest(message)),
            }) => message,
            _ => panic!("closed stopped comparison grammar required"),
        };
        let call: StoppedNativeCall = serde_json::from_slice(&message.body).unwrap();
        let reply = match call {
            StoppedNativeCall::Assess(capture) => {
                assert_eq!(capture, original);
                admission
                    .as_ref()
                    .unwrap()
                    .assess(
                        peer.principal,
                        FieldVersion::new(1),
                        ServerEpoch::new(17),
                        peer.signer.clone(),
                        artifact.as_ref(),
                    )
                    .await
                    .map(StoppedNativeReply::Assessment)
                    .unwrap_or_else(StoppedNativeReply::Failed)
            }
            StoppedNativeCall::Admit(reference) => {
                assert_eq!(reference.original, original);
                let accepted = admission
                    .as_ref()
                    .unwrap()
                    .accept(
                        &reference,
                        peer.principal,
                        FieldVersion::new(1),
                        ServerEpoch::new(17),
                    )
                    .await;
                match accepted {
                    Ok(progress) => {
                        drop(admission.take());
                        let supervisor = peer.supervisor.clone();
                        let artifact = artifact.take();
                        let worker = tokio::spawn(async move {
                            supervisor
                                .execute_stopped_native(original, artifact.as_ref())
                                .await
                        });
                        completions.lock().await.push(worker);
                        if peer.lose_admit.swap(false, Ordering::SeqCst) {
                            return;
                        }
                        StoppedNativeReply::Accepted(progress)
                    }
                    Err(failure) => StoppedNativeReply::Failed(failure),
                }
            }
        };
        let message = common::AccountRuntimeMessage {
            request: message.request,
            request_generation: message.request_generation,
            body: serde_json::to_vec(&reply).unwrap(),
        };
        let payload = if peer.major == 1 {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeReply(message)),
            })
        } else {
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeReply(message)),
            })
        };
        if output.send(codec.encode(&payload).unwrap()).await.is_err() {
            return;
        }
        if admission.is_none() {
            return;
        }
    }
}

struct Fixture {
    _guest_root: tempfile::TempDir,
    account: Arc<AccountServiceFixture>,
    _mac_root: tempfile::TempDir,
    key: AccountBuildKey,
    journal: Arc<JournalController>,
    pairing: Arc<NativePairingRuntime>,
    binding: PairedServerBinding,
    ssh: Arc<Ssh>,
    peer: Arc<Peer>,
    manager: Arc<Manager>,
    clock: Arc<Clock>,
    server: ServerHandle,
    worker: ClientRuntime,
}
async fn wait_for_worker(server: &ServerHandle, ready: bool, peer: &Peer) {
    let mut state = server.state();
    let mut replica = server.replica();
    let initial_failures = match *state.borrow() {
        spaceterm_client::api::ConnectionState::Offline { retry, .. } => retry.failures,
        _ => 0,
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let current = state.borrow().clone();
            if matches!(current, spaceterm_client::api::ConnectionState::Connected { .. })
                && replica.borrow().is_ready()
            {
                assert!(ready, "incompatible peer must not become ready");
                return;
            }
            match current {
                spaceterm_client::api::ConnectionState::Incompatible { .. } => {
                    assert!(!ready, "compatible fixture reached incompatible state");
                    return;
                }
                spaceterm_client::api::ConnectionState::Offline { failure, retry }
                    if retry.failures > initial_failures =>
                {
                    panic!("authenticated worker failed: {:?}", failure.class());
                }
                _ => {}
            }
            tokio::select! {
                changed = state.changed() => changed.unwrap(),
                changed = replica.changed() => changed.unwrap(),
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        let (phase, failure) = match *state.borrow() {
            spaceterm_client::api::ConnectionState::Connecting { phase: spaceterm_client::api::ConnectingPhase::Dialing } => ("dialing", None),
            spaceterm_client::api::ConnectionState::Connecting { phase: spaceterm_client::api::ConnectingPhase::Authenticating } => ("authenticating", None),
            spaceterm_client::api::ConnectionState::Connecting { .. } => ("reconciling", None),
            spaceterm_client::api::ConnectionState::Connected { .. } => ("connected", None),
            spaceterm_client::api::ConnectionState::Offline { failure, .. } => ("offline", Some(failure.class())),
            spaceterm_client::api::ConnectionState::Incompatible { .. } => ("incompatible", None),
            spaceterm_client::api::ConnectionState::Stopped => ("stopped", None),
        };
        panic!("worker deadline: phase={phase}, failure={failure:?}, replica_ready={}, opens={}, handshakes={}, publications={}", replica.borrow().is_ready(), peer.opens.load(Ordering::SeqCst), peer.handshakes.load(Ordering::SeqCst), peer.publications.load(Ordering::SeqCst));
    });
}

impl Fixture {
    async fn new(major: u32, native_capability: bool) -> Self {
        Self::with_desired(major, native_capability, DesiredServerState::Running).await
    }
    async fn with_desired(
        major: u32,
        native_capability: bool,
        desired: DesiredServerState,
    ) -> Self {
        Self::with_capabilities(major, native_capability, desired, true).await
    }
    async fn with_capabilities(
        major: u32,
        native_capability: bool,
        desired: DesiredServerState,
        baseline: bool,
    ) -> Self {
        let guest_root = tempfile::tempdir().unwrap();
        let guest = Arc::new(
            AccountServiceFixture::new(guest_root.path(), BuildIdentity::Development).unwrap(),
        );
        let (trust, archive) =
            signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
        let approved = InstallationStatus {
            installation: spaceterm_host::startup::InstallationId(std::num::NonZeroU64::MIN),
            version: archive.manifest().version.clone(),
            artifact_sha256: archive.artifact_sha256(),
        };
        let registration = guest
            .initialize_with_installation(desired, Some(approved))
            .unwrap();
        let installed =
            spaceterm_host::server_installation::test_support::store(&registration, trust.clone())
                .unwrap();
        let original = installed.stage(&archive).unwrap();
        installed
            .activate(original, installed.snapshot().unwrap().generation, 1)
            .unwrap();
        drop(installed);
        drop(archive);
        // This is an isolated test-created guest identity, never the user's key store.
        let signer = ServerKeyStore::open(&registration)
            .unwrap()
            .load_identity(registration.server_pin())
            .unwrap();
        let running = Arc::new(AtomicBool::new(desired == DesiredServerState::Running));
        let manager = Arc::new(Manager {
            running: running.clone(),
            effects: AtomicUsize::new(0),
        });
        let processes = Arc::new(Processes { running });
        let clock = Arc::new(Clock(AtomicU64::new(100)));
        let supervisor = Arc::new(guest.with_installation_trust(
            guest.with_providers(manager.clone(), processes.clone(), clock.clone(), None),
            trust,
        ));
        let custody = Arc::new(
            RuntimeCustody::begin(&registration, ServerEpoch::new(17), signer.clone()).unwrap(),
        );
        if desired == DesiredServerState::Stopped {
            custody.completed_instance(&registration).unwrap();
        }
        let id = ServerId::new(registration.server_pin().identity);
        let client_signer = Arc::new(TestKey::new(99));
        let binding = PairedServerBinding::new(
            ConnectionTarget::new(
                registration.server_pin(),
                DeviceId::new([99; 16]),
                PairingId::new(id, 1).unwrap(),
                ClientId::new(id, 1).unwrap(),
                RouteDescription::new(RouteParts::Wss {
                    address: "192.0.2.10:443".parse().unwrap(),
                    server_name: "original.example.test".into(),
                })
                .unwrap(),
                CredentialReference::new([9; 16]),
                LocalPairingGeneration::new(1).unwrap(),
            )
            .unwrap(),
        );
        let streams = PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: binding.target().client(),
                pairing: binding.target().pairing(),
            },
            ordinary: OperationStreamId::new(
                OperationStreamKey::new(id, 3).unwrap(),
                binding.target().pairing(),
                binding.target().client(),
            )
            .unwrap(),
            self_revocation: OperationStreamId::new(
                OperationStreamKey::new(id, 4).unwrap(),
                binding.target().pairing(),
                binding.target().client(),
            )
            .unwrap(),
        })
        .unwrap();
        let receipt = ReceiptCodec::new(binding.target().pin())
            .sign(
                PairingReceiptBody::ClientBinding {
                    target: PairingTarget::new(PairingTargetParts {
                        server: id,
                        device: binding.target().device(),
                        pairing: binding.target().pairing(),
                    })
                    .unwrap(),
                    keys: PairingKeys::new(PairingKeysParts {
                        device_identity_public_key: TestKey::new(7).public_key(),
                        pairing_authentication_public_key: client_signer.public_key(),
                    })
                    .unwrap(),
                    client: streams.clone(),
                    operation: OperationBinding {
                        id: OperationId::new(
                            OperationStreamId::new(
                                OperationStreamKey::new(id, 5).unwrap(),
                                binding.target().pairing(),
                                binding.target().client(),
                            )
                            .unwrap(),
                            OperationSequence::new(1).unwrap(),
                        ),
                        digest: RequestDigest::from_bytes([1; 32]),
                    },
                    revision: StoreRevision::new(1),
                },
                signer.as_ref(),
            )
            .unwrap();
        let authorization = PairingAuthorization::new(PairingAuthorizationParts {
            record: spaceterm_core::pairing::PairingRecord {
                id: binding.target().pairing(),
                device: binding.target().device(),
                state: PairingState::Authorized,
            },
            authorization_version: FieldVersion::new(1),
            keys: PairingKeys::new(PairingKeysParts {
                device_identity_public_key: TestKey::new(7).public_key(),
                pairing_authentication_public_key: client_signer.public_key(),
            })
            .unwrap(),
            clients: vec![streams.clone()],
            revocation_receipt: None,
        })
        .unwrap();
        let frozen = spaceterm_core::pairing::policy::PairingSnapshot::new(
            id,
            ServerEpoch::new(17),
            StoreRevision::new(1),
            IdentityHighWater {
                pairing: 1,
                client: 1,
                operation_stream: 5,
                ..Default::default()
            },
            PairingRecovery::new(PairingRecoveryParts {
                authorizations: vec![authorization],
                enrollments: vec![],
            })
            .unwrap(),
        )
        .unwrap();
        let principal = spaceterm_core::pairing::policy::PairingPolicy::new(Default::default())
            .unwrap()
            .authorize_client(
                &frozen,
                binding.target().pairing(),
                binding.target().client(),
            )
            .unwrap();
        let saved = SavedBinding::verified(binding.clone(), streams, receipt, &Verifier).unwrap();
        let mac_root = tempfile::Builder::new()
            .prefix("remote-host-services-")
            .tempdir_in(std::fs::canonicalize("/var/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(mac_root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let native = NativeClientJournal::create(
            mac_root.path().to_owned(),
            Arc::new(CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let journal = Arc::new(
            JournalController::recover(
                Arc::new(native),
                Arc::new(CanonicalCodec),
                Arc::new(Verifier),
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        journal
            .transact(vec![
                ClientJournalChange::ReserveCredential(JournalCredentialSlot {
                    namespace: "dev.spaceterm.remote-host-services-test".into(),
                    server: Some(id),
                    reference: binding.target().credential(),
                    public_key: None,
                    route_reference: None,
                    route: None,
                    ssh_capture: None,
                    generation: binding.target().incarnation(),
                }),
                ClientJournalChange::RecordCredentialPublicKey {
                    reference: binding.target().credential(),
                    public_key: client_signer.public_key(),
                },
                ClientJournalChange::BindCredentialRoute {
                    reference: binding.target().credential(),
                    route_reference: RevocationRouteReference::new([9; 16]),
                    route: binding.target().route().clone(),
                },
                ClientJournalChange::SaveBinding(Box::new(saved)),
            ])
            .await
            .unwrap();
        let pairing = Arc::new(NativePairingRuntime::with_adapters(
            journal.clone(),
            Arc::new(Credentials(client_signer.clone())),
            Arc::new(NoEnrollment),
            Arc::new(NoEnrollment),
            Arc::new(NoLocalEffects),
            HandshakeConfig::default(),
        ));
        let mut capabilities = vec![
            CAPABILITY_NORMALIZED_CREATION_TOPOLOGY,
            CAPABILITY_ACCOUNT_RUNTIME,
        ];
        if baseline {
            capabilities.push(CAPABILITY_CANONICAL_DOMAIN);
        }
        if native_capability {
            capabilities.push(CAPABILITY_REMOTE_NATIVE_LIFECYCLE);
        }
        capabilities.sort_unstable();
        let peer = Arc::new(Peer {
            binding: binding.clone(),
            signer,
            client_signer: client_signer.clone(),
            custody,
            principal,
            supervisor: supervisor.clone(),
            processes,
            capabilities: capabilities.clone(),
            withdraw_native: AtomicBool::new(false),
            major,
            opens: AtomicUsize::new(0),
            handshakes: AtomicUsize::new(0),
            publications: AtomicUsize::new(0),
            assess_calls: AtomicUsize::new(0),
            admit_calls: AtomicUsize::new(0),
            lose_assessment: AtomicBool::new(false),
            lose_admit: AtomicBool::new(false),
            lose_before_admit: AtomicBool::new(false),
        });
        let services = ClientServices {
            routes: Arc::new(Connector(peer.clone())),
            journal: journal.clone(),
            credentials: Arc::new(ClientAuth(client_signer)),
            organization: Arc::new(Decoder),
            delegate: Arc::new(connection_support::Delegate::default()),
            discovery: None,
            limits: ClientLimits::default(),
            additional_capabilities: capabilities
                .into_iter()
                .filter(|capability| {
                    matches!(
                        *capability,
                        CAPABILITY_ACCOUNT_RUNTIME | CAPABILITY_REMOTE_NATIVE_LIFECYCLE
                    )
                })
                .collect(),
        };
        let (registry, worker) = ClientRegistry::recover(services).await.unwrap();
        let server = registry.server(id).unwrap();
        if desired == DesiredServerState::Running || !baseline {
            server.connect().await.unwrap();
            wait_for_worker(&server, baseline, &peer).await;
        }
        let key = registration.key();
        let ssh = Arc::new(Ssh {
            key,
            pin: binding.target().pin(),
            supervisor,
            calls: Mutex::new(vec![]),
            lose_capture: AtomicBool::new(false),
            lose_ack: AtomicBool::new(false),
            lose_stage_resolve: AtomicBool::new(false),
            wrong_owner: AtomicBool::new(false),
            peer: peer.clone(),
            stopped_opens: Arc::new(AtomicUsize::new(0)),
            capture_race: Mutex::new(None),
            revalidation_hook: Mutex::new(None),
            stopped_completions: Arc::new(AsyncMutex::new(vec![])),
        });
        Self {
            _guest_root: guest_root,
            account: guest,
            _mac_root: mac_root,
            key,
            journal,
            pairing,
            binding,
            ssh,
            peer,
            manager,
            clock,
            server,
            worker,
        }
    }
    async fn provider(&self) -> RemoteHostServices {
        RemoteHostServices::new(
            self.pairing.clone(),
            self.binding.target().local_pairing().unwrap(),
            self.binding.target().client(),
            self.binding.clone(),
            self.server.queries(self.journal.clone()).unwrap(),
            self.key,
            self.ssh.clone(),
        )
        .await
        .unwrap()
    }
    fn records(&self) -> Vec<spaceterm_client::journal::RemoteNativeRequest> {
        self.journal
            .recover_remote_native_requests(
                &self.binding.target().local_pairing().unwrap(),
                self.binding.target().client(),
            )
            .unwrap()
    }
}

async fn seal_pairing(fixture: &Fixture) {
    use spaceterm_client::journal::{JournalRevocation, RevocationPhase, SavedTarget};
    use spaceterm_core::operations::Command;
    let binding = &fixture.binding;
    let state = fixture.journal.snapshot().unwrap();
    let saved = state
        .bindings()
        .iter()
        .find(|saved| saved.binding().unwrap() == *binding)
        .unwrap();
    let original = PendingSelfRevocation::new(PendingParts {
        original_server: binding.target().server(),
        original_pairing: binding.target().pairing(),
        device: binding.target().device(),
        client: binding.target().client(),
        operation: OperationId::new(
            saved.streams().parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        ),
        route: RevocationRouteReference::new([9; 16]),
        credential: binding.target().credential(),
        notice_dismissed: false,
    })
    .unwrap();
    let request = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(
        original.operation(),
        Command::Pairing(PairingCommand::SelfRevoke {
            target: PairingTarget::new(PairingTargetParts {
                server: original.server(),
                pairing: original.pairing(),
                device: original.device(),
            })
            .unwrap(),
        }),
        &CanonicalCodec,
    )
    .unwrap();
    let task = PendingRevocationTask::new(PendingRevocationTaskParts {
        original,
        local_pairing: binding.target().local_pairing().unwrap(),
        pinned_server_public_key: binding.target().pin().public_key,
        request_digest: request.binding().digest,
    })
    .unwrap();
    fixture
        .journal
        .transact(vec![ClientJournalChange::BeginRevocation(Box::new(
            JournalRevocation {
                task,
                target: SavedTarget::from_target(binding.target()),
                phase: RevocationPhase::AwaitingConfirmation,
                notice_dismissed: false,
                attempts: 0,
            },
        ))])
        .await
        .unwrap();
    assert!(!fixture.journal.ordinary_use_allowed(binding).unwrap());
}

#[tokio::test]
async fn retained_account_lookup_is_comparison_only_and_requires_exact_submitted_original() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let pairing = fixture.binding.target().local_pairing().unwrap();
    let client = fixture.binding.target().client();
    let pin = fixture.binding.target().pin();
    let request = provider.reserve_request().await.unwrap();
    assert_eq!(
        RemoteHostServices::retained_account_key(&fixture.journal, &pairing, client, pin).unwrap(),
        None
    );
    let assessment = provider.assess(request, ServiceAction::Stop).await.unwrap();
    assert_eq!(
        RemoteHostServices::retained_account_key(&fixture.journal, &pairing, client, pin).unwrap(),
        None
    );
    fixture.peer.lose_before_admit.store(true, Ordering::SeqCst);
    assert!(provider.submit(assessment).await.is_err());
    assert_eq!(
        RemoteHostServices::retained_account_key(&fixture.journal, &pairing, client, pin).unwrap(),
        Some(fixture.key)
    );
    let other = ClientId::new(client.server(), client.local() + 1).unwrap();
    assert_eq!(
        RemoteHostServices::retained_account_key(&fixture.journal, &pairing, other, pin).unwrap(),
        None
    );
    let mut wrong_pin = pin;
    wrong_pin.public_key[0] ^= 1;
    assert_eq!(
        RemoteHostServices::retained_account_key(&fixture.journal, &pairing, client, wrong_pin)
            .unwrap_err(),
        ManagementFailure::UnsafeNativeState
    );
    seal_pairing(&fixture).await;
    assert_eq!(
        RemoteHostServices::retained_account_key(&fixture.journal, &pairing, client, pin).unwrap(),
        Some(fixture.key)
    );
    assert_eq!(
        provider.reserve_request().await.unwrap_err(),
        ManagementFailure::Revoked
    );
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn retained_account_lookup_refuses_conflicting_keys_without_native_effects() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    provider.assess(request, ServiceAction::Stop).await.unwrap();
    let first = fixture.records()[0].clone();
    let second = fixture.ssh.supervisor.allocate_request().unwrap();
    let capture = fixture
        .ssh
        .supervisor
        .capture_runtime_request(second, ServiceAction::Stop)
        .unwrap();
    let body = serde_json::to_vec(&AccountRuntimeCall::AssessNativeLifecycle(capture)).unwrap();
    let reply = fixture
        .server
        .queries(fixture.journal.clone())
        .unwrap()
        .native_account_runtime(&body)
        .await
        .unwrap();
    let AccountRuntimeReply::NativeLifecycleAssessment(reference) =
        serde_json::from_slice(&reply).unwrap()
    else {
        panic!("genuine signed second assessment required");
    };
    let pairing = fixture.binding.target().local_pairing().unwrap();
    let client = fixture.binding.target().client();
    // Mutate comparison bytes at the public journal boundary, not native authority.
    let mut scope: serde_json::Value = serde_json::from_slice(first.original_scope()).unwrap();
    scope["request"] = serde_json::to_value(second).unwrap();
    scope["key"]["account_uid"] =
        serde_json::to_value(fixture.key.account_uid.checked_add(1).unwrap()).unwrap();
    let scope = serde_json::to_vec(&scope).unwrap();
    let mut assessment: serde_json::Value =
        serde_json::from_slice(first.assessment().unwrap()).unwrap();
    assessment["reference"] = serde_json::to_value(reference).unwrap();
    let assessment = serde_json::to_vec(&assessment).unwrap();
    fixture
        .journal
        .retain_remote_native_request(
            spaceterm_client::journal::RemoteNativeRequest::new(
                pairing.clone(),
                client,
                *second.as_bytes(),
                scope.clone(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    fixture
        .journal
        .retain_remote_native_intent(
            pairing.clone(),
            client,
            *second.as_bytes(),
            scope.clone(),
            first.intent().unwrap().to_vec(),
        )
        .await
        .unwrap();
    fixture
        .journal
        .retain_remote_native_assessment(
            pairing.clone(),
            client,
            *second.as_bytes(),
            scope.clone(),
            assessment.clone(),
        )
        .await
        .unwrap();
    fixture
        .journal
        .mark_remote_native_submitted(
            pairing.clone(),
            client,
            *second.as_bytes(),
            scope,
            Some(assessment),
        )
        .await
        .unwrap();
    fixture
        .journal
        .mark_remote_native_submitted(
            pairing.clone(),
            client,
            first.request_id(),
            first.original_scope().to_vec(),
            first.assessment().map(<[u8]>::to_vec),
        )
        .await
        .unwrap();
    assert_eq!(
        RemoteHostServices::retained_account_key(
            &fixture.journal,
            &pairing,
            client,
            fixture.binding.target().pin()
        )
        .unwrap_err(),
        ManagementFailure::UnsafeNativeState
    );
    assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_seal_refuses_new_work_but_resolves_and_acknowledges_only_retained_original() {
    let fixture = Fixture::new(2, true).await;
    let queries = fixture.server.queries(fixture.journal.clone()).unwrap();
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    let assessment = provider.assess(request, ServiceAction::Stop).await.unwrap();
    fixture.peer.lose_admit.store(true, Ordering::SeqCst);
    assert!(provider.submit(assessment.clone()).await.is_err());
    assert!(fixture.records()[0].submitted());
    seal_pairing(&fixture).await;
    let effects = fixture.manager.effects.load(Ordering::SeqCst);
    assert_eq!(
        provider.reserve_request().await.unwrap_err(),
        ManagementFailure::Revoked
    );
    assert!(provider.assess(request, ServiceAction::Stop).await.is_err());
    assert!(provider.submit(assessment).await.is_err());
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), effects);
    assert_eq!(
        queries
            .account_runtime(b"sealed ordinary call")
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::AuthorizationDenied
    );
    let recovered = RemoteHostServices::new(
        fixture.pairing.clone(),
        fixture.binding.target().local_pairing().unwrap(),
        fixture.binding.target().client(),
        fixture.binding.clone(),
        queries.clone(),
        fixture.key,
        fixture.ssh.clone(),
    )
    .await
    .unwrap();
    let originals = recovered.retained_requests().await.unwrap();
    assert_eq!(originals.len(), 1);
    assert_eq!(originals[0].request, request);
    assert_eq!(
        originals[0].state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    assert!(fixture.records().is_empty());
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peer.assess_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.ssh.stopped_opens.load(Ordering::SeqCst), 0);
    assert!(
        RemoteHostServices::new(
            fixture.pairing.clone(),
            fixture.binding.target().local_pairing().unwrap(),
            fixture.binding.target().client(),
            fixture.binding.clone(),
            queries,
            fixture.key,
            fixture.ssh.clone(),
        )
        .await
        .is_err()
    );
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_seal_cannot_import_a_missing_assessment_or_restore_foreground_authority_over_ssh() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    fixture.peer.lose_assessment.store(true, Ordering::SeqCst);
    assert!(provider.assess(request, ServiceAction::Stop).await.is_err());
    assert!(fixture.records()[0].assessment().is_none());
    seal_pairing(&fixture).await;
    assert_eq!(
        provider
            .assess(request, ServiceAction::Stop)
            .await
            .unwrap_err(),
        ManagementFailure::Revoked
    );
    assert_eq!(
        provider.resolve(request).await.unwrap_err(),
        ManagementFailure::Revoked
    );
    assert!(fixture.records()[0].assessment().is_none());
    assert!(!fixture.records()[0].submitted());
    assert_eq!(fixture.peer.assess_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn paired_live_stop_uses_one_worker_and_retires_only_after_exact_guest_ack() {
    for major in [1, 2] {
        let fixture = Fixture::new(major, true).await;
        let provider = fixture.provider().await;
        let request = provider.reserve_request().await.unwrap();
        assert_eq!(fixture.records()[0].request_id(), *request.as_bytes());
        let assessment = provider.assess(request, ServiceAction::Stop).await.unwrap();
        assert_eq!(assessment.prompt.affected_sessions, 2);
        assert_eq!(assessment.runtime_epoch, Some(ServerEpoch::new(17)));
        assert!(assessment.prompt.token.as_str().len() < 128);
        fixture.ssh.lose_ack.store(true, Ordering::SeqCst);
        assert_eq!(
            provider.submit(assessment).await.unwrap_err(),
            ManagementFailure::IoUnavailable
        );
        assert!(fixture.records()[0].result().is_some());
        assert!(fixture.records()[0].submitted());
        let progress = provider.resolve(request).await.unwrap();
        assert_eq!(
            progress.state,
            ServiceRequestState::Completed(ServiceOutcome::Stopped)
        );
        assert!(fixture.records().is_empty());
        assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.peer.assess_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 1);
        assert!(!fixture.manager.running.load(Ordering::SeqCst));
        fixture.worker.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn lost_assessment_reply_stays_pending_offline_and_reconnect_restores_exact_original() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    fixture.ssh.lose_capture.store(true, Ordering::SeqCst);
    assert_eq!(
        provider
            .assess(request, ServiceAction::Stop)
            .await
            .unwrap_err(),
        ManagementFailure::IoUnavailable
    );
    let capture = fixture
        .ssh
        .supervisor
        .restore_runtime_request(request)
        .unwrap();
    fixture.peer.lose_assessment.store(true, Ordering::SeqCst);
    assert!(provider.assess(request, ServiceAction::Stop).await.is_err());
    let original = fixture
        .ssh
        .supervisor
        .restore_runtime_lifecycle_reference(capture)
        .unwrap();
    assert_eq!(original.original, capture);
    assert_eq!(
        provider
            .assess(request, ServiceAction::Stop)
            .await
            .unwrap_err(),
        ManagementFailure::OutcomePending
    );
    assert!(fixture.records()[0].assessment().is_none());
    fixture.server.retry().await.unwrap();
    wait_for_worker(&fixture.server, true, &fixture.peer).await;
    let restored = provider.assess(request, ServiceAction::Stop).await.unwrap();
    assert_eq!(restored.runtime_epoch, Some(original.epoch));
    assert_eq!(
        fixture
            .ssh
            .supervisor
            .restore_runtime_lifecycle_reference(capture)
            .unwrap(),
        original
    );
    assert_eq!(fixture.peer.assess_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.records().len(), 1);
    assert_eq!(fixture.records()[0].request_id(), *request.as_bytes());
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn incompatible_peer_cannot_assess_or_admit_and_never_falls_back_to_start_or_enrollment() {
    let fixture = Fixture::new(1, false).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    assert_eq!(
        provider
            .assess(request, ServiceAction::Stop)
            .await
            .unwrap_err(),
        ManagementFailure::Incompatible
    );
    assert_eq!(fixture.peer.assess_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_start_refuses_known_incompatible_worker_before_stopped_capture_or_effects() {
    let fixture = Fixture::with_capabilities(2, true, DesiredServerState::Stopped, false).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    let calls = fixture.ssh.calls.lock().unwrap().len();
    assert_eq!(
        provider.start(request).await.unwrap_err(),
        ManagementFailure::Incompatible
    );
    assert_eq!(fixture.ssh.calls.lock().unwrap().len(), calls);
    assert_eq!(fixture.ssh.stopped_opens.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 1);
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_start_refuses_ready_peer_missing_native_capability_before_native_observation() {
    let fixture = Fixture::new(1, false).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    let calls = fixture.ssh.calls.lock().unwrap().len();
    assert_eq!(
        provider.start(request).await.unwrap_err(),
        ManagementFailure::Incompatible
    );
    assert_eq!(fixture.ssh.calls.lock().unwrap().len(), calls);
    assert_eq!(fixture.ssh.stopped_opens.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn capability_loss_during_ssh_revalidation_cannot_admit_on_a_replacement_shared_worker() {
    for major in [1, 2] {
        let fixture = Fixture::new(major, true).await;
        let provider = fixture.provider().await;
        let request = provider.reserve_request().await.unwrap();
        let assessment = provider.assess(request, ServiceAction::Stop).await.unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let hook_fired = fired.clone();
        let journal = fixture.journal.clone();
        let pairing = fixture.binding.target().local_pairing().unwrap();
        let client = fixture.binding.target().client();
        let server = fixture.server.clone();
        let peer = fixture.peer.clone();
        *fixture.ssh.revalidation_hook.lock().unwrap() = Some(Arc::new(move || {
            let fired = hook_fired.clone();
            let journal = journal.clone();
            let pairing = pairing.clone();
            let server = server.clone();
            let peer = peer.clone();
            Box::pin(async move {
                let submitted = journal
                    .recover_remote_native_requests(&pairing, client)
                    .unwrap()
                    .iter()
                    .any(|record| record.request_id() == *request.as_bytes() && record.submitted());
                if !submitted || fired.swap(true, Ordering::SeqCst) {
                    return Ok(());
                }
                peer.withdraw_native.store(true, Ordering::SeqCst);
                server.retry().await.unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        if peer.opens.load(Ordering::SeqCst) >= 2 && server.replica().borrow().is_ready()
                            && matches!(*server.state().borrow(), spaceterm_client::api::ConnectionState::Connected { info } if !info.capabilities.supports(CAPABILITY_REMOTE_NATIVE_LIFECYCLE)) { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.unwrap();
                Ok(())
            })
        }));
        assert_eq!(
            provider.submit(assessment).await.unwrap_err(),
            ManagementFailure::Incompatible
        );
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
        *fixture.ssh.revalidation_hook.lock().unwrap() = None;
        let progress = provider.resolve(request).await.unwrap();
        assert_eq!(
            progress.phase,
            spaceterm_host::account_service::ServicePhase::Reserved
        );
        assert_eq!(progress.state, ServiceRequestState::Pending);
        assert!(fixture.records()[0].submitted());
        assert!(fixture.records()[0].result().is_none());
        fixture.worker.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn lost_admission_reply_resolves_original_after_disconnect_and_never_replays_confirmation() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    let original = provider.assess(request, ServiceAction::Stop).await.unwrap();
    fixture.peer.lose_admit.store(true, Ordering::SeqCst);
    assert!(provider.submit(original.clone()).await.is_err());
    assert!(fixture.records()[0].submitted());
    assert_eq!(
        provider.submit(original).await.unwrap_err(),
        ManagementFailure::StaleAssessment
    );
    let recovered = fixture.provider().await;
    let progress = recovered.resolve(request).await.unwrap();
    assert_eq!(
        progress.state,
        ServiceRequestState::Completed(ServiceOutcome::Stopped)
    );
    assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 1);
    assert!(fixture.records().is_empty());
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn lost_unaccepted_admission_remains_exact_reserved_original_after_seal_without_readmission()
{
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    let assessment = provider.assess(request, ServiceAction::Stop).await.unwrap();
    let retained = fixture.records()[0].assessment().unwrap().to_vec();
    fixture.peer.lose_before_admit.store(true, Ordering::SeqCst);
    assert!(provider.submit(assessment.clone()).await.is_err());
    seal_pairing(&fixture).await;
    let progress = provider.resolve(request).await.unwrap();
    assert_eq!(
        progress.phase,
        spaceterm_host::account_service::ServicePhase::Reserved
    );
    assert_eq!(progress.state, ServiceRequestState::Pending);
    assert_eq!(fixture.records()[0].assessment(), Some(retained.as_slice()));
    assert!(fixture.records()[0].submitted());
    assert!(fixture.records()[0].result().is_none());
    assert!(provider.submit(assessment).await.is_err());
    assert_eq!(fixture.peer.assess_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn ssh_restored_assessment_from_another_client_cannot_be_imported_as_own_confirmation() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    let target = fixture.binding.target();
    let other = ClientId::new(target.server(), 2).unwrap();
    let stream = |local| {
        OperationStreamId::new(
            OperationStreamKey::new(target.server(), local).unwrap(),
            target.pairing(),
            other,
        )
        .unwrap()
    };
    let clients = PairingClientStreams::new(PairingClientStreamsParts {
        client: ClientRecord {
            id: other,
            pairing: target.pairing(),
        },
        ordinary: stream(6),
        self_revocation: stream(7),
    })
    .unwrap();
    let authorization = PairingAuthorization::new(PairingAuthorizationParts {
        record: spaceterm_core::pairing::PairingRecord {
            id: target.pairing(),
            device: target.device(),
            state: PairingState::Authorized,
        },
        authorization_version: FieldVersion::new(1),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: TestKey::new(7).public_key(),
            pairing_authentication_public_key: TestKey::new(99).public_key(),
        })
        .unwrap(),
        clients: vec![clients],
        revocation_receipt: None,
    })
    .unwrap();
    let snapshot = spaceterm_core::pairing::policy::PairingSnapshot::new(
        target.server(),
        ServerEpoch::new(17),
        StoreRevision::new(1),
        IdentityHighWater {
            pairing: 1,
            client: 2,
            operation_stream: 7,
            ..Default::default()
        },
        PairingRecovery::new(PairingRecoveryParts {
            authorizations: vec![authorization],
            enrollments: vec![],
        })
        .unwrap(),
    )
    .unwrap();
    let principal = spaceterm_core::pairing::policy::PairingPolicy::default()
        .authorize_client(&snapshot, target.pairing(), other)
        .unwrap();
    let authority = fixture
        .peer
        .custody
        .native_lifecycle_authority(principal, FieldVersion::new(1))
        .unwrap();
    let original = fixture
        .ssh
        .supervisor
        .capture_runtime_request(request, ServiceAction::Stop)
        .unwrap();
    let foreign = fixture
        .ssh
        .supervisor
        .assess_runtime_lifecycle(original, &authority, fixture.peer.processes.as_ref())
        .await
        .unwrap();
    assert_eq!(
        provider
            .assess(request, ServiceAction::Stop)
            .await
            .unwrap_err(),
        ManagementFailure::PairingRequired
    );
    assert!(fixture.records()[0].assessment().is_none());
    assert!(!fixture.records()[0].submitted());
    assert_eq!(
        fixture
            .ssh
            .supervisor
            .restore_runtime_lifecycle_reference(original)
            .unwrap(),
        foreign
    );
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn altered_foreground_prompt_cannot_admit_the_retained_original() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    let mut original = provider.assess(request, ServiceAction::Stop).await.unwrap();
    original.prompt.affected_sessions += 1;
    assert_eq!(
        provider.submit(original).await.unwrap_err(),
        ManagementFailure::StaleAssessment
    );
    assert!(!fixture.records()[0].submitted());
    assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn simultaneous_remote_providers_cannot_submit_each_others_original_client_scope() {
    let left = Fixture::new(1, true).await;
    let right = Fixture::new(2, true).await;
    let left_provider = left.provider().await;
    let right_provider = right.provider().await;
    let (left_request, right_request) = tokio::join!(
        left_provider.reserve_request(),
        right_provider.reserve_request()
    );
    let left_request = left_request.unwrap();
    let right_request = right_request.unwrap();
    let (left_assessment, right_assessment) = tokio::join!(
        left_provider.assess(left_request, ServiceAction::Stop),
        right_provider.assess(right_request, ServiceAction::Stop)
    );
    let left_assessment = left_assessment.unwrap();
    let right_assessment = right_assessment.unwrap();
    assert_eq!(left_assessment.prompt.request, left_request);
    assert_eq!(right_assessment.prompt.request, right_request);
    assert_eq!(
        left_provider.submit(right_assessment).await.unwrap_err(),
        ManagementFailure::StaleAssessment
    );
    assert_eq!(
        right_provider.submit(left_assessment).await.unwrap_err(),
        ManagementFailure::StaleAssessment
    );
    assert!(!left.records()[0].submitted());
    assert!(!right.records()[0].submitted());
    assert_eq!(left.manager.effects.load(Ordering::SeqCst), 0);
    assert_eq!(right.manager.effects.load(Ordering::SeqCst), 0);
    left.worker.shutdown().await.unwrap();
    right.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn retained_recovery_enumerates_only_canonical_originals_not_guest_request_inventory() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let foreign = fixture.ssh.supervisor.allocate_request().unwrap();
    let original = fixture
        .ssh
        .supervisor
        .capture_runtime_request(foreign, ServiceAction::Stop)
        .unwrap();
    let authority = fixture
        .peer
        .custody
        .native_lifecycle_authority(fixture.peer.principal, FieldVersion::new(1))
        .unwrap();
    let reference = fixture
        .ssh
        .supervisor
        .assess_runtime_lifecycle(original, &authority, fixture.peer.processes.as_ref())
        .await
        .unwrap();
    fixture
        .ssh
        .supervisor
        .admit_runtime_lifecycle(reference, &authority, fixture.peer.processes.as_ref())
        .await
        .unwrap();
    assert!(
        !fixture
            .ssh
            .supervisor
            .retained_service_requests()
            .unwrap()
            .is_empty()
    );
    assert!(provider.retained_requests().await.unwrap().is_empty());
    assert!(fixture.records().is_empty());
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    assert!(
        !fixture
            .ssh
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, NativeLifecycleAccountRequest::Resolve { .. }))
    );
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn different_canonical_runtime_cannot_borrow_an_authenticated_worker() {
    let fixture = Fixture::new(2, true).await;
    let other = Fixture::new(2, true).await;
    let result = RemoteHostServices::new(
        other.pairing.clone(),
        fixture.binding.target().local_pairing().unwrap(),
        fixture.binding.target().client(),
        fixture.binding.clone(),
        fixture.server.queries(fixture.journal.clone()).unwrap(),
        fixture.key,
        fixture.ssh.clone(),
    )
    .await;
    assert!(matches!(result, Err(ManagementFailure::UnsafeNativeState)));
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 1);
    assert!(fixture.ssh.calls.lock().unwrap().is_empty());
    fixture.worker.shutdown().await.unwrap();
    other.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn stopped_start_rejects_a_changed_registration_after_observation_before_signing_or_effects()
{
    let fixture = Fixture::with_desired(2, true, DesiredServerState::Stopped).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    let before = provider.inspect().await.unwrap();
    let account = fixture.account.clone();
    *fixture.ssh.capture_race.lock().unwrap() = Some(Box::new(move || {
        account.stage_installation(serde_json::from_value(serde_json::json!({ "installation": 2, "version": "1.2.4", "artifact_sha256": vec![4; 32] })).unwrap()).unwrap();
    }));
    assert_eq!(
        provider.start(request).await.unwrap_err(),
        ManagementFailure::StaleAssessment
    );
    assert_eq!(fixture.ssh.stopped_opens.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    let current = provider.inspect().await.unwrap();
    assert_eq!(current.status.desired, Some(DesiredServerState::Stopped));
    assert_eq!(current.status.approved, before.status.approved);
    assert!(fixture.records()[0].assessment().is_none());
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn stopped_original_confirmation_keeps_its_expiry_and_cannot_submit_after_expiration() {
    let fixture = Fixture::with_desired(2, true, DesiredServerState::Stopped).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    let original = provider
        .assess(request, ServiceAction::Uninstall)
        .await
        .unwrap();
    let capture = fixture
        .ssh
        .supervisor
        .restore_stopped_native(request)
        .unwrap();
    let reference = fixture
        .ssh
        .supervisor
        .restore_stopped_native_reference(capture)
        .unwrap();
    fixture
        .clock
        .0
        .store(reference.expires_at.millis, Ordering::SeqCst);
    let restored = provider
        .restore_assessment(ServiceAction::Uninstall, original.prompt.token.clone())
        .await
        .unwrap();
    assert_eq!(restored.prompt.token, original.prompt.token);
    assert_eq!(
        fixture
            .ssh
            .supervisor
            .restore_stopped_native_reference(capture)
            .unwrap(),
        reference
    );
    assert_eq!(
        provider.submit(restored).await.unwrap_err(),
        ManagementFailure::StaleAssessment
    );
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture
            .ssh
            .supervisor
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn stopped_start_uses_owned_saved_mac_signer_and_resolves_lost_admit_without_another_handshake()
 {
    let fixture = Fixture::with_desired(2, true, DesiredServerState::Stopped).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    fixture.peer.lose_admit.store(true, Ordering::SeqCst);
    assert!(provider.start(request).await.is_err());
    assert!(fixture.records()[0].submitted());
    assert!(fixture.records()[0].assessment().is_some());
    let handshakes = fixture.ssh.stopped_opens.load(Ordering::SeqCst);
    let restored = fixture.provider().await;
    let progress = restored.resolve(request).await.unwrap();
    assert_eq!(
        progress.state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    assert_eq!(fixture.ssh.stopped_opens.load(Ordering::SeqCst), handshakes);
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture
            .ssh
            .supervisor
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Running
    );
    assert!(fixture.records().is_empty());
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn live_and_stopped_uninstall_retain_terminal_result_until_exact_guest_ack() {
    for desired in [DesiredServerState::Running, DesiredServerState::Stopped] {
        for major in [1, 2] {
            let fixture = Fixture::with_desired(major, true, desired).await;
            let provider = fixture.provider().await;
            let request = provider.reserve_request().await.unwrap();
            let assessment = provider
                .assess(request, ServiceAction::Uninstall)
                .await
                .unwrap();
            assert_eq!(
                assessment.prompt.affected_sessions,
                if desired == DesiredServerState::Running {
                    2
                } else {
                    0
                }
            );
            assert_eq!(
                assessment.runtime_epoch.is_some(),
                desired == DesiredServerState::Running
            );
            fixture.ssh.lose_ack.store(true, Ordering::SeqCst);
            assert_eq!(
                provider.submit(assessment).await.unwrap_err(),
                ManagementFailure::IoUnavailable
            );
            assert!(fixture.records()[0].submitted());
            assert!(fixture.records()[0].result().is_some());
            let stopped_handshakes = fixture.ssh.stopped_opens.load(Ordering::SeqCst);
            let progress = provider.resolve(request).await.unwrap();
            assert_eq!(progress.request, request);
            assert_eq!(
                progress.state,
                ServiceRequestState::Completed(ServiceOutcome::Uninstalled)
            );
            assert_eq!(
                fixture.ssh.stopped_opens.load(Ordering::SeqCst),
                stopped_handshakes
            );
            assert_eq!(
                fixture.peer.opens.load(Ordering::SeqCst),
                usize::from(desired == DesiredServerState::Running)
            );
            assert_eq!(
                fixture.peer.admit_calls.load(Ordering::SeqCst),
                usize::from(desired == DesiredServerState::Running)
            );
            assert!(!fixture.manager.running.load(Ordering::SeqCst));
            assert!(fixture.records().is_empty());
            fixture.worker.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn signed_stopped_stage_and_exact_activation_keep_stopped_without_a_live_worker() {
    for major in [1, 2] {
        let fixture = Fixture::with_desired(major, true, DesiredServerState::Stopped).await;
        let provider = fixture.provider().await;
        let before = provider.inspect().await.unwrap();
        let (_, artifact) =
            signed_installation::fixture("1.2.4", 7, signed_installation::native_machine());
        let digest = artifact.artifact_sha256();
        let request = provider.reserve_request().await.unwrap();
        let staged = provider.stage(request, artifact).await.unwrap();
        let ServiceRequestState::Completed(ServiceOutcome::Staged(status)) = staged.state else {
            panic!("exact stage must complete");
        };
        assert_eq!(status.artifact_sha256, digest);
        let after_stage = provider.inspect().await.unwrap();
        assert_eq!(
            after_stage.status.desired,
            Some(DesiredServerState::Stopped)
        );
        assert_eq!(after_stage.status.approved, before.status.approved);
        assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
        let request = provider.reserve_request().await.unwrap();
        let original = provider
            .assess(request, ServiceAction::Activate(status.installation))
            .await
            .unwrap();
        assert_eq!(original.prompt.affected_sessions, 0);
        assert_eq!(original.runtime_epoch, None);
        let token = original.prompt.token.clone();
        drop(provider);
        let reopened = fixture.provider().await;
        let restored = reopened
            .restore_assessment(ServiceAction::Activate(status.installation), token)
            .await
            .unwrap();
        assert_eq!(restored.prompt.token, original.prompt.token);
        let result = reopened.submit(restored).await.unwrap();
        assert_eq!(
            result.state,
            ServiceRequestState::Completed(ServiceOutcome::Activated(status.clone()))
        );
        let after = reopened.inspect().await.unwrap();
        assert_eq!(after.status.desired, Some(DesiredServerState::Stopped));
        assert_eq!(after.status.approved, Some(status));
        assert!(after.status.staged.is_none());
        assert!(!fixture.manager.running.load(Ordering::SeqCst));
        assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 0);
        assert!(fixture.records().is_empty());
        fixture.worker.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn stopped_stage_lost_ack_resupply_after_local_seal_never_opens_another_handshake() {
    let fixture = Fixture::with_desired(2, true, DesiredServerState::Stopped).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    let (_, artifact) =
        signed_installation::fixture("1.2.4", 7, signed_installation::native_machine());
    fixture.ssh.lose_ack.store(true, Ordering::SeqCst);
    assert_eq!(
        provider.stage(request, artifact).await.unwrap_err(),
        ManagementFailure::IoUnavailable
    );
    assert!(fixture.records()[0].result().is_some());
    let opens = fixture.ssh.stopped_opens.load(Ordering::SeqCst);
    seal_pairing(&fixture).await;
    let (_, original) =
        signed_installation::fixture("1.2.4", 7, signed_installation::native_machine());
    let progress = provider.stage(request, original).await.unwrap();
    assert!(matches!(
        progress.state,
        ServiceRequestState::Completed(ServiceOutcome::Staged(_))
    ));
    assert_eq!(fixture.ssh.stopped_opens.load(Ordering::SeqCst), opens);
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 0);
    assert!(fixture.records().is_empty());
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn running_stage_preserves_approved_running_and_sessions_on_the_same_authenticated_worker() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let before = provider.inspect().await.unwrap();
    let (_, artifact) =
        signed_installation::fixture("1.2.4", 7, signed_installation::native_machine());
    let digest = artifact.artifact_sha256();
    let request = provider.reserve_request().await.unwrap();
    let progress = provider.stage(request, artifact).await.unwrap();
    let ServiceRequestState::Completed(ServiceOutcome::Staged(status)) = progress.state else {
        panic!("exact running stage must complete");
    };
    assert_eq!(status.artifact_sha256, digest);
    let after = provider.inspect().await.unwrap();
    assert_eq!(after.status.desired, Some(DesiredServerState::Running));
    assert_eq!(after.status.approved, before.status.approved);
    assert!(fixture.manager.running.load(Ordering::SeqCst));
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.ssh.stopped_opens.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.peer.assess_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 1);
    assert!(fixture.records().is_empty());
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn accepted_running_stage_waits_for_exact_original_archive_and_resupply_never_readmits() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let (_, artifact) =
        signed_installation::fixture("1.2.4", 7, signed_installation::native_machine());
    let digest = artifact.artifact_sha256();
    let request = provider.reserve_request().await.unwrap();
    fixture.ssh.lose_stage_resolve.store(true, Ordering::SeqCst);
    assert_eq!(
        provider.stage(request, artifact).await.unwrap_err(),
        ManagementFailure::IoUnavailable
    );
    let pending = provider.resolve(request).await.unwrap();
    assert_eq!(pending.state, ServiceRequestState::Pending);
    assert!(fixture.records()[0].submitted());
    let (_, substitute) =
        signed_installation::fixture("1.2.5", 7, signed_installation::native_machine());
    assert_eq!(
        provider.stage(request, substitute).await.unwrap_err(),
        ManagementFailure::StaleAssessment
    );
    drop(provider);
    let reopened = fixture.provider().await;
    seal_pairing(&fixture).await;
    let (_, original) =
        signed_installation::fixture("1.2.4", 7, signed_installation::native_machine());
    assert_eq!(original.artifact_sha256(), digest);
    let progress = reopened.stage(request, original).await.unwrap();
    assert!(matches!(
        progress.state,
        ServiceRequestState::Completed(ServiceOutcome::Staged(_))
    ));
    assert_eq!(fixture.peer.assess_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peer.admit_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.peer.opens.load(Ordering::SeqCst), 1);
    assert!(fixture.records().is_empty());
    fixture.worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn changed_ssh_owner_refuses_original_request_before_any_native_or_runtime_effect() {
    let fixture = Fixture::new(2, true).await;
    let provider = fixture.provider().await;
    let request = provider.reserve_request().await.unwrap();
    fixture.ssh.wrong_owner.store(true, Ordering::SeqCst);
    assert_eq!(
        provider
            .assess(request, ServiceAction::Stop)
            .await
            .unwrap_err(),
        ManagementFailure::AuthorizationDenied
    );
    assert_eq!(fixture.ssh.calls.lock().unwrap().len(), 1);
    assert_eq!(fixture.peer.assess_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.manager.effects.load(Ordering::SeqCst), 0);
    fixture.worker.shutdown().await.unwrap();
}
