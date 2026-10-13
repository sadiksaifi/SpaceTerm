use super::*;
use spaceterm_client_native::NativeClientProvider;
use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    identity::{ClientId, DeviceId},
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    pairing::{CredentialReference, LocalPairingGeneration},
    retirement::SessionCleanupObservation,
};
use spaceterm_host::{
    account_service::{
        InstallationStatus, LingerState, RegisteredServer, RuntimeAssessment, ServiceClock,
        ServiceFailure, ServiceFuture, ServiceManager, ServiceProcesses, ServiceRequestState,
        ServiceTime, SupervisorState, test_support::AccountServiceFixture,
    },
    startup::{BuildIdentity, DesiredServerState},
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

struct Root(std::path::PathBuf);
impl Root {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        #[cfg(target_os = "macos")]
        let parent = std::path::PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let parent = std::env::temp_dir();
        let path = parent.join(format!(
            "spaceterm-settings-native-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
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
    state: Mutex<SupervisorState>,
    effects: Mutex<Vec<&'static str>>,
    block_start: AtomicBool,
    start_entered: tokio::sync::Notify,
    start_release: tokio::sync::Notify,
}
impl ServiceManager for Manager {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async { Ok(*self.state.lock().unwrap()) })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async { Ok(cfg!(target_os = "linux").then_some(LingerState::Enabled)) })
    }
    fn configure<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.effects.lock().unwrap().push("configure");
            if registration.desired_state() == DesiredServerState::Stopped {
                *self.state.lock().unwrap() = SupervisorState::Disabled;
            }
            Ok(())
        })
    }
    fn start<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.lock().unwrap().push("start");
            *self.state.lock().unwrap() = SupervisorState::Running;
            if self.block_start.load(Ordering::SeqCst) {
                self.start_entered.notify_one();
                self.start_release.notified().await;
            }
            Ok(())
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.lock().unwrap().push("stop");
            *self.state.lock().unwrap() = SupervisorState::Inactive;
            Ok(())
        })
    }
    fn uninstall<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            self.effects.lock().unwrap().push("uninstall");
            *self.state.lock().unwrap() = SupervisorState::NotInstalled;
            Ok(())
        })
    }
    fn verify_installation(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        Ok(())
    }
}
struct Processes(AtomicU64);
impl ServiceProcesses for Processes {
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            let mut token = [7; 32];
            token[..8].copy_from_slice(&self.0.fetch_add(1, Ordering::SeqCst).to_be_bytes());
            Ok(RuntimeAssessment {
                server: ServerId::new(registration.server_pin().identity),
                epoch: None,
                generation: 1,
                sessions: vec![],
                token,
            })
        })
    }
    fn revalidate<'a>(
        &'a self,
        _: &'a RegisteredServer,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
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
        _: NativeRetirementAction,
        _: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn verify_absent<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
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
struct Fixture {
    _root: Root,
    account: AccountServiceFixture,
    manager: Arc<Manager>,
    processes: Arc<Processes>,
    clock: Arc<Clock>,
}
impl Fixture {
    fn new(desired: Option<DesiredServerState>) -> Self {
        let root = Root::new();
        let account = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        if let Some(desired) = desired {
            let installation: InstallationStatus = serde_json::from_value(serde_json::json!({
                "installation":1,"version":"1.2.3","artifact_sha256":vec![3;32]
            }))
            .unwrap();
            account
                .initialize_with_installation(desired, Some(installation))
                .unwrap();
        }
        let state = match desired {
            Some(DesiredServerState::Running) => SupervisorState::Running,
            Some(DesiredServerState::Stopped) => SupervisorState::Disabled,
            None => SupervisorState::NotInstalled,
        };
        Self {
            _root: root,
            account,
            manager: Arc::new(Manager {
                state: Mutex::new(state),
                effects: Mutex::new(vec![]),
                block_start: AtomicBool::new(false),
                start_entered: tokio::sync::Notify::new(),
                start_release: tokio::sync::Notify::new(),
            }),
            processes: Arc::new(Processes(AtomicU64::new(1))),
            clock: Arc::new(Clock(AtomicU64::new(100))),
        }
    }
    fn provider(&self) -> NativeClientProvider {
        NativeClientProvider::from_supervisor(self.account.with_providers(
            self.manager.clone(),
            self.processes.clone(),
            self.clock.clone(),
            None,
        ))
    }
}

fn binding(registration: &RegisteredServer, generation: u64) -> PairedServerBinding {
    let server = ServerId::new(registration.server_pin().identity);
    let route =
        spaceterm_client_native::route::LocalRouteAuthority::new(registration.endpoint().unwrap())
            .unwrap();
    PairedServerBinding::new(
        spaceterm_client::api::ConnectionTarget::new(
            registration.server_pin(),
            DeviceId::new([17; 16]),
            PairingId::new(server, 1).unwrap(),
            ClientId::new(server, 1).unwrap(),
            route.description().clone(),
            CredentialReference::new([23; 16]),
            LocalPairingGeneration::new(generation).unwrap(),
        )
        .unwrap(),
    )
}

#[test]
fn servers_settings_uses_real_native_provider_for_selected_local_pairing() {
    let fixture = Fixture::new(Some(DesiredServerState::Stopped));
    let provider = fixture.provider();
    let registration = provider
        .management()
        .supervisor()
        .discover()
        .unwrap()
        .unwrap();
    let original = binding(&registration, 1);
    let mut services = SettingsServices::default();
    services
        .admit_native(&provider, &registration, &original)
        .unwrap();
    assert_eq!(services.hosts.len(), 1);
    assert!(Arc::ptr_eq(&services.hosts[0].1, &provider.management()));
    assert_eq!(
        services.hosts[0].0,
        original.target().local_pairing().unwrap()
    );
    assert_ne!(
        services.hosts[0].0,
        binding(&registration, 2).target().local_pairing().unwrap()
    );
    assert_eq!(
        services.lifecycles[0].1,
        if cfg!(target_os = "macos") {
            ServerServiceLifecycle::LoginSession
        } else {
            ServerServiceLifecycle::BootWithLingering
        }
    );
    assert_eq!(services.machines[0].1.scopes.len(), 2);
    assert!(
        services.machines[0]
            .1
            .scopes
            .iter()
            .flat_map(|scope| &scope.endpoints)
            .all(|endpoint| endpoint.port() == 47232)
    );
    assert!(fixture.manager.effects.lock().unwrap().is_empty());
}

#[test]
fn servers_settings_uses_real_native_provider_without_granting_remote_route_host_authority() {
    let fixture = Fixture::new(Some(DesiredServerState::Stopped));
    let provider = fixture.provider();
    let registration = provider
        .management()
        .supervisor()
        .discover()
        .unwrap()
        .unwrap();
    let original = binding(&registration, 1);
    let target = original.target();
    let remote = PairedServerBinding::new(
        spaceterm_client::api::ConnectionTarget::new(
            target.pin(),
            target.device(),
            target.pairing(),
            target.client(),
            spaceterm_client::api::RouteDescription::new(RouteParts::Wss {
                address: "127.0.0.1:443".parse().unwrap(),
                server_name: "paired-server".into(),
            })
            .unwrap(),
            target.credential(),
            target.incarnation(),
        )
        .unwrap(),
    );
    let mut services = SettingsServices::default();
    services
        .admit_native(&provider, &registration, &remote)
        .unwrap();
    assert!(services.hosts.is_empty());
    assert!(services.lifecycles.is_empty());
    assert!(services.machines.is_empty());
}

#[tokio::test]
async fn servers_settings_native_pending_request_resolves_without_reallocation() {
    let fixture = Fixture::new(Some(DesiredServerState::Stopped));
    let provider = fixture.provider();
    let native = provider.management();
    let registration = native.supervisor().discover().unwrap().unwrap();
    let binding = binding(&registration, 1);
    let original = native.supervisor().allocate_request().unwrap();
    fixture.manager.block_start.store(true, Ordering::SeqCst);
    let submitted = native.clone();
    let start = tokio::spawn(async move { submitted.start(original).await });
    fixture.manager.start_entered.notified().await;
    // A new application owner uses the same durable host journal while the original effect is in flight.
    let reopened = fixture.provider().management();
    let read = recover_native(&reopened, &binding).await.unwrap();
    assert_eq!(read.status.pending, Some(original));
    assert!(
        matches!(read.recovered, Some(ResultPublication::Native { request, unresolved: true, .. }) if request == original)
    );
    fixture.manager.start_release.notify_one();
    let completed = start.await.unwrap().unwrap();
    assert!(matches!(completed.state, ServiceRequestState::Completed(_)));
    // The lost completed reply is recovered under that same ID, without replaying Start.
    let read = recover_native(&fixture.provider().management(), &binding)
        .await
        .unwrap();
    assert!(
        matches!(read.recovered, Some(ResultPublication::Native { request, unresolved: false, failure: None }) if request == original)
    );
    let next = native.supervisor().allocate_request().unwrap();
    assert_eq!(
        u64::from_be_bytes(next.as_bytes()[8..].try_into().unwrap()),
        u64::from_be_bytes(original.as_bytes()[8..].try_into().unwrap()) + 1
    );
    assert_eq!(
        fixture.manager.effects.lock().unwrap().as_slice(),
        &["configure", "start"]
    );
}

#[tokio::test]
async fn servers_settings_uses_real_native_provider_for_stop_assessment() {
    let fixture = Fixture::new(Some(DesiredServerState::Running));
    let native = fixture.provider().management();
    let registration = native.supervisor().discover().unwrap().unwrap();
    let original = binding(&registration, 1);
    check_native(&native, &original).await.unwrap();
    let assessment = native.assess(ServiceAction::Stop).await.unwrap();
    let prompt = NativeActionPrompt::from_assessment(&assessment).unwrap();
    let restored = native
        .restore_assessment(ServiceAction::Stop, prompt.token)
        .await
        .unwrap();
    let result = native_result(
        prompt.request,
        native.stop(restored.confirm().unwrap()).await,
    );
    assert!(
        matches!(result, ResultPublication::Native { request, unresolved: false, failure: None } if request == prompt.request)
    );
    assert_eq!(
        native
            .supervisor()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
}
