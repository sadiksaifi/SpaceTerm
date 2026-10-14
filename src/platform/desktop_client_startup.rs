//! Recovery handoff from native client composition to the one application runtime.
//! Native providers supply authenticated ports and the already recovered journal. This
//! module offers separate foreground setup before recovery. Ordinary recovery never
//! installs, starts or enrolls, and the desktop runtime never owns service lifetime.

use std::{rc::Rc, sync::Arc};

mod native;
#[cfg(test)]
pub(crate) use native::pending_ipc_startup;
pub(crate) use native::production;
#[cfg(test)]
pub(crate) use native::protocol::{
    Decoder as AcceptanceDecoder, NativeProtocol as AcceptanceProtocol,
};

use gpui::{App, Entity, Task};
use spaceterm_client::{
    journal::JournalController,
    operations::{OperationPeer, OperationTracker},
    pane_channel::PaneChannels,
    pending::{LocalUseInvalidator, PendingTasks},
    ports::ClientServices,
    registry::ClientRegistry,
    view::{ViewPersistence, ViewStore},
};
use spaceterm_core::identity::ServerId;

pub(crate) const NATIVE_PROTOCOL_CAPABILITIES: [u32; 10] = [
    spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
    spaceterm_protocol::negotiation::CAPABILITY_SERVER_DIRECTORIES,
    spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
    spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
    spaceterm_protocol::negotiation::CAPABILITY_NATIVE_NOTIFICATIONS,
    spaceterm_protocol::negotiation::CAPABILITY_APPLICATION_MOUSE,
    spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA,
    spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA_FRAGMENTS,
    spaceterm_protocol::negotiation::CAPABILITY_NATIVE_PASTE_CORRELATION,
    spaceterm_protocol::negotiation::CAPABILITY_ENHANCED_COPY,
];

use crate::ui::{
    server_client_runtime::{
        DesktopClientRuntime,
        composed::{self, DesktopClientComposition},
    },
    server_desktop_contracts::{DesktopClientError, DesktopServerInfo, DisplayName},
};

/// The native-client lane supplies these ports from its single credential and connection owner.
/// Recovery replaces the connection journal with this exact controller before registration.
#[allow(
    dead_code,
    reason = "native client provider supplies the published ports at startup"
)]
pub(crate) struct NativeDesktopOwners {
    #[cfg(all(feature = "desktop-test-support", debug_assertions))]
    pub(crate) fixture: bool,
    pub(crate) services: ClientServices,
    pub(crate) journal: Arc<JournalController>,
    pub(crate) channels: Arc<PaneChannels>,
    pub(crate) operations: Arc<dyn OperationPeer>,
    pub(crate) invalidator: Arc<dyn LocalUseInvalidator>,
    pub(crate) local_server: Option<ServerId>,
    pub(crate) tracker: Option<OperationTracker>,
    pub(crate) registry_binding: Option<Arc<dyn NativeRegistryBinding>>,
    pub(crate) settings: crate::ui::settings_window::server_runtime::SettingsServices,
}

impl Clone for NativeDesktopOwners {
    fn clone(&self) -> Self {
        Self {
            #[cfg(all(feature = "desktop-test-support", debug_assertions))]
            fixture: self.fixture,
            services: ClientServices {
                routes: self.services.routes.clone(),
                credentials: self.services.credentials.clone(),
                journal: self.services.journal.clone(),
                organization: self.services.organization.clone(),
                delegate: self.services.delegate.clone(),
                discovery: self.services.discovery.clone(),
                limits: self.services.limits,
                additional_capabilities: self.services.additional_capabilities.clone(),
            },
            journal: self.journal.clone(),
            channels: self.channels.clone(),
            operations: self.operations.clone(),
            invalidator: self.invalidator.clone(),
            local_server: self.local_server,
            tracker: self.tracker.clone(),
            registry_binding: self.registry_binding.clone(),
            settings: self.settings.clone(),
        }
    }
}

pub(crate) trait NativeRegistryBinding: Send + Sync {
    fn retained_organization(
        &self,
    ) -> spaceterm_client::ports::ClientFuture<
        '_,
        Vec<spaceterm_core::organization::OrganizationSnapshot>,
    > {
        Box::pin(async { Ok(vec![]) })
    }
    fn bind(
        &self,
        registry: ClientRegistry,
    ) -> Result<(), spaceterm_client::failure::ClientFailure>;
}
trait NativeOwnerFactory: Send + Sync {
    fn bind_window(&self, _: &gpui::Window, _: &mut App) {}
    fn foreground_setup(
        &self,
    ) -> spaceterm_client::ports::ClientFuture<
        '_,
        Option<Arc<dyn crate::platform::local_server_client::foreground::ForegroundSetup>>,
    > {
        Box::pin(async { Ok(None) })
    }
    fn recover(&self) -> spaceterm_client::ports::ClientFuture<'_, NativeDesktopOwners>;
}

/// A recovered value crosses from the native executor to GPUI exactly once.
/// Installation cannot perform filesystem, credential, enrollment or native service IO.
pub(crate) trait RecoveredDesktopClient: Send {
    fn install(
        self: Box<Self>,
        cx: &mut App,
    ) -> Result<Entity<DesktopClientRuntime>, DesktopClientError>;
}

impl<S: ViewStore + 'static> RecoveredDesktopClient for DesktopClientComposition<S> {
    fn install(
        self: Box<Self>,
        cx: &mut App,
    ) -> Result<Entity<DesktopClientRuntime>, DesktopClientError> {
        install_recovered(*self, None, cx)
    }
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "application host installs recovered native client owners"
    )
)]
fn install_recovered<S: ViewStore + 'static>(
    composition: DesktopClientComposition<S>,
    enrollment: Option<
        crate::ui::settings_window::server_enrollment::native::NativeEnrollmentServices,
    >,
    cx: &mut App,
) -> Result<Entity<DesktopClientRuntime>, DesktopClientError> {
    if let Some(installed) =
        cx.try_global::<crate::ui::server_client_runtime::InstalledDesktopClient>()
    {
        return Ok(installed.0.clone());
    }
    let executor = composition.executor.clone();
    let handles: Vec<_> = composition
        .registry
        .servers()
        .into_iter()
        .filter_map(|server| composition.registry.server(server.server))
        .collect();
    let runtime = composed::install_with_enrollment(composition, enrollment, cx)?;
    // The registry owns network workers. Connect only requests their first attempt after
    // installation has bound the journal gate and recovered the personal attachment owner.
    for handle in handles {
        executor.spawn(async move {
            let _ = handle.connect().await;
        });
    }
    Ok(runtime)
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "application host supplies foreground enrollment capabilities"
    )
)]
struct RecoveredWithEnrollment<S: ViewStore> {
    composition: DesktopClientComposition<S>,
    enrollment:
        Option<crate::ui::settings_window::server_enrollment::native::NativeEnrollmentServices>,
}
impl<S: ViewStore + 'static> RecoveredDesktopClient for RecoveredWithEnrollment<S> {
    fn install(
        self: Box<Self>,
        cx: &mut App,
    ) -> Result<Entity<DesktopClientRuntime>, DesktopClientError> {
        install_recovered(self.composition, self.enrollment, cx)
    }
}

/// Application-scoped native recovery. The host constructs this once, never per window or Pane.
pub(crate) trait DesktopClientStartup {
    fn bind_window(&self, _: &gpui::Window, _: &mut App) {}
    fn recover(
        &self,
        cx: &mut App,
    ) -> Task<Result<Box<dyn RecoveredDesktopClient>, DesktopClientError>>;
}

/// Production factory over the native provider's published authenticated ports. The host
/// retains this factory and its existing native executor for the entire application lifetime.
#[allow(
    dead_code,
    reason = "native client provider supplies authenticated connection ports"
)]
pub(crate) struct NativeDesktopStartup<S: ViewStore> {
    owners: Arc<tokio::sync::Mutex<Option<NativeDesktopOwners>>>,
    factory: Option<Arc<dyn NativeOwnerFactory>>,
    store: SharedViewStore<S>,
    consumed: Arc<std::sync::atomic::AtomicBool>,
    executor: Arc<tokio::runtime::Runtime>,
    // Shared with the recovery task so a failed attempt keeps the services for retry.
    enrollment: Arc<
        std::sync::Mutex<
            Option<crate::ui::settings_window::server_enrollment::native::NativeEnrollmentServices>,
        >,
    >,
}

struct SharedViewStore<S: ViewStore>(Arc<std::sync::Mutex<S>>);
impl<S: ViewStore> Clone for SharedViewStore<S> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<S: ViewStore> ViewStore for SharedViewStore<S> {
    type Version = S::Version;
    fn load(
        &mut self,
        maximum: usize,
    ) -> Result<
        Option<spaceterm_client::view::StoredView<S::Version>>,
        spaceterm_client::view::ViewStoreFailure,
    > {
        self.0
            .lock()
            .map_err(|_| storage_failure(spaceterm_client::view::ViewStoreStage::Load))?
            .load(maximum)
    }
    fn replace(
        &mut self,
        expected: Option<&S::Version>,
        document: &spaceterm_client::view::EncodedView,
    ) -> spaceterm_client::view::ViewCommit<S::Version> {
        match self.0.lock() {
            Ok(mut store) => store.replace(expected, document),
            Err(_) => spaceterm_client::view::ViewCommit::NotApplied(storage_failure(
                spaceterm_client::view::ViewStoreStage::Write,
            )),
        }
    }
}
fn storage_failure(
    stage: spaceterm_client::view::ViewStoreStage,
) -> spaceterm_client::view::ViewStoreFailure {
    spaceterm_client::view::ViewStoreFailure {
        class: spaceterm_client::view::ViewStoreFailureClass::Unavailable,
        stage,
    }
}

impl<S: ViewStore + 'static> NativeDesktopStartup<S> {
    #[cfg(test)]
    pub(crate) fn new(
        owners: NativeDesktopOwners,
        store: S,
        executor: Arc<tokio::runtime::Runtime>,
    ) -> Self {
        Self {
            owners: Arc::new(tokio::sync::Mutex::new(Some(owners))),
            factory: None,
            store: SharedViewStore(Arc::new(std::sync::Mutex::new(store))),
            consumed: Arc::default(),
            executor,
            enrollment: Arc::default(),
        }
    }
    /// Foreground native capabilities are retained with this one startup, not window state.
    #[allow(
        dead_code,
        reason = "application host supplies foreground enrollment capabilities"
    )]
    pub(crate) fn with_enrollment(
        self,
        services: crate::ui::settings_window::server_enrollment::native::NativeEnrollmentServices,
    ) -> Self {
        *self
            .enrollment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(services);
        self
    }
}

impl<S: ViewStore + 'static> DesktopClientStartup for NativeDesktopStartup<S> {
    fn bind_window(&self, window: &gpui::Window, cx: &mut App) {
        if let Some(factory) = &self.factory {
            factory.bind_window(window, cx);
        }
    }
    fn recover(
        &self,
        cx: &mut App,
    ) -> Task<Result<Box<dyn RecoveredDesktopClient>, DesktopClientError>> {
        use std::sync::atomic::Ordering;
        if self
            .consumed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Task::ready(Err(DesktopClientError::Cancelled));
        }
        let owners = self.owners.clone();
        let store = self.store.clone();
        let consumed = self.consumed.clone();
        let executor = self.executor.clone();
        let factory = self.factory.clone();
        let enrollment = self.enrollment.clone();
        cx.spawn(async move |cx| {
            let setup_factory = factory.clone();
            let setup = executor
                .spawn(async move {
                    match setup_factory {
                        Some(factory) => factory.foreground_setup().await,
                        None => Ok(None),
                    }
                })
                .await;
            let result = match setup {
                Ok(Ok(Some(setup))) => {
                    if cx.update(|cx| {
                        cx.try_global::<DesktopStartupState>()
                            .is_some_and(|state| state.cancelled)
                    }) {
                        return Err(DesktopClientError::Cancelled);
                    }
                    let consent = cx.update(|cx| {
                        crate::platform::local_server_client::foreground::confirm(setup.kind(), cx)
                    });
                    if consent.await {
                        if cx.update(|cx| {
                            cx.try_global::<DesktopStartupState>()
                                .is_some_and(|state| state.cancelled)
                        }) {
                            return Err(DesktopClientError::Cancelled);
                        }
                        let result = executor
                            .spawn(async move { setup.execute().await })
                            .await
                            .unwrap_or_else(|_| {
                                Err(spaceterm_client::failure::ClientFailure::new(
                                    spaceterm_client::failure::ClientFailureClass::RecoveryRequired,
                                ))
                            });
                        if let Err(failure) = result {
                            if cx.update(|cx| {
                                cx.try_global::<DesktopStartupState>()
                                    .is_some_and(|state| state.cancelled)
                            }) {
                                return Err(DesktopClientError::Cancelled);
                            }
                            cx.update(|cx| {
                                crate::platform::local_server_client::foreground::notify_failure(
                                    failure, cx,
                                )
                            })
                            .await;
                        }
                        Ok(())
                    } else {
                        Ok(())
                    }
                }
                Ok(Ok(None)) => Ok(()),
                Ok(Err(error)) => Err(DesktopClientError::from(error)),
                Err(_) => Err(DesktopClientError::LocalStorageUnavailable),
            };
            if let Err(error) = result {
                consumed.store(false, Ordering::Release);
                return Err(error);
            }
            let recovery = executor.spawn(async move {
                let result = async {
                    let mut retained = owners.lock().await;
                    if retained.is_none() {
                        let factory = factory.ok_or(DesktopClientError::LocalStorageUnavailable)?;
                        *retained =
                            Some(factory.recover().await.map_err(DesktopClientError::from)?);
                    }
                    let original = retained
                        .as_ref()
                        .ok_or(DesktopClientError::LocalStorageUnavailable)?
                        .clone();
                    drop(retained);
                    recover(original, store).await.map(|composition| {
                        let enrollment = enrollment
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take();
                        Box::new(RecoveredWithEnrollment {
                            composition,
                            enrollment,
                        }) as Box<dyn RecoveredDesktopClient>
                    })
                }
                .await;
                // Retry retains the original recovered owners and store. A failed load never
                // grants enrollment, fresh credentials or replacement state.
                if result.is_err() {
                    consumed.store(false, Ordering::Release);
                }
                result
            });
            let _executor = executor;
            recovery
                .await
                .map_err(|_| DesktopClientError::LocalStorageUnavailable)?
        })
    }
}

/// Recovers personal presentation before creating registry actors. Suppression comes from the
/// canonical journal even when there is no personal-view document. Registry recovery does not
/// connect; the installer connects only after the attachment owner and journal gate are ready.
#[allow(
    dead_code,
    reason = "native client provider calls this recovery handoff"
)]
pub(crate) async fn recover<S: ViewStore + 'static>(
    owners: NativeDesktopOwners,
    store: S,
) -> Result<DesktopClientComposition<S>, DesktopClientError> {
    let NativeDesktopOwners {
        #[cfg(all(feature = "desktop-test-support", debug_assertions))]
        fixture,
        mut services,
        journal,
        channels,
        operations,
        invalidator,
        local_server,
        tracker,
        registry_binding,
        mut settings,
    } = owners;
    journal.ensure_healthy().map_err(DesktopClientError::from)?;
    let view_journal = journal.clone();
    let (persistence, _view) = tokio::task::spawn_blocking(move || {
        let mut persistence = ViewPersistence::new(store);
        // The portable launch hook rereads suppression after storage IO and preserves every
        // malformed, locked or unavailable document instead of replacing it with defaults.
        let (view, _) =
            spaceterm_client::view::launch::restore(&mut persistence, &view_journal, &[])
                .map_err(|_| DesktopClientError::LocalStorageUnavailable)?;
        Ok::<_, DesktopClientError>((persistence, view))
    })
    .await
    .map_err(|_| DesktopClientError::LocalStorageUnavailable)??;
    // No connection-specific or pairing-specific second journal may admit a suppressed binding.
    services.journal = journal.clone();
    let snapshots = match &registry_binding {
        Some(binding) => binding
            .retained_organization()
            .await
            .map_err(DesktopClientError::from)?,
        None => vec![],
    };
    let (registry, runtime) = ClientRegistry::recover_with_retained(services, snapshots)
        .await
        .map_err(DesktopClientError::from)?;
    let recovered_registry = registry.clone();
    let recovered_journal = journal.clone();
    let (persistence, view) = tokio::task::spawn_blocking(move || {
        let mut persistence = persistence;
        let (view, _) = recovered_registry
            .restore_view(&mut persistence, &recovered_journal)
            .map_err(|_| DesktopClientError::LocalStorageUnavailable)?;
        Ok::<_, DesktopClientError>((persistence, view))
    })
    .await
    .map_err(|_| DesktopClientError::LocalStorageUnavailable)??;
    if let Some(binding) = registry_binding {
        binding
            .bind(registry.clone())
            .map_err(DesktopClientError::from)?;
    }
    let servers = registry
        .servers()
        .into_iter()
        .map(|summary| DesktopServerInfo {
            server: summary.server,
            name: if local_server == Some(summary.server) {
                DisplayName::reported("This Computer")
            } else {
                DisplayName::unnamed()
            },
            local: local_server == Some(summary.server),
        })
        .collect();
    // Retain the factory's live native capabilities, including foreground authorization.
    // Registry recovery adds authenticated inventory without discovering another host provider.
    let recovered_settings = crate::ui::settings_window::server_runtime::SettingsServices::recover(
        &registry, &journal, None,
    )
    .map_err(DesktopClientError::from)?;
    settings.inventories.extend(recovered_settings.inventories);
    let composition = DesktopClientComposition {
        registry,
        runtime,
        channels,
        tracker: tracker.unwrap_or_else(|| OperationTracker::new(journal.clone())),
        operations,
        pending: PendingTasks::new(journal, invalidator),
        persistence,
        view,
        executor: tokio::runtime::Handle::current(),
        servers,
        settings,
    };
    #[cfg(all(feature = "desktop-test-support", debug_assertions))]
    let composition = if fixture {
        native::test_composition::initialize_workspace(composition).await?
    } else {
        composition
    };
    Ok(composition)
}

/// Retained startup task and single admission gate. Reopen waits for the original task instead
/// of recovering another journal or creating another native client identity.
pub(crate) struct DesktopStartupState {
    pub(crate) pending: bool,
    cancelled: bool,
    pub(crate) failure: Option<DesktopClientError>,
    pub(crate) task: Option<Task<()>>,
}
impl gpui::Global for DesktopStartupState {}

pub(crate) fn begin(
    provider: Rc<dyn DesktopClientStartup>,
    cx: &mut App,
    completed: impl FnOnce(Result<(), DesktopClientError>, &mut App) + 'static,
) {
    use crate::ui::server_client_runtime::InstalledDesktopClient;
    if cx.try_global::<InstalledDesktopClient>().is_some() {
        completed(Ok(()), cx);
        return;
    }
    if cx.try_global::<DesktopStartupState>().is_some_and(|state| {
        state.pending
            || state.cancelled
            || state.failure != Some(DesktopClientError::LocalStorageUnavailable)
    }) {
        return;
    }
    let recovered = provider.recover(cx);
    cx.set_global(DesktopStartupState {
        pending: true,
        cancelled: false,
        failure: None,
        task: None,
    });
    let task = cx.spawn(async move |cx| {
        let recovered = recovered.await;
        cx.update(|cx| {
            if cx.global::<DesktopStartupState>().cancelled {
                cx.global_mut::<DesktopStartupState>().pending = false;
                // Dropping recovered owners invalidates their connections without detach,
                // control release or server retirement. The quit path owns its completion.
                return;
            }
            let result = recovered.and_then(|client| client.install(cx).map(|_| ()));
            let state = cx.global_mut::<DesktopStartupState>();
            state.pending = false;
            state.failure = result.as_ref().err().copied();
            completed(result, cx);
        });
    });
    cx.global_mut::<DesktopStartupState>().task = Some(task);
}

/// An accepted quit prevents late recovery from installing a client or opening a window.
/// The original recovery task keeps ownership until completion; cancellation creates no retry.
pub(crate) fn cancel_for_quit(cx: &mut App) {
    if cx.try_global::<DesktopStartupState>().is_some() {
        let state = cx.global_mut::<DesktopStartupState>();
        state.cancelled = true;
        state.failure = Some(DesktopClientError::Cancelled);
    }
}

#[cfg(test)]
mod tests;
