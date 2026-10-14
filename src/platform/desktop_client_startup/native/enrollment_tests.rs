use super::*;
use gpui::TestAppContext;

#[gpui::test]
async fn desktop_production_factory_installs_foreground_enrollment_with_empty_registry(
    cx: &mut TestAppContext,
) {
    use crate::platform::{
        app_directories::{AppDirectories, AppDirectoryEnvironment},
        unix_secure_filesystem::UnixSecureFilesystem,
    };
    use crate::ui::settings_window::server_enrollment::{
        EnrollmentAttempt, EnrollmentFailure, EnrollmentTarget, InstalledEnrollment,
    };
    cx.executor().allow_parking();
    let root = crate::platform::local_server_client::tests::Root::new();
    let environment = AppDirectoryEnvironment {
        home: Some(root.0.as_os_str().to_owned()),
        xdg_state_home: Some(root.0.join("state").into_os_string()),
        ..Default::default()
    };
    let paths = Arc::new(
        AppPaths::from_directories(
            AppDirectories::resolve_xdg("enrollment-composition-test", &environment, None).unwrap(),
            104,
            Arc::new(UnixSecureFilesystem),
        )
        .unwrap(),
    );
    let executor = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let storage = Arc::new(spaceterm_client::testing::MemoryJournal::default());
    let journal = executor
        .block_on(JournalController::recover(
            storage,
            Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        ))
        .map(Arc::new)
        .unwrap();
    let enrollment = Arc::default();
    let startup = NativeDesktopStartup {
        owners: Arc::new(tokio::sync::Mutex::new(None)),
        factory: Some(Arc::new(Factory {
            #[cfg(all(feature = "desktop-test-support", debug_assertions))]
            fixture: None,
            paths: paths.clone(),
            ssh: crate::platform::server_client_route::SshStartup {
                executable: "/usr/bin/ssh".into(),
                home: root.0.clone(),
                environment: vec![],
            },
            askpass: crate::platform::server_client_route::askpass::DesktopTunnelAskPass::new(
                paths.clone(),
                Arc::new(crate::platform::unix_askpass_transport::AskPassWindowFactory::new(None)),
            ),
            // Journal storage and native account discovery are external boundaries.
            // The real Factory, route connector, pairing runtime and installer remain.
            context: tokio::sync::Mutex::new(Some(Arc::new(Context {
                journal: journal.clone(),
                native: None,
                native_failure: None,
            }))),
            enrollment: Arc::clone(&enrollment),
        })),
        store: SharedViewStore(Arc::new(std::sync::Mutex::new(NativeViewStore::new(paths)))),
        consumed: Arc::default(),
        executor,
        enrollment,
    };
    let recovered = cx.update(|cx| startup.recover(cx)).await.unwrap();
    let runtime = cx.update(|cx| recovered.install(cx)).unwrap();
    assert!(cx.update(|cx| runtime.read(cx).servers().is_empty()));
    let provider = cx.update(|cx| cx.global::<InstalledEnrollment>().0.clone());
    let result = cx
        .update(|cx| {
            provider.inspect(
                EnrollmentTarget::Direct {
                    address: "wss://127.0.0.1:443".into(),
                    invitation: zeroize::Zeroizing::new("invalid".into()),
                },
                EnrollmentAttempt::new(1),
                cx,
            )
        })
        .await;
    assert!(matches!(
        result,
        Err(EnrollmentFailure::Pairing(PairingFailure::InvalidRequest))
    ));
    assert!(journal.snapshot().unwrap().bindings().is_empty());
}

/// Keep the production Factory, local discovery and native route composition. Only
/// the journal, account service and native credential boundaries are substituted.
pub(crate) fn pending_ipc_startup(
    paths: Arc<AppPaths>,
    journal: Arc<JournalController>,
    native: NativeClientProvider,
    credentials: Arc<dyn NativeDeviceCredentials>,
    ordinary_credentials: Arc<dyn CredentialSource>,
    executor: Arc<tokio::runtime::Runtime>,
) -> Rc<dyn DesktopClientStartup> {
    struct CredentialFactory {
        factory: Factory,
        credentials: Arc<dyn NativeDeviceCredentials>,
        ordinary: Arc<dyn CredentialSource>,
    }
    impl NativeOwnerFactory for CredentialFactory {
        fn foreground_setup(
            &self,
        ) -> ClientFuture<
            '_,
            Option<Arc<dyn crate::platform::local_server_client::foreground::ForegroundSetup>>,
        > {
            self.factory.foreground_setup()
        }
        fn recover(&self) -> ClientFuture<'_, NativeDesktopOwners> {
            Box::pin(async {
                let credentials = self.credentials.clone();
                let mut owners = self
                    .factory
                    .recover_with_enrollment(move |journal, routes, invalidator, local, executor| {
                        let connector = Arc::new(
                            spaceterm_client_native::restricted_routes::NativePairingConnector::with_shared_routes(
                                journal.clone(), routes.clone(),
                            ),
                        );
                        let mut services = crate::ui::settings_window::server_enrollment::native::production(
                            journal.clone(), routes, invalidator.clone(), local, executor,
                        );
                        services.pairing = Arc::new(
                            spaceterm_client_native::pairing_runtime::NativePairingRuntime::with_adapters(
                                journal,
                                credentials,
                                connector.clone(),
                                connector,
                                invalidator,
                                spaceterm_protocol::authentication::HandshakeConfig::default(),
                            ),
                        );
                        services
                    })
                    .await?;
                owners.services.credentials = self.ordinary.clone();
                Ok(owners)
            })
        }
    }
    let enrollment = Arc::default();
    Rc::new(NativeDesktopStartup {
        owners: Arc::new(tokio::sync::Mutex::new(None)),
        factory: Some(Arc::new(CredentialFactory {
            factory: Factory {
                #[cfg(all(feature = "desktop-test-support", debug_assertions))]
                fixture: None,
                paths: paths.clone(),
                ssh: crate::platform::server_client_route::SshStartup {
                    executable: "/usr/bin/ssh".into(),
                    home: paths.managed_ssh_config().parent().unwrap().to_owned(),
                    environment: vec![],
                },
                askpass: crate::platform::server_client_route::askpass::DesktopTunnelAskPass::new(
                    paths.clone(),
                    Arc::new(
                        crate::platform::unix_askpass_transport::AskPassWindowFactory::new(None),
                    ),
                ),
                context: tokio::sync::Mutex::new(Some(Arc::new(Context {
                    journal,
                    native: Some(native),
                    native_failure: None,
                }))),
                enrollment: Arc::clone(&enrollment),
            },
            credentials,
            ordinary: ordinary_credentials,
        })),
        store: SharedViewStore(Arc::new(std::sync::Mutex::new(NativeViewStore::new(paths)))),
        consumed: Arc::default(),
        executor,
        enrollment,
    })
}
