use super::*;
use crate::platform::local_server_client::foreground::ForegroundSetup;
use spaceterm_client::management_cli::ManagementFailure;
use spaceterm_client_native::{NativeClientProvider, native::artifacts::ArtifactTransport};
use spaceterm_host::account_service::{
    LingerAuthorization, LingerRequest, LingerState, RegisteredServer, ServiceClock,
    ServiceFailure, ServiceFuture, ServiceManager, ServiceTime, SupervisorState,
    test_support::AccountServiceFixture,
};
use spaceterm_host::startup::BuildIdentity;
use spaceterm_ui::a11y_testing::{A11yTree, perform};

#[derive(Default)]
struct Effects {
    manager: AtomicUsize,
    authorization: AtomicUsize,
}
impl ServiceManager for Effects {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        self.manager.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(SupervisorState::NotInstalled) })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        self.manager.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Some(LingerState::Disabled)) })
    }
    fn configure<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        self.manager.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ServiceFailure::SupervisorUnavailable) })
    }
    fn start<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        self.configure(registration)
    }
    fn stop<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        self.configure(registration)
    }
    fn uninstall<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        self.configure(registration)
    }
    fn verify_installation(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        self.manager.fetch_add(1, Ordering::SeqCst);
        Err(ServiceFailure::SupervisorUnavailable)
    }
}
impl LingerAuthorization for Effects {
    fn enable_linger<'a>(&'a self, _: &'a LingerRequest) -> ServiceFuture<'a, ()> {
        self.authorization.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ServiceFailure::AuthorizationDenied) })
    }
}
impl ServiceClock for Effects {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: [7; 32],
            millis: 1,
        })
    }
}
struct Downloads {
    calls: AtomicUsize,
    requested: tokio::sync::watch::Sender<bool>,
}
impl ArtifactTransport for Downloads {
    fn get(&self, _: &str, _: usize) -> Result<Vec<u8>, ManagementFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _ = self.requested.send(true);
        Err(ManagementFailure::NativeUnavailable)
    }
}
struct SetupFactory {
    owners: NativeDesktopOwners,
    setup: Arc<dyn ForegroundSetup>,
    offered: tokio::sync::watch::Sender<u64>,
    recovered: AtomicUsize,
}
impl NativeOwnerFactory for SetupFactory {
    fn foreground_setup(&self) -> ClientFuture<'_, Option<Arc<dyn ForegroundSetup>>> {
        Box::pin(async {
            self.offered.send_modify(|count| *count += 1);
            Ok(Some(self.setup.clone()))
        })
    }
    fn recover(&self) -> ClientFuture<'_, NativeDesktopOwners> {
        Box::pin(async {
            self.recovered.fetch_add(1, Ordering::SeqCst);
            Ok(self.owners.clone())
        })
    }
}

#[gpui::test]
async fn desktop_first_run_foreground_action_is_the_only_setup_admission_and_failure_keeps_saved_servers(
    cx: &mut gpui::TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::ui::init).unwrap();
    let executor = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let fixture = executor.block_on(Fixture::new());
    let root = crate::platform::local_server_client::tests::Root::new();
    let account = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let effects = Arc::new(Effects::default());
    let (requested, mut downloads_requested) = tokio::sync::watch::channel(false);
    let downloads = Arc::new(Downloads {
        calls: AtomicUsize::new(0),
        requested,
    });
    let native = NativeClientProvider::from_supervisor(account.with_providers(
        effects.clone(),
        Arc::new(spaceterm_server::account_startup::test_support::StartupAbsence),
        effects.clone(),
        Some(effects.clone()),
    ))
    .with_artifact_transport(downloads.clone());
    let setup =
        crate::platform::local_server_client::setup_offer(native, fixture.journal.clone(), None)
            .unwrap()
            .unwrap();
    let (offered, _offers) = tokio::sync::watch::channel(0);
    let factory = Arc::new(SetupFactory {
        owners: fixture.owners(),
        setup,
        offered,
        recovered: AtomicUsize::new(0),
    });
    let startup = || NativeDesktopStartup {
        owners: Arc::new(tokio::sync::Mutex::new(None)),
        factory: Some(factory.clone() as Arc<dyn NativeOwnerFactory>),
        store: SharedViewStore(Arc::new(std::sync::Mutex::new(fixture.store()))),
        consumed: Arc::default(),
        executor: executor.clone(),
        enrollment: Arc::default(),
    };
    let (window_created, windows_created) = async_channel::unbounded();
    let _windows = cx.update(|cx| {
        cx.observe_new::<crate::platform::local_server_client::foreground::SetupWindow>(
            move |_, window, _| {
                if let Some(window) = window {
                    let _ = window_created.try_send(window.window_handle());
                }
            },
        )
    });
    let first = startup();
    let recovery = cx.update(|cx| first.recover(cx));
    windows_created.recv().await.unwrap();
    cx.run_until_parked();
    assert_eq!(downloads.calls.load(Ordering::SeqCst), 0);
    assert_eq!(effects.manager.load(Ordering::SeqCst), 0);
    assert_eq!(effects.authorization.load(Ordering::SeqCst), 0);
    assert_eq!(factory.recovered.load(Ordering::SeqCst), 0);
    assert!(!account.anchor().exists());
    let mut window = gpui::VisualTestContext::from_window(cx.windows()[0], cx);
    let tree = A11yTree::read(&mut window);
    perform(
        &mut window,
        tree.node("Not Now"),
        gpui::accesskit::Action::Click,
    );
    drop(recovery.await.unwrap());
    assert_eq!(factory.recovered.load(Ordering::SeqCst), 1);
    assert_eq!(downloads.calls.load(Ordering::SeqCst), 0);
    assert!(!account.anchor().exists());

    let second = startup();
    let recovery = cx.update(|cx| second.recover(cx));
    windows_created.recv().await.unwrap();
    cx.run_until_parked();
    let mut window = gpui::VisualTestContext::from_window(cx.windows()[0], cx);
    let tree = A11yTree::read(&mut window);
    perform(
        &mut window,
        tree.node("Set Up Server"),
        gpui::accesskit::Action::Click,
    );
    downloads_requested.changed().await.unwrap();
    windows_created.recv().await.unwrap();
    cx.run_until_parked();
    assert_eq!(downloads.calls.load(Ordering::SeqCst), 1);
    assert_eq!(effects.manager.load(Ordering::SeqCst), 0);
    assert_eq!(effects.authorization.load(Ordering::SeqCst), 0);
    let mut window = gpui::VisualTestContext::from_window(cx.windows()[0], cx);
    let tree = A11yTree::read(&mut window);
    assert!(tree.find("Server setup did not complete").is_some());
    perform(
        &mut window,
        tree.node("Continue"),
        gpui::accesskit::Action::Click,
    );
    let recovered = recovery.await.unwrap();
    let installed = cx.update(|cx| recovered.install(cx).unwrap());
    assert_eq!(cx.update(|cx| installed.read(cx).servers().len()), 1);
    assert_eq!(factory.recovered.load(Ordering::SeqCst), 2);
    assert!(!account.anchor().exists());
    assert!(!account.data_directory().exists());
}
