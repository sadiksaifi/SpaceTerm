//! Public stopped executor with real signed archives, native process/listener
//! observations, canonical SQLite pairing and ordinary ServerAuth. Only the
//! external service manager is simulated; its startup probes the real instance lease.

use super::*;
use spaceterm_core::{authority::*, pairing::PairingReceiptBody, storage::OrganizationStore};
use spaceterm_distribution::{UpdateKey, VerifiedAccountServerArtifact};
use spaceterm_host::{
    account_service::{
        AccountStartupAdmission, LingerState, NativeServiceInstallations, NativeServiceProcesses,
        RegisteredServer, ServiceAction, ServiceClock, ServiceFailure, ServiceFuture,
        ServiceInstallations, ServiceManager, ServiceOutcome, ServicePhase, ServiceRequestState,
        ServiceSupervisor, ServiceTime, StoppedNativeAction, StoppedNativeCall, StoppedNativeReply,
        StoppedNativeRequest, SupervisorState,
        runtime_inventory::{RetainedRuntimeProcesses, RuntimeCustody},
    },
    server_keys::ServerKeyStore,
    startup::InstallationId,
};
use spaceterm_protocol::{
    authentication::{ClientHandshake, Established, HandshakeConfig, Principal, Purpose},
    codec::Payload,
    connection::AuthenticatedConnection,
    negotiation::{
        CAPABILITY_ACCOUNT_RUNTIME, CAPABILITY_CANONICAL_DOMAIN, CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
    },
    wire::{common, v1, v2},
};
use spaceterm_server::{
    account_native_maintenance::{
        AccountNativeMaintenanceExecutor, StoppedNativeSession, StoppedNativeTarget,
    },
    account_startup::AccountStartup,
    adapters::core::CorePorts,
    service::RecoveredServer,
    shutdown::TrustedShutdownRequest,
};
use std::{
    num::NonZeroU64,
    path::PathBuf,
    sync::{
        Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[path = "account_artifact.rs"]
mod artifact;
#[path = "account_native_maintenance_producer.rs"]
mod producer_tests;

struct Clock {
    base: ServiceTime,
    offset: AtomicU64,
}
impl Clock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            base: ServiceTime::current().unwrap(),
            offset: AtomicU64::new(0),
        })
    }
    fn expire(&self) {
        self.offset.store(30_000, Ordering::SeqCst);
    }
}
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: self.base.boot,
            millis: self.base.millis + self.offset.load(Ordering::SeqCst),
        })
    }
}

// This adapter injects test publisher trust, retaining the real native installation
// selection, signature, digest, storage-compatibility and filesystem checks.
struct SignedInstallations(UpdateKey);
impl ServiceInstallations for SignedInstallations {
    fn approved_executable(
        &self,
        registration: &RegisteredServer,
    ) -> Result<PathBuf, ServiceFailure> {
        NativeServiceInstallations::test_approved_executable(registration, self.0.clone())
    }
    fn uninstall(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        let scope = spaceterm_host::server_installation::InstallationScope::new(
            registration,
            &ManagerRemoval,
        )?;
        let store = spaceterm_host::server_installation::test_support::store_with_scope(
            &scope,
            self.0.clone(),
        )?;
        store.uninstall(&scope)
    }
}

struct ManagerRemoval;
impl spaceterm_host::server_installation::InstallationAdmission for ManagerRemoval {
    fn verify(
        &self,
        registration: &RegisteredServer,
        operation: spaceterm_host::server_installation::InstallationOperation,
    ) -> Result<(), ServiceFailure> {
        use spaceterm_host::server_installation::InstallationOperation;
        if registration.desired_state() != DesiredServerState::Stopped
            || !matches!(
                operation,
                InstallationOperation::Open | InstallationOperation::Uninstall
            )
        {
            return Err(ServiceFailure::RequestConflict);
        }
        Ok(())
    }
}

struct Manager {
    installations: Arc<dyn ServiceInstallations>,
    running: AtomicBool,
    starts: AtomicU64,
    stops: AtomicU64,
    uninstalls: AtomicU64,
    startup_lease_released: AtomicBool,
    supervisor: Mutex<Weak<ServiceSupervisor>>,
    clock: Arc<Clock>,
    expire_status: AtomicBool,
}
impl ServiceManager for Manager {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async move {
            if self.expire_status.swap(false, Ordering::SeqCst) {
                self.clock.expire();
            }
            Ok(if self.running.load(Ordering::SeqCst) {
                SupervisorState::Running
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
            Ok(if cfg!(target_os = "linux") {
                Some(LingerState::Enabled)
            } else {
                None
            })
        })
    }
    fn configure<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn start<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            let supervisor = self
                .supervisor
                .lock()
                .unwrap()
                .upgrade()
                .ok_or(ServiceFailure::Missing)?;
            if !matches!(
                supervisor.admit_startup()?,
                AccountStartupAdmission::Ready(_)
            ) {
                return Err(ServiceFailure::Pending);
            }
            self.startup_lease_released.store(true, Ordering::SeqCst);
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.running.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        })
    }
    fn uninstall<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.installations.uninstall(registration)?;
            self.uninstalls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn verify_installation(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        self.installations
            .approved_executable(registration)
            .map(|_| ())
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    account: AccountServiceFixture,
    home: ServerDirectory,
    supervisor: Arc<ServiceSupervisor>,
    manager: Arc<Manager>,
    target: StoppedNativeTarget,
    epoch: ServerEpoch,
}
impl Fixture {
    async fn new(revoked: bool) -> Self {
        let (trust, archive) = artifact::fixture("1.2.3", 91, artifact::native_machine());
        let approved = spaceterm_host::account_service::InstallationStatus {
            installation: InstallationId(NonZeroU64::new(1).unwrap()),
            version: archive.manifest().version.clone(),
            artifact_sha256: archive.artifact_sha256(),
        };
        let (root, account, home) = initialized_account_with_installation(
            DesiredServerState::Running,
            Some(approved.clone()),
        );
        let registration = account.reopen().discover().unwrap().unwrap();
        let installed =
            spaceterm_host::server_installation::test_support::store(&registration, trust.clone())
                .unwrap();
        let id = installed.stage(&archive).unwrap();
        assert_eq!(id, approved.installation);
        installed
            .activate(
                id,
                installed.snapshot().unwrap().generation,
                schema::SCHEMA_VERSION,
            )
            .unwrap();
        drop(installed);
        drop(archive);

        // The actual signed prior owner's PID/birth must be absent, not merely
        // outside the manager's scope. Seed and retire the real actor in a child
        // test process, then join its OS exit before maintenance opens the store.
        let child_root = root.path().to_owned();
        tokio::task::spawn_blocking(move || seed_in_child(child_root, revoked))
            .await
            .unwrap();
        let pin = registration.server_pin();
        let identity = StoreIdentity {
            server: ServerId::new(pin.identity),
            account: registration.key(),
            server_public_key: pin.public_key,
        };
        let database = Database::open_existing_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        let services =
            spaceterm_server::adapters::store::production_services(identity, home.clone()).unwrap();
        let recovered = SqliteOrganizationStore::open_existing(
            database,
            identity,
            StoreLimits::production(),
            services,
        )
        .unwrap()
        .recover()
        .unwrap();
        let epoch = recovered.last_epoch;
        let pairings = recovered.pairing.unwrap();
        let authorization = &pairings.parts().authorizations[0];
        let client = &authorization.parts().clients[0];
        let principal = Principal {
            server: pin.identity,
            device: authorization.parts().record.device.bytes(),
            pairing: native::local(client.parts().client.pairing.local()),
            client: native::local(client.parts().client.id.local()),
        };
        assert_eq!(principal.device, [41; 16]);

        let clock = Clock::new();
        let installations: Arc<dyn ServiceInstallations> =
            Arc::new(SignedInstallations(trust.clone()));
        let manager = Arc::new(Manager {
            installations: installations.clone(),
            running: AtomicBool::new(false),
            starts: AtomicU64::new(0),
            stops: AtomicU64::new(0),
            uninstalls: AtomicU64::new(0),
            startup_lease_released: AtomicBool::new(false),
            supervisor: Mutex::new(Weak::new()),
            clock: clock.clone(),
            expire_status: AtomicBool::new(false),
        });
        let processes = Arc::new(
            NativeServiceProcesses::current(installations)
                .unwrap()
                .with_manager(manager.clone())
                .with_clock(clock.clone())
                .with_runtime(Arc::new(RetainedRuntimeProcesses)),
        );
        let supervisor = Arc::new(account.with_installation_trust(
            account.with_providers(manager.clone(), processes, clock.clone(), None),
            trust,
        ));
        *manager.supervisor.lock().unwrap() = Arc::downgrade(&supervisor);
        let stop = supervisor.allocate_request().unwrap();
        let stopped = supervisor
            .assess(stop, ServiceAction::Stop)
            .await
            .unwrap()
            .confirm()
            .unwrap();
        supervisor.stop(stopped).await.unwrap();
        manager.stops.store(0, Ordering::SeqCst);
        Self {
            _root: root,
            account,
            home,
            supervisor,
            manager,
            target: StoppedNativeTarget {
                key: registration.key(),
                pin,
                principal,
            },
            epoch,
        }
    }
    fn capture(&self, action: StoppedNativeAction) -> StoppedNativeRequest {
        self.supervisor
            .capture_stopped_native(self.supervisor.allocate_request().unwrap(), action)
            .unwrap()
    }
    async fn open(
        &self,
        original: StoppedNativeRequest,
        archive: Option<VerifiedAccountServerArtifact>,
    ) -> AccountNativeMaintenanceExecutor {
        AccountNativeMaintenanceExecutor::open(
            self.supervisor.clone(),
            self.home.clone(),
            self.target,
            original,
            archive,
        )
        .await
        .unwrap()
    }
    fn recovered(&self) -> spaceterm_core::storage::RecoveredState {
        let registration = self.supervisor.discover().unwrap().unwrap();
        let identity = StoreIdentity {
            server: ServerId::new(self.target.pin.identity),
            account: self.target.key,
            server_public_key: self.target.pin.public_key,
        };
        let database = Database::open_existing_for_application(
            registration.organization_directory(),
            migrations(),
            schema::APPLICATION_ID,
        )
        .unwrap();
        let services =
            spaceterm_server::adapters::store::production_services(identity, self.home.clone())
                .unwrap();
        SqliteOrganizationStore::open_existing(
            database,
            identity,
            StoreLimits::production(),
            services,
        )
        .unwrap()
        .recover()
        .unwrap()
    }
}

struct FixtureChild(std::process::Child);
impl Drop for FixtureChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn seed_in_child(root: PathBuf, revoked: bool) {
    use std::{
        io::Read as _,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let mut child = FixtureChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "native_maintenance::stopped_native_pairing_fixture_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("SPACETERM_STOPPED_NATIVE_FIXTURE_ROOT", root)
            .env(
                "SPACETERM_STOPPED_NATIVE_FIXTURE_REVOKED",
                if revoked { "1" } else { "0" },
            )
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "pairing fixture child did not exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut diagnostics = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut diagnostics)
        .unwrap();
    assert!(status.success(), "{diagnostics}");
}

#[tokio::test]
#[ignore = "child fixture process, invoked with an isolated test root"]
async fn stopped_native_pairing_fixture_child() {
    let root = std::env::var_os("SPACETERM_STOPPED_NATIVE_FIXTURE_ROOT").unwrap();
    let root = PathBuf::from(root);
    let revoked = std::env::var("SPACETERM_STOPPED_NATIVE_FIXTURE_REVOKED").unwrap() == "1";
    let account = AccountServiceFixture::new(&root, BuildIdentity::Development).unwrap();
    let initial = Arc::new(account.reopen());
    let registration = initial.discover().unwrap().unwrap();
    let pin = registration.server_pin();
    let home = ServerDirectory::new(
        ServerId::new(pin.identity),
        root.as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let identity = StoreIdentity {
        server: ServerId::new(pin.identity),
        account: registration.key(),
        server_public_key: pin.public_key,
    };
    let opened = AccountStartup::open(initial.discover().unwrap().unwrap())
        .unwrap()
        .open_store(home.clone())
        .unwrap();
    let credentials = opened.credentials.clone();
    let mut boundary = native::Fixture::new();
    let shutdown = boundary.host.clone();
    boundary.config.host.authority = Arc::new(RunningAuthority(pin));
    boundary.config.core =
        CorePorts::production(identity, home, boundary.dependencies.clock.clone()).unwrap();
    boundary.dependencies.store = Box::new(opened.store);
    boundary.dependencies.credentials = opened.credentials;
    let server = RecoveredServer::recover(boundary.config, boundary.dependencies)
        .await
        .unwrap();
    let owner = AccountOwnerRequestExecutor::running(initial.clone(), server.handle()).unwrap();
    let receipt = enroll_owner(&owner, initial.allocate_request().unwrap(), pin, 41, 2).await;
    let PairingReceiptBody::Enrollment {
        target: paired,
        client,
        ..
    } = &receipt.parts().claims
    else {
        panic!("enrollment")
    };
    if revoked {
        revoke_owner_pairing(
            &server.handle(),
            credentials.clone(),
            initial,
            paired.clone(),
            client,
            41,
        )
        .await;
    }
    owner.close().await.unwrap();
    let epoch = server.handle().snapshot().epoch();
    let custody = RuntimeCustody::begin(&registration, epoch, credentials).unwrap();
    let report = server
        .handle()
        .shutdown(TrustedShutdownRequest {
            authority: shutdown,
        })
        .await
        .unwrap();
    server.wait().await.unwrap();
    custody.completed_instance(&registration).unwrap();
    assert!(report.cleanup_confirmed && report.started_writes_finished && report.listeners_closed);
}

fn config(major: u32) -> HandshakeConfig {
    HandshakeConfig {
        majors: vec![major],
        capabilities: vec![
            CAPABILITY_CANONICAL_DOMAIN,
            CAPABILITY_ACCOUNT_RUNTIME,
            CAPABILITY_REMOTE_NATIVE_LIFECYCLE,
        ],
        ..Default::default()
    }
}
async fn connect(
    executor: AccountNativeMaintenanceExecutor,
    target: StoppedNativeTarget,
    major: u32,
) -> (StoppedNativeSession, AuthenticatedConnection) {
    let (client, hello) = ClientHandshake::begin(
        target.pin,
        target.principal,
        Purpose::Ordinary,
        config(major),
        Arc::new(DeviceKey::new(42)),
    )
    .unwrap();
    let (server, hello) = executor.accept_hello(&hello).await.unwrap();
    let (client, proof) = client.receive_server(&hello).unwrap();
    let (server, ready) = server.receive_proof(&proof).await.unwrap();
    let Established::Ordinary(client) = client.receive_ready(&ready).unwrap() else {
        panic!("ordinary")
    };
    (server, client)
}
async fn call(
    server: &mut StoppedNativeSession,
    client: &mut AuthenticatedConnection,
    value: StoppedNativeCall,
) -> StoppedNativeReply {
    let request = common::AccountRuntimeMessage {
        request: 1,
        request_generation: 1,
        body: serde_json::to_vec(&value).unwrap(),
    };
    let payload = if client.major() == 1 {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::AccountRuntimeRequest(request)),
        })
    } else {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::AccountRuntimeRequest(request)),
        })
    };
    let frame = client.encode(&payload).unwrap();
    let response = server.frame(&frame).await.unwrap();
    let response = client.decode(&response).unwrap();
    let body = match response {
        Payload::Major1(message) => {
            let Some(v1::message::Kind::AccountRuntimeReply(reply)) = message.kind else {
                panic!("reply")
            };
            reply.body
        }
        Payload::Major2(message) => {
            let Some(v2::message::Kind::AccountRuntimeReply(reply)) = message.kind else {
                panic!("reply")
            };
            reply.body
        }
    };
    serde_json::from_slice(&body).unwrap()
}
async fn assess(
    server: &mut StoppedNativeSession,
    client: &mut AuthenticatedConnection,
    original: StoppedNativeRequest,
) -> spaceterm_host::account_service::StoppedNativeAssessmentRef {
    let StoppedNativeReply::Assessment(reference) =
        call(server, client, StoppedNativeCall::Assess(original)).await
    else {
        panic!("assessment")
    };
    reference
}

#[tokio::test]
async fn paired_stopped_explicit_start_uses_saved_client_and_releases_store_instance_before_manager()
 {
    for major in [1, 2] {
        let fixture = Fixture::new(false).await;
        let before = fixture.recovered();
        let original = fixture.capture(StoppedNativeAction::Start);
        let (mut server, mut client) =
            connect(fixture.open(original, None).await, fixture.target, major).await;
        let reference = assess(&mut server, &mut client, original).await;
        assert_eq!(reference.epoch, fixture.epoch);
        assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
        let StoppedNativeReply::Accepted(accepted) = call(
            &mut server,
            &mut client,
            StoppedNativeCall::Admit(reference),
        )
        .await
        else {
            panic!("accepted")
        };
        assert_eq!(accepted.request, original.request);
        let result = server.finish().await.unwrap();
        assert_eq!(
            result.state,
            ServiceRequestState::Completed(ServiceOutcome::Started)
        );
        assert!(
            fixture
                .manager
                .startup_lease_released
                .load(Ordering::SeqCst)
        );
        assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .supervisor
                .discover()
                .unwrap()
                .unwrap()
                .desired_state(),
            DesiredServerState::Running
        );
        let after = fixture.recovered();
        assert_eq!(after.last_epoch, before.last_epoch);
        assert_eq!(after.organization, before.organization);
        assert_eq!(after.pairing, before.pairing);
        fixture
            .supervisor
            .acknowledge_stopped_native_exact(&result)
            .unwrap();
        fixture
            .supervisor
            .acknowledge_stopped_native_exact(&result)
            .unwrap();
        assert_eq!(
            fixture
                .supervisor
                .restore_stopped_native(original.request)
                .err(),
            Some(ServiceFailure::RequestRetired)
        );
    }
}

#[tokio::test]
async fn paired_stopped_signed_stage_and_exact_activation_keep_stop_and_canonical_state() {
    let fixture = Fixture::new(false).await;
    let before = fixture.recovered();
    let (_, archive) = artifact::fixture("1.2.4", 91, artifact::native_machine());
    let digest = archive.artifact_sha256();
    let original = fixture.capture(StoppedNativeAction::Stage {
        artifact_sha256: digest,
    });
    let (mut server, mut client) = connect(
        fixture.open(original, Some(archive)).await,
        fixture.target,
        2,
    )
    .await;
    let reference = assess(&mut server, &mut client, original).await;
    assert!(matches!(
        call(
            &mut server,
            &mut client,
            StoppedNativeCall::Admit(reference)
        )
        .await,
        StoppedNativeReply::Accepted(_)
    ));
    let staged = server.finish().await.unwrap();
    let ServiceRequestState::Completed(ServiceOutcome::Staged(status)) = &staged.state else {
        panic!("staged")
    };
    assert_eq!(status.artifact_sha256, digest);
    let selected = fixture.supervisor.discover().unwrap().unwrap();
    assert_eq!(selected.desired_state(), DesiredServerState::Stopped);
    assert_ne!(
        selected.approved_installation().unwrap().installation,
        status.installation
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
    fixture
        .supervisor
        .acknowledge_stopped_native_exact(&staged)
        .unwrap();
    fixture
        .supervisor
        .acknowledge_stopped_native_exact(&staged)
        .unwrap();

    let original = fixture.capture(StoppedNativeAction::Activate(status.installation));
    let (mut server, mut client) =
        connect(fixture.open(original, None).await, fixture.target, 2).await;
    let reference = assess(&mut server, &mut client, original).await;
    assert!(matches!(
        call(
            &mut server,
            &mut client,
            StoppedNativeCall::Admit(reference)
        )
        .await,
        StoppedNativeReply::Accepted(_)
    ));
    let activated = server.finish().await.unwrap();
    assert_eq!(
        activated.state,
        ServiceRequestState::Completed(ServiceOutcome::Activated(status.clone()))
    );
    let selected = fixture.supervisor.discover().unwrap().unwrap();
    assert_eq!(selected.desired_state(), DesiredServerState::Stopped);
    assert_eq!(selected.approved_installation(), Some(status));
    assert!(selected.staged_installation().is_none());
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
    let after = fixture.recovered();
    assert_eq!(after.organization, before.organization);
    assert_eq!(after.last_epoch, before.last_epoch);
    assert_eq!(after.pairing, before.pairing);
}

#[tokio::test]
async fn stopped_stage_refuses_a_verified_package_from_another_publisher_before_acceptance() {
    let fixture = Fixture::new(false).await;
    let (_, archive) = artifact::fixture("1.2.4", 92, artifact::native_machine());
    let original = fixture.capture(StoppedNativeAction::Stage {
        artifact_sha256: archive.artifact_sha256(),
    });
    let (mut server, mut client) = connect(
        fixture.open(original, Some(archive)).await,
        fixture.target,
        2,
    )
    .await;
    assert_eq!(
        call(
            &mut server,
            &mut client,
            StoppedNativeCall::Assess(original)
        )
        .await,
        StoppedNativeReply::Failed(ServiceFailure::ArtifactRejected)
    );
    drop(server);
    assert!(
        fixture
            .supervisor
            .discover()
            .unwrap()
            .unwrap()
            .staged_installation()
            .is_none()
    );
    assert_eq!(
        fixture
            .supervisor
            .resolve_stopped_native_exact(original)
            .await
            .err(),
        Some(ServiceFailure::Pending)
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn paired_stopped_uninstall_retains_identity_pairing_organization_and_terminal_ack() {
    let fixture = Fixture::new(false).await;
    let before = fixture.recovered();
    let original = fixture.capture(StoppedNativeAction::Uninstall);
    let (mut server, mut client) =
        connect(fixture.open(original, None).await, fixture.target, 2).await;
    let reference = assess(&mut server, &mut client, original).await;
    assert!(matches!(
        call(
            &mut server,
            &mut client,
            StoppedNativeCall::Admit(reference)
        )
        .await,
        StoppedNativeReply::Accepted(_)
    ));
    let result = server.finish().await.unwrap();
    assert_eq!(
        result.state,
        ServiceRequestState::Completed(ServiceOutcome::Uninstalled)
    );
    let retained = fixture.supervisor.discover().unwrap().unwrap();
    assert!(!retained.is_installed());
    assert_eq!(retained.server_pin(), fixture.target.pin);
    assert_eq!(retained.desired_state(), DesiredServerState::Stopped);
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
    let after = fixture.recovered();
    assert_eq!(after.organization, before.organization);
    assert_eq!(after.pairing, before.pairing);
    assert_eq!(after.last_epoch, before.last_epoch);
    assert_eq!(
        fixture
            .supervisor
            .resolve_stopped_native_exact(original)
            .await
            .unwrap(),
        result
    );
    fixture
        .supervisor
        .acknowledge_stopped_native_exact(&result)
        .unwrap();
    fixture
        .supervisor
        .acknowledge_stopped_native_exact(&result)
        .unwrap();
}

#[tokio::test]
async fn stopped_ordinary_auth_refuses_canonically_revoked_saved_pairing_without_reenrollment() {
    let fixture = Fixture::new(true).await;
    let before = fixture.recovered();
    let original = fixture.capture(StoppedNativeAction::Start);
    let executor = fixture.open(original, None).await;
    let (_, hello) = ClientHandshake::begin(
        fixture.target.pin,
        fixture.target.principal,
        Purpose::Ordinary,
        config(2),
        Arc::new(DeviceKey::new(42)),
    )
    .unwrap();
    assert_eq!(
        executor.accept_hello(&hello).await.err(),
        Some(ServiceFailure::PairingRequired)
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.recovered().pairing, before.pairing);
    assert_eq!(
        fixture
            .supervisor
            .resolve_stopped_native_exact(original)
            .await
            .err(),
        Some(ServiceFailure::Pending)
    );
}

#[tokio::test]
async fn stopped_ordinary_auth_requires_exact_saved_device_pairing_client_and_key() {
    for mismatch in 0..3 {
        let fixture = Fixture::new(false).await;
        let original = fixture.capture(StoppedNativeAction::Start);
        let mut target = fixture.target;
        if mismatch == 0 {
            target.principal.device = [99; 16];
        }
        if mismatch == 1 {
            target.principal.client = native::local(999);
        }
        let executor = AccountNativeMaintenanceExecutor::open(
            fixture.supervisor.clone(),
            fixture.home.clone(),
            target,
            original,
            None,
        )
        .await
        .unwrap();
        let (client, hello) = ClientHandshake::begin(
            target.pin,
            target.principal,
            Purpose::Ordinary,
            config(2),
            Arc::new(DeviceKey::new(if mismatch == 2 { 99 } else { 42 })),
        )
        .unwrap();
        if mismatch < 2 {
            assert_eq!(
                executor.accept_hello(&hello).await.err(),
                Some(ServiceFailure::PairingRequired)
            );
        } else {
            let (server, reply) = executor.accept_hello(&hello).await.unwrap();
            let (_, proof) = client.receive_server(&reply).unwrap();
            assert_eq!(
                server.receive_proof(&proof).await.err(),
                Some(ServiceFailure::PairingRequired)
            );
        }
        assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn stopped_absence_is_confirmed_before_existing_keys_are_opened() {
    let fixture = Fixture::new(false).await;
    let original = fixture.capture(StoppedNativeAction::Start);
    let registration = fixture.supervisor.discover().unwrap().unwrap();
    let listener =
        std::os::unix::net::UnixListener::bind(registration.endpoint().unwrap().path()).unwrap();
    fs::remove_file(fixture.account.data_directory().join("identity")).unwrap();
    assert_eq!(
        AccountNativeMaintenanceExecutor::open(
            fixture.supervisor.clone(),
            fixture.home.clone(),
            fixture.target,
            original,
            None
        )
        .await
        .err(),
        Some(ServiceFailure::CleanupUnresolved)
    );
    drop(listener);
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stopped_assessment_restore_preserves_original_expiry_and_never_promotes_reserved() {
    let fixture = Fixture::new(false).await;
    let original = fixture.capture(StoppedNativeAction::Start);
    let (mut server, mut client) =
        connect(fixture.open(original, None).await, fixture.target, 2).await;
    let reference = assess(&mut server, &mut client, original).await;
    drop(server);
    let preserved = fixture
        .supervisor
        .restore_stopped_native_reference(original)
        .unwrap();
    assert_eq!(preserved, reference);
    let reserved = fixture
        .supervisor
        .resolve_stopped_native_exact(original)
        .await
        .unwrap();
    assert_eq!(reserved.phase, ServicePhase::Reserved);
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
    let (mut server, mut client) =
        connect(fixture.open(original, None).await, fixture.target, 2).await;
    assert_eq!(assess(&mut server, &mut client, original).await, reference);
    fixture.manager.expire_status.store(true, Ordering::SeqCst);
    assert_eq!(
        call(
            &mut server,
            &mut client,
            StoppedNativeCall::Admit(reference.clone())
        )
        .await,
        StoppedNativeReply::Failed(ServiceFailure::StaleAssessment)
    );
    drop(server);
    assert_eq!(
        fixture
            .supervisor
            .restore_stopped_native_reference(original)
            .unwrap(),
        reference
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stopped_pairing_and_original_scope_cannot_be_replaced_by_a_public_reference() {
    let fixture = Fixture::new(false).await;
    let original = fixture.capture(StoppedNativeAction::Start);
    let (mut server, mut client) =
        connect(fixture.open(original, None).await, fixture.target, 2).await;
    let reference = assess(&mut server, &mut client, original).await;
    let mut replacement = reference.clone();
    replacement.request_digest = RequestDigest::from_bytes([77; 32]);
    assert_eq!(
        call(
            &mut server,
            &mut client,
            StoppedNativeCall::Admit(replacement)
        )
        .await,
        StoppedNativeReply::Failed(ServiceFailure::PairingRequired)
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
    assert!(matches!(
        call(
            &mut server,
            &mut client,
            StoppedNativeCall::Admit(reference)
        )
        .await,
        StoppedNativeReply::Accepted(_)
    ));
    assert_eq!(server.finish().await.unwrap().request, original.request);
}

#[tokio::test]
async fn stopped_owned_native_worker_completes_original_after_lost_admission_reply() {
    let fixture = Fixture::new(false).await;
    let original = fixture.capture(StoppedNativeAction::Start);
    let (mut server, mut client) =
        connect(fixture.open(original, None).await, fixture.target, 2).await;
    let reference = assess(&mut server, &mut client, original).await;
    let payload = Payload::Major2(v2::Message {
        kind: Some(v2::message::Kind::AccountRuntimeRequest(
            common::AccountRuntimeMessage {
                request: 2,
                request_generation: 1,
                body: serde_json::to_vec(&StoppedNativeCall::Admit(reference)).unwrap(),
            },
        )),
    });
    let bytes = client.encode(&payload).unwrap();
    let _lost_reply = server.frame(&bytes).await.unwrap();
    assert!(server.is_admitted());
    drop(server);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while fixture.manager.starts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        fixture
            .supervisor
            .resolve_stopped_native_exact(original)
            .await
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(result.request, original.request);
    assert_eq!(
        result.state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stopped_authentication_keeps_one_five_second_deadline_and_requires_capabilities() {
    let fixture = Fixture::new(false).await;
    let original = fixture.capture(StoppedNativeAction::Start);
    let executor = fixture.open(original, None).await;
    let (client, hello) = ClientHandshake::begin(
        fixture.target.pin,
        fixture.target.principal,
        Purpose::Ordinary,
        config(2),
        Arc::new(DeviceKey::new(42)),
    )
    .unwrap();
    let (server, reply) = executor.accept_hello(&hello).await.unwrap();
    let (_, proof) = client.receive_server(&reply).unwrap();
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    assert_eq!(
        server.receive_proof(&proof).await.err(),
        Some(ServiceFailure::PairingRequired)
    );
    tokio::time::resume();

    let executor = fixture.open(original, None).await;
    let mut advertisement = config(2);
    advertisement
        .capabilities
        .retain(|capability| *capability != CAPABILITY_REMOTE_NATIVE_LIFECYCLE);
    let (client, hello) = ClientHandshake::begin(
        fixture.target.pin,
        fixture.target.principal,
        Purpose::Ordinary,
        advertisement,
        Arc::new(DeviceKey::new(42)),
    )
    .unwrap();
    let (server, reply) = executor.accept_hello(&hello).await.unwrap();
    let (_, proof) = client.receive_server(&reply).unwrap();
    assert_eq!(
        server.receive_proof(&proof).await.err(),
        Some(ServiceFailure::PairingRequired)
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stopped_capture_checks_original_pin_uid_build_and_generation_under_its_lease() {
    let fixture = Fixture::new(false).await;
    let registration = fixture.supervisor.discover().unwrap().unwrap();
    let request = fixture.supervisor.allocate_request().unwrap();
    assert_eq!(
        fixture
            .supervisor
            .capture_stopped_native_expected(
                request,
                StoppedNativeAction::Start,
                fixture.target.key,
                fixture.target.pin,
                registration.generation() + 1
            )
            .err(),
        Some(ServiceFailure::StaleAssessment)
    );
    let original = fixture
        .supervisor
        .capture_stopped_native_expected(
            request,
            StoppedNativeAction::Start,
            fixture.target.key,
            fixture.target.pin,
            registration.generation(),
        )
        .unwrap();
    for mismatch in 0..3 {
        let mut target = fixture.target;
        match mismatch {
            0 => target.pin.public_key = [99; 32],
            1 => target.key.account_uid += 1,
            _ => target.key.build_identity = BuildIdentity::Preflight,
        }
        assert_eq!(
            AccountNativeMaintenanceExecutor::open(
                fixture.supervisor.clone(),
                fixture.home.clone(),
                target,
                original,
                None
            )
            .await
            .err(),
            Some(ServiceFailure::IdentityMismatch)
        );
    }
    assert_eq!(
        fixture.supervisor.restore_stopped_native(request).unwrap(),
        original
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stopped_uncertain_accepted_publication_is_owned_and_drained_after_eof() {
    let fixture = Fixture::new(false).await;
    let before = fixture.recovered();
    let original = fixture.capture(StoppedNativeAction::Start);
    let (mut server, mut client) =
        connect(fixture.open(original, None).await, fixture.target, 2).await;
    let reference = assess(&mut server, &mut client, original).await;
    spaceterm_host::account_service::test_support::fail_next_publication_durability();
    assert_eq!(
        call(
            &mut server,
            &mut client,
            StoppedNativeCall::Admit(reference.clone())
        )
        .await,
        StoppedNativeReply::Failed(ServiceFailure::Indeterminate)
    );
    assert!(server.needs_completion());
    // EOF ends observations, not exact-original publication recovery. A producer
    // joins this owner even though no successful Accepted reply was observed.
    drop(client);
    let result = server.finish().await.unwrap();
    assert_eq!(result.request, original.request);
    assert_eq!(result.request_digest, reference.request_digest);
    assert_eq!(
        result.state,
        ServiceRequestState::Completed(ServiceOutcome::Started)
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .manager
            .startup_lease_released
            .load(Ordering::SeqCst)
    );
    let after = fixture.recovered();
    assert_eq!(after.last_epoch, before.last_epoch);
    assert_eq!(after.organization, before.organization);
    assert_eq!(after.pairing, before.pairing);
}

#[tokio::test]
async fn stopped_unclean_signed_instance_is_not_replaced_by_an_empty_native_scope() {
    let fixture = Fixture::new(false).await;
    let original = fixture.capture(StoppedNativeAction::Start);
    let registration = fixture.supervisor.discover().unwrap().unwrap();
    let signer = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(fixture.target.pin)
        .unwrap();
    let _unclean = RuntimeCustody::begin(
        &registration,
        ServerEpoch::new(fixture.epoch.get() + 1),
        signer,
    )
    .unwrap();
    assert_eq!(
        AccountNativeMaintenanceExecutor::open(
            fixture.supervisor.clone(),
            fixture.home.clone(),
            fixture.target,
            original,
            None
        )
        .await
        .err(),
        Some(ServiceFailure::CleanupUnresolved)
    );
    assert_eq!(fixture.manager.starts.load(Ordering::SeqCst), 0);
}
