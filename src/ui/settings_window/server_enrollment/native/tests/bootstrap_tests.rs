//! Production provider acceptance with SSH, credential custody and manager boundaries injected.
//! Account setup, signed installation, owner actor, handshake and journal remain real.

use super::*;
use spaceterm_client::pairing::bootstrap::*;
use spaceterm_client::{
    journal::{JournalLimits, ReceiptVerifier},
    pairing::{
        credentials::*,
        enrollment::{EnrollmentConnector, PairingTransport},
        revocation::RestrictedConnector,
    },
};
use spaceterm_client_native::bootstrap::production::{
    BootstrapHostConnection, BootstrapHostFactory, BootstrapRemoteConnection,
    ProductionSshBootstrap,
};
use spaceterm_core::{operation_ledger::canonical::CanonicalCodec, pairing::SignedPairingReceipt};
use spaceterm_distribution::{UpdateKey, VerifiedAccountServerArtifact};
use spaceterm_host::{
    account_service::{
        LingerState, NativeServiceInstallations, RegisteredServer, RuntimeAssessment,
        ServiceAction, ServiceClock, ServiceFailure, ServiceFuture, ServiceManager,
        ServiceProcesses, ServiceSupervisor, ServiceTime, SupervisorState,
        test_support::AccountServiceFixture,
    },
    bootstrap::{
        linger::{LingerFuture, LingerInvocation, ScopedLingerAuthentication},
        process::{BootstrapProbeFailure, BootstrapProbeFuture},
        setup::AccountBootstrapSetup,
        wire::*,
    },
    shell_launch::ShellLaunchPlanner,
    startup::{BuildIdentity, DesiredServerState},
    transport::{MessageTransport, PeerFacts, TransportError, TransportFuture, TransportRoute},
};
use spaceterm_server::{
    account_startup::{AccountStartup, RecoveredAccountServer, test_support::StartupAbsence},
    adapters::terminal::AccountLaunchFacts,
};
use std::{
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use crate::signed_installation_fixture as signed;

type CredentialEntry = (
    PreparedCredential,
    Arc<spaceterm_client::testing::DeterministicKey>,
);
#[derive(Clone, Default)]
struct Credentials(Arc<Mutex<Vec<CredentialEntry>>>);
impl NativeDeviceCredentials for Credentials {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            let mut keys = self.0.lock().unwrap();
            let key = Arc::new(
                spaceterm_client::testing::DeterministicKey::new(30 + keys.len() as u8).unwrap(),
            );
            let prepared = PreparedCredential {
                scope: reservation.scope().clone(),
                public_key: spaceterm_protocol::authentication::SigningKey::public_key(
                    key.as_ref(),
                ),
            };
            keys.push((prepared.clone(), key));
            Ok(prepared)
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|(key, _)| &key.scope == scope)
                .map(|(key, _)| key.clone()))
        })
    }
    fn signing_key<'a>(
        &'a self,
        prepared: &'a PreparedCredential,
        lease: &'a CredentialUse,
    ) -> PairingFuture<
        'a,
        Result<Arc<dyn spaceterm_protocol::authentication::SigningKey>, PairingFailure>,
    > {
        Box::pin(async move {
            lease.validate(&prepared.scope)?;
            self.0
                .lock()
                .unwrap()
                .iter()
                .find(|(key, _)| key == prepared)
                .map(|(_, key)| {
                    key.clone() as Arc<dyn spaceterm_protocol::authentication::SigningKey>
                })
                .ok_or(PairingFailure::CredentialUnavailable)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        _: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async { panic!("Bootstrap must not delete a credential") })
    }
}
struct NoPairingRoute;
impl EnrollmentConnector for NoPairingRoute {
    fn open<'a>(
        &'a self,
        _: spaceterm_protocol::authentication::ServerPin,
        _: spaceterm_core::pairing::RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async {
            panic!("Bootstrap uses its retained owner channel, not a replacement route")
        })
    }
}
impl RestrictedConnector for NoPairingRoute {
    fn open<'a>(
        &'a self,
        _: &'a PendingRevocationTask,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async { panic!("Bootstrap cannot revoke") })
    }
}
struct Verify;
impl ReceiptVerifier for Verify {
    fn verify(
        &self,
        pin: spaceterm_protocol::authentication::ServerPin,
        receipt: &SignedPairingReceipt,
    ) -> Result<(), ClientFailure> {
        spaceterm_protocol::pairing_adapter::ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))
    }
}
struct Clock;
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        ServiceTime::current()
    }
}
struct Manager {
    key: UpdateKey,
    running: AtomicBool,
    delayed_start: AtomicBool,
    starting: AtomicBool,
    start_requested: tokio::sync::Notify,
    starting_observed: tokio::sync::Notify,
    effects: Mutex<Vec<&'static str>>,
    linux: AtomicBool,
    linger_enabled: AtomicBool,
}
impl ServiceManager for Manager {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async {
            Ok(if self.running.load(Ordering::SeqCst) {
                SupervisorState::Running
            } else if self.starting.load(Ordering::SeqCst) {
                self.starting_observed.notify_one();
                SupervisorState::Starting
            } else {
                SupervisorState::Inactive
            })
        })
    }
    fn scoped_processes<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, Vec<u32>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async {
            Ok(if self.linux.load(Ordering::SeqCst) {
                Some(if self.linger_enabled.load(Ordering::SeqCst) {
                    LingerState::Enabled
                } else {
                    LingerState::Disabled
                })
            } else {
                None
            })
        })
    }
    fn configure<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.verify_installation(registration)?;
            self.effects.lock().unwrap().push("configure");
            Ok(())
        })
    }
    fn start<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.lock().unwrap().push("start");
            if self.delayed_start.load(Ordering::SeqCst) {
                self.starting.store(true, Ordering::SeqCst);
            } else {
                self.running.store(true, Ordering::SeqCst);
            }
            self.start_requested.notify_one();
            Ok(())
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.lock().unwrap().push("stop");
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        })
    }
    fn uninstall<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { panic!("Bootstrap cannot uninstall") })
    }
    fn verify_installation(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        NativeServiceInstallations::test_approved_executable(registration, self.key.clone())
            .map(drop)
    }
}
struct Remote {
    fixture: Arc<AccountServiceFixture>,
    supervisor: Arc<ServiceSupervisor>,
    manager: Arc<Manager>,
    root: PathBuf,
    requests: Mutex<Vec<&'static str>>,
    lost_reply: AtomicBool,
    install_reply_latency: Mutex<Option<std::time::Duration>>,
    lost_owner_reply: AtomicBool,
    lost_ack_channels: AtomicUsize,
    fail_owner_binding: AtomicBool,
    owner_authorizations: Arc<AtomicUsize>,
    owner_helpers: Arc<AtomicUsize>,
    failed_owner_exit: Mutex<Option<Arc<OwnerExit>>>,
    before_start_admission: Mutex<Option<Arc<StartAdmissionGate>>>,
    prompt_allowed: AtomicBool,
    start_during_prompt: AtomicBool,
    prompt_count: AtomicUsize,
    password_deliveries: AtomicUsize,
    linger_invocations: AtomicUsize,
    cancel_owner_ready: Mutex<Option<Arc<AtomicBool>>>,
    ssh_capture: Mutex<Option<(PathBuf, spaceterm_client::journal::JournalSshCapture)>>,
    change_config_on_owner_frame: Mutex<Option<(usize, PathBuf, String)>>,
    owner_client_frames: Arc<AtomicUsize>,
    unsupported: AtomicBool,
    prospective_roots: Mutex<Option<(PathBuf, PathBuf)>>,
    actor: tokio::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::task::JoinHandle<()>,
        )>,
    >,
    owners: AtomicUsize,
}
impl Remote {
    fn route_value(&self) -> BootstrapRoute {
        BootstrapRoute::capture(
            [1; 32],
            [2; 32],
            [3; 32],
            self.supervisor.key().account_uid,
            BuildChannel::Development,
        )
        .unwrap()
    }
    async fn observed(&self) -> Result<AccountObservation, ServiceFailure> {
        let facts = AccountBootstrapSetup::new((*self.supervisor).clone())
            .inspect()
            .await?;
        let remote_socket = self.supervisor.discover()?.map(|registered| {
            registered
                .endpoint()
                .unwrap()
                .path()
                .to_str()
                .unwrap()
                .to_owned()
        });
        Ok(AccountObservation {
            facts,
            remote_socket,
        })
    }
    async fn ensure_actor(&self) {
        let mut actor = self.actor.lock().await;
        if actor.is_some() {
            return;
        }
        let registration = self.supervisor.discover().unwrap().unwrap();
        let startup = AccountStartup::admit(&self.supervisor).unwrap();
        let server = RecoveredAccountServer::recover(
            self.supervisor.clone(),
            startup,
            AccountLaunchFacts {
                uid: registration.key().account_uid,
                home: self.root.clone(),
                planner: ShellLaunchPlanner::for_test(self.root.join("shell"), self.root.clone()),
                machine: spaceterm_terminal::metadata::LocalMachine::default(),
            },
        )
        .await
        .unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = server
                .run_until(async {
                    let _ = stopped.await;
                })
                .await;
        });
        let endpoint = registration.endpoint().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if tokio::net::UnixStream::connect(endpoint.path())
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        *actor = Some((stop, task));
    }
    async fn close(&self) {
        if let Some((stop, task)) = self.actor.lock().await.take() {
            let _ = stop.send(());
            task.await.unwrap();
        }
    }
}
impl BootstrapRemoteConnection for Remote {
    fn route(&self) -> BootstrapRoute {
        self.route_value()
    }
    fn destination(&self) -> &str {
        "test-account"
    }
    fn revalidate(&self) -> Result<(), BootstrapFailure> {
        Ok(())
    }
    fn probe(&self) -> BootstrapProbeFuture<'_, Vec<u8>> {
        Box::pin(async move {
            self.revalidate()
                .map_err(|_| BootstrapProbeFailure::RouteChanged)?;
            let uid = self.supervisor.key().account_uid;
            if self.unsupported.load(Ordering::SeqCst) {
                return Ok(format!("STBP2\nuid={uid}\nos=Linux\narch=x86_64\nversion=22.04\nlibc=musl\nkernel=5.15.0\nmanager=systemd-user\nmanager_version=249\nmanager_reachable=yes\nlinger=yes\nsudo=available\n").into_bytes());
            }
            if self.manager.linux.load(Ordering::SeqCst) {
                Ok(format!("STBP2\nuid={uid}\nos=Linux\narch=x86_64\nversion=22.04\nlibc=glibc 2.35\nkernel=5.15.0\nmanager=systemd-user\nmanager_version=249\nmanager_reachable=yes\nlinger={}\nsudo=available\n", if self.manager.linger_enabled.load(Ordering::SeqCst) { "yes" } else { "no" }).into_bytes())
            } else {
                Ok(format!("STBP2\nuid={uid}\nos=Darwin\narch=arm64\nversion=26.0.0\nlibc=none\nkernel=25.0.0\nmanager=launchagent\nmanager_version=1\nmanager_reachable=yes\nlinger=none\nsudo=unavailable\n").into_bytes())
            }
        })
    }
    fn account<'a>(
        &'a self,
        request: AccountRequest,
        artifact: Option<&'a VerifiedAccountServerArtifact>,
    ) -> PairingFuture<'a, Result<AccountReply, BootstrapFailure>> {
        Box::pin(async move {
            self.revalidate()?;
            let setup = AccountBootstrapSetup::new((*self.supervisor).clone());
            let is_install = matches!(request, AccountRequest::Install { .. });
            let result = async {
                match request {
                    AccountRequest::Inspect => {
                        self.requests.lock().unwrap().push("inspect");
                        Ok(AccountReply::Observation(self.observed().await?))
                    }
                    AccountRequest::Reserve => {
                        self.requests.lock().unwrap().push("reserve");
                        Ok(AccountReply::Reserved(setup.reserve_request()?))
                    }
                    AccountRequest::ReserveOwner { proposal } => {
                        self.requests.lock().unwrap().push("reserve-owner");
                        let request = match self.supervisor.retained_owner_proposal(
                            proposal.parts().server,
                            &proposal.parts().keys,
                        )? {
                            Some((request, original)) => {
                                assert_eq!(original, proposal);
                                request
                            }
                            None => {
                                let request = setup.reserve_request()?;
                                self.supervisor.record_owner_proposal(request, proposal)?;
                                request
                            }
                        };
                        Ok(AccountReply::Reserved(request))
                    }
                    AccountRequest::Install {
                        request,
                        artifact: expected,
                        transfer_bytes,
                    } => {
                        self.requests.lock().unwrap().push("install");
                        let artifact = artifact.unwrap();
                        assert_eq!(artifact.artifact_sha256(), expected);
                        let mut bytes = Vec::new();
                        spaceterm_client_native::bootstrap::write_artifact(artifact, &mut bytes)
                            .unwrap();
                        assert_eq!(bytes.len() as u64, transfer_bytes);
                        let (_, artifact) = signed::fixture("9.0.0", 7, signed::native_machine());
                        assert_eq!(artifact.artifact_sha256(), expected);
                        let mut assessment = self.fixture.assess_test_install(artifact);
                        if let Some((data, runtime)) =
                            self.prospective_roots.lock().unwrap().clone()
                        {
                            assessment = assessment.with_roots(data, runtime)?;
                        }
                        let facts = setup
                            .install_and_observe(request, assessment.confirm(), expected)
                            .await?;
                        let observation = self.observed().await?;
                        if observation.facts != facts {
                            return Err(ServiceFailure::StaleAssessment);
                        }
                        Ok(AccountReply::Observation(observation))
                    }
                    AccountRequest::AuthorizeOwner {
                        request,
                        expected,
                        proposal,
                    } => {
                        self.requests.lock().unwrap().push("authorize-owner");
                        let digest = setup.authorize_owner(request, &expected, &proposal).await?;
                        Ok(AccountReply::Owner {
                            observation: self.observed().await?,
                            digest,
                        })
                    }
                    AccountRequest::OwnerBinding { request, proposal } => {
                        self.requests.lock().unwrap().push("owner-binding");
                        if self.fail_owner_binding.swap(false, Ordering::SeqCst) {
                            return Err(ServiceFailure::StorageUnavailable);
                        }
                        let claim =
                            spaceterm_protocol::pairing_adapter::enrollment_proposal_digest(
                                &proposal,
                            )
                            .unwrap();
                        let digest = self
                            .supervisor
                            .retained_owner_enrollment_digest(
                                request,
                                proposal.parts().server,
                                claim,
                            )?
                            .unwrap();
                        Ok(AccountReply::Owner {
                            observation: self.observed().await?,
                            digest,
                        })
                    }
                    AccountRequest::Resolve { request, artifact } => {
                        self.requests.lock().unwrap().push("resolve");
                        setup.resolve(request, artifact).await?;
                        Ok(AccountReply::Observation(self.observed().await?))
                    }
                    AccountRequest::RetainedOwner { keys } => {
                        let pin = self.supervisor.discover()?.unwrap().server_pin();
                        Ok(AccountReply::RetainedOwner(
                            self.supervisor.retained_owner_proposal(
                                spaceterm_core::identity::ServerId::new(pin.identity),
                                &keys,
                            )?,
                        ))
                    }
                    AccountRequest::Start { expected } => {
                        self.requests.lock().unwrap().push("explicit-start");
                        if setup.inspect().await?.registration != expected {
                            return Err(ServiceFailure::StaleAssessment);
                        }
                        let spaceterm_host::bootstrap::setup::BootstrapAccountRegistration::Stopped { pin, generation } = expected else {
                            return Err(ServiceFailure::StaleAssessment);
                        };
                        let registration = self.supervisor.discover()?.ok_or(ServiceFailure::StaleAssessment)?;
                        if registration.server_pin() != pin
                            || registration.generation() != generation
                            || registration.desired_state() != DesiredServerState::Stopped
                        {
                            return Err(ServiceFailure::StaleAssessment);
                        }
                        let expected = spaceterm_host::account_service::StoppedServerExpectation {
                            pin,
                            generation,
                            approved: registration.approved_installation().cloned().ok_or(ServiceFailure::Missing)?,
                        };
                        let gate = self.before_start_admission.lock().unwrap().take();
                        if let Some(gate) = gate {
                            gate.inspected.notify_one();
                            gate.admit.notified().await;
                        }
                        self.supervisor
                            .start_stopped(self.supervisor.allocate_request()?, expected)
                            .await?;
                        Ok(AccountReply::Observation(self.observed().await?))
                    }
                    AccountRequest::OwnerChannel => {
                        panic!("owner channel uses its independent stream")
                    }
                }
            }
            .await;
            if is_install {
                let latency = self.install_reply_latency.lock().unwrap().take();
                if let Some(latency) = latency {
                    tokio::time::sleep(latency).await;
                }
            }
            if is_install && self.lost_reply.swap(false, Ordering::SeqCst) {
                return Err(BootstrapFailure::new(
                    PairingFailure::Offline,
                    BootstrapPhase::Applied,
                ));
            }
            result.map_err(|error| {
                spaceterm_client_native::bootstrap::service_failure(error, BootstrapPhase::Applied)
            })
        })
    }
    fn owner_channel(
        &self,
    ) -> PairingFuture<'_, Result<Arc<dyn MessageTransport>, PairingFailure>> {
        Box::pin(async move {
            self.ensure_actor().await;
            self.owners.fetch_add(1, Ordering::SeqCst);
            let (a, ar) = tokio::sync::mpsc::channel(1);
            let (b, br) = tokio::sync::mpsc::channel(1);
            let uid = self.supervisor.key().account_uid;
            let lost_reply = self.lost_owner_reply.swap(false, Ordering::SeqCst);
            let ack_failure = self
                .lost_ack_channels
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    (left > 0).then(|| left - 1)
                })
                .ok();
            let server = Channel {
                loss_at: if lost_reply { 5 } else { 0 },
                frame_count: AtomicUsize::new(0),
                issued: Some(self.owner_authorizations.clone()),
                change_config_on_owner_frame: self
                    .change_config_on_owner_frame
                    .lock()
                    .unwrap()
                    .take(),
                sent: Arc::new(AtomicUsize::new(0)),
                cancel_on_ready: self.cancel_owner_ready.lock().unwrap().take(),
                send: tokio::sync::Mutex::new(Some(a)),
                receive: tokio::sync::Mutex::new(br),
                completion: tokio::sync::Mutex::new(None),
                uid,
            };
            let supervisor = self.supervisor.clone();
            self.owner_helpers.fetch_add(1, Ordering::SeqCst);
            let helper = OwnerHelper(self.owner_helpers.clone());
            let exit = if ack_failure == Some(1) {
                self.failed_owner_exit.lock().unwrap().take()
            } else {
                None
            };
            let task = tokio::spawn(async move {
                let _helper = helper;
                let result =
                    spaceterm_server::deployment::owner::execute(supervisor, &server).await;
                if lost_reply {
                    assert!(result.is_err());
                } else if ack_failure.is_none() {
                    assert!(
                        result.is_ok(),
                        "owner exchange failed: {result:?}; sent frames: {}",
                        server.frame_count.load(Ordering::SeqCst),
                    );
                }
                if let Some(exit) = exit {
                    exit.entered.notify_one();
                    exit.release.notified().await;
                }
            });
            Ok(Arc::new(Channel {
                // Enrollment's fifth frame is its ACK. On a fresh historical
                // session it is the lookup confirming that same cumulative ACK.
                loss_at: if ack_failure.is_some() { 5 } else { 0 },
                frame_count: AtomicUsize::new(0),
                issued: None,
                change_config_on_owner_frame: None,
                sent: self.owner_client_frames.clone(),
                cancel_on_ready: None,
                send: tokio::sync::Mutex::new(Some(b)),
                receive: tokio::sync::Mutex::new(ar),
                completion: tokio::sync::Mutex::new(Some(task)),
                uid,
            }) as Arc<dyn MessageTransport>)
        })
    }
    fn prompt(&self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        Box::pin(async {
            self.prompt_count.fetch_add(1, Ordering::SeqCst);
            if self.start_during_prompt.swap(false, Ordering::SeqCst) {
                // Model another deliberate account action while this prompt waits.
                // That actor enables linger and Starts through the real native supervisor.
                self.manager.linger_enabled.store(true, Ordering::SeqCst);
                self.requests.lock().unwrap().push("concurrent-start");
                let request = self.supervisor.allocate_request().map_err(|error| {
                    spaceterm_client_native::bootstrap::service_failure(
                        error,
                        BootstrapPhase::Authorized,
                    )
                })?;
                self.supervisor.start(request).await.map_err(|error| {
                    spaceterm_client_native::bootstrap::service_failure(
                        error,
                        BootstrapPhase::Authorized,
                    )
                })?;
            }
            if self.prompt_allowed.load(Ordering::SeqCst) {
                Ok(Zeroizing::new(b"scoped-test-response".to_vec()))
            } else {
                Err(BootstrapFailure::new(
                    PairingFailure::AuthorizationDenied,
                    BootstrapPhase::Authorized,
                ))
            }
        })
    }
    fn captured_ssh(&self) -> Option<spaceterm_client::journal::JournalSshCapture> {
        self.ssh_capture
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, capture)| capture.clone())
    }
    fn revalidate_capture(&self) -> PairingFuture<'_, Result<(), BootstrapFailure>> {
        Box::pin(async {
            // The external SSH boundary holds an immutable original configuration.
            // Tests change the actual local file at a framed delivery boundary.
            if let Some((path, original)) = self.ssh_capture.lock().unwrap().as_ref() {
                let current = std::fs::read_to_string(path).map_err(|_| {
                    BootstrapFailure::new(PairingFailure::RouteChanged, BootstrapPhase::Enrolled)
                })?;
                if current != original.configuration() {
                    return Err(BootstrapFailure::new(
                        PairingFailure::RouteChanged,
                        BootstrapPhase::Enrolled,
                    ));
                }
            }
            self.revalidate()
        })
    }
    fn enable_linger<'a>(
        &'a self,
        invocation: &'a LingerInvocation,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> LingerFuture<'a, ()> {
        Box::pin(async move {
            assert_eq!(invocation.executable(), "/usr/bin/sudo");
            assert_eq!(invocation.account_uid(), self.supervisor.key().account_uid);
            self.linger_invocations.fetch_add(1, Ordering::SeqCst);
            let response = authentication.respond(invocation.account_uid()).await?;
            self.password_deliveries.fetch_add(1, Ordering::SeqCst);
            assert_eq!(response.as_slice(), b"scoped-test-response");
            self.manager.linger_enabled.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}
#[derive(Default)]
struct StartAdmissionGate {
    inspected: tokio::sync::Notify,
    admit: tokio::sync::Notify,
}
#[derive(Default)]
struct OwnerExit {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
struct OwnerHelper(Arc<AtomicUsize>);
impl Drop for OwnerHelper {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Channel {
    loss_at: usize,
    frame_count: AtomicUsize,
    issued: Option<Arc<AtomicUsize>>,
    change_config_on_owner_frame: Option<(usize, PathBuf, String)>,
    sent: Arc<AtomicUsize>,
    cancel_on_ready: Option<Arc<AtomicBool>>,
    send: tokio::sync::Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>,
    receive: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    completion: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    uid: u32,
}
impl MessageTransport for Channel {
    fn peer_facts(&self) -> PeerFacts {
        PeerFacts {
            route: TransportRoute::PrivateIpc,
            local_uid: Some(self.uid),
            local_pid: None,
            pinned_server_key: None,
        }
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            self.sent.fetch_add(1, Ordering::SeqCst);
            let count = self.frame_count.fetch_add(1, Ordering::SeqCst) + 1;
            if count == 1
                && let Some(issued) = &self.issued
            {
                let reply: spaceterm_server::deployment::owner::OwnerEnrollmentReply =
                    serde_json::from_slice(bytes).unwrap();
                if reply.disposition == spaceterm_server::deployment::owner::OwnerReplyDisposition::AuthorizationDelivered {
                    issued.fetch_add(1, Ordering::SeqCst);
                }
            }
            if let Some((frame, path, replacement)) = &self.change_config_on_owner_frame
                && count == *frame
            {
                std::fs::write(path, replacement).unwrap();
            }
            if count == 4
                && let Some(valid) = &self.cancel_on_ready
            {
                valid.store(false, Ordering::Release);
            }
            if count == self.loss_at {
                self.send.lock().await.take();
                return Err(TransportError::Closed);
            }
            self.send
                .lock()
                .await
                .as_ref()
                .ok_or(TransportError::Closed)?
                .send(bytes.to_vec())
                .await
                .map_err(|_| TransportError::Closed)
        })
    }
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.receive.lock().await.recv().await) })
    }
    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async {
            self.send.lock().await.take();
            if let Some(task) = self.completion.lock().await.take() {
                task.await.map_err(|_| TransportError::Unavailable)?;
            }
            if self.loss_at != 0 && self.frame_count.load(Ordering::SeqCst) >= self.loss_at {
                // The native adapter has already retired/reaped a failed frame's
                // child and its repeated close reports this same classification.
                return Err(TransportError::Unavailable);
            }
            Ok(())
        })
    }
}
struct Factory {
    remote: Arc<Remote>,
}
impl BootstrapHostFactory for Factory {
    fn open(
        &self,
        host: String,
        _: u64,
    ) -> PairingFuture<'_, Result<BootstrapHostConnection, BootstrapFailure>> {
        Box::pin(async move {
            assert_eq!(host, "test-account");
            let (_, artifact) = signed::fixture("9.0.0", 7, signed::native_machine());
            Ok(BootstrapHostConnection {
                connection: self.remote.clone(),
                artifact: Some(artifact),
            })
        })
    }
}
struct Harness {
    root: tempfile::TempDir,
    remote: Arc<Remote>,
    journal: Arc<spaceterm_client::journal::JournalController>,
    pairing: Arc<NativePairingRuntime>,
    credentials: Credentials,
    provider: super::super::bootstrap::NativeForegroundBootstrap,
}
impl Harness {
    async fn new() -> Self {
        let parent = if cfg!(target_os = "macos") {
            PathBuf::from("/private/tmp")
        } else {
            spaceterm_host::account_service::TrustedAccountFacts::current()
                .unwrap()
                .home()
                .to_owned()
        };
        let root = tempfile::Builder::new()
            .prefix("stb")
            .tempdir_in(parent)
            .unwrap();
        let fixture =
            Arc::new(AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap());
        let (key, _) = signed::fixture("9.0.0", 7, signed::native_machine());
        let manager = Arc::new(Manager {
            key: key.clone(),
            running: AtomicBool::new(false),
            delayed_start: AtomicBool::new(false),
            starting: AtomicBool::new(false),
            start_requested: tokio::sync::Notify::new(),
            starting_observed: tokio::sync::Notify::new(),
            effects: Mutex::new(Vec::new()),
            linux: AtomicBool::new(cfg!(target_os = "linux")),
            linger_enabled: AtomicBool::new(true),
        });
        let supervisor = fixture.with_installation_trust(
            fixture.with_providers(
                manager.clone(),
                Arc::new(StartupAbsence),
                Arc::new(Clock),
                None,
            ),
            key,
        );
        let client = root.path().join("client");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&client)
            .unwrap();
        let codec = Arc::new(CanonicalCodec);
        let verifier = Arc::new(Verify);
        let storage = spaceterm_client_native::journal::NativeClientJournal::create(
            client,
            codec.clone(),
            verifier.clone(),
            JournalLimits::default(),
        )
        .await
        .unwrap();
        let journal = Arc::new(
            spaceterm_client::journal::JournalController::recover(
                Arc::new(storage),
                codec,
                verifier,
                JournalLimits::default(),
            )
            .await
            .unwrap(),
        );
        let credentials = Credentials::default();
        let pairing = Arc::new(NativePairingRuntime::with_adapters(
            journal.clone(),
            Arc::new(credentials.clone()),
            Arc::new(NoPairingRoute),
            Arc::new(NoPairingRoute),
            Arc::new(Invalidator),
            HandshakeConfig::default(),
        ));
        let remote = Arc::new(Remote {
            fixture,
            supervisor: Arc::new(supervisor),
            manager,
            root: root.path().to_owned(),
            requests: Mutex::new(Vec::new()),
            lost_reply: AtomicBool::new(false),
            install_reply_latency: Mutex::new(None),
            lost_owner_reply: AtomicBool::new(false),
            lost_ack_channels: AtomicUsize::new(0),
            fail_owner_binding: AtomicBool::new(false),
            owner_authorizations: Arc::new(AtomicUsize::new(0)),
            owner_helpers: Arc::new(AtomicUsize::new(0)),
            failed_owner_exit: Mutex::new(None),
            before_start_admission: Mutex::new(None),
            prompt_allowed: AtomicBool::new(false),
            start_during_prompt: AtomicBool::new(false),
            prompt_count: AtomicUsize::new(0),
            password_deliveries: AtomicUsize::new(0),
            linger_invocations: AtomicUsize::new(0),
            cancel_owner_ready: Mutex::new(None),
            ssh_capture: Mutex::new(None),
            change_config_on_owner_frame: Mutex::new(None),
            owner_client_frames: Arc::new(AtomicUsize::new(0)),
            unsupported: AtomicBool::new(false),
            prospective_roots: Mutex::new(None),
            actor: tokio::sync::Mutex::new(None),
            owners: AtomicUsize::new(0),
        });
        let provider =
            super::super::bootstrap::NativeForegroundBootstrap(ProductionSshBootstrap::new(
                pairing.clone(),
                Arc::new(Factory {
                    remote: remote.clone(),
                }),
            ));
        Self {
            root,
            remote,
            journal,
            pairing,
            credentials,
            provider,
        }
    }
    async fn install_existing(&self) {
        let setup = AccountBootstrapSetup::new((*self.remote.supervisor).clone());
        let request = setup.reserve_request().unwrap();
        let (_, artifact) = signed::fixture("9.0.0", 7, signed::native_machine());
        setup
            .install(request, self.remote.fixture.confirm_install(artifact))
            .await
            .unwrap();
        self.remote.manager.effects.lock().unwrap().clear();
        self.remote.requests.lock().unwrap().clear();
    }
    async fn stop_existing(&self) {
        self.install_existing().await;
        let stopper = self.remote.fixture.with_providers(
            self.remote.manager.clone(),
            Arc::new(EmptyRuntimeScope),
            Arc::new(Clock),
            None,
        );
        let action = stopper
            .assess(stopper.allocate_request().unwrap(), ServiceAction::Stop)
            .await
            .unwrap()
            .confirm()
            .unwrap();
        stopper.stop(action).await.unwrap();
        self.remote.manager.effects.lock().unwrap().clear();
        self.remote.requests.lock().unwrap().clear();
    }
    async fn candidate(&self, generation: u64) -> SshCandidate {
        let attempt = EnrollmentAttempt::new(generation);
        let (facts, pending) = self
            .provider
            .inspect("test-account".into(), attempt.clone())
            .await
            .unwrap();
        assert!(pending.is_none());
        SshCandidate::new(attempt, facts).unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_installs_or_reuses_then_enrolls() {
    for existing in [false, true] {
        let h = Harness::new().await;
        if existing {
            h.install_existing().await;
        }
        let original = h
            .remote
            .supervisor
            .discover()
            .unwrap()
            .map(|registration| registration.server_pin());
        let candidate = h.candidate(1).await;
        let completion = h
            .provider
            .0
            .bootstrap(candidate.confirm().unwrap(), candidate.facts())
            .await
            .unwrap();
        assert_eq!(completion.outcome.reused, existing);
        if let Some(pin) = original {
            assert_eq!(completion.saved.pin(), pin);
        }
        let state = h.journal.snapshot().unwrap();
        assert_eq!(state.bindings().len(), 1);
        assert!(state.pairing().acknowledgements().is_empty());
        let binding = state.bindings()[0].binding().unwrap();
        assert!(h.journal.ordinary_use_allowed(&binding).unwrap());
        assert!(
            matches!(binding.target().route().parts(), spaceterm_client::api::RouteParts::Ssh { destination, .. } if destination == "test-account")
        );
        assert_eq!(h.remote.owners.load(Ordering::SeqCst), 1);
        assert_eq!(
            h.remote.manager.effects.lock().unwrap().as_slice(),
            if existing {
                &[][..]
            } else {
                &["configure", "start"][..]
            }
        );
        assert!(h.root.path().exists());
        h.remote.close().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_unsupported_host_preflight_has_no_mutation() {
    let h = Harness::new().await;
    h.remote.unsupported.store(true, Ordering::SeqCst);
    assert!(matches!(
        h.provider
            .inspect("test-account".into(), EnrollmentAttempt::new(1))
            .await,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::UnsupportedTarget)
        ))
    ));
    assert!(!h.remote.fixture.anchor().exists());
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_denied_scoped_authorization_has_no_mutation() {
    let h = Harness::new().await;
    h.install_existing().await;
    h.remote.manager.linux.store(true, Ordering::SeqCst);
    h.remote
        .manager
        .linger_enabled
        .store(false, Ordering::SeqCst);
    let candidate = h.candidate(1).await;
    assert!(matches!(
        h.provider
            .bootstrap(candidate.confirm().unwrap(), candidate)
            .await,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::AuthorizationDenied)
        ))
    ));
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
    assert!(
        !h.remote
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(*request, "reserve" | "reserve-owner" | "authorize-owner"))
    );
}
// The fixture has no running process until owner_channel starts its account actor.
// Supply that exact empty external runtime scope only to the explicit Stop test.
struct EmptyRuntimeScope;
impl ServiceProcesses for EmptyRuntimeScope {
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            Ok(RuntimeAssessment {
                server: spaceterm_core::identity::ServerId::new(registration.server_pin().identity),
                epoch: None,
                generation: 1,
                sessions: Vec::new(),
                token: [9; 32],
            })
        })
    }
    fn revalidate<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            assert_eq!(
                assessment.server.bytes(),
                registration.server_pin().identity
            );
            assert_eq!(assessment.token, [9; 32]);
            assert!(assessment.sessions.is_empty());
            StartupAbsence.verify_absent(registration).await
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
        registration: &'a RegisteredServer,
        _: spaceterm_core::native_management::NativeManagementRequestRef,
        _: spaceterm_core::native_management::NativeRetirementAction,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<spaceterm_core::retirement::SessionCleanupObservation>> {
        Box::pin(async move {
            self.revalidate(registration, assessment).await?;
            Ok(Vec::new())
        })
    }
    fn verify_absent<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        StartupAbsence.verify_absent(registration)
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        epoch: spaceterm_core::authority::ServerEpoch,
        sessions: &'a [spaceterm_core::authority::SessionKey],
    ) -> ServiceFuture<'a, Vec<spaceterm_core::retirement::SessionCleanupObservation>> {
        StartupAbsence.previous_instance_cleanup(registration, epoch, sessions)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_deliberately_stopped_requires_separate_start() {
    let h = Harness::new().await;
    h.install_existing().await;
    let stopper = h.remote.fixture.with_providers(
        h.remote.manager.clone(),
        Arc::new(EmptyRuntimeScope),
        Arc::new(Clock),
        None,
    );
    let request = stopper.allocate_request().unwrap();
    let action = stopper
        .assess(request, ServiceAction::Stop)
        .await
        .unwrap()
        .confirm()
        .unwrap();
    stopper.stop(action).await.unwrap();
    h.remote.manager.effects.lock().unwrap().clear();
    let candidate = h.candidate(1).await;
    assert!(matches!(
        candidate.confirm(),
        Err(EnrollmentFailure::Pairing(
            PairingFailure::ExplicitStartRequired
        ))
    ));
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    assert_eq!(
        h.remote
            .supervisor
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
    h.provider.start(candidate).await.unwrap();
    assert_eq!(
        h.remote.manager.effects.lock().unwrap().as_slice(),
        &["configure", "start"]
    );
    assert_eq!(
        h.remote
            .supervisor
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Running
    );
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_changed_roots_refuses_parallel_installation() {
    let h = Harness::new().await;
    h.install_existing().await;
    let original = h.remote.supervisor.discover().unwrap().unwrap();
    let candidate = h.candidate(1).await;
    // Model newly selected XDG roots at the remote native install boundary. The
    // existing home anchor still owns the original registration and its roots.
    let changed_data = h.root.path().join("changed-data");
    let changed_runtime = h.root.path().join("changed-runtime");
    *h.remote.prospective_roots.lock().unwrap() =
        Some((changed_data.clone(), changed_runtime.clone()));
    let completion = h
        .provider
        .bootstrap(candidate.confirm().unwrap(), candidate)
        .await
        .unwrap();
    assert!(completion.outcome.reused);
    assert_eq!(completion.saved.pin(), original.server_pin());
    assert!(!changed_data.exists());
    assert!(!changed_runtime.exists());
    let after = h.remote.supervisor.discover().unwrap().unwrap();
    assert_eq!(original.server_pin(), after.server_pin());
    assert_eq!(original.data_directory(), after.data_directory());
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 1);
    assert!(!h.remote.requests.lock().unwrap().contains(&"install"));
    h.remote.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_lost_install_reply_resolves_original_then_enrolls_once() {
    let h = Harness::new().await;
    let candidate = h.candidate(1).await;
    h.remote.lost_reply.store(true, Ordering::SeqCst);
    let completion = h
        .provider
        .0
        .bootstrap(candidate.confirm().unwrap(), candidate.facts())
        .await
        .unwrap();
    assert!(!completion.outcome.reused);
    let requests = h.remote.requests.lock().unwrap().clone();
    assert_eq!(
        requests
            .iter()
            .filter(|request| **request == "install")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| **request == "resolve")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| **request == "authorize-owner")
            .count(),
        1
    );
    assert_eq!(
        h.remote.manager.effects.lock().unwrap().as_slice(),
        &["configure", "start"]
    );
    assert_eq!(h.journal.snapshot().unwrap().bindings().len(), 1);
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 1);
    h.remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_lost_owner_reply_recovers_original_after_provider_recreation() {
    let h = Harness::new().await;
    h.install_existing().await;
    let candidate = h.candidate(1).await;
    h.remote.lost_owner_reply.store(true, Ordering::SeqCst);
    assert!(matches!(
        h.provider
            .bootstrap(candidate.confirm().unwrap(), candidate)
            .await,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::Offline)
        ))
    ));
    let original = h.pairing.inventory().await.unwrap();
    assert_eq!(original.prepared.len(), 1);
    let original_pairing = original.prepared[0].local_pairing.clone();
    let provider = super::super::bootstrap::NativeForegroundBootstrap(ProductionSshBootstrap::new(
        h.pairing.clone(),
        Arc::new(Factory {
            remote: h.remote.clone(),
        }),
    ));
    let attempt = EnrollmentAttempt::new(2);
    let (facts, pending) = provider
        .inspect("test-account".into(), attempt.clone())
        .await
        .unwrap();
    let pending = pending.unwrap();
    let candidate = SshCandidate::new(attempt, facts)
        .unwrap()
        .retain_request(pending)
        .unwrap();
    let recovered = provider.resolve(candidate, pending).await.unwrap();
    assert_eq!(recovered.saved.key(), &original_pairing);
    let state = h.journal.snapshot().unwrap();
    assert_eq!(state.bindings().len(), 1);
    assert!(state.pairing().acknowledgements().is_empty());
    let requests = h.remote.requests.lock().unwrap().clone();
    assert_eq!(
        requests.iter().filter(|r| **r == "reserve-owner").count(),
        1
    );
    assert_eq!(
        requests.iter().filter(|r| **r == "authorize-owner").count(),
        1
    );
    assert_eq!(requests.iter().filter(|r| **r == "resolve").count(), 1);
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    h.remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_account_endpoint_rejects_nonembedded_artifact_trust() {
    let h = Harness::new().await;
    let setup = AccountBootstrapSetup::new((*h.remote.supervisor).clone());
    let request = setup.reserve_request().unwrap();
    let (_, artifact) = signed::fixture("9.0.0", 7, signed::native_machine());
    let mut transfer = Vec::new();
    spaceterm_client_native::bootstrap::write_artifact(&artifact, &mut transfer).unwrap();
    let (a, ar) = tokio::sync::mpsc::channel(1);
    let (b, br) = tokio::sync::mpsc::channel(1);
    let uid = h.remote.supervisor.key().account_uid;
    let server = Channel {
        loss_at: 0,
        frame_count: AtomicUsize::new(0),
        issued: None,
        change_config_on_owner_frame: None,
        sent: Arc::new(AtomicUsize::new(0)),
        cancel_on_ready: None,
        send: tokio::sync::Mutex::new(Some(a)),
        receive: tokio::sync::Mutex::new(br),
        completion: tokio::sync::Mutex::new(None),
        uid,
    };
    let supervisor = h.remote.supervisor.clone();
    let task = tokio::spawn(async move {
        spaceterm_server::deployment::bootstrap::execute(supervisor, &server)
            .await
            .unwrap();
    });
    let client = Channel {
        loss_at: 0,
        frame_count: AtomicUsize::new(0),
        issued: None,
        change_config_on_owner_frame: None,
        sent: Arc::new(AtomicUsize::new(0)),
        cancel_on_ready: None,
        send: tokio::sync::Mutex::new(Some(b)),
        receive: tokio::sync::Mutex::new(ar),
        completion: tokio::sync::Mutex::new(Some(task)),
        uid,
    };
    client
        .send(
            &serde_json::to_vec(&AccountRequest::Install {
                request,
                artifact: artifact.artifact_sha256(),
                transfer_bytes: transfer.len() as u64,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    for chunk in transfer.chunks(TRANSFER_CHUNK) {
        client.send(chunk).await.unwrap();
    }
    let reply: AccountReply =
        serde_json::from_slice(&client.recv().await.unwrap().unwrap()).unwrap();
    assert!(matches!(
        reply,
        AccountReply::Failure(ServiceFailure::ArtifactRejected)
    ));
    client.close().await.unwrap();
    assert!(h.remote.supervisor.discover().unwrap().is_none());
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_cancellation_after_ready_never_sends_enrollment() {
    let h = Harness::new().await;
    h.install_existing().await;
    let candidate = h.candidate(1).await;
    *h.remote.cancel_owner_ready.lock().unwrap() = Some(candidate.attempt.valid.clone());
    assert!(matches!(
        h.provider
            .bootstrap(candidate.confirm().unwrap(), candidate)
            .await,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::Cancelled)
        ))
    ));
    assert_eq!(h.remote.owner_client_frames.load(Ordering::SeqCst), 3);
    assert!(h.journal.snapshot().unwrap().bindings().is_empty());
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    h.remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_rejects_another_foreground_scope_with_the_same_counter() {
    let h = Harness::new().await;
    let candidate = h.candidate(1).await;
    assert!(matches!(
        h.provider
            .0
            .bind_foreground(candidate.facts.route, 1, Arc::new(AtomicBool::new(true))),
        Err(BootstrapFailure {
            class: BootstrapFailureClass::Pairing(PairingFailure::RouteChanged),
            ..
        })
    ));
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    assert!(h.journal.snapshot().unwrap().bindings().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_failed_ack_waits_for_owned_helper_retirement() {
    let h = Harness::new().await;
    h.install_existing().await;
    let candidate = h.candidate(1).await;
    h.remote.lost_ack_channels.store(2, Ordering::SeqCst);
    let exit = Arc::new(OwnerExit::default());
    *h.remote.failed_owner_exit.lock().unwrap() = Some(exit.clone());
    let bootstrap = h
        .provider
        .bootstrap(candidate.confirm().unwrap(), candidate);
    tokio::pin!(bootstrap);
    let failure = tokio::time::timeout(std::time::Duration::from_secs(300), async {
        tokio::select! {
            _ = exit.entered.notified() => (),
            _ = &mut bootstrap => panic!("bootstrap returned before owner helper retirement"),
        }
        let pending = std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(bootstrap.as_mut(), cx).is_pending())
        })
        .await;
        assert!(
            pending,
            "failed bootstrap must await its exact owner helper"
        );
        assert_eq!(h.remote.owner_helpers.load(Ordering::SeqCst), 1);
        exit.release.notify_one();
        bootstrap.await
    })
    .await
    .unwrap();
    assert!(matches!(
        failure,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::Offline)
        ))
    ));
    assert_eq!(h.remote.owner_helpers.load(Ordering::SeqCst), 0);
    let retained = h.pairing.inventory().await.unwrap();
    assert_eq!(retained.saved.len(), 1);
    assert_eq!(retained.enrollment_acknowledgements.len(), 1);
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 1);
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_lost_ack_recovers_saved_original_after_provider_recreation() {
    let h = Harness::new().await;
    h.install_existing().await;
    let candidate = h.candidate(1).await;
    h.remote.lost_ack_channels.store(2, Ordering::SeqCst);
    assert!(matches!(
        h.provider
            .bootstrap(candidate.confirm().unwrap(), candidate)
            .await,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::Offline)
        ))
    ));
    assert_eq!(h.remote.owner_helpers.load(Ordering::SeqCst), 0);
    let original = h.pairing.inventory().await.unwrap();
    assert!(original.prepared.is_empty());
    assert_eq!(original.enrollment_acknowledgements.len(), 1);
    assert_eq!(original.saved.len(), 1);
    let work = &original.enrollment_acknowledgements[0];
    let (native_request, claim) = h
        .remote
        .supervisor
        .retained_owner_proposal(
            work.prepared.local_pairing.parts().server,
            &work.prepared.keys,
        )
        .unwrap()
        .unwrap();
    let original_binding = h.journal.snapshot().unwrap().bindings()[0].clone();
    let provider = super::super::bootstrap::NativeForegroundBootstrap(ProductionSshBootstrap::new(
        h.pairing.clone(),
        Arc::new(Factory {
            remote: h.remote.clone(),
        }),
    ));
    let attempt = EnrollmentAttempt::new(2);
    let (facts, pending) = provider
        .inspect("test-account".into(), attempt.clone())
        .await
        .unwrap();
    let pending = pending.expect("saved enrollment must retain its native owner request until ACK");
    assert_eq!(pending.request(), *native_request.as_bytes());
    let candidate = SshCandidate::new(attempt, facts)
        .unwrap()
        .retain_request(pending)
        .unwrap();
    let completion = provider.resolve(candidate, pending).await.unwrap();
    assert_eq!(h.remote.owner_helpers.load(Ordering::SeqCst), 0);
    assert_eq!(completion.prepared, work.prepared);
    assert_eq!(completion.saved, original.saved[0]);
    let recovered = h.pairing.inventory().await.unwrap();
    assert!(recovered.enrollment_acknowledgements.is_empty());
    assert_eq!(
        recovered.generation_high_water,
        original.generation_high_water
    );
    assert_eq!(
        recovered.credential_preparations,
        original.credential_preparations
    );
    assert_eq!(
        h.journal.snapshot().unwrap().bindings(),
        &[original_binding]
    );
    assert_eq!(
        h.remote
            .supervisor
            .retained_owner_proposal(
                work.prepared.local_pairing.parts().server,
                &work.prepared.keys,
            )
            .unwrap(),
        Some((native_request, claim))
    );
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 1);
    let requests = h.remote.requests.lock().unwrap().clone();
    assert_eq!(
        requests.iter().filter(|r| **r == "reserve-owner").count(),
        1
    );
    assert_eq!(
        requests.iter().filter(|r| **r == "authorize-owner").count(),
        1
    );
    assert_eq!(requests.iter().filter(|r| **r == "resolve").count(), 1);
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    h.remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_owner_binding_failure_precedes_single_use_authorization() {
    let h = Harness::new().await;
    h.install_existing().await;
    let candidate = h.candidate(1).await;
    h.remote.fail_owner_binding.store(true, Ordering::SeqCst);
    assert!(
        h.provider
            .bootstrap(candidate.confirm().unwrap(), candidate)
            .await
            .is_err()
    );
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 0);
    let original = h.pairing.inventory().await.unwrap();
    assert!(original.prepared.is_empty());
    assert!(original.saved.is_empty());
    let authentication = original
        .credential_preparations
        .iter()
        .find(|credential| {
            matches!(
                credential.scope.parts().purpose,
                CredentialPurpose::PairingAuthentication { .. }
            )
        })
        .unwrap();
    let prepared_key = h
        .pairing
        .recover_credential(authentication.generation)
        .await
        .unwrap()
        .unwrap();
    let server = h.remote.supervisor.discover().unwrap().unwrap();
    let keys =
        spaceterm_core::pairing::PairingKeys::new(spaceterm_core::pairing::PairingKeysParts {
            device_identity_public_key: h
                .pairing
                .recover_credential(
                    original
                        .credential_preparations
                        .iter()
                        .find(|credential| {
                            credential.scope.parts().purpose == CredentialPurpose::DeviceIdentity
                        })
                        .unwrap()
                        .generation,
                )
                .await
                .unwrap()
                .unwrap()
                .public_key,
            pairing_authentication_public_key: prepared_key.public_key,
        })
        .unwrap();
    let original_request = h
        .remote
        .supervisor
        .retained_owner_proposal(
            spaceterm_core::identity::ServerId::new(server.server_pin().identity),
            &keys,
        )
        .unwrap()
        .unwrap();
    let retry = h.candidate(2).await;
    let completion = h
        .provider
        .bootstrap(retry.confirm().unwrap(), retry)
        .await
        .unwrap();
    assert_eq!(completion.prepared.credential, prepared_key);
    assert_eq!(
        completion.prepared.local_pairing.parts().generation,
        authentication.generation
    );
    assert_eq!(
        h.pairing.inventory().await.unwrap().generation_high_water,
        original.generation_high_water
    );
    assert_eq!(
        h.remote
            .supervisor
            .retained_owner_proposal(
                spaceterm_core::identity::ServerId::new(server.server_pin().identity),
                &keys,
            )
            .unwrap()
            .unwrap(),
        original_request
    );
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 1);
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    h.remote.close().await;
}

struct SessionRemote {
    remote: Arc<Remote>,
    route: BootstrapRoute,
}
impl BootstrapRemoteConnection for SessionRemote {
    fn route(&self) -> BootstrapRoute {
        self.route
    }
    fn destination(&self) -> &str {
        self.remote.destination()
    }
    fn revalidate(&self) -> Result<(), BootstrapFailure> {
        self.remote.revalidate()
    }
    fn probe(&self) -> BootstrapProbeFuture<'_, Vec<u8>> {
        self.remote.probe()
    }
    fn account<'a>(
        &'a self,
        request: AccountRequest,
        artifact: Option<&'a VerifiedAccountServerArtifact>,
    ) -> PairingFuture<'a, Result<AccountReply, BootstrapFailure>> {
        self.remote.account(request, artifact)
    }
    fn owner_channel(
        &self,
    ) -> PairingFuture<'_, Result<Arc<dyn MessageTransport>, PairingFailure>> {
        self.remote.owner_channel()
    }
    fn prompt(&self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, BootstrapFailure>> {
        self.remote.prompt()
    }
    fn enable_linger<'a>(
        &'a self,
        invocation: &'a LingerInvocation,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> LingerFuture<'a, ()> {
        self.remote.enable_linger(invocation, authentication)
    }
}
struct RacingFactory {
    remote: Arc<Remote>,
    opened: AtomicUsize,
    delay_first: bool,
    first_opened: tokio::sync::Notify,
    release_first: tokio::sync::Notify,
    sessions: Mutex<Vec<std::sync::Weak<SessionRemote>>>,
}
impl RacingFactory {
    fn new(remote: Arc<Remote>, delay_first: bool) -> Self {
        Self {
            remote,
            opened: AtomicUsize::new(0),
            delay_first,
            first_opened: tokio::sync::Notify::new(),
            release_first: tokio::sync::Notify::new(),
            sessions: Mutex::new(Vec::new()),
        }
    }
}
impl BootstrapHostFactory for RacingFactory {
    fn open(
        &self,
        _: String,
        _: u64,
    ) -> PairingFuture<'_, Result<BootstrapHostConnection, BootstrapFailure>> {
        Box::pin(async move {
            let session = self.opened.fetch_add(1, Ordering::SeqCst) + 1;
            let connection = Arc::new(SessionRemote {
                remote: self.remote.clone(),
                route: BootstrapRoute::capture(
                    [session as u8; 32],
                    [2; 32],
                    [3; 32],
                    self.remote.supervisor.key().account_uid,
                    BuildChannel::Development,
                )
                .unwrap(),
            });
            self.sessions
                .lock()
                .unwrap()
                .push(Arc::downgrade(&connection));
            if session == 1 && self.delay_first {
                self.first_opened.notify_one();
                self.release_first.notified().await;
            }
            Ok(BootstrapHostConnection {
                connection,
                artifact: None,
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_canceled_same_generation_reverse_inspection_preserves_other_sheet() {
    let h = Harness::new().await;
    h.install_existing().await;
    let factory = Arc::new(RacingFactory::new(h.remote.clone(), true));
    let provider = Arc::new(ProductionSshBootstrap::new(
        h.pairing.clone(),
        factory.clone(),
    ));
    let a_valid = Arc::new(AtomicBool::new(true));
    let a_provider = provider.clone();
    let a_scope = a_valid.clone();
    let a = tokio::spawn(async move {
        a_provider
            .inspect_with_pending_foreground("test-account".into(), 1, a_scope)
            .await
    });
    factory.first_opened.notified().await;
    let b_valid = Arc::new(AtomicBool::new(true));
    let (facts, pending) = provider
        .inspect_with_pending_foreground("test-account".into(), 1, b_valid.clone())
        .await
        .unwrap();
    assert!(pending.is_none());
    a_valid.store(false, Ordering::Release);
    factory.release_first.notify_one();
    assert!(matches!(
        a.await.unwrap(),
        Err(BootstrapFailure {
            class: BootstrapFailureClass::Pairing(PairingFailure::Cancelled),
            ..
        })
    ));
    provider.bind_foreground(facts.route, 1, b_valid).unwrap();
    let intent = ForegroundBootstrap::confirm_observed(facts, 1).unwrap();
    let completion = provider.bootstrap(intent, facts).await.unwrap();
    assert_eq!(
        completion.outcome.server.pin,
        h.remote
            .supervisor
            .discover()
            .unwrap()
            .unwrap()
            .server_pin()
    );
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 1);
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    h.remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_canceled_inspection_releases_session_without_provider_call() {
    let h = Harness::new().await;
    h.install_existing().await;
    let factory = Arc::new(RacingFactory::new(h.remote.clone(), false));
    let provider = ProductionSshBootstrap::new(h.pairing.clone(), factory.clone());
    let valid = Arc::new(AtomicBool::new(true));
    provider
        .inspect_with_pending_foreground("test-account".into(), 1, valid.clone())
        .await
        .unwrap();
    let session = factory.sessions.lock().unwrap()[0].clone();
    assert!(session.upgrade().is_some());
    valid.store(false, Ordering::Release);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while session.upgrade().is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the retained session must drop without another provider operation");
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_stopped_disabled_linger_explicit_start_uses_scoped_exchange() {
    let h = Harness::new().await;
    h.stop_existing().await;
    h.remote.manager.linux.store(true, Ordering::SeqCst);
    h.remote
        .manager
        .linger_enabled
        .store(false, Ordering::SeqCst);
    h.remote.prompt_allowed.store(true, Ordering::SeqCst);
    let original = h.remote.supervisor.discover().unwrap().unwrap();
    let candidate = h.candidate(1).await;
    assert!(matches!(
        candidate.confirm(),
        Err(EnrollmentFailure::Pairing(
            PairingFailure::ExplicitStartRequired
        ))
    ));
    assert_eq!(h.remote.prompt_count.load(Ordering::SeqCst), 0);
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    h.provider.start(candidate).await.unwrap();
    assert_eq!(h.remote.prompt_count.load(Ordering::SeqCst), 1);
    assert_eq!(h.remote.linger_invocations.load(Ordering::SeqCst), 1);
    assert!(h.remote.manager.linger_enabled.load(Ordering::SeqCst));
    assert_eq!(
        h.remote.manager.effects.lock().unwrap().as_slice(),
        &["configure", "start"]
    );
    let started = h.remote.supervisor.discover().unwrap().unwrap();
    assert_eq!(started.desired_state(), DesiredServerState::Running);
    assert_eq!(started.server_pin(), original.server_pin());
    assert_eq!(started.data_directory(), original.data_directory());
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
    assert!(h.pairing.inventory().await.unwrap().saved.is_empty());
    assert_eq!(
        h.remote
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| **r == "explicit-start")
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_stopped_disabled_linger_denial_preserves_stopped_registration() {
    let h = Harness::new().await;
    h.stop_existing().await;
    h.remote.manager.linux.store(true, Ordering::SeqCst);
    h.remote
        .manager
        .linger_enabled
        .store(false, Ordering::SeqCst);
    let original = h.remote.supervisor.discover().unwrap().unwrap();
    let candidate = h.candidate(1).await;
    assert!(matches!(
        h.provider.start(candidate).await,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::AuthorizationDenied)
        ))
    ));
    let stopped = h.remote.supervisor.discover().unwrap().unwrap();
    assert_eq!(stopped.desired_state(), DesiredServerState::Stopped);
    assert_eq!(stopped.server_pin(), original.server_pin());
    assert_eq!(stopped.generation(), original.generation());
    assert!(!h.remote.manager.linger_enabled.load(Ordering::SeqCst));
    assert_eq!(h.remote.prompt_count.load(Ordering::SeqCst), 1);
    assert_eq!(h.remote.linger_invocations.load(Ordering::SeqCst), 1);
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    assert!(
        !h.remote
            .requests
            .lock()
            .unwrap()
            .contains(&"explicit-start")
    );
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_initial_starting_completes_original_request_without_second_start() {
    assert_initial_starting_completion(false).await;
}

#[tokio::test]
async fn desktop_ssh_bootstrap_native_five_second_pending_completes_original_at_six_seconds() {
    assert_initial_starting_completion(true).await;
}

#[tokio::test]
async fn desktop_ssh_bootstrap_expired_initial_deadline_defers_original_recovery_without_enrolling()
{
    use spaceterm_host::account_service::{ServicePhase, ServiceRequestState};

    let h = Harness::new().await;
    h.remote.manager.delayed_start.store(true, Ordering::SeqCst);
    *h.remote.install_reply_latency.lock().unwrap() = Some(std::time::Duration::from_secs(1));
    let candidate = h.candidate(1).await;
    tokio::time::pause();
    let original = Mutex::new(None);
    let transition = async {
        h.remote.manager.start_requested.notified().await;
        h.remote.manager.starting_observed.notified().await;
        // Admission is the startup boundary, not the earlier credential preflight.
        let started = tokio::time::Instant::now();
        let requests = h
            .remote
            .supervisor
            .retained_initial_install_requests()
            .unwrap();
        assert_eq!(requests.len(), 1);
        let request = requests[0].clone();
        let (_, artifact) = h
            .remote
            .supervisor
            .inspect_install_request(request.request)
            .unwrap();
        tokio::time::sleep_until(started + std::time::Duration::from_secs(5)).await;
        let at_native_bound = h
            .remote
            .supervisor
            .inspect_install_request(request.request)
            .unwrap()
            .0;
        assert_eq!(at_native_bound.request_digest, request.request_digest);
        assert_eq!(at_native_bound.phase, ServicePhase::Starting);
        assert_eq!(at_native_bound.state, ServiceRequestState::Pending);
        // Compare against the same fifteen-second timer edge, including Tokio's
        // timer quantization, rather than permitting an arbitrary extra duration.
        tokio::time::sleep_until(started + std::time::Duration::from_secs(15)).await;
        *original.lock().unwrap() = Some((request, artifact, tokio::time::Instant::now()));
        tokio::time::sleep_until(started + std::time::Duration::from_secs(16)).await;
        h.remote.manager.running.store(true, Ordering::SeqCst);
        h.remote.manager.starting.store(false, Ordering::SeqCst);
        // The Pending reply crosses the fifteen-second deadline. Let its detached
        // receiver finish so a post-deadline automatic Resolve cannot escape this test.
        tokio::time::sleep_until(started + std::time::Duration::from_secs(17)).await;
    };
    let observe = async {
        let result = h
            .provider
            .0
            .bootstrap(candidate.confirm().unwrap(), candidate.facts())
            .await;
        (result, tokio::time::Instant::now())
    };
    let ((result, returned), ()) =
        tokio::time::timeout(std::time::Duration::from_secs(300), async {
            tokio::join!(observe, transition)
        })
        .await
        .unwrap();
    assert!(matches!(
        result,
        Err(BootstrapFailure {
            class: BootstrapFailureClass::Pairing(PairingFailure::RecoveryRequired),
            phase: BootstrapPhase::Applied,
        })
    ));
    let (original, artifact, expired) = original.into_inner().unwrap().unwrap();
    assert_eq!(returned, expired);
    let (pending, retained_artifact) = h
        .remote
        .supervisor
        .inspect_install_request(original.request)
        .unwrap();
    assert_eq!(pending.request, original.request);
    assert_eq!(pending.request_digest, original.request_digest);
    assert_eq!(pending.phase, ServicePhase::Starting);
    assert_eq!(pending.state, ServiceRequestState::Pending);
    assert_eq!(retained_artifact, artifact);
    let requests = h.remote.requests.lock().unwrap().clone();
    assert_eq!(requests.iter().filter(|r| **r == "reserve").count(), 1);
    assert_eq!(requests.iter().filter(|r| **r == "install").count(), 1);
    assert!(!requests.contains(&"resolve"));
    assert!(!requests.contains(&"reserve-owner"));
    assert!(!requests.contains(&"authorize-owner"));
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 0);
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
    assert_eq!(h.credentials.0.lock().unwrap().len(), 1);
    let inventory = h.pairing.inventory().await.unwrap();
    assert!(inventory.prepared.is_empty());
    assert!(inventory.saved.is_empty());
    assert!(inventory.enrollment_acknowledgements.is_empty());
    assert!(h.journal.snapshot().unwrap().bindings().is_empty());
    tokio::time::resume();

    // Native install recovery is an explicit observation of this same original.
    // ProductionSshBootstrap.resolve retains its narrower owner-enrollment authority.
    let recovered = AccountBootstrapSetup::new((*h.remote.supervisor).clone())
        .resolve(original.request, artifact)
        .await
        .unwrap();
    assert!(matches!(
        recovered.registration,
        spaceterm_host::bootstrap::setup::BootstrapAccountRegistration::Running {
            artifact_identity,
            ..
        } if artifact_identity == artifact
    ));
    let (finished, retained_artifact) = h
        .remote
        .supervisor
        .inspect_install_request(original.request)
        .unwrap();
    assert_eq!(finished.request, original.request);
    assert_eq!(finished.request_digest, original.request_digest);
    assert_eq!(finished.phase, ServicePhase::Completed);
    assert!(matches!(finished.state, ServiceRequestState::Completed(_)));
    assert_eq!(retained_artifact, artifact);
    assert_eq!(
        h.remote
            .supervisor
            .retained_initial_install_requests()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        h.remote.manager.effects.lock().unwrap().as_slice(),
        &["configure", "start"]
    );
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 0);
    assert_eq!(h.credentials.0.lock().unwrap().len(), 1);
    assert!(h.journal.snapshot().unwrap().bindings().is_empty());
}

async fn assert_initial_starting_completion(after_native_bound: bool) {
    let h = Harness::new().await;
    h.remote.manager.delayed_start.store(true, Ordering::SeqCst);
    let candidate = h.candidate(1).await;
    if after_native_bound {
        tokio::time::pause();
    }
    let original = Mutex::new(None);
    let transition = async {
        h.remote.manager.start_requested.notified().await;
        h.remote.manager.starting_observed.notified().await;
        let requests = h
            .remote
            .supervisor
            .retained_initial_install_requests()
            .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].phase,
            spaceterm_host::account_service::ServicePhase::Starting
        );
        assert_eq!(
            requests[0].state,
            spaceterm_host::account_service::ServiceRequestState::Pending
        );
        *original.lock().unwrap() = Some(requests[0].clone());
        assert_eq!(
            h.remote.manager.effects.lock().unwrap().as_slice(),
            &["configure", "start"]
        );
        if after_native_bound {
            let started = tokio::time::Instant::now();
            tokio::time::sleep_until(started + std::time::Duration::from_secs(5)).await;
            let at_native_bound = h
                .remote
                .supervisor
                .inspect_install_request(requests[0].request)
                .unwrap()
                .0;
            assert_eq!(at_native_bound.request, requests[0].request);
            assert_eq!(at_native_bound.request_digest, requests[0].request_digest);
            assert_eq!(
                at_native_bound.phase,
                spaceterm_host::account_service::ServicePhase::Starting
            );
            assert_eq!(
                at_native_bound.state,
                spaceterm_host::account_service::ServiceRequestState::Pending
            );
            assert_eq!(
                h.remote.manager.effects.lock().unwrap().as_slice(),
                &["configure", "start"]
            );
            tokio::time::sleep_until(started + std::time::Duration::from_secs(6)).await;
        }
        // The same accepted request becomes Running without another native Start.
        h.remote.manager.running.store(true, Ordering::SeqCst);
        h.remote.manager.starting.store(false, Ordering::SeqCst);
        if after_native_bound {
            // Real owner IPC/enrollment follows the controlled startup schedule.
            tokio::time::resume();
        }
    };
    // This watchdog covers durable installation and enrollment as well as
    // startup. Native observation is bounded at five seconds; Bootstrap keeps one fifteen-second deadline.
    let (completion, ()) = tokio::time::timeout(std::time::Duration::from_secs(300), async {
        tokio::join!(
            h.provider
                .bootstrap(candidate.confirm().unwrap(), candidate),
            transition
        )
    })
    .await
    .unwrap();
    let completion = completion.unwrap();
    let original = original.into_inner().unwrap().unwrap();
    let (finished, artifact) = h
        .remote
        .supervisor
        .inspect_install_request(original.request)
        .unwrap();
    assert_eq!(finished.request, original.request);
    assert_eq!(finished.request_digest, original.request_digest);
    assert_eq!(
        finished.phase,
        spaceterm_host::account_service::ServicePhase::Completed
    );
    assert!(matches!(
        finished.state,
        spaceterm_host::account_service::ServiceRequestState::Completed(_)
    ));
    assert_eq!(
        artifact,
        completion.outcome.server.approved_artifact_identity
    );
    assert_eq!(
        h.remote.manager.effects.lock().unwrap().as_slice(),
        &["configure", "start"]
    );
    assert_eq!(
        h.remote
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| **r == "install")
            .count(),
        1
    );
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 1);
    assert_eq!(h.journal.snapshot().unwrap().bindings().len(), 1);
    assert!(
        h.journal
            .snapshot()
            .unwrap()
            .pairing()
            .acknowledgements()
            .is_empty()
    );
    h.remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_configuration_change_at_binding_rejects_alternate_capture() {
    let h = Harness::new().await;
    h.install_existing().await;
    let CapturedProvider {
        provider,
        configuration,
        tunnel_started,
        ..
    } = captured_provider(&h);
    let alternate = "Host st-route-0\n hostname alternate.example\n user other\n port 2222\n";
    let attempt = EnrollmentAttempt::new(1);
    let (facts, pending) = provider
        .inspect("test-account".into(), attempt.clone())
        .await
        .unwrap();
    assert!(pending.is_none());
    let candidate = SshCandidate::new(attempt, facts).unwrap();
    // Change the actual local configuration after the single-use delivery and
    // before prepare_binding. The original authenticated account stays fixed.
    *h.remote.change_config_on_owner_frame.lock().unwrap() =
        Some((2, configuration.clone(), alternate.into()));
    assert!(matches!(
        provider
            .bootstrap(candidate.confirm().unwrap(), candidate)
            .await,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::RouteChanged)
        ))
    ));
    assert!(!tunnel_started.exists());
    assert_eq!(std::fs::read_to_string(&configuration).unwrap(), alternate);
    let state = h.journal.snapshot().unwrap();
    assert!(state.bindings().is_empty());
    assert!(state.pairing().prepared().is_empty());
    assert!(
        state
            .pairing()
            .slots()
            .iter()
            .all(|slot| slot.ssh_capture.is_none() && slot.route.is_none())
    );
    // Only the owner prefix was sent. No hello, proof or enrollment submission
    // can authorize a different SSH configuration after binding rejection.
    assert_eq!(h.remote.owner_client_frames.load(Ordering::SeqCst), 1);
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 1);
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    h.remote.close().await;
}

struct CapturedProvider {
    provider: super::super::bootstrap::NativeForegroundBootstrap,
    configuration: PathBuf,
    original: spaceterm_client::journal::JournalSshCapture,
    tunnel_started: PathBuf,
    connector: Arc<spaceterm_client_native::restricted_routes::NativePairingConnector>,
}
fn captured_provider(h: &Harness) -> CapturedProvider {
    use spaceterm_client_native::{
        restricted_routes::NativePairingConnector,
        route::{NativeRouteConnector, SshRouteConfiguration, TunnelEndpoints},
    };
    use spaceterm_host::ipc::PrivateIpcDirectory;
    use std::os::unix::fs::PermissionsExt;
    let directory = h.root.path().join("ssh-boundary");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let executable = directory.join("ssh-boundary");
    let configuration = directory.join("config");
    let tunnel_started = directory.join("tunnel-started");
    std::fs::write(
        &executable,
        r#"#!/usr/bin/python3 -I
import pathlib, sys
if '-G' in sys.argv:
    configuration = pathlib.Path(sys.argv[sys.argv.index('-F') + 1]).read_text()
    for line in configuration.splitlines():
        line = line.strip()
        if line.lower().startswith('host '):
            continue
        if line:
            print(line)
    sys.exit(0)
pathlib.Path(__file__).with_name('tunnel-started').touch()
sys.exit(1)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let original = spaceterm_client::journal::JournalSshCapture::new(
        "Host st-route-0\n hostname original.example\n user owner\n port 22\n".into(),
    )
    .unwrap();
    std::fs::write(&configuration, original.configuration()).unwrap();
    let ssh = SshRouteConfiguration::new(
        executable,
        configuration.clone(),
        h.root.path().to_owned(),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
        TunnelEndpoints::new(PrivateIpcDirectory::open(&directory).unwrap()).unwrap(),
        None,
    )
    .unwrap();
    let routes = Arc::new(NativeRouteConnector::new(Vec::new(), Some(ssh)).unwrap());
    let connector = Arc::new(NativePairingConnector::with_shared_routes(
        h.journal.clone(),
        routes,
    ));
    let pairing = Arc::new(NativePairingRuntime::with_adapters(
        h.journal.clone(),
        Arc::new(h.credentials.clone()),
        connector.clone(),
        connector.clone(),
        Arc::new(Invalidator),
        HandshakeConfig::default(),
    ));
    *h.remote.ssh_capture.lock().unwrap() = Some((configuration.clone(), original.clone()));
    let provider = super::super::bootstrap::NativeForegroundBootstrap(ProductionSshBootstrap::new(
        pairing,
        Arc::new(Factory {
            remote: h.remote.clone(),
        }),
    ));
    CapturedProvider {
        provider,
        configuration,
        original,
        tunnel_started,
        connector,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_persists_original_capture_for_native_shared_restricted_route() {
    let h = Harness::new().await;
    h.install_existing().await;
    let CapturedProvider {
        provider,
        configuration,
        original,
        tunnel_started,
        connector,
    } = captured_provider(&h);
    let attempt = EnrollmentAttempt::new(1);
    let (facts, pending) = provider
        .inspect("test-account".into(), attempt.clone())
        .await
        .unwrap();
    assert!(pending.is_none());
    let candidate = SshCandidate::new(attempt, facts).unwrap();
    let completion = provider
        .bootstrap(candidate.confirm().unwrap(), candidate)
        .await
        .unwrap();
    let state = h.journal.snapshot().unwrap();
    assert_eq!(
        state
            .pairing()
            .slot(completion.prepared.credential.scope.reference())
            .unwrap()
            .ssh_capture
            .as_ref(),
        Some(&original)
    );
    let alternate = "Host st-route-0\n hostname alternate.example\n user other\n port 2222\n";
    std::fs::write(&configuration, alternate).unwrap();
    // This is the real shared native connector. Its -G process boundary reads
    // actual config bytes. A changed account/endpoint cannot launch a tunnel.
    assert!(matches!(
        EnrollmentConnector::open(
            connector.as_ref(),
            completion.prepared.pin,
            completion.prepared.route
        )
        .await,
        Err(PairingFailure::RouteChanged)
    ));
    assert!(!tunnel_started.exists());
    assert_eq!(
        h.journal
            .snapshot()
            .unwrap()
            .pairing()
            .slot(completion.prepared.credential.scope.reference())
            .unwrap()
            .ssh_capture
            .as_ref(),
        Some(&original)
    );
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 1);
    h.remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_lost_ack_reopens_after_failed_channel_retirement() {
    let h = Harness::new().await;
    h.install_existing().await;
    let candidate = h.candidate(1).await;
    h.remote.lost_ack_channels.store(1, Ordering::SeqCst);
    let completion = h
        .provider
        .bootstrap(candidate.confirm().unwrap(), candidate)
        .await
        .unwrap();
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 2);
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 1);
    let inventory = h.pairing.inventory().await.unwrap();
    assert!(inventory.prepared.is_empty());
    assert!(inventory.enrollment_acknowledgements.is_empty());
    assert_eq!(inventory.saved, vec![completion.saved]);
    {
        let requests = h.remote.requests.lock().unwrap();
        assert_eq!(
            requests.iter().filter(|r| **r == "reserve-owner").count(),
            1
        );
        assert_eq!(
            requests.iter().filter(|r| **r == "authorize-owner").count(),
            1
        );
    }
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    h.remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_configuration_change_after_ready_never_submits_enrollment() {
    let h = Harness::new().await;
    h.install_existing().await;
    let CapturedProvider {
        provider,
        configuration,
        original,
        tunnel_started,
        ..
    } = captured_provider(&h);
    let attempt = EnrollmentAttempt::new(1);
    let (facts, pending) = provider
        .inspect("test-account".into(), attempt.clone())
        .await
        .unwrap();
    assert!(pending.is_none());
    let candidate = SshCandidate::new(attempt, facts).unwrap();
    let alternate = "Host st-route-0\n hostname alternate.example\n user other\n port 2222\n";
    *h.remote.change_config_on_owner_frame.lock().unwrap() =
        Some((4, configuration.clone(), alternate.into()));
    assert!(matches!(
        provider
            .bootstrap(candidate.confirm().unwrap(), candidate)
            .await,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::RouteChanged)
        ))
    ));
    assert_eq!(std::fs::read_to_string(&configuration).unwrap(), alternate);
    assert_eq!(h.remote.owner_client_frames.load(Ordering::SeqCst), 3);
    assert_eq!(h.remote.owner_authorizations.load(Ordering::SeqCst), 1);
    assert!(!tunnel_started.exists());
    let state = h.journal.snapshot().unwrap();
    assert!(state.bindings().is_empty());
    assert_eq!(state.pairing().prepared().len(), 1);
    let captures = state
        .pairing()
        .slots()
        .iter()
        .filter_map(|slot| slot.ssh_capture.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(captures, vec![&original]);
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    h.remote.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_stopped_registration_change_during_prompt_blocks_password_and_start()
{
    let h = Harness::new().await;
    h.stop_existing().await;
    h.remote.manager.linux.store(true, Ordering::SeqCst);
    h.remote
        .manager
        .linger_enabled
        .store(false, Ordering::SeqCst);
    h.remote.prompt_allowed.store(true, Ordering::SeqCst);
    let original = h.remote.supervisor.discover().unwrap().unwrap();
    let candidate = h.candidate(1).await;
    h.remote.start_during_prompt.store(true, Ordering::SeqCst);
    assert!(matches!(
        h.provider.start(candidate).await,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::RouteChanged)
        ))
    ));
    let changed = h.remote.supervisor.discover().unwrap().unwrap();
    assert_eq!(changed.server_pin(), original.server_pin());
    assert_ne!(changed.generation(), original.generation());
    assert_eq!(changed.desired_state(), DesiredServerState::Running);
    assert_eq!(h.remote.prompt_count.load(Ordering::SeqCst), 1);
    assert_eq!(h.remote.password_deliveries.load(Ordering::SeqCst), 0);
    let requests = h.remote.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| **r == "concurrent-start")
            .count(),
        1
    );
    assert!(!requests.contains(&"explicit-start"));
    assert_eq!(
        h.remote.manager.effects.lock().unwrap().as_slice(),
        &["configure", "start"]
    );
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_ssh_bootstrap_activation_after_final_inspection_rejects_stale_start() {
    use spaceterm_host::account_service::{ServiceOutcome, ServiceRequestState};

    let h = Harness::new().await;
    h.stop_existing().await;
    let original = h.remote.supervisor.discover().unwrap().unwrap();
    let candidate = h.candidate(1).await;
    let gate = Arc::new(StartAdmissionGate::default());
    *h.remote.before_start_admission.lock().unwrap() = Some(gate.clone());
    let start = h.provider.start(candidate);
    tokio::pin!(start);
    let failure = tokio::time::timeout(std::time::Duration::from_secs(300), async {
        tokio::select! {
            _ = gate.inspected.notified() => (),
            _ = &mut start => panic!("Start must wait at the post-inspection native admission boundary"),
        }
        let (_, artifact) = signed::fixture("9.0.1", 7, signed::native_machine());
        let staged = h.remote.supervisor.stage_installation(
            h.remote.supervisor.allocate_request().unwrap(),
            h.remote.fixture.confirm_install(artifact),
        ).await.unwrap();
        let ServiceRequestState::Completed(ServiceOutcome::Staged(staged)) = staged.state else {
            panic!("the newer artifact must remain staged before activation");
        };
        let activator = h.remote.fixture.with_installation_trust(
            h.remote.fixture.with_providers(
                h.remote.manager.clone(),
                Arc::new(spaceterm_host::account_service::runtime_inventory::RetainedRuntimeProcesses),
                Arc::new(Clock),
                None,
            ),
            h.remote.manager.key.clone(),
        );
        let activation = activator.assess(
            activator.allocate_request().unwrap(),
            ServiceAction::Activate(staged.installation),
        ).await.unwrap().confirm().unwrap();
        activator.activate(activation).await.unwrap();
        let changed = h.remote.supervisor.discover().unwrap().unwrap();
        assert_eq!(changed.server_pin(), original.server_pin());
        assert!(changed.generation() > original.generation());
        assert_eq!(changed.desired_state(), DesiredServerState::Stopped);
        assert_eq!(changed.approved_installation(), Some(&staged));
        assert_ne!(changed.approved_installation(), original.approved_installation());
        h.remote.manager.effects.lock().unwrap().clear();
        gate.admit.notify_one();
        start.await
    }).await.unwrap();
    assert!(matches!(
        failure,
        Err(EnrollmentFailure::Bootstrap(
            BootstrapFailureClass::Pairing(PairingFailure::RouteChanged)
        ))
    ));
    assert_eq!(
        h.remote
            .supervisor
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
    assert!(h.remote.manager.effects.lock().unwrap().is_empty());
    assert!(!h.remote.manager.running.load(Ordering::SeqCst));
    assert_eq!(h.remote.owners.load(Ordering::SeqCst), 0);
    assert!(h.journal.snapshot().unwrap().bindings().is_empty());
    h.remote.close().await;
}
