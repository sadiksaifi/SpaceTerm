use super::*;
#[path = "../../../crates/spaceterm-host/tests/support/signed_installation.rs"]
mod signed_installation;
use spaceterm_distribution::UpdateKey;
use spaceterm_host::{
    account_service::{
        LingerState, NativeServiceInstallations, NativeServiceProcesses, RegisteredServer,
        ServiceClock, ServiceFailure, ServiceFuture, ServiceManager, ServiceSupervisor,
        ServiceTime, SupervisorState, test_support::AccountServiceFixture,
    },
    organization_store::{bootstrap::LegacyWorktreeLocation, migrations, schema},
    startup::{BuildIdentity, DesiredServerState},
    storage::Database,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

struct Manager {
    key: UpdateKey,
    running: AtomicBool,
    effects: Mutex<Vec<&'static str>>,
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
        Box::pin(async {
            Ok(if cfg!(target_os = "linux") {
                Some(LingerState::Enabled)
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
            self.running.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        panic!("foreground setup must not stop an existing service")
    }
    fn uninstall<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        panic!("foreground setup must not remove an existing service")
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
fn reopen_supervisor(account: &AccountServiceFixture, manager: Arc<Manager>) -> ServiceSupervisor {
    let processes = Arc::new(
        NativeServiceProcesses::current(Arc::new(NativeServiceInstallations))
            .unwrap()
            .with_manager(manager.clone()),
    );
    let provider = account.with_providers(manager.clone(), processes, Arc::new(Clock), None);
    account.with_installation_trust(provider, manager.key.clone())
}
fn template(registration: &RegisteredServer) -> String {
    let database = Database::open_existing_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    database
        .query_row(
            "SELECT worktree_location FROM server_settings WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}
fn setup(
    native: NativeClientProvider,
    journal: Arc<JournalController>,
    expected: Option<ServerPin>,
) -> NativeSetup {
    NativeSetup {
        native,
        journal,
        expected,
        legacy: Some(
            LegacyWorktreeLocation::parse("~/legacy/{repository}/{branch}".into()).unwrap(),
        ),
        install: tokio::sync::Mutex::new(None),
        execution: tokio::sync::Mutex::new(()),
    }
}

#[test]
fn desktop_confirmed_fresh_install_captures_legacy_worktree_location_once() {
    let root = crate::platform::local_server_client::tests::Root::new();
    let account = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let (key, artifact) =
        signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
    let manager = Arc::new(Manager {
        key,
        running: AtomicBool::new(false),
        effects: Mutex::new(vec![]),
    });
    let supervisor = reopen_supervisor(&account, manager);
    let (runtime, journal) = crate::platform::local_server_client::tests::journal();
    let setup = setup(
        NativeClientProvider::from_supervisor(supervisor.clone()),
        journal,
        None,
    );
    let request = supervisor.allocate_request().unwrap();
    runtime
        .block_on(supervisor.install(
            request,
            setup.seed(account.assess_test_install(artifact)).confirm(),
        ))
        .unwrap();
    assert_eq!(
        template(&supervisor.discover().unwrap().unwrap()),
        "~/legacy/{repository}/{branch}"
    );
}

#[test]
fn desktop_setup_preserves_existing_headless_worktree_location() {
    let root = crate::platform::local_server_client::tests::Root::new();
    let account = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let (key, artifact) =
        signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
    let manager = Arc::new(Manager {
        key,
        running: AtomicBool::new(false),
        effects: Mutex::new(vec![]),
    });
    let supervisor = reopen_supervisor(&account, manager.clone());
    let (runtime, journal) = crate::platform::local_server_client::tests::journal();
    runtime
        .block_on(supervisor.install(
            supervisor.allocate_request().unwrap(),
            account.confirm_install(artifact),
        ))
        .unwrap();
    let registration = supervisor.discover().unwrap().unwrap();
    let original = template(&registration);
    let setup = setup(
        NativeClientProvider::from_supervisor(supervisor.clone()),
        journal,
        Some(registration.server_pin()),
    );
    let (_, artifact) =
        signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
    runtime
        .block_on(supervisor.install(
            supervisor.allocate_request().unwrap(),
            setup.seed(account.assess_test_install(artifact)).confirm(),
        ))
        .unwrap();
    assert_eq!(template(&supervisor.discover().unwrap().unwrap()), original);
    assert_eq!(
        supervisor.discover().unwrap().unwrap().server_pin(),
        registration.server_pin()
    );
    assert_eq!(*manager.effects.lock().unwrap(), ["configure", "start"]);
}

#[test]
fn desktop_first_run_resolves_original_install_after_client_crash() {
    let root = crate::platform::local_server_client::tests::Root::new();
    let account = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let (key, artifact) =
        signed_installation::fixture("1.2.3", 7, signed_installation::native_machine());
    let manager = Arc::new(Manager {
        key,
        running: AtomicBool::new(false),
        effects: Mutex::new(vec![]),
    });
    let supervisor = reopen_supervisor(&account, manager.clone());
    let (runtime, journal) = crate::platform::local_server_client::tests::journal();
    let request = supervisor.allocate_request().unwrap();
    spaceterm_host::account_service::test_support::interrupt_after_phase(ServicePhase::Publishing);
    assert_eq!(
        runtime.block_on(
            supervisor.install(
                request,
                account
                    .assess_test_install(artifact)
                    .with_local_desktop_seed(Some(
                        LegacyWorktreeLocation::parse("~/original/{branch}".into()).unwrap()
                    ))
                    .confirm()
            )
        ),
        Err(ServiceFailure::SupervisorUnavailable)
    );
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let requests_before = supervisor.retained_initial_install_requests().unwrap();
    assert_eq!(requests_before.len(), 1);
    drop(supervisor);
    let recovered = reopen_supervisor(&account, manager.clone());
    let setup = offer(
        NativeClientProvider::from_supervisor(recovered.clone()),
        journal,
        Some(LegacyWorktreeLocation::parse("~/replacement/{branch}".into()).unwrap()),
    )
    .unwrap()
    .unwrap();
    assert!(
        manager.effects.lock().unwrap().is_empty(),
        "launch discovery must not resume installation"
    );
    // The fake service starts no server. Enrollment cannot connect, but the
    // original native installation must finish before that typed failure.
    assert!(runtime.block_on(setup.execute()).is_err());
    let original = recovered.inspect_install_request(request).unwrap().0;
    assert_eq!(original.phase, ServicePhase::Completed);
    assert!(matches!(
        original.state,
        ServiceRequestState::Completed(ServiceOutcome::Installed { .. })
    ));
    assert_eq!(
        recovered.retained_initial_install_requests().unwrap(),
        vec![original]
    );
    assert_eq!(recovered.discover().unwrap().unwrap().server_pin(), pin);
    assert_eq!(
        template(&recovered.discover().unwrap().unwrap()),
        "~/original/{branch}"
    );
    assert_eq!(*manager.effects.lock().unwrap(), ["configure", "start"]);
    assert_eq!(
        recovered.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
}
