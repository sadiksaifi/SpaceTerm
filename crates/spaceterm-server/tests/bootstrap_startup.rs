#![cfg(all(
    feature = "test-support",
    any(target_os = "macos", target_os = "linux")
))]

#[path = "../../spaceterm-host/tests/support/signed_installation.rs"]
mod signed_installation;

use spaceterm_distribution::UpdateKey;
use spaceterm_host::{
    account_service::{
        LingerState, NativeServiceInstallations, NativeServiceProcesses, RegisteredServer,
        ServiceClock, ServiceFailure, ServiceFuture, ServiceManager, ServicePhase,
        ServiceRequestState, ServiceSupervisor, ServiceTime, SupervisorState,
        test_support::AccountServiceFixture,
    },
    bootstrap::{
        setup::{AccountBootstrapSetup, BootstrapAccountRegistration},
        wire::{AccountReply, AccountRequest},
    },
    startup::BuildIdentity,
    transport::{MessageTransport, PeerFacts, TransportFuture, TransportRoute},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Default)]
struct ConfigureGate {
    entered: tokio::sync::Notify,
    released: tokio::sync::Notify,
}

struct Manager {
    key: UpdateKey,
    configure_gate: Option<Arc<ConfigureGate>>,
    observations: AtomicUsize,
    running_at: Mutex<Option<tokio::time::Instant>>,
    startup_delay: Duration,
    startup_observed: tokio::sync::Notify,
    effects: Mutex<Vec<&'static str>>,
}
impl ServiceManager for Manager {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async move {
            self.observations.fetch_add(1, Ordering::Relaxed);
            match *self.running_at.lock().unwrap() {
                None => Ok(SupervisorState::Inactive),
                Some(until) if tokio::time::Instant::now() < until => {
                    self.startup_observed.notify_one();
                    Ok(SupervisorState::Starting)
                }
                Some(_) => Ok(SupervisorState::Running),
            }
        })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async { Ok(cfg!(target_os = "linux").then_some(LingerState::Enabled)) })
    }
    fn configure<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.verify_installation(registration)?;
            self.effects.lock().unwrap().push("configure");
            if let Some(gate) = &self.configure_gate {
                gate.entered.notify_one();
                gate.released.notified().await;
            }
            Ok(())
        })
    }
    fn start<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.effects.lock().unwrap().push("start");
            *self.running_at.lock().unwrap() =
                Some(tokio::time::Instant::now() + self.startup_delay);
            Ok(())
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { panic!("Bootstrap observation must not Stop") })
    }
    fn uninstall<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { panic!("Bootstrap observation must not uninstall") })
    }
    fn verify_installation(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        NativeServiceInstallations::test_approved_executable(registration, self.key.clone())
            .map(drop)
    }
}
struct Clock;
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: [1; 32],
            millis: 0,
        })
    }
}
fn account_supervisor(account: &AccountServiceFixture, manager: Arc<Manager>) -> ServiceSupervisor {
    let processes = Arc::new(
        NativeServiceProcesses::current(Arc::new(NativeServiceInstallations))
            .unwrap()
            .with_manager(manager.clone()),
    );
    account.with_installation_trust(
        account.with_providers(manager.clone(), processes, Arc::new(Clock), None),
        manager.key.clone(),
    )
}
fn root() -> tempfile::TempDir {
    let account = spaceterm_host::account_service::TrustedAccountFacts::current().unwrap();
    let parent = if cfg!(target_os = "macos") {
        std::path::Path::new("/private/tmp")
    } else {
        account.home()
    };
    tempfile::tempdir_in(parent).unwrap()
}
struct Channel {
    request: Mutex<Option<Vec<u8>>>,
    reply: Mutex<Option<Vec<u8>>>,
}
impl MessageTransport for Channel {
    fn peer_facts(&self) -> PeerFacts {
        PeerFacts {
            route: TransportRoute::PrivateIpc,
            local_uid: None,
            local_pid: None,
            pinned_server_key: None,
        }
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            assert!(self.reply.lock().unwrap().replace(bytes.to_vec()).is_none());
            Ok(())
        })
    }
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.request.lock().unwrap().take()) })
    }
    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
async fn resolve_rpc(
    supervisor: &ServiceSupervisor,
    request: spaceterm_host::native_deployment::contracts::NativeRequestId,
    artifact: [u8; 32],
) -> AccountReply {
    let channel = Channel {
        request: Mutex::new(Some(
            serde_json::to_vec(&AccountRequest::Resolve { request, artifact }).unwrap(),
        )),
        reply: Mutex::new(None),
    };
    spaceterm_server::deployment::bootstrap::execute(Arc::new(supervisor.clone()), &channel)
        .await
        .unwrap();
    serde_json::from_slice(channel.reply.lock().unwrap().as_ref().unwrap()).unwrap()
}

#[tokio::test(start_paused = true)]
async fn bootstrap_account_rpc_resolves_original_running_at_six_seconds_without_start_replay() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let root = root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) = signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
        let digest = artifact.artifact_sha256();
        let manager = Arc::new(Manager { key, configure_gate: None, observations: AtomicUsize::new(0), running_at: Mutex::new(None), startup_delay: Duration::from_secs(6), startup_observed: tokio::sync::Notify::new(), effects: Mutex::new(Vec::new()) });
        let supervisor = account_supervisor(&account, manager.clone());
        let request = supervisor.allocate_request().unwrap();
        let started = tokio::time::Instant::now();
        let original = supervisor.install(request, account.confirm_install(artifact)).await.unwrap();
        assert_eq!(started.elapsed(), Duration::from_secs(5));
        assert_eq!(original.phase, ServicePhase::Starting);
        assert_eq!(original.state, ServiceRequestState::Pending);
        assert!(matches!(resolve_rpc(&supervisor, request, digest).await, AccountReply::Failure(ServiceFailure::Pending)));
        tokio::time::sleep_until(started + Duration::from_secs(6)).await;
        let reopened = account_supervisor(&account, manager.clone());
        let AccountReply::Observation(observed) = resolve_rpc(&reopened, request, digest).await else { panic!("the RPC must resolve the original accepted service") };
        let registration = reopened.discover().unwrap().unwrap();
        assert!(matches!(observed.facts.registration, BootstrapAccountRegistration::Running { pin, artifact_identity, .. } if pin == registration.server_pin() && artifact_identity == digest));
        assert_eq!(observed.remote_socket.as_deref(), registration.endpoint().unwrap().path().to_str());
        let completed = reopened.inspect_install_request(request).unwrap().0;
        assert_eq!(completed.request, original.request);
        assert_eq!(completed.request_digest, original.request_digest);
        assert_eq!(completed.phase, ServicePhase::Completed);
        assert!(matches!(resolve_rpc(&reopened, request, digest).await, AccountReply::Observation(_)));
        assert_eq!(manager.effects.lock().unwrap().as_slice(), ["configure", "start"]);
    }).await.expect("the complete native wait and RPC recovery must remain bounded");
}

#[tokio::test(start_paused = true)]
async fn bootstrap_account_rpc_retains_same_original_after_overall_fifteen_second_deadline() {
    tokio::time::timeout(Duration::from_secs(25), async {
        let root = root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) =
            signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
        let digest = artifact.artifact_sha256();
        let manager = Arc::new(Manager {
            key,
            configure_gate: None,
            observations: AtomicUsize::new(0),
            running_at: Mutex::new(None),
            startup_delay: Duration::from_secs(16),
            startup_observed: tokio::sync::Notify::new(),
            effects: Mutex::new(Vec::new()),
        });
        let supervisor = account_supervisor(&account, manager.clone());
        let setup = AccountBootstrapSetup::new(supervisor.clone());
        let request = setup.reserve_request().unwrap();
        let started = tokio::time::Instant::now();
        assert_eq!(
            setup
                .install_and_observe(request, account.confirm_install(artifact), digest)
                .await,
            Err(ServiceFailure::Pending)
        );
        assert_eq!(started.elapsed(), Duration::from_secs(15));
        let original = supervisor.inspect_install_request(request).unwrap();
        assert_eq!(original.0.phase, ServicePhase::Starting);
        assert_eq!(original.0.state, ServiceRequestState::Pending);
        assert_eq!(original.1, digest);
        let journal = std::fs::read(account.anchor().join("requests")).unwrap();
        assert!(matches!(
            resolve_rpc(&supervisor, request, digest).await,
            AccountReply::Failure(ServiceFailure::Pending)
        ));
        assert_eq!(
            supervisor.inspect_install_request(request).unwrap(),
            original
        );
        assert_eq!(
            std::fs::read(account.anchor().join("requests")).unwrap(),
            journal
        );
        tokio::time::sleep_until(started + Duration::from_secs(16)).await;
        assert!(matches!(
            resolve_rpc(&supervisor, request, digest).await,
            AccountReply::Observation(_)
        ));
        assert_eq!(
            supervisor
                .inspect_install_request(request)
                .unwrap()
                .0
                .request_digest,
            original.0.request_digest
        );
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            ["configure", "start"]
        );
    })
    .await
    .expect("the overall deadline and later original RPC recovery must remain bounded");
}

#[tokio::test(start_paused = true)]
async fn bootstrap_account_rpc_refuses_changed_approval_without_adopting_another_registration() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let root = root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) =
            signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
        let digest = artifact.artifact_sha256();
        let manager = Arc::new(Manager {
            key,
            configure_gate: None,
            observations: AtomicUsize::new(0),
            running_at: Mutex::new(None),
            startup_delay: Duration::from_secs(6),
            startup_observed: tokio::sync::Notify::new(),
            effects: Mutex::new(Vec::new()),
        });
        let supervisor = account_supervisor(&account, manager.clone());
        let request = supervisor.allocate_request().unwrap();
        let original = supervisor
            .install(request, account.confirm_install(artifact))
            .await
            .unwrap();
        let path = account.anchor().join("registration");
        let mut registration: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        registration["approved"]["artifact_sha256"] = serde_json::json!(vec![9; 32]);
        registration.sort_all_objects();
        std::fs::write(path, serde_json::to_vec(&registration).unwrap()).unwrap();
        assert!(matches!(
            resolve_rpc(&supervisor, request, digest).await,
            AccountReply::Failure(ServiceFailure::RequestConflict)
        ));
        assert_eq!(
            supervisor.inspect_install_request(request).unwrap().0,
            original
        );
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            ["configure", "start"]
        );
    })
    .await
    .expect("changed approval must refuse the original RPC without effects");
}

#[tokio::test(start_paused = true)]
async fn bootstrap_account_rpc_recovers_canceled_observer_without_resubmitting_native_start() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let root = root();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let (key, artifact) =
            signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
        let digest = artifact.artifact_sha256();
        let manager = Arc::new(Manager {
            key,
            configure_gate: None,
            observations: AtomicUsize::new(0),
            running_at: Mutex::new(None),
            startup_delay: Duration::from_secs(6),
            startup_observed: tokio::sync::Notify::new(),
            effects: Mutex::new(Vec::new()),
        });
        let supervisor = account_supervisor(&account, manager.clone());
        let setup = AccountBootstrapSetup::new(supervisor.clone());
        let request = setup.reserve_request().unwrap();
        let confirmation = account.confirm_install(artifact);
        let started = tokio::time::Instant::now();
        let observer = tokio::spawn(async move {
            setup
                .install_and_observe(request, confirmation, digest)
                .await
        });
        manager.startup_observed.notified().await;
        let original = supervisor.inspect_install_request(request).unwrap();
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        tokio::time::sleep_until(started + Duration::from_secs(5)).await;
        assert_eq!(
            supervisor.inspect_install_request(request).unwrap(),
            original
        );
        tokio::time::sleep_until(started + Duration::from_secs(6)).await;
        let reopened = account_supervisor(&account, manager.clone());
        assert!(matches!(
            resolve_rpc(&reopened, request, digest).await,
            AccountReply::Observation(_)
        ));
        let completed = reopened.inspect_install_request(request).unwrap().0;
        assert_eq!(completed.request, original.0.request);
        assert_eq!(completed.request_digest, original.0.request_digest);
        assert_eq!(completed.phase, ServicePhase::Completed);
        assert_eq!(
            manager.effects.lock().unwrap().as_slice(),
            ["configure", "start"]
        );
    })
    .await
    .expect("canceled observer RPC recovery must remain bounded");
}

#[derive(Clone, Copy)]
enum Retirement {
    Eof,
    BrokenInput,
    BrokenOutput,
}
struct RetirementChannel {
    request: Mutex<Option<Vec<u8>>>,
    replies: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    retirement: Retirement,
    retired: tokio::sync::Notify,
}
impl MessageTransport for RetirementChannel {
    fn peer_facts(&self) -> PeerFacts {
        Channel {
            request: Mutex::new(None),
            reply: Mutex::new(None),
        }
        .peer_facts()
    }
    fn send<'a>(&'a self, bytes: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            self.replies.send(bytes.to_vec()).unwrap();
            if matches!(self.retirement, Retirement::BrokenOutput) {
                self.retired.notify_one();
                return Err(spaceterm_host::transport::TransportError::Closed);
            }
            Ok(())
        })
    }
    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            if let Some(request) = self.request.lock().unwrap().take() {
                return Ok(Some(request));
            }
            self.retired.notify_one();
            if matches!(self.retirement, Retirement::BrokenInput) {
                Err(spaceterm_host::transport::TransportError::Closed)
            } else {
                Ok(None)
            }
        })
    }
    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test(start_paused = true)]
async fn bootstrap_helper_retirement_keeps_admitted_install_owned_after_fifteen_second_pending() {
    // The signed fixture enters through the existing confirmed-install boundary.
    // The foreground Resolve RPC uses the production borrowed-channel endpoint;
    // its Pending reply and retirement must not cancel that original execution.
    // Testing borrowed broken output separately prevents an early `send?` return
    // from bypassing the same drain that EOF takes.
    for retirement in [
        Retirement::Eof,
        Retirement::BrokenInput,
        Retirement::BrokenOutput,
    ] {
        tokio::time::timeout(Duration::from_secs(30), async {
            let root = root();
            let account =
                AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
            let (key, artifact) =
                signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
            let digest = artifact.artifact_sha256();
            let gate = Arc::new(ConfigureGate::default());
            let manager = Arc::new(Manager {
                key,
                configure_gate: Some(gate.clone()),
                observations: AtomicUsize::new(0),
                running_at: Mutex::new(None),
                startup_delay: Duration::from_secs(6),
                startup_observed: tokio::sync::Notify::new(),
                effects: Mutex::new(Vec::new()),
            });
            let supervisor = account_supervisor(&account, manager.clone());
            let setup = AccountBootstrapSetup::new(supervisor.clone());
            let request = setup.reserve_request().unwrap();
            let confirmation = account.confirm_install(artifact);
            let started = tokio::time::Instant::now();
            let observer = tokio::spawn(async move {
                setup
                    .install_and_observe(request, confirmation, digest)
                    .await
            });
            gate.entered.notified().await;
            let original = supervisor.inspect_install_request(request).unwrap();
            let registration = supervisor.discover().unwrap().unwrap();
            let pin = registration.server_pin();
            let approved = registration.approved_installation().unwrap().clone();
            assert_eq!(original.0.request, request);
            assert_eq!(original.0.phase, ServicePhase::Starting);
            assert_eq!(original.0.state, ServiceRequestState::Pending);
            assert_eq!(original.1, digest);
            assert_eq!(approved.artifact_sha256, digest);

            tokio::time::advance(Duration::from_secs(15)).await;
            assert_eq!(observer.await.unwrap(), Err(ServiceFailure::Pending));
            assert_eq!(started.elapsed(), Duration::from_secs(15));
            assert_eq!(manager.effects.lock().unwrap().as_slice(), ["configure"]);

            let (replies, mut received) = tokio::sync::mpsc::unbounded_channel();
            let channel = Arc::new(RetirementChannel {
                request: Mutex::new(Some(
                    serde_json::to_vec(&AccountRequest::Resolve {
                        request,
                        artifact: digest,
                    })
                    .unwrap(),
                )),
                replies,
                retirement,
                retired: tokio::sync::Notify::new(),
            });
            let endpoint_supervisor = Arc::new(supervisor.clone());
            let endpoint_channel = channel.clone();
            let endpoint = tokio::spawn(async move {
                spaceterm_server::deployment::bootstrap::execute(
                    endpoint_supervisor,
                    &*endpoint_channel,
                )
                .await
            });
            let reply: AccountReply =
                serde_json::from_slice(&received.recv().await.unwrap()).unwrap();
            assert!(matches!(
                reply,
                AccountReply::Failure(ServiceFailure::Pending)
            ));
            assert_eq!(started.elapsed(), Duration::from_secs(15));
            channel.retired.notified().await;
            assert!(
                !endpoint.is_finished(),
                "helper retirement must retain its runtime"
            );
            let draining_clone = supervisor.clone();
            let drain =
                tokio::spawn(async move { draining_clone.finish_install_execution().await });
            tokio::task::yield_now().await;
            assert!(
                !drain.is_finished(),
                "all supervisor clones share execution custody"
            );

            tokio::time::advance(Duration::from_secs(1)).await;
            assert!(!endpoint.is_finished());
            gate.released.notify_one();
            manager.startup_observed.notified().await;
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
            tokio::time::advance(Duration::from_secs(5)).await;
            let result = endpoint.await.unwrap();
            if matches!(retirement, Retirement::Eof) {
                assert_eq!(result, Ok(()));
            } else {
                assert_eq!(
                    result,
                    Err(spaceterm_server::deployment::owner::OwnerEndpointFailure::Transport)
                );
            }
            drain.await.unwrap();
            assert_eq!(started.elapsed(), Duration::from_secs(21));
            // The native five-second observation finishes without renewing itself
            // to Running at six seconds. Retirement adds no lookup or Start replay.
            assert_eq!(
                supervisor.inspect_install_request(request).unwrap(),
                original
            );
            assert_eq!(supervisor.discover().unwrap().unwrap().server_pin(), pin);
            assert_eq!(
                supervisor
                    .discover()
                    .unwrap()
                    .unwrap()
                    .approved_installation(),
                Some(&approved)
            );
            assert!(manager.observations.load(Ordering::Relaxed) <= 55);
            tokio::time::advance(Duration::from_secs(1)).await;
            let reopened = account_supervisor(&account, manager.clone());
            assert!(matches!(
                resolve_rpc(&reopened, request, digest).await,
                AccountReply::Observation(_)
            ));
            let completed = reopened.inspect_install_request(request).unwrap();
            assert_eq!(completed.0.request, request);
            assert_eq!(completed.0.request_digest, original.0.request_digest);
            assert_eq!(completed.1, digest);
            assert_eq!(completed.0.phase, ServicePhase::Completed);
            assert!(matches!(completed.0.state, ServiceRequestState::Completed(
                spaceterm_host::account_service::ServiceOutcome::Installed {
                    server, installation, reused: false,
                }
            ) if server == pin && installation == approved));
            assert_eq!(
                manager.effects.lock().unwrap().as_slice(),
                ["configure", "start"]
            );
        })
        .await
        .expect("Pending, retirement, native observation and original resolution stay bounded");
    }
}

fn stopped_owner_account(root: &std::path::Path) -> AccountServiceFixture {
    use spaceterm_core::{identity::ServerId, organization::ServerDirectory};
    use spaceterm_host::{
        organization_store::{
            SqliteOrganizationStore, StoreIdentity, StoreLimits,
            bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
            migrations, schema,
        },
        startup::DesiredServerState,
        storage::Database,
    };
    let account = AccountServiceFixture::new(root, BuildIdentity::Development).unwrap();
    let registration = account.initialize(DesiredServerState::Stopped).unwrap();
    let identity = StoreIdentity {
        server: ServerId::new(registration.server_pin().identity),
        account: registration.key(),
        server_public_key: registration.server_pin().public_key,
    };
    let home = ServerDirectory::new(
        identity.server,
        root.as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let database = Database::open_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    let services = spaceterm_server::adapters::store::production_services(identity, home).unwrap();
    assert!(matches!(
        SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity,
                origin: NewServerOrigin::LocalHeadless
            },
            StoreLimits::production(),
            services,
        ),
        InitializationDisposition::Durable { .. }
    ));
    account
}

struct OwnerKey(ring::signature::Ed25519KeyPair);
impl OwnerKey {
    fn new(seed: u8) -> Self {
        Self(ring::signature::Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap())
    }
}
impl spaceterm_protocol::authentication::SigningKey for OwnerKey {
    fn public_key(&self) -> [u8; 32] {
        use ring::signature::KeyPair as _;
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], spaceterm_protocol::ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
fn owner_request(
    request: spaceterm_host::native_deployment::contracts::NativeRequestId,
    server: spaceterm_core::identity::ServerId,
) -> spaceterm_server::deployment::owner::OwnerEnrollmentRequest {
    use spaceterm_core::{identity::DeviceId, pairing::*};
    use spaceterm_protocol::{
        authentication::SigningKey as _, pairing_adapter::VerifiedDeviceClaim,
    };
    let device = OwnerKey::new(61);
    let authentication = OwnerKey::new(62);
    let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
        server,
        device: DeviceId::new([61; 16]),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: device.public_key(),
            pairing_authentication_public_key: authentication.public_key(),
        })
        .unwrap(),
        claim_nonce: [61; 32],
    })
    .unwrap();
    let (device_proof, authentication_proof) =
        VerifiedDeviceClaim::proofs(&proposal, &device, &authentication).unwrap();
    spaceterm_server::deployment::owner::OwnerEnrollmentRequest {
        request,
        proposal,
        device_proof: device_proof.to_vec(),
        authentication_proof: authentication_proof.to_vec(),
    }
}

const HANGUP_IO_BUDGET: Duration = Duration::from_secs(10);

// NativeStdio sets NONBLOCK on its duplicated descriptors. The fixture also
// tolerates that flag on any shared open-file description, without renewing a
// frame deadline after a partial read/write, EINTR or EAGAIN.
struct HangupIo(std::os::unix::net::UnixStream);
impl HangupIo {
    fn new(stream: std::os::unix::net::UnixStream) -> Self {
        stream.set_nonblocking(true).unwrap();
        Self(stream)
    }
    fn wait(
        &self,
        flags: rustix::event::PollFlags,
        until: std::time::Instant,
    ) -> std::io::Result<()> {
        use rustix::{
            event::{PollFd, Timespec, poll},
            io::Errno,
        };
        loop {
            let remaining = until
                .checked_duration_since(std::time::Instant::now())
                .ok_or(std::io::ErrorKind::TimedOut)?;
            let timeout = Timespec {
                tv_sec: remaining.as_secs() as _,
                tv_nsec: remaining.subsec_nanos() as _,
            };
            match poll(&mut [PollFd::new(&self.0, flags)], Some(&timeout)) {
                Ok(0) => return Err(std::io::ErrorKind::TimedOut.into()),
                Ok(_) => return Ok(()),
                Err(Errno::INTR) => {}
                Err(_) => return Err(std::io::ErrorKind::Other.into()),
            }
        }
    }
    fn read_exact_until(
        &self,
        mut bytes: &mut [u8],
        until: std::time::Instant,
    ) -> std::io::Result<()> {
        use rustix::{
            event::PollFlags,
            io::{Errno, read},
        };
        while !bytes.is_empty() {
            self.wait(PollFlags::IN, until)?;
            match read(&self.0, &mut *bytes) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                Ok(count) => bytes = &mut bytes[count..],
                Err(Errno::INTR | Errno::AGAIN) => {}
                Err(_) => return Err(std::io::ErrorKind::Other.into()),
            }
        }
        Ok(())
    }
    fn write_all_until(&self, mut bytes: &[u8], until: std::time::Instant) -> std::io::Result<()> {
        use rustix::{
            event::PollFlags,
            io::{Errno, write},
        };
        while !bytes.is_empty() {
            self.wait(PollFlags::OUT, until)?;
            match write(&self.0, bytes) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(Errno::INTR | Errno::AGAIN) => {}
                Err(_) => return Err(std::io::ErrorKind::Other.into()),
            }
        }
        Ok(())
    }
    fn send_frame(&self, bytes: &[u8]) {
        let until = std::time::Instant::now() + HANGUP_IO_BUDGET;
        self.write_all_until(&(bytes.len() as u32).to_be_bytes(), until)
            .unwrap();
        self.write_all_until(bytes, until).unwrap();
    }
    fn recv_frame(&self, mode: &str) -> zeroize::Zeroizing<Vec<u8>> {
        let until = std::time::Instant::now() + HANGUP_IO_BUDGET;
        let mut prefix = [0; 4];
        let prefix_result = self.read_exact_until(&mut prefix, until);
        assert!(
            prefix_result.is_ok(),
            "native {mode} frame prefix did not arrive within its original budget: {:?}",
            prefix_result.err().map(|error| error.kind())
        );
        let count = u32::from_be_bytes(prefix) as usize;
        assert!((1..=spaceterm_server::deployment::owner::OWNER_FRAME_LIMIT).contains(&count));
        let mut bytes = zeroize::Zeroizing::new(vec![0; count]);
        self.read_exact_until(&mut bytes, until).unwrap();
        bytes
    }
}

const HANGUP_ROOT: &str = "SPACETERM_TEST_BOOTSTRAP_HANGUP_ROOT";
const HANGUP_MODE: &str = "SPACETERM_TEST_BOOTSTRAP_HANGUP_MODE";
const HANGUP_CHANNEL_FD: i32 = 64;
const HANGUP_RELEASE_FD: i32 = 65;

// Run the production native framing and signal path in a separate process. The
// inherited socket descriptors keep libtest output away from the framed stream.
// Existing account fixtures inject the manager; no real service is contacted.
#[test]
fn bootstrap_hangup_child() {
    use std::os::{fd::FromRawFd as _, unix::net::UnixStream};
    let Some(root) = std::env::var_os(HANGUP_ROOT) else {
        return;
    };
    let mode = std::env::var(HANGUP_MODE).unwrap();
    // SAFETY: the parent supplies these two exclusively owned descriptors to a
    // single selected child test. No other child test reads standard input/output.
    unsafe {
        assert_eq!(
            libc::dup2(HANGUP_CHANNEL_FD, libc::STDIN_FILENO),
            libc::STDIN_FILENO
        );
        assert_eq!(
            libc::dup2(HANGUP_CHANNEL_FD, libc::STDOUT_FILENO),
            libc::STDOUT_FILENO
        );
        assert_eq!(libc::close(HANGUP_CHANNEL_FD), 0);
    }
    // SAFETY: this descriptor was transferred by the parent and is owned here once.
    let release = HangupIo::new(unsafe { UnixStream::from_raw_fd(HANGUP_RELEASE_FD) });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let account =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        let mut observer = None;
        let mut release_task = None;
        let mut original = None;
        let mut installation_manager = None;
        let supervisor = if mode == "install" {
            let (key, artifact) =
                signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
            let digest = artifact.artifact_sha256();
            let gate = Arc::new(ConfigureGate::default());
            let manager = Arc::new(Manager {
                key,
                configure_gate: Some(gate.clone()),
                observations: AtomicUsize::new(0),
                running_at: Mutex::new(None),
                startup_delay: Duration::from_secs(1),
                startup_observed: tokio::sync::Notify::new(),
                effects: Mutex::new(Vec::new()),
            });
            let supervisor = account_supervisor(&account, manager.clone());
            let setup = AccountBootstrapSetup::new(supervisor.clone());
            let request = setup.reserve_request().unwrap();
            let confirmation = account.confirm_install(artifact);
            observer = Some(tokio::spawn(async move {
                setup
                    .install_and_observe(request, confirmation, digest)
                    .await
            }));
            gate.entered.notified().await;
            original = Some((
                request,
                supervisor.inspect_install_request(request).unwrap(),
                supervisor.discover().unwrap().unwrap().server_pin(),
            ));
            release_task = Some(tokio::task::spawn_blocking(move || {
                let mut byte = [0];
                release
                    .read_exact_until(&mut byte, std::time::Instant::now() + HANGUP_IO_BUDGET)
                    .unwrap();
                assert_eq!(byte, [1]);
                gate.released.notify_one();
            }));
            installation_manager = Some(manager);
            supervisor
        } else if mode == "stopped-owner" {
            (*spaceterm_server::account_startup::test_support::supervisor(
                std::path::Path::new(&root),
                Arc::new(spaceterm_server::account_startup::test_support::StartupAbsence),
            )
            .unwrap())
            .clone()
        } else {
            account.reopen()
        };
        let result = spaceterm_server::deployment::run_native_with_supervisor(
            spaceterm_server::deployment::ServerInvocation::BootstrapAccount,
            Arc::new(supervisor.clone()),
        )
        .await;
        assert_eq!(
            result,
            Err(spaceterm_server::deployment::owner::OwnerEndpointFailure::Transport)
        );
        if mode == "session-leader" {
            assert!(
                !account.anchor().exists(),
                "session refusal precedes native admission"
            );
        }
        if mode == "stopped-owner" {
            use spaceterm_core::{
                identity::ServerId,
                pairing::{PairingKeys, PairingKeysParts},
            };
            use spaceterm_protocol::authentication::SigningKey as _;
            let registration = supervisor.discover().unwrap().unwrap();
            let server = ServerId::new(registration.server_pin().identity);
            let keys = PairingKeys::new(PairingKeysParts {
                device_identity_public_key: OwnerKey::new(61).public_key(),
                pairing_authentication_public_key: OwnerKey::new(62).public_key(),
            })
            .unwrap();
            let (request, proposal) = supervisor
                .retained_owner_proposal(server, &keys)
                .unwrap()
                .unwrap();
            let claim =
                spaceterm_protocol::pairing_adapter::enrollment_proposal_digest(&proposal).unwrap();
            let digest = supervisor
                .retained_owner_enrollment_digest(request, server, claim)
                .unwrap()
                .unwrap();
            // Prove exact restricted-owner cleanup before this process or runtime
            // exits. Merely dropping the foreground future leaves actor custody
            // and its instance lease alive, so this readmission would fail.
            let admission = supervisor
                .admit_stopped_owner(request, digest, server, claim)
                .unwrap();
            admission.revalidate().unwrap();
            assert_eq!(
                admission.registration().desired_state(),
                spaceterm_host::startup::DesiredServerState::Stopped
            );
        }
        if let Some((request, original, pin)) = original {
            let completed = supervisor.inspect_install_request(request).unwrap();
            assert_eq!(completed.0.request, request);
            assert_eq!(completed.0.request_digest, original.0.request_digest);
            assert_eq!(completed.1, original.1);
            assert_eq!(completed.0.phase, ServicePhase::Completed);
            assert_eq!(supervisor.discover().unwrap().unwrap().server_pin(), pin);
            assert_eq!(
                installation_manager
                    .unwrap()
                    .effects
                    .lock()
                    .unwrap()
                    .as_slice(),
                ["configure", "start"]
            );
            assert!(observer.unwrap().await.unwrap().is_ok());
            release_task.unwrap().await.unwrap();
        }
    });
}

#[test]
fn bootstrap_native_hangup_retires_foreground_without_killing_admitted_install() {
    use std::{
        os::{
            fd::AsRawFd as _,
            unix::{net::UnixStream, process::CommandExt as _},
        },
        process::{Child, Command, Stdio},
        time::Instant,
    };
    struct ChildOwner(Child);
    impl ChildOwner {
        fn wait_bounded(&mut self, started: Instant) -> std::process::ExitStatus {
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    return status;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "native helper retirement must stay bounded"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    impl Drop for ChildOwner {
        fn drop(&mut self) {
            if self.0.try_wait().unwrap().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    for mode in [
        "foreground",
        "owner",
        "stopped-owner",
        "install",
        "session-leader",
    ] {
        // Match the existing native-owner fixture: real SQLite synchronization
        // stays off the shared Linux home/build filesystem. This changes neither
        // native authority checks nor the ten-second subprocess I/O budgets.
        let parent = if cfg!(target_os = "macos") {
            "/private/tmp"
        } else {
            "/var/tmp"
        };
        let root = tempfile::tempdir_in(parent).unwrap();
        let stopped = (mode == "stopped-owner").then(|| stopped_owner_account(root.path()));
        let (channel, child_channel) = UnixStream::pair().unwrap();
        let (release, child_release) = UnixStream::pair().unwrap();
        let channel = HangupIo::new(channel);
        let release = HangupIo::new(release);
        let child_channel = rustix::io::fcntl_dupfd_cloexec(child_channel, 66).unwrap();
        let child_release = rustix::io::fcntl_dupfd_cloexec(child_release, 66).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "bootstrap_hangup_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HANGUP_ROOT, root.path())
            .env(HANGUP_MODE, mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: only async-signal-safe descriptor duplication runs after fork.
        // Fixed test-only descriptors are distinct from both retained source FDs.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(child_channel.as_raw_fd(), HANGUP_CHANNEL_FD) < 0
                    || libc::dup2(child_release.as_raw_fd(), HANGUP_RELEASE_FD) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        if mode == "session-leader" {
            command.process_group(0);
        }
        let mut child = ChildOwner(spaceterm_host::process::spawn(&mut command).unwrap());
        // Command's pre_exec closure owns duplicated child endpoints. Keeping it
        // alive in the parent would hide child EOF behind our own retained peers.
        drop(command);
        if mode == "session-leader" {
            assert!(
                child.wait_bounded(Instant::now()).success(),
                "a helper that cannot leave the SSH session refuses before admission"
            );
            continue;
        }
        let reserve = serde_json::to_vec(&AccountRequest::Reserve).unwrap();
        channel.send_frame(&reserve);
        let reply = channel.recv_frame(mode);
        let AccountReply::Reserved(request) =
            serde_json::from_slice::<AccountReply>(&reply).unwrap()
        else {
            panic!("the helper must reserve before admitting owner or install work");
        };
        // SAFETY: these are read-only observations of the retained live child.
        // The helper and all fixed manager children now belong to its own session,
        // not the SSH staging shell's retiring process group.
        unsafe {
            assert_eq!(libc::getsid(child.0.id() as i32), child.0.id() as i32);
            assert_eq!(libc::getpgid(child.0.id() as i32), child.0.id() as i32);
        }
        if matches!(mode, "owner" | "stopped-owner") {
            let owner = serde_json::to_vec(&AccountRequest::OwnerChannel).unwrap();
            channel.send_frame(&owner);
        }
        if let Some(account) = stopped {
            let server = spaceterm_core::identity::ServerId::new(
                account
                    .reopen()
                    .discover()
                    .unwrap()
                    .unwrap()
                    .server_pin()
                    .identity,
            );
            let owner = owner_request(request, server);
            account
                .reopen()
                .record_owner_proposal(request, owner.proposal.clone())
                .unwrap();
            let owner = serde_json::to_vec(&owner).unwrap();
            channel.send_frame(&owner);
            for index in 0..2 {
                let reply = channel.recv_frame(mode);
                if index == 0 {
                    let reply: spaceterm_server::deployment::owner::OwnerEnrollmentReply =
                        serde_json::from_slice(&reply).unwrap();
                    assert_eq!(reply.disposition, spaceterm_server::deployment::owner::OwnerReplyDisposition::AuthorizationDelivered);
                }
            }
        }
        // Keep stdin open and leave a partial next frame. Neither EOF nor the
        // 60/600-second framing deadline can explain prompt hangup retirement.
        channel
            .write_all_until(&[0], Instant::now() + HANGUP_IO_BUDGET)
            .unwrap();
        let retired_at = Instant::now();
        // SAFETY: the live exact child PID is retained and cannot be reused here.
        assert_eq!(unsafe { libc::kill(child.0.id() as i32, libc::SIGHUP) }, 0);
        if mode == "install" {
            std::thread::sleep(Duration::from_millis(100));
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "hangup must not kill admitted configure"
            );
            release
                .write_all_until(&[1], Instant::now() + HANGUP_IO_BUDGET)
                .unwrap();
        }
        let status = child.wait_bounded(retired_at);
        assert!(
            status.success(),
            "the isolated native hangup child must confirm retirement and custody"
        );
    }
}
