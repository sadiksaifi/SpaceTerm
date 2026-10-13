//! Recovery handoff from native client composition to the one application runtime.
//! Native providers supply authenticated ports and the already recovered journal. This
//! module neither enrolls devices nor installs, starts or activates server code.

use std::{rc::Rc, sync::Arc};

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
    pub(crate) services: ClientServices,
    pub(crate) journal: Arc<JournalController>,
    pub(crate) channels: Arc<PaneChannels>,
    pub(crate) operations: Arc<dyn OperationPeer>,
    pub(crate) invalidator: Arc<dyn LocalUseInvalidator>,
    pub(crate) local_server: Option<ServerId>,
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
        if let Some(installed) =
            cx.try_global::<crate::ui::server_client_runtime::InstalledDesktopClient>()
        {
            return Ok(installed.0.clone());
        }
        let executor = self.executor.clone();
        let handles: Vec<_> = self
            .registry
            .servers()
            .into_iter()
            .filter_map(|server| self.registry.server(server.server))
            .collect();
        let runtime = composed::install(*self, cx)?;
        // The registry owns network workers. Connect only requests their first attempt after
        // installation has bound the journal gate and recovered the personal attachment owner.
        for handle in handles {
            executor.spawn(async move {
                let _ = handle.connect().await;
            });
        }
        Ok(runtime)
    }
}

/// Application-scoped native recovery. The host constructs this once, never per window or Pane.
pub(crate) trait DesktopClientStartup {
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
    owners: std::cell::RefCell<Option<(NativeDesktopOwners, S)>>,
    executor: Arc<tokio::runtime::Runtime>,
}

impl<S: ViewStore + 'static> NativeDesktopStartup<S> {
    #[allow(
        dead_code,
        reason = "native client provider supplies authenticated connection ports"
    )]
    pub(crate) fn new(
        owners: NativeDesktopOwners,
        store: S,
        executor: Arc<tokio::runtime::Runtime>,
    ) -> Self {
        Self {
            owners: std::cell::RefCell::new(Some((owners, store))),
            executor,
        }
    }
}

impl<S: ViewStore + 'static> DesktopClientStartup for NativeDesktopStartup<S> {
    fn recover(
        &self,
        cx: &mut App,
    ) -> Task<Result<Box<dyn RecoveredDesktopClient>, DesktopClientError>> {
        let Some((owners, store)) = self.owners.borrow_mut().take() else {
            return Task::ready(Err(DesktopClientError::Cancelled));
        };
        let executor = self.executor.clone();
        let recovery = executor.spawn(async move {
            recover(owners, store)
                .await
                .map(|composition| Box::new(composition) as Box<dyn RecoveredDesktopClient>)
        });
        cx.spawn(async move |_| {
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
        mut services,
        journal,
        channels,
        operations,
        invalidator,
        local_server,
    } = owners;
    journal.ensure_healthy().map_err(DesktopClientError::from)?;
    let view_journal = journal.clone();
    let (persistence, view) = tokio::task::spawn_blocking(move || {
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
    let (registry, runtime) = ClientRegistry::recover(services)
        .await
        .map_err(DesktopClientError::from)?;
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
    Ok(DesktopClientComposition {
        registry,
        runtime,
        channels,
        tracker: OperationTracker::new(journal.clone()),
        operations,
        pending: PendingTasks::new(journal, invalidator),
        persistence,
        view,
        executor: tokio::runtime::Handle::current(),
        servers,
        settings: crate::ui::settings_window::server_runtime::SettingsServices::default(),
    })
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
    if cx.try_global::<DesktopStartupState>().is_some() {
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
