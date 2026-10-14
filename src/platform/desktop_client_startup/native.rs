//! Production native desktop owners. All routes use the pinned authenticated actor.
//! Recovery reads retained bindings. Only the explicit foreground setup offer may install or enroll.

mod organization_cache;
pub(super) mod protocol;
#[cfg(test)]
pub(super) use organization_cache::OrganizationCache;
#[cfg(all(feature = "desktop-test-support", debug_assertions))]
pub(super) mod test_composition;

use super::*;
use crate::platform::{app_paths::AppPaths, client_view_store::NativeViewStore};
use spaceterm_client::{
    api::ConnectionTarget,
    failure::{ClientFailure, ClientFailureClass},
    journal::{JournalLimits, ReceiptVerifier},
    pairing::{
        credentials::NativeDeviceCredentials,
        journal::{JournalPairingStore, LocalPairingStore},
    },
    ports::{ClientFuture, ConnectionPurpose, CredentialSource, SigningAccess},
    projection::ProjectionBudgets,
};
use spaceterm_client_native::{NativeClientProvider, credentials::adapter::DeviceCredentials};
use spaceterm_core::pairing::{PairingFailure, SignedPairingReceipt};
use spaceterm_protocol::{authentication::ServerPin, pairing_adapter::ReceiptCodec};
use std::sync::atomic::AtomicBool;

pub(crate) fn production(
    paths: Arc<AppPaths>,
    ssh: crate::platform::server_client_route::SshStartup,
    askpass: Arc<dyn crate::ssh::askpass::AskPassWindowFactory>,
) -> Result<Rc<dyn DesktopClientStartup>, crate::desktop_profile::DesktopProfileError> {
    #[cfg(all(feature = "desktop-test-support", debug_assertions))]
    let fixture = test_composition::Fixture::from_arguments(std::env::args_os().skip(1))?;
    let executor = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| crate::desktop_profile::DesktopProfileError::InvalidCombination)?;
    let enrollment = Arc::default();
    Ok(Rc::new(NativeDesktopStartup {
        owners: Arc::new(tokio::sync::Mutex::new(None)),
        factory: Some(Arc::new(Factory {
            #[cfg(all(feature = "desktop-test-support", debug_assertions))]
            fixture,
            paths: paths.clone(),
            legacy: std::sync::Mutex::new(None),
            context: tokio::sync::Mutex::new(None),
            enrollment: Arc::clone(&enrollment),
            ssh,
            askpass: crate::platform::server_client_route::askpass::DesktopTunnelAskPass::new(
                paths.clone(),
                askpass,
            ),
        })),
        store: SharedViewStore(Arc::new(std::sync::Mutex::new(NativeViewStore::new(paths)))),
        consumed: Arc::new(AtomicBool::new(false)),
        executor: Arc::new(executor),
        enrollment,
    }))
}

struct Factory {
    #[cfg(all(feature = "desktop-test-support", debug_assertions))]
    fixture: Option<test_composition::Fixture>,
    paths: Arc<AppPaths>,
    ssh: crate::platform::server_client_route::SshStartup,
    askpass: Arc<crate::platform::server_client_route::askpass::DesktopTunnelAskPass>,
    legacy: std::sync::Mutex<Option<spaceterm_host::organization_store::bootstrap::LegacyWorktreeLocation>>,
    context: tokio::sync::Mutex<Option<Arc<Context>>>,
    enrollment: Arc<
        std::sync::Mutex<
            Option<crate::ui::settings_window::server_enrollment::native::NativeEnrollmentServices>,
        >,
    >,
}
struct Context {
    journal: Arc<JournalController>,
    native: Option<NativeClientProvider>,
    native_failure: Option<ClientFailure>,
}
impl Factory {
    async fn context(&self) -> Result<Arc<Context>, ClientFailure> {
        let mut retained = self.context.lock().await;
        if let Some(context) = retained.as_ref() {
            return Ok(context.clone());
        }
        use crate::platform::{
            app_directories::AppDirectoryRoot,
            client_journal::{self, JournalOpenMode},
        };
        let paths = self.paths.clone();
        let inspect = paths.clone();
        let existing = tokio::task::spawn_blocking(move || {
            inspect
                .filesystem()
                .open_private_directory(
                    &inspect
                        .directories()
                        .root(AppDirectoryRoot::State)
                        .join("client-connections")
                        .join("desktop"),
                )
                .map(|directory| directory.is_some())
                .map_err(|_| fail(ClientFailureClass::StorageUnavailable))
        })
        .await
        .map_err(|_| fail(ClientFailureClass::StorageUnavailable))??;
        let journal = client_journal::open(
            &paths,
            if existing {
                JournalOpenMode::Existing
            } else {
                JournalOpenMode::Initialize
            },
            Arc::new(spaceterm_core::operation_ledger::canonical::CanonicalCodec),
            Arc::new(Verifier),
            JournalLimits::default(),
        )
        .await?;
        #[cfg(all(feature = "desktop-test-support", debug_assertions))]
        let fixture_native = self
            .fixture
            .as_ref()
            .map(|fixture| fixture.native())
            .transpose()?;
        let native = tokio::task::spawn_blocking(move || {
            #[cfg(all(feature = "desktop-test-support", debug_assertions))]
            if let Some(native) = fixture_native {
                return Ok(native);
            }
            #[cfg(target_os = "linux")]
            let authorization = Some(Arc::new(
                spaceterm_host::account_service::PkexecLingerAuthorization::new(),
            )
                as Arc<dyn spaceterm_host::account_service::LingerAuthorization>);
            #[cfg(not(target_os = "linux"))]
            let authorization = None;
            NativeClientProvider::with_authorization(authorization)
        })
        .await
        .map_err(|_| fail(ClientFailureClass::Unavailable))?;
        let native_failure = native.as_ref().err().copied().map(|failure| {
            use spaceterm_client::management_cli::ManagementFailure;
            fail(match failure {
                ManagementFailure::StorageLocked
                | ManagementFailure::StorageUnavailable
                | ManagementFailure::CredentialUnavailable => {
                    ClientFailureClass::StorageUnavailable
                }
                ManagementFailure::UnsafeNativeState => ClientFailureClass::UnsafeStorage,
                ManagementFailure::AuthorizationDenied | ManagementFailure::Revoked => {
                    ClientFailureClass::AuthorizationDenied
                }
                ManagementFailure::Incompatible => ClientFailureClass::Incompatible,
                ManagementFailure::Indeterminate | ManagementFailure::OutcomePending => {
                    ClientFailureClass::RecoveryRequired
                }
                ManagementFailure::LimitExceeded => ClientFailureClass::ResourceLimit,
                _ => ClientFailureClass::Unavailable,
            })
        });
        let native = native.ok();
        let context = Arc::new(Context {
            journal,
            native,
            native_failure,
        });
        *retained = Some(context.clone());
        Ok(context)
    }
}
impl NativeOwnerFactory for Factory {
    fn bind_window(&self, window: &gpui::Window, cx: &mut App) {
        self.askpass.bind_window(window, cx);
        if let Some(appearance) = cx.try_global::<crate::ui::appearance_runtime::AppearanceRuntime>()
            && let Ok(mut legacy) = self.legacy.lock()
        {
            // Only committed, validated client preferences can seed confirmed local creation.
            *legacy = spaceterm_host::organization_store::bootstrap::LegacyWorktreeLocation::parse(
                appearance.settings.snapshot().committed.git.worktree_path_template.clone(),
            ).ok();
        }
    }
    fn foreground_setup(
        &self,
    ) -> ClientFuture<
        '_,
        Option<Arc<dyn crate::platform::local_server_client::foreground::ForegroundSetup>>,
    > {
        Box::pin(async {
            #[cfg(all(feature = "desktop-test-support", debug_assertions))]
            if self.fixture.is_some() {
                return Ok(None);
            }
            let context = self.context().await?;
            let Some(native) = context.native.clone() else {
                return Ok(None);
            };
            let journal = context.journal.clone();
            let legacy = self.legacy.lock().map_err(|_| fail(ClientFailureClass::StorageUnavailable))?.clone();
            // Failure of local native discovery cannot retire healthy saved remote servers.
            tokio::task::spawn_blocking(move || {
                crate::platform::local_server_client::setup_offer(native, journal, legacy)
            })
            .await
            .map_err(|_| fail(ClientFailureClass::Unavailable))
            .map(|offer| offer.ok().flatten())
        })
    }
    fn recover(&self) -> ClientFuture<'_, NativeDesktopOwners> {
        Box::pin(async {
            let context = self.context().await?;
            let journal = context.journal.clone();
            let native = context.native.clone();
            #[cfg(all(feature = "desktop-test-support", debug_assertions))]
            if let Some(fixture) = &self.fixture {
                fixture
                    .enroll(
                        native
                            .as_ref()
                            .ok_or_else(|| fail(ClientFailureClass::Unavailable))?,
                        journal.clone(),
                    )
                    .await?;
            }
            let local = if let Some(provider) = native.clone() {
                let discovery_journal = journal.clone();
                tokio::task::spawn_blocking(move || {
                    crate::platform::local_server_client::discover(&provider, &discovery_journal)
                })
                .await
                .map_err(|_| fail(ClientFailureClass::Unavailable))?
                .ok()
                .flatten()
            } else {
                None
            };
            let local_server = local
                .as_ref()
                .map(|local| {
                    spaceterm_core::identity::ServerId::new(
                        local.registration.server_pin().identity,
                    )
                })
                .or_else(|| {
                    journal
                        .snapshot()
                        .ok()?
                        .bindings()
                        .iter()
                        .find_map(|saved| {
                            let binding = saved.binding().ok()?;
                            matches!(
                                binding.target().route().parts(),
                                spaceterm_client::api::RouteParts::Ipc { .. }
                            )
                            .then(|| binding.target().server())
                        })
                });
            let mut settings =
                crate::ui::settings_window::server_runtime::SettingsServices::default();
            if let (Some(native), Some(local)) = (&native, &local)
                && let Some(binding) = &local.binding
            {
                // Endpoint admission failure disables only this computer's native Settings
                // capability. Its connection actor retains the classified local failure.
                let _ = settings.admit_native(native, &local.registration, binding);
            }
            let paths = self.paths.clone();
            let ssh = self.ssh.clone();
            let askpass = self.askpass.clone();
            let configuration = tokio::task::spawn_blocking(move || {
                crate::platform::server_client_route::ssh_configuration(&paths, &ssh, Some(askpass))
            })
            .await
            .map_err(|_| fail(ClientFailureClass::Unavailable))?;
            // Restricted pairing shares the same native transport configuration.
            // Ordinary IPC still refreshes trusted registration on every attempt.
            let local_routes = local
                .and_then(|local| {
                    local.route.or_else(|| {
                        local.binding.as_ref()?;
                        spaceterm_client_native::route::LocalRouteAuthority::new(
                            local.registration.endpoint().ok()?,
                        )
                        .ok()
                    })
                })
                .into_iter()
                .collect();
            let (remote, pairing_routes): (Arc<dyn spaceterm_client::ports::RouteConnector>, _) =
                match configuration {
                    Ok((configuration, owner)) => {
                        let shared = crate::platform::server_client_route::connector(
                            local_routes,
                            Some(configuration),
                        )?;
                        (
                            crate::platform::server_client_route::retain_runtime(
                                shared.clone(),
                                owner,
                            ),
                            shared,
                        )
                    }
                    Err(_) => {
                        let shared =
                            crate::platform::server_client_route::connector(local_routes, None)?;
                        (shared.clone(), shared)
                    }
                };
            let routes = crate::platform::server_client_route::refreshing_connector_with_failure(
                native.clone(),
                context.native_failure,
                journal.clone(),
                remote,
            );
            let tracker = OperationTracker::new(journal.clone());
            let protocol = Arc::new(protocol::NativeProtocol::new(
                journal.clone(),
                tracker.clone(),
            ));
            let channels = Arc::new(PaneChannels::new(
                protocol.clone(),
                ProjectionBudgets::supported()?,
            )?);
            protocol.set_channels(channels.clone())?;
            let enrollment = crate::ui::settings_window::server_enrollment::native::production(
                journal.clone(),
                pairing_routes,
                protocol.clone(),
                native.clone(),
                tokio::runtime::Handle::current(),
            );
            *self
                .enrollment
                .lock()
                .map_err(|_| fail(ClientFailureClass::Unavailable))? = Some(enrollment);
            protocol.set_cache(organization_cache::OrganizationCache::new(
                self.paths.clone(),
            ))?;
            let credentials: Arc<dyn CredentialSource> = Arc::new(Credentials {
                store: JournalPairingStore::new(journal.clone()),
                credentials: DeviceCredentials::native(),
            });
            #[cfg(all(feature = "desktop-test-support", debug_assertions))]
            let credentials: Arc<dyn CredentialSource> = match &self.fixture {
                Some(fixture) => Arc::new(Credentials {
                    store: JournalPairingStore::new(journal.clone()),
                    credentials: fixture.credentials.clone(),
                }),
                None => credentials,
            };
            Ok(NativeDesktopOwners {
                #[cfg(all(feature = "desktop-test-support", debug_assertions))]
                fixture: self.fixture.is_some(),
                services: ClientServices {
                    routes,
                    credentials,
                    journal: journal.clone(),
                    organization: Arc::new(protocol::Decoder),
                    delegate: protocol.clone(),
                    discovery: native
                        .map(crate::platform::local_server_client::trusted_discovery)
                        .or_else(|| {
                            context
                                .native_failure
                                .map(crate::platform::server_client_route::failed_local_discovery)
                        }),
                    limits: spaceterm_client::limits::ClientLimits::default(),
                    additional_capabilities: protocol::desktop_capabilities(),
                },
                journal,
                channels,
                tracker: Some(tracker),
                operations: protocol.clone(),
                invalidator: protocol.clone(),
                local_server,
                registry_binding: Some(protocol),
                settings,
            })
        })
    }
}

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        let claims = ReceiptCodec::new(pin)
            .verify(receipt)
            .map_err(|_| fail(ClientFailureClass::InvalidStorage))?;
        if claims != receipt.parts().claims {
            return Err(fail(ClientFailureClass::InvalidStorage));
        }
        Ok(())
    }
}

struct Credentials<N> {
    store: JournalPairingStore<JournalController>,
    credentials: N,
}
impl<N: NativeDeviceCredentials + Send + Sync> CredentialSource for Credentials<N> {
    fn signing_access<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, SigningAccess> {
        Box::pin(async move {
            if purpose != ConnectionPurpose::Ordinary {
                return Err(fail(ClientFailureClass::AuthorizationDenied));
            }
            let key = target.local_pairing()?;
            let snapshot = self.store.recover().await.map_err(pairing_failure)?;
            let saved = snapshot
                .active(&key)
                .ok_or_else(|| fail(ClientFailureClass::AuthorizationDenied))?;
            if saved.pin() != target.pin() || saved.client().parts().client.id != target.client() {
                return Err(fail(ClientFailureClass::PinMismatch));
            }
            let authority = self
                .store
                .ordinary_authority(&key)
                .await
                .map_err(pairing_failure)?;
            let signer = self
                .credentials
                .signing_key(saved.credential(), &authority.lease())
                .await
                .map_err(pairing_failure)?;
            Ok(SigningAccess(signer))
        })
    }
}
fn fail(class: ClientFailureClass) -> ClientFailure {
    ClientFailure::new(class)
}
fn pairing_failure(failure: PairingFailure) -> ClientFailure {
    fail(match failure {
        PairingFailure::CredentialUnavailable => ClientFailureClass::StorageUnavailable,
        PairingFailure::PairingRevoked => ClientFailureClass::AuthorizationDenied,
        PairingFailure::AuthorizationDenied => ClientFailureClass::AuthorizationDenied,
        _ => ClientFailureClass::InvalidStorage,
    })
}

#[cfg(test)]
mod enrollment_tests;
