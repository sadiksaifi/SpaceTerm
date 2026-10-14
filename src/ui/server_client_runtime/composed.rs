//! Desktop composition of the portable client owners. Native transports and credentials stay
//! in the client registry; this adapter never manufactures a connection or a control grant.

use super::{
    DesktopClientNotifier, DesktopClientProvider, OperationOutcomeNotice, PanePresentationDelivery,
};
use crate::ui::{
    pane_presentation::{BlockedLocation, DisplayName, IntendedAction, OfflineCause},
    server_desktop_contracts::*,
};
use gpui::{App, AppContext as _, Task};
use spaceterm_client::{
    api::{ConnectionState, PairedServerBinding},
    operations::{OperationPeer, OperationStatus, OperationTicket, OperationTracker},
    pane_channel::{PaneChannel, PaneChannels, PaneReadiness, WorkspaceLease},
    pending::{PendingRuntime, PendingTasks},
    registry::{ClientRegistry, ClientRuntime, ServerHandle},
    replica::{ReplicaSnapshot, ReplicaState},
    view::{
        self, ClientViewState, PresentationFacts, PresentationState, ReplicaFacts,
        ReplicaObservation, ReplicaReadiness, ServerViewStatus, StartupEffect, StartupIntents,
        UseFacts, ViewPersistence, ViewStore,
    },
};
use spaceterm_core::{authority::SessionKey, identity::*, organization::CreationTopologyReadModel};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, VecDeque},
    rc::Rc,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

mod native_ports;
mod server_ports;
use server_ports::{GitPublication, RepairAdmission};

/// Owners recovered on the client's Tokio executor before installing the desktop runtime.
/// The tracker, pending tasks and Pane channels must use the registry's single journal.
pub(crate) struct DesktopClientComposition<S: ViewStore> {
    pub(crate) registry: ClientRegistry,
    pub(crate) runtime: ClientRuntime,
    pub(crate) channels: Arc<PaneChannels>,
    pub(crate) tracker: OperationTracker,
    pub(crate) operations: Arc<dyn OperationPeer>,
    pub(crate) pending: PendingTasks,
    pub(crate) persistence: ViewPersistence<S>,
    pub(crate) view: ClientViewState,
    pub(crate) executor: tokio::runtime::Handle,
    pub(crate) servers: Vec<DesktopServerInfo>,
    pub(crate) settings: crate::ui::settings_window::server_runtime::SettingsServices,
}

pub(crate) struct ComposedDesktopClientProvider<S: ViewStore> {
    inner: Rc<Inner>,
    persistence: Arc<Mutex<ViewPersistence<S>>>,
}

/// Installs the one application-scoped client after pairing and view recovery. Reopening a
/// Workspace window reuses it; installation never starts a service or enrolls a credential.
#[allow(
    dead_code,
    reason = "native enrollment supplies these recovered owners at startup"
)]
pub(crate) fn install<S: ViewStore + 'static>(
    composition: DesktopClientComposition<S>,
    cx: &mut App,
) -> Result<gpui::Entity<super::DesktopClientRuntime>, DesktopClientError> {
    install_with_enrollment(composition, None, cx)
}

pub(crate) fn install_with_enrollment<S: ViewStore + 'static>(
    mut composition: DesktopClientComposition<S>,
    enrollment: Option<
        crate::ui::settings_window::server_enrollment::native::NativeEnrollmentServices,
    >,
    cx: &mut App,
) -> Result<gpui::Entity<super::DesktopClientRuntime>, DesktopClientError> {
    if let Some(installed) = cx.try_global::<super::InstalledDesktopClient>() {
        return Ok(installed.0.clone());
    }
    if enrollment.as_ref().is_some_and(|services| {
        !Arc::ptr_eq(services.pairing.journal(), composition.tracker.journal())
    }) {
        return Err(DesktopClientError::LocalStorageUnavailable);
    }
    let enrollment_registry = composition.registry.clone();
    let saved = composition
        .tracker
        .journal()
        .snapshot()
        .map_err(DesktopClientError::from)?;
    composition.servers.retain(|name| {
        saved
            .bindings()
            .iter()
            .filter_map(|saved| saved.binding().ok())
            .any(|binding| {
                binding.target().server() == name.server
                    && composition
                        .tracker
                        .journal()
                        .ordinary_use_allowed(&binding)
                        .unwrap_or(false)
            })
    });
    let connections = composition
        .servers
        .iter()
        .map(|name| {
            let binding = saved
                .bindings()
                .iter()
                .filter_map(|saved| saved.binding().ok())
                .find(|binding| binding.target().server() == name.server)
                .ok_or(DesktopClientError::AccessDenied)?;
            let handle = composition
                .registry
                .server(name.server)
                .ok_or(DesktopClientError::TargetGone)?;
            let mut source =
                crate::ui::settings_window::server_runtime::SettingsConnection::from_handle(
                    binding,
                    name.clone(),
                    &handle,
                    &composition.tracker,
                )
                .map_err(DesktopClientError::from)?;
            composition
                .settings
                .bind(&mut source)
                .map_err(DesktopClientError::from)?;
            Ok(source)
        })
        .collect::<Result<Vec<_>, DesktopClientError>>()?;
    let settings = crate::ui::settings_window::server_runtime::SettingsComposition {
        connections,
        tracker: composition.tracker.clone(),
        operations: composition.operations.clone(),
        pending: composition.pending.clone(),
        executor: composition.executor.clone(),
    };
    let (notifier, notifications) = super::notification_channel();
    let provider = Rc::new(ComposedDesktopClientProvider::new(
        composition,
        notifier,
        cx,
    )?);
    if let Some(services) = &enrollment {
        // The scheduler belongs to the recovered journal, not to an ordinary connection.
        // Enter its executor only to spawn the existing worker; installation does no native IO.
        let pending = {
            let _entered = provider.inner.executor.enter();
            services
                .pairing
                .start_scheduler()
                .map_err(DesktopClientError::from)?
        };
        provider
            .inner
            .runtime
            .borrow_mut()
            .as_mut()
            .ok_or(DesktopClientError::Cancelled)?
            .pending = Some(pending);
    }
    let runtime = cx.new(|cx| {
        super::DesktopClientRuntime::new(
            super::DesktopClientDependencies {
                provider: provider.clone(),
                notifications,
            },
            cx,
        )
    });
    cx.set_global(super::InstalledDesktopClient(runtime.clone()));
    crate::ui::settings_window::server_runtime::install(settings, cx);
    if let Some(services) = enrollment {
        crate::ui::settings_window::server_enrollment::install(
            Rc::new(crate::ui::settings_window::server_enrollment::native::NativeEnrollmentProvider::new(
                services, enrollment_registry, provider,
            )), cx,
        );
    }
    Ok(runtime)
}

#[derive(Clone)]
struct Source {
    binding: PairedServerBinding,
    name: DesktopServerInfo,
    state: watch::Receiver<ConnectionState>,
    replica: watch::Receiver<ReplicaState>,
    handle: Option<ServerHandle>,
}

#[derive(Default)]
struct State {
    view: ClientViewState,
    frames: BTreeMap<ServerId, Arc<DesktopServerFrame>>,
    revision: DesktopViewRevision,
    leases: HashMap<WorkspaceId, WorkspaceLease>,
    channels: HashMap<PaneId, PaneChannel>,
    startup: HashMap<ServerId, StartupIntents>,
    executors: HashMap<ServerId, Arc<tokio::sync::Mutex<view::launch::StartupExecutor>>>,
    startup_channels: HashMap<(ServerId, view::UseTicket), PaneChannel>,
    measurements: HashMap<PaneId, PaneMeasurementRequest>,
    presentations: HashMap<PaneId, PresentationState>,
    revalidated: HashMap<PaneId, (PaneViewToken, view::ViewRevision)>,
    outcomes: HashMap<OperationId, (Arc<Outcome>, OrganizationRevision)>,
    watching: std::collections::HashSet<OperationId>,
    repair_admissions: std::collections::HashSet<PaneId>,
    detach_requests: HashMap<UiRequestId, view::WorkspaceAttachment>,
    blocked: HashMap<ServerId, (BlockedLocation, IntendedAction)>,
}

/// Both client workers survive an empty server list and drain before application quit.
struct DesktopClientWorkers {
    client: ClientRuntime,
    pending: Option<PendingRuntime>,
}
impl DesktopClientWorkers {
    async fn shutdown(self) -> Result<(), spaceterm_client::failure::ClientFailure> {
        let Self { client, pending } = self;
        let pending = async move {
            if let Some(pending) = pending {
                pending.shutdown().await
            } else {
                Ok(())
            }
        };
        // A failure in one worker must not skip joining the other's owned native effects.
        let (pending, client) = tokio::join!(pending, client.shutdown());
        pending.and(client)
    }
}

struct Inner {
    git: gpui::Entity<crate::ui::server_git::ServerGitStore>,
    git_publications: RefCell<BTreeMap<ServerId, GitPublication>>,
    sources: RefCell<BTreeMap<ServerId, Source>>,
    order: RefCell<Vec<ServerId>>,
    channels: Arc<PaneChannels>,
    tracker: OperationTracker,
    operations: Arc<dyn OperationPeer>,
    pending: PendingTasks,
    runtime: RefCell<Option<DesktopClientWorkers>>,
    executor: tokio::runtime::Handle,
    notifier: DesktopClientNotifier,
    state: RefCell<State>,
    tasks: RefCell<Vec<Task<()>>>,
    edits: Arc<tokio::sync::Mutex<()>>,
}

impl<S: ViewStore + 'static> ComposedDesktopClientProvider<S> {
    pub(crate) fn new(
        composition: DesktopClientComposition<S>,
        notifier: DesktopClientNotifier,
        cx: &mut App,
    ) -> Result<Self, DesktopClientError> {
        let DesktopClientComposition {
            registry,
            runtime,
            channels,
            tracker,
            operations,
            pending,
            persistence,
            mut view,
            executor,
            servers,
            settings: _,
        } = composition;
        if !Arc::ptr_eq(tracker.journal(), pending.journal()) {
            return Err(DesktopClientError::LocalStorageUnavailable);
        }
        tracker
            .journal()
            .bind_pane_channels(channels.as_ref().clone())
            .map_err(DesktopClientError::from)?;
        let saved = tracker
            .journal()
            .snapshot()
            .map_err(DesktopClientError::from)?;
        // A detach can commit while native recovery is awaiting the GUI executor. Apply the
        // latest markers before any attachment, lease or save becomes observable, even when
        // no server has published a retained replica yet.
        view.filter_suppressed(&saved).map_err(view_error)?;
        let mut sources = BTreeMap::new();
        let mut order = Vec::new();
        for name in servers {
            let handle = registry
                .server(name.server)
                .ok_or(DesktopClientError::TargetGone)?;
            handle
                .queries(tracker.journal().clone())
                .map_err(DesktopClientError::from)?;
            let binding = saved
                .bindings()
                .iter()
                .filter_map(|saved| saved.binding().ok())
                .find(|binding| binding.target().server() == name.server)
                .ok_or(DesktopClientError::AccessDenied)?;
            if !tracker
                .journal()
                .ordinary_use_allowed(&binding)
                .map_err(DesktopClientError::from)?
            {
                continue;
            }
            order.push(name.server);
            if sources
                .insert(
                    name.server,
                    Source {
                        binding,
                        name,
                        state: handle.state(),
                        replica: handle.replica(),
                        handle: Some(handle),
                    },
                )
                .is_some()
            {
                return Err(DesktopClientError::WrongServer);
            }
        }
        let inner = Rc::new(Inner {
            git: crate::ui::server_git::InstalledServerGit::install(cx),
            git_publications: RefCell::new(BTreeMap::new()),
            sources: RefCell::new(sources),
            order: RefCell::new(order),
            channels,
            tracker,
            operations,
            pending,
            runtime: RefCell::new(Some(DesktopClientWorkers {
                client: runtime,
                pending: None,
            })),
            executor,
            notifier,
            state: RefCell::new(State {
                view,
                ..State::default()
            }),
            tasks: RefCell::new(Vec::new()),
            edits: Arc::new(tokio::sync::Mutex::new(())),
        });
        inner.install(cx)?;
        Ok(Self {
            inner,
            persistence: Arc::new(Mutex::new(persistence)),
        })
    }

    /// Native management reports only a classified block and the original intended action.
    /// Retry retains Connect versus Start; a route or a diagnostic never changes authority.
    pub(crate) fn set_connection_block(
        &self,
        server: ServerId,
        block: Option<(BlockedLocation, IntendedAction)>,
    ) -> Result<(), DesktopClientError> {
        if !self.inner.sources.borrow().contains_key(&server) {
            return Err(DesktopClientError::TargetGone);
        }
        let mut state = self.inner.state.borrow_mut();
        match block {
            Some(block) => {
                state.blocked.insert(server, block);
            }
            None => {
                state.blocked.remove(&server);
            }
        }
        self.inner.notifier.frame_changed(server);
        Ok(())
    }

    fn save(&self, cx: &mut App) -> Task<Result<(), DesktopClientError>> {
        let inner = self.inner.clone();
        let persistence = self.persistence.clone();
        let background = cx.background_executor().clone();
        cx.spawn(async move |_| {
            let _edit = inner.edits.lock().await;
            let persisted = inner.state.borrow().view.persisted().map_err(view_error)?;
            background
                .spawn(async move {
                    persistence
                        .lock()
                        .map_err(|_| DesktopClientError::LocalStorageUnavailable)?
                        .save(&persisted)
                        .map_err(view_error)
                })
                .await
        })
    }
}

impl<S: ViewStore + 'static>
    crate::ui::settings_window::server_enrollment::native::EnrollmentPublication
    for ComposedDesktopClientProvider<S>
{
    fn publish(
        &self,
        enrollment: &crate::ui::settings_window::server_enrollment::DurableEnrollment,
        handle: ServerHandle,
        cx: &mut App,
    ) -> Result<(), crate::ui::settings_window::server_enrollment::EnrollmentFailure> {
        use spaceterm_core::pairing::PairingFailure;
        let journal = self.inner.tracker.journal();
        let snapshot = journal
            .snapshot()
            .map_err(|_| PairingFailure::StorageUnavailable)?;
        let binding = snapshot
            .bindings()
            .iter()
            .find(|binding| binding.proof() == enrollment.saved().receipt())
            .ok_or(PairingFailure::InvalidDurableState)?
            .binding()
            .map_err(|_| PairingFailure::InvalidDurableState)?;
        if !journal
            .ordinary_use_allowed(&binding)
            .map_err(|_| PairingFailure::StorageUnavailable)?
        {
            return Err(PairingFailure::PairingRevoked.into());
        }
        let server = binding.target().server();
        if handle.identity() != server {
            return Err(PairingFailure::IdentityMismatch.into());
        }
        let previous = self.inner.source(&server);
        if let Some(existing) = &previous {
            if existing.binding == binding {
                return Ok(());
            }
            if journal
                .ordinary_use_allowed(&existing.binding)
                .map_err(|_| PairingFailure::StorageUnavailable)?
            {
                return Err(PairingFailure::IdentityMismatch.into());
            }
        }
        let name = DesktopServerInfo {
            server,
            name: DisplayName::unnamed(),
            local: matches!(
                binding.target().route().parts(),
                spaceterm_client::api::RouteParts::Ipc { .. }
            ),
        };
        let mut settings =
            crate::ui::settings_window::server_runtime::SettingsConnection::from_handle(
                binding.clone(),
                name.clone(),
                &handle,
                &self.inner.tracker,
            )
            .map_err(|_| PairingFailure::InvalidDurableState)?;
        if let Some(services) = &enrollment.settings {
            services
                .bind(&mut settings)
                .map_err(|_| PairingFailure::InvalidDurableState)?;
        }
        crate::ui::settings_window::server_runtime::add_connection(settings, cx)
            .map_err(|_| PairingFailure::InvalidDurableState)?;
        self.inner.sources.borrow_mut().insert(
            server,
            Source {
                binding,
                name,
                state: handle.state(),
                replica: handle.replica(),
                handle: Some(handle),
            },
        );
        if previous.is_none() {
            self.inner.order.borrow_mut().push(server);
        }
        {
            let mut state = self.inner.state.borrow_mut();
            state.frames.remove(&server);
            state
                .view
                .filter_suppressed(&snapshot)
                .map_err(|_| PairingFailure::InvalidDurableState)?;
        }
        self.inner
            .follow_server(server, cx)
            .map_err(|_| PairingFailure::InvalidDurableState)?;
        self.inner.notifier.frame_changed(server);
        Ok(())
    }
}

impl Inner {
    fn source(&self, server: &ServerId) -> Option<Source> {
        self.sources.borrow().get(server).cloned()
    }

    fn install(self: &Rc<Self>, cx: &mut App) -> Result<(), DesktopClientError> {
        // Subscribe before the final snapshot so a concurrent suppression commit cannot land
        // between the initial read and the watcher and disappear from both observations.
        let mut revisions = self.tracker.journal().revisions();
        let mut health = self.tracker.journal().health();
        self.state
            .borrow_mut()
            .view
            .filter_suppressed(
                &self
                    .tracker
                    .journal()
                    .snapshot()
                    .map_err(DesktopClientError::from)?,
            )
            .map_err(view_error)?;
        for server in self.order.borrow().iter().copied() {
            self.follow_server(server, cx)?;
        }
        let weak = Rc::downgrade(self);
        self.tasks.borrow_mut().push(cx.spawn(async move |cx| {
            loop {
                let alive = tokio::select! { changed = revisions.changed() => changed.is_ok(), changed = health.changed() => changed.is_ok() };
                if !alive { break; }
                let Some(inner) = weak.upgrade() else { break; };
                let _edit = inner.edits.lock().await;
                cx.update(|cx| {
                    for server in inner.order.borrow().iter() { inner.sync_git(*server, cx); }
                });
                let markers = inner.tracker.journal().snapshot();
                let mut state = inner.state.borrow_mut();
                if let Ok(markers) = markers {
                    let _ = state.view.filter_suppressed(&markers);
                    state.outcomes.retain(|operation, _| markers.operation(*operation).is_some());
                    state.watching.retain(|operation| markers.operation(*operation).is_some());
                    let _ = inner.publish_frames(&mut state);
                } else {
                    state.frames.clear();
                    for server in inner.order.borrow().iter() { inner.notifier.frame_changed(*server); }
                }
            }
        }));
        Ok(())
    }

    fn follow_server(
        self: &Rc<Self>,
        server: ServerId,
        cx: &mut App,
    ) -> Result<(), DesktopClientError> {
        let source = self.source(&server).ok_or(DesktopClientError::TargetGone)?;
        self.refresh(server)?;
        self.sync_git(server, cx);
        let mut connection = source.state.clone();
        let mut replica = source.replica.clone();
        let weak = Rc::downgrade(self);
        self.tasks.borrow_mut().push(cx.spawn(async move |cx| {
                loop {
                    let alive = tokio::select! { result = connection.changed() => result.is_ok(), result = replica.changed() => result.is_ok() };
                    if !alive { break; }
                    let Some(inner) = weak.upgrade() else { break; };
                    let _edit = inner.edits.lock().await;
                    let effects = cx.update(|cx| {
                        inner.sync_git(server, cx);
                        inner.refresh(server)
                    }).unwrap_or_default();
                    drop(_edit);
                    let _ = inner.execute(effects, cx).await;
                    // The authenticated actor continues reading while the presentation owner
                    // drains the canonical tasks through its shared operation peer.
                    let _ = inner.drain_detaches(server).await;
                }
            }));
        Ok(())
    }

    async fn drain_detaches(&self, server: ServerId) -> Result<(), DesktopClientError> {
        let source = self.source(&server).ok_or(DesktopClientError::TargetGone)?;
        if !matches!(*source.state.borrow(), ConnectionState::Connected { .. })
            || !source.replica.borrow().is_ready()
        {
            return Ok(());
        }
        let key = source
            .binding
            .target()
            .local_pairing()
            .map_err(DesktopClientError::from)?;
        let workspaces: Vec<_> = self
            .pending
            .journal()
            .snapshot()
            .map_err(DesktopClientError::from)?
            .detaches()
            .iter()
            .filter(|task| task.local_pairing == key)
            .map(|task| task.workspace)
            .collect();
        if workspaces.is_empty() {
            return Ok(());
        }
        let pending = self.pending.clone();
        let operations = self.operations.clone();
        self.executor
            .spawn(async move {
                for workspace in workspaces {
                    drain_original_detach(
                        &pending,
                        &source.binding,
                        workspace,
                        operations.as_ref(),
                    )
                    .await?;
                }
                Ok::<_, spaceterm_client::failure::ClientFailure>(())
            })
            .await
            .map_err(|_| DesktopClientError::Cancelled)?
            .map_err(DesktopClientError::from)
    }

    fn snapshot(&self, server: ServerId) -> Result<Arc<ReplicaSnapshot>, DesktopClientError> {
        self.source(&server)
            .ok_or(DesktopClientError::TargetGone)?
            .replica
            .borrow()
            .retained()
            .cloned()
            .ok_or(DesktopClientError::Reconciling)
    }

    fn observation<'a>(
        &self,
        server: ServerId,
        snapshot: &'a ReplicaSnapshot,
        synchronizing: bool,
    ) -> Result<ReplicaObservation<'a>, DesktopClientError> {
        let source = self.source(&server).ok_or(DesktopClientError::TargetGone)?;
        let connection = source.state.borrow().clone();
        let (status, binding, ready) = match connection {
            ConnectionState::Connected { info } => (
                ServerViewStatus::Connected,
                Some(info.binding),
                info.server == snapshot.cursor.server
                    && info.epoch == snapshot.cursor.epoch
                    && info.organization_revision == snapshot.cursor.revision
                    && source.replica.borrow().is_ready(),
            ),
            ConnectionState::Connecting { .. } => (ServerViewStatus::Connecting, None, false),
            ConnectionState::Offline { .. } => (ServerViewStatus::Offline, None, false),
            ConnectionState::Incompatible { .. } => (ServerViewStatus::Incompatible, None, false),
            ConnectionState::Stopped => (ServerViewStatus::Stopped, None, false),
        };
        let readiness = if ready && !synchronizing {
            ReplicaReadiness::Ready
        } else if status == ServerViewStatus::Connected {
            ReplicaReadiness::Synchronizing
        } else {
            ReplicaReadiness::Retained
        };
        ReplicaObservation::new(
            &snapshot.organization,
            snapshot.accepted_creations(),
            ReplicaFacts {
                pairing: source.binding.target().pairing(),
                client: source.binding.target().client(),
                connection: binding,
                readiness,
                status,
            },
        )
        .map_err(view_error)
    }

    fn refresh(&self, server: ServerId) -> Result<Vec<StartupEffect>, DesktopClientError> {
        let snapshot = match self.snapshot(server) {
            Ok(snapshot) => snapshot,
            Err(DesktopClientError::Reconciling) => {
                self.notifier.frame_changed(server);
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
        let observation = self.observation(server, &snapshot, false)?;
        let mut state = self.state.borrow_mut();
        state
            .view
            .filter_suppressed(
                &self
                    .tracker
                    .journal()
                    .snapshot()
                    .map_err(DesktopClientError::from)?,
            )
            .map_err(view_error)?;
        state.view.reconcile(&observation).map_err(view_error)?;
        let represented: std::collections::HashSet<_> = state
            .view
            .workspaces()
            .iter()
            .flat_map(|workspace| {
                workspace
                    .tab_order()
                    .iter()
                    .filter_map(|&tab| workspace.arrangement(tab))
                    .flat_map(|arrangement| arrangement.pane_ids())
            })
            .collect();
        state.channels.retain(|pane, _| represented.contains(pane));
        state
            .presentations
            .retain(|pane, _| represented.contains(pane));
        state
            .revalidated
            .retain(|pane, _| represented.contains(pane));
        state
            .measurements
            .retain(|pane, _| represented.contains(pane));
        let attachments: HashMap<_, _> = state
            .view
            .attachments()
            .iter()
            .map(|attachment| (attachment.workspace(), attachment.incarnation()))
            .collect();
        state
            .leases
            .retain(|workspace, lease| attachments.get(workspace) == Some(&lease.incarnation()));
        let context = state
            .startup
            .get(&server)
            .and_then(StartupIntents::pending)
            .map(|pending| pending.context.target().pane)
            .and_then(|pane| state.channels.get(&pane))
            .map(|channel| channel.state().borrow().context)
            .unwrap_or(ContextGeneration::new(1));
        let view = state.view.clone();
        let effects = state
            .startup
            .entry(server)
            .or_default()
            .revalidate(UseFacts {
                view: &view,
                replica: &observation,
                context,
            })
            .effects;
        self.publish_frames(&mut state)?;
        Ok(effects)
    }

    fn publish_frames(&self, state: &mut State) -> Result<(), DesktopClientError> {
        let revision = state
            .revision
            .checked_successor()
            .ok_or(DesktopClientError::CounterExhausted)?;
        let personal = Arc::new(state.view.clone());
        for (&server, source) in self.sources.borrow().iter() {
            if !self
                .tracker
                .journal()
                .ordinary_use_allowed(&source.binding)
                .unwrap_or(false)
            {
                state.frames.remove(&server);
                self.notifier.frame_changed(server);
                continue;
            }
            let Some(snapshot) = source.replica.borrow().retained().cloned() else {
                continue;
            };
            let observation = self.observation(server, &snapshot, false)?;
            let replica_status = match observation.readiness() {
                ReplicaReadiness::Ready => ReplicaStatus::Ready,
                ReplicaReadiness::Synchronizing => ReplicaStatus::Resynchronizing,
                ReplicaReadiness::Retained => ReplicaStatus::RetainedOffline,
            };
            state.frames.insert(
                server,
                Arc::new(DesktopServerFrame {
                    server,
                    connection: source.state.borrow().clone(),
                    replica_status,
                    organization: Arc::new(snapshot.organization.clone()),
                    personal: personal.clone(),
                    view_revision: revision,
                }),
            );
            self.notifier.frame_changed(server);
        }
        state.revision = revision;
        Ok(())
    }

    fn channel(&self, pane: PaneId) -> Result<PaneChannel, DesktopClientError> {
        let source = self
            .source(&pane.server())
            .ok_or(DesktopClientError::TargetGone)?;
        if !self
            .tracker
            .journal()
            .ordinary_use_allowed(&source.binding)
            .map_err(DesktopClientError::from)?
        {
            return Err(DesktopClientError::AccessDenied);
        }
        let mut state = self.state.borrow_mut();
        if let Some(channel) = state.channels.get(&pane) {
            return Ok(channel.clone());
        }
        let workspace = state
            .view
            .workspaces()
            .iter()
            .find(|workspace| {
                workspace.tab_order().iter().any(|&tab| {
                    workspace
                        .arrangement(tab)
                        .is_some_and(|arrangement| arrangement.contains_pane(pane))
                })
            })
            .ok_or(DesktopClientError::TargetGone)?;
        let attachment = workspace.attachment();
        let lease = match state.leases.get(&attachment.workspace()) {
            Some(lease) if lease.incarnation() == attachment.incarnation() => lease.clone(),
            _ => {
                let lease = self
                    .channels
                    .attach(attachment.workspace(), attachment.incarnation())
                    .map_err(DesktopClientError::from)?;
                state.leases.insert(attachment.workspace(), lease.clone());
                lease
            }
        };
        let channel = lease.pane(pane).map_err(DesktopClientError::from)?;
        state.channels.insert(pane, channel.clone());
        Ok(channel)
    }

    fn token(&self, pane: PaneId) -> Result<PaneViewToken, DesktopClientError> {
        let channel = self.channel(pane)?;
        let state = self.state.borrow();
        let attachment = state
            .view
            .workspaces()
            .iter()
            .find(|workspace| {
                workspace.tab_order().iter().any(|&tab| {
                    workspace
                        .arrangement(tab)
                        .is_some_and(|arrangement| arrangement.contains_pane(pane))
                })
            })
            .ok_or(DesktopClientError::TargetGone)?
            .attachment();
        Ok(PaneViewToken {
            pane,
            attachment_generation: attachment.incarnation(),
            context_generation: channel.state().borrow().context,
        })
    }

    fn presentation(
        &self,
        pane: PaneId,
        displayed: Option<SessionKey>,
    ) -> Result<PanePresentationDelivery, DesktopClientError> {
        let channel = self.channel(pane)?;
        // Capture once. TerminalPane rejects the delivery if this exact observation changed.
        let observed = *channel.state().borrow();
        let snapshot = self.snapshot(pane.server())?;
        let snapshot_runtime = snapshot
            .organization
            .runtime()
            .iter()
            .find(|runtime| runtime.pane == pane)
            .copied();
        let current_connection = match &*self
            .source(&pane.server())
            .ok_or(DesktopClientError::TargetGone)?
            .state
            .borrow()
        {
            ConnectionState::Connected { info } => Some(*info),
            _ => None,
        };
        let coherent_channel =
            observed
                .connection
                .zip(current_connection)
                .is_some_and(|(observed, current)| {
                    observed.binding == current.binding
                        && observed.epoch == current.epoch
                        && observed.attempt == current.attempt
                });
        let synchronizing = matches!(
            observed.readiness,
            PaneReadiness::Reconciling | PaneReadiness::Offline
        ) || observed.runtime != snapshot_runtime
            || !coherent_channel;
        let observation = self.observation(pane.server(), &snapshot, synchronizing)?;
        let token = self.token(pane)?;
        let source = self
            .source(&pane.server())
            .ok_or(DesktopClientError::TargetGone)?;
        let mut state = self.state.borrow_mut();
        let workspace = state
            .view
            .workspaces()
            .iter()
            .find(|workspace| {
                workspace.tab_order().iter().any(|&tab| {
                    workspace
                        .arrangement(tab)
                        .is_some_and(|arrangement| arrangement.contains_pane(pane))
                })
            })
            .ok_or(DesktopClientError::TargetGone)?
            .workspace();
        let facts = PresentationFacts::observe(
            &state.view,
            &observation,
            workspace,
            pane,
            state
                .startup
                .get(&pane.server())
                .unwrap_or(&StartupIntents::default()),
            displayed,
        )
        .map_err(view_error)?;
        let decision = state
            .presentations
            .entry(pane)
            .or_default()
            .reduce(&facts)
            .map_err(view_error)?;
        let cause = offline_cause(
            &source.state.borrow(),
            source.name.name.clone(),
            state.blocked.get(&pane.server()).cloned(),
        );
        Ok(PanePresentationDelivery {
            token,
            observation: observed,
            decision,
            device: DisplayName::unnamed(),
            cause,
        })
    }

    fn use_request(
        &self,
        request: &DesktopRequest,
    ) -> Result<Vec<StartupEffect>, DesktopClientError> {
        let (pane, source) = match request {
            DesktopRequest::SelectPane { pane, .. }
            | DesktopRequest::StartSession { pane, .. }
            | DesktopRequest::Retry { pane, .. } => (
                *pane,
                request
                    .explicit_use()
                    .ok_or(DesktopClientError::StaleContext)?,
            ),
            DesktopRequest::OpenTab { tab, .. } => {
                let state = self.state.borrow();
                let pane = state
                    .view
                    .workspaces()
                    .iter()
                    .find_map(|workspace| {
                        workspace
                            .arrangement(*tab)
                            .and_then(|arrangement| arrangement.focused_pane())
                    })
                    .ok_or(DesktopClientError::TargetGone)?;
                (
                    pane,
                    request
                        .explicit_use()
                        .ok_or(DesktopClientError::StaleContext)?,
                )
            }
            _ => return Err(DesktopClientError::UnsupportedCapability),
        };
        let channel = self.channel(pane)?;
        let snapshot = self.snapshot(pane.server())?;
        let observation = self.observation(pane.server(), &snapshot, false)?;
        if observation.readiness() != ReplicaReadiness::Ready
            && matches!(
                request,
                DesktopRequest::StartSession { .. } | DesktopRequest::Retry { .. }
            )
        {
            return Err(DesktopClientError::Offline);
        }
        let mut state = self.state.borrow_mut();
        let workspace = state
            .view
            .workspaces()
            .iter()
            .find(|workspace| {
                workspace.tab_order().iter().any(|&tab| {
                    workspace
                        .arrangement(tab)
                        .is_some_and(|arrangement| arrangement.contains_pane(pane))
                })
            })
            .ok_or(DesktopClientError::TargetGone)?
            .workspace();
        let tab = snapshot
            .organization
            .state()
            .panes
            .iter()
            .find(|record| record.id == pane)
            .ok_or(DesktopClientError::TargetGone)?
            .tab;
        state
            .view
            .activate_workspace(workspace)
            .map_err(view_error)?;
        state
            .view
            .activate_tab(workspace, tab)
            .map_err(view_error)?;
        state
            .view
            .arrange(workspace, tab, view::ArrangementEdit::Focus(pane))
            .map_err(view_error)?;
        // Offline selection is personal navigation, never queued startup.
        if observation.readiness() != ReplicaReadiness::Ready {
            self.publish_frames(&mut state)?;
            return Ok(Vec::new());
        }
        let context = channel.state().borrow().context;
        let view = state.view.clone();
        let effects = state
            .startup
            .entry(pane.server())
            .or_default()
            .begin(
                UseFacts {
                    view: &view,
                    replica: &observation,
                    context,
                },
                view::SelectionTarget {
                    workspace,
                    tab,
                    pane,
                },
                source,
            )
            .map_err(view_error)?
            .effects;
        self.publish_frames(&mut state)?;
        Ok(effects)
    }

    fn measured(
        &self,
        measurement: PaneMeasurement,
    ) -> Result<Vec<StartupEffect>, DesktopClientError> {
        if self.token(measurement.token.pane)? != measurement.token {
            return Err(DesktopClientError::StaleContext);
        }
        let server = measurement.token.pane.server();
        let snapshot = self.snapshot(server)?;
        let observation = self.observation(server, &snapshot, false)?;
        let mut state = self.state.borrow_mut();
        let view = state.view.clone();
        let effects = state
            .startup
            .entry(server)
            .or_default()
            .measured(
                UseFacts {
                    view: &view,
                    replica: &observation,
                    context: measurement.token.context_generation,
                },
                measurement.view_measurement()?,
            )
            .map_err(view_error)?
            .effects;
        self.publish_frames(&mut state)?;
        Ok(effects)
    }

    async fn execute(
        self: &Rc<Self>,
        effects: Vec<StartupEffect>,
        cx: &mut gpui::AsyncApp,
    ) -> Result<Option<OperationId>, DesktopClientError> {
        use view::launch::StartupExecution;
        let mut queue = VecDeque::from(effects);
        let mut operation = None;
        while let Some(effect) = queue.pop_front() {
            let (server, ticket, channel) = match effect {
                StartupEffect::RequestUse { ticket, target, .. }
                | StartupEffect::AcquireControl { ticket, target, .. }
                | StartupEffect::RequestMeasurement { ticket, target, .. } => {
                    let channel = self.channel(target.pane)?;
                    self.state
                        .borrow_mut()
                        .startup_channels
                        .insert((target.pane.server(), ticket), channel.clone());
                    (target.pane.server(), ticket, channel)
                }
                StartupEffect::CompleteUse { ticket, fence, .. } => {
                    let server = fence.session().pane.server();
                    let channel = self
                        .state
                        .borrow()
                        .startup_channels
                        .get(&(server, ticket))
                        .cloned()
                        .ok_or(DesktopClientError::StaleContext)?;
                    (server, ticket, channel)
                }
                StartupEffect::CancelUse { ticket, intent } => {
                    let server = intent.stream().server();
                    let Some(channel) = self
                        .state
                        .borrow()
                        .startup_channels
                        .get(&(server, ticket))
                        .cloned()
                    else {
                        continue;
                    };
                    (server, ticket, channel)
                }
            };
            let executor = self
                .state
                .borrow_mut()
                .executors
                .entry(server)
                .or_insert_with(|| {
                    Arc::new(tokio::sync::Mutex::new(view::launch::StartupExecutor::new()))
                })
                .clone();
            let binding = self
                .source(&server)
                .ok_or(DesktopClientError::TargetGone)?
                .binding
                .clone();
            let tracker = self.tracker.clone();
            let peer = self.operations.clone();
            let original = channel.clone();
            let result = self
                .executor
                .spawn(async move {
                    executor
                        .lock()
                        .await
                        .execute(effect, &original, &binding, &tracker, peer.as_ref())
                        .await
                })
                .await
                .map_err(|_| DesktopClientError::Cancelled)?;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let mut state = self.state.borrow_mut();
                    state.startup.entry(server).or_default().rejected(ticket);
                    state.startup_channels.remove(&(server, ticket));
                    state.measurements.remove(&channel.pane());
                    drop(state);
                    self.notifier.frame_changed(server);
                    return Err(error.into());
                }
            };
            match result {
                StartupExecution::Authorized(authorization) => {
                    let snapshot = self.snapshot(server)?;
                    let observation = self.observation(server, &snapshot, false)?;
                    let mut state = self.state.borrow_mut();
                    let view = state.view.clone();
                    let effects = state
                        .startup
                        .entry(server)
                        .or_default()
                        .authorized(
                            UseFacts {
                                view: &view,
                                replica: &observation,
                                context: channel.state().borrow().context,
                            },
                            authorization,
                        )
                        .map_err(view_error)?
                        .effects;
                    queue.extend(effects);
                }
                StartupExecution::Measurement(request) => {
                    let token = self.token(request.target.pane)?;
                    if token.context_generation != request.context {
                        return Err(DesktopClientError::StaleContext);
                    }
                    self.state.borrow_mut().measurements.insert(
                        request.target.pane,
                        PaneMeasurementRequest {
                            token,
                            ticket: request.ticket,
                            layout: request.layout,
                        },
                    );
                }
                StartupExecution::Submitted(completed) => {
                    {
                        let mut state = self.state.borrow_mut();
                        state.measurements.remove(&channel.pane());
                        state.startup_channels.remove(&(server, ticket));
                    }
                    operation = Some(completed.id());
                    cx.update(|cx| self.watch_operation(completed, cx));
                }
                StartupExecution::Cancelled | StartupExecution::Controlled(_) => {
                    let mut state = self.state.borrow_mut();
                    state.measurements.remove(&channel.pane());
                    state.startup_channels.remove(&(server, ticket));
                }
            }
            self.notifier.frame_changed(server);
        }
        Ok(operation)
    }

    fn watch_operation(self: &Rc<Self>, ticket: OperationTicket, cx: &mut App) {
        self.tasks.borrow_mut().retain(|task| !task.is_ready());
        let id = ticket.id();
        if !self.state.borrow_mut().watching.insert(id) {
            return;
        }
        let mut status = ticket.status();
        let weak = Rc::downgrade(self);
        self.tasks.borrow_mut().push(cx.spawn(async move |_| {
            loop {
                let completed = match status.borrow_and_update().clone() {
                    OperationStatus::Completed {
                        outcome,
                        publication_revision,
                    } => Some((Arc::new(outcome), publication_revision)),
                    OperationStatus::Retired => return,
                    _ => None,
                };
                if let Some((outcome, revision)) = completed {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    inner
                        .state
                        .borrow_mut()
                        .outcomes
                        .insert(id, (outcome.clone(), revision));
                    let _ = inner
                        .notifier
                        .operation_outcome(OperationOutcomeNotice {
                            server: id.stream().server(),
                            operation: id,
                            outcome,
                        })
                        .await;
                    return;
                }
                if status.changed().await.is_err() {
                    return;
                }
            }
        }));
    }
}

impl<S: ViewStore + 'static> DesktopClientProvider for ComposedDesktopClientProvider<S> {
    fn directory_source(
        &self,
        server: ServerId,
    ) -> Result<Rc<dyn crate::ui::directory_picker::DirectorySource>, DesktopClientError> {
        let source = self
            .inner
            .source(&server)
            .ok_or(DesktopClientError::TargetGone)?;
        let name = source
            .name
            .name
            .get()
            .map_or("Unnamed Server", |name| name.as_ref());
        let port = self.directory_connection(server)?;
        crate::ui::directory_picker::ServerDirectorySource::new(
            server,
            name,
            port,
            spaceterm_core::server_directories::DirectoryPurpose::WorkspacePin,
        )
        .map(|source| Rc::new(source) as Rc<dyn crate::ui::directory_picker::DirectorySource>)
        .map_err(|_| DesktopClientError::Offline)
    }

    fn directory_connection(
        &self,
        server: ServerId,
    ) -> Result<Arc<dyn spaceterm_client::directories::DirectoryConnectionPort>, DesktopClientError>
    {
        server_ports::directory_connection(&self.inner, server)
    }

    fn retained_pane_repair(
        &self,
        pane: PaneId,
        cx: &mut App,
    ) -> Result<Option<(OperationId, Command)>, DesktopClientError> {
        server_ports::retained_pane_repair(&self.inner, pane, cx)
    }

    fn servers(&self) -> Vec<DesktopServerInfo> {
        self.inner
            .order
            .borrow()
            .iter()
            .filter_map(|server| self.inner.source(server))
            .filter(|source| {
                self.inner
                    .tracker
                    .journal()
                    .ordinary_use_allowed(&source.binding)
                    .unwrap_or(false)
            })
            .map(|source| source.name.clone())
            .collect()
    }
    fn frame(&self, server: ServerId) -> Option<Arc<DesktopServerFrame>> {
        let source = self.inner.source(&server)?;
        if !self
            .inner
            .tracker
            .journal()
            .ordinary_use_allowed(&source.binding)
            .ok()?
        {
            return None;
        }
        self.inner.state.borrow().frames.get(&server).cloned()
    }
    fn pane_view_token(&self, pane: PaneId) -> Result<PaneViewToken, DesktopClientError> {
        self.inner.token(pane)
    }
    fn pane_channel(&self, pane: PaneId) -> Result<PaneChannel, DesktopClientError> {
        self.inner.channel(pane)
    }
    fn pane_presentation(
        &self,
        pane: PaneId,
        displayed: Option<SessionKey>,
    ) -> Result<PanePresentationDelivery, DesktopClientError> {
        self.inner.presentation(pane, displayed)
    }
    fn pane_measurement(&self, pane: PaneId) -> Option<PaneMeasurementRequest> {
        let request = self.inner.state.borrow().measurements.get(&pane).copied()?;
        (self.inner.token(pane).ok()? == request.token).then_some(request)
    }
    fn revalidate_pane(&self, pane: PaneId, cx: &mut App) {
        let Ok(_edit) = self.inner.edits.try_lock() else {
            return;
        };
        let pending = self
            .inner
            .state
            .borrow()
            .startup
            .get(&pane.server())
            .and_then(StartupIntents::pending);
        if pending.is_none_or(|pending| pending.context.target().pane != pane) {
            return;
        }
        let Ok(token) = self.inner.token(pane) else {
            return;
        };
        let revision = self.inner.state.borrow().view.revision();
        if self.inner.state.borrow().revalidated.get(&pane) == Some(&(token, revision)) {
            return;
        }
        self.inner
            .state
            .borrow_mut()
            .revalidated
            .insert(pane, (token, revision));
        let Ok(effects) = self.inner.refresh(pane.server()) else {
            return;
        };
        if effects.is_empty() {
            return;
        }
        let inner = self.inner.clone();
        self.inner
            .tasks
            .borrow_mut()
            .retain(|task| !task.is_ready());
        self.inner
            .tasks
            .borrow_mut()
            .push(cx.spawn(async move |cx| {
                let _ = inner.execute(effects, cx).await;
            }));
    }

    fn outcome(&self, operation: OperationId) -> Option<Arc<Outcome>> {
        if let Some((outcome, _)) = self.inner.state.borrow().outcomes.get(&operation) {
            return Some(outcome.clone());
        }
        match self
            .inner
            .tracker
            .ticket(operation)
            .ok()?
            .status()
            .borrow()
            .clone()
        {
            OperationStatus::Completed { outcome, .. } => Some(Arc::new(outcome)),
            _ => None,
        }
    }
    fn outcome_revision(&self, operation: OperationId) -> Option<OrganizationRevision> {
        if let Some((_, revision)) = self.inner.state.borrow().outcomes.get(&operation) {
            return Some(*revision);
        }
        match self
            .inner
            .tracker
            .ticket(operation)
            .ok()?
            .status()
            .borrow()
            .clone()
        {
            OperationStatus::Completed {
                publication_revision,
                ..
            } => Some(publication_revision),
            _ => None,
        }
    }

    fn begin(
        &self,
        request: DesktopRequest,
        cx: &mut App,
    ) -> Result<Task<Result<Option<OperationId>, DesktopClientError>>, DesktopClientError> {
        self.begin_ui_request(UiRequestId::FIRST, request, cx)
    }
    fn begin_ui_request(
        &self,
        id: UiRequestId,
        request: DesktopRequest,
        cx: &mut App,
    ) -> Result<Task<Result<Option<OperationId>, DesktopClientError>>, DesktopClientError> {
        let inner = self.inner.clone();
        // Reserve the personal-state transaction at admission, not at the task's first
        // poll. GPUI may poll later requests first; they must not overwrite an earlier save.
        let admission = self
            .inner
            .edits
            .clone()
            .try_lock_owned()
            .map_err(|_| DesktopClientError::AdmissionFull)?;
        match request {
            request @ (DesktopRequest::SelectPane { .. }
            | DesktopRequest::OpenTab { .. }
            | DesktopRequest::StartSession { .. }
            | DesktopRequest::Retry { .. }) => {
                let effects = inner.use_request(&request)?;
                Ok(cx.spawn(async move |cx| inner.execute(effects, cx).await))
            }
            DesktopRequest::Measured(measurement) => {
                let effects = inner.measured(measurement)?;
                Ok(cx.spawn(async move |cx| inner.execute(effects, cx).await))
            }
            DesktopRequest::TakeOver { pane, .. } => {
                let channel = inner.channel(pane)?;
                Ok(cx.spawn(async move |cx| {
                    let ticket = inner
                        .executor
                        .spawn(async move { channel.take_over().await })
                        .await
                        .map_err(|_| DesktopClientError::Cancelled)?
                        .map_err(DesktopClientError::from)?;
                    let id = ticket.id();
                    cx.update(|cx| inner.watch_operation(ticket, cx));
                    Ok(Some(id))
                }))
            }
            DesktopRequest::RetryConnection(server) => {
                let handle = inner
                    .source(&server)
                    .and_then(|source| source.handle.clone())
                    .ok_or(DesktopClientError::UnsupportedCapability)?;
                Ok(cx.spawn(async move |_| {
                    inner
                        .executor
                        .spawn(async move { handle.retry().await })
                        .await
                        .map_err(|_| DesktopClientError::Cancelled)?
                        .map_err(DesktopClientError::from)?;
                    Ok(None)
                }))
            }
            DesktopRequest::Shared { server, command } => {
                if command.server() != server {
                    return Err(DesktopClientError::WrongServer);
                }
                let frame = self.frame(server).ok_or(DesktopClientError::Reconciling)?;
                match frame.replica_status {
                    ReplicaStatus::Ready => {}
                    ReplicaStatus::Resynchronizing => return Err(DesktopClientError::Reconciling),
                    ReplicaStatus::RetainedOffline => return Err(DesktopClientError::Offline),
                }
                let binding = inner
                    .source(&server)
                    .ok_or(DesktopClientError::TargetGone)?
                    .binding
                    .clone();
                let reservation = if let Command::RepairPaneDirectory { pane, .. } = &command {
                    if self.retained_pane_repair(*pane, cx)?.is_some() {
                        return Err(DesktopClientError::Reconciling);
                    }
                    if !inner.state.borrow_mut().repair_admissions.insert(*pane) {
                        return Err(DesktopClientError::AdmissionFull);
                    }
                    Some(RepairAdmission {
                        pane: *pane,
                        owner: Rc::downgrade(&inner),
                    })
                } else {
                    None
                };
                Ok(cx.spawn(async move |cx| {
                    let _reservation = reservation;
                    let tracker = inner.tracker.clone();
                    let peer = inner.operations.clone();
                    let ticket = inner
                        .executor
                        .spawn(async move {
                            tracker.submit(&binding, command, None, peer.as_ref()).await
                        })
                        .await
                        .map_err(|_| DesktopClientError::Cancelled)?
                        .map_err(DesktopClientError::from)?;
                    let id = ticket.id();
                    cx.update(|cx| inner.watch_operation(ticket, cx));
                    Ok(Some(id))
                }))
            }
            request @ (DesktopRequest::PersonalEdit(_) | DesktopRequest::AttachWorkspace(_)) => {
                let persistence = self.persistence.clone();
                let background = cx.background_executor().clone();
                Ok(cx.spawn(async move |_| {
                    let _edit = admission;
                    let mut candidate = inner.state.borrow().view.clone();
                    let mut attached_lease = None;
                    match request {
                        DesktopRequest::PersonalEdit(edit) => {
                            apply_edit(&inner, &mut candidate, edit)?
                        }
                        DesktopRequest::AttachWorkspace(workspace) => {
                            let snapshot = inner.snapshot(workspace.server())?;
                            let observation =
                                inner.observation(workspace.server(), &snapshot, false)?;
                            let facts = ReplicaFacts {
                                pairing: observation.pairing(),
                                client: observation.client(),
                                connection: observation.connection(),
                                readiness: observation.readiness(),
                                status: observation.status(),
                            };
                            let binding = inner
                                .source(&workspace.server())
                                .ok_or(DesktopClientError::TargetGone)?
                                .binding
                                .clone();
                            let journal = inner.tracker.journal().clone();
                            let channels = inner.channels.clone();
                            let attached = inner
                                .executor
                                .spawn(async move {
                                    let observation = ReplicaObservation::new(
                                        &snapshot.organization,
                                        snapshot.accepted_creations(),
                                        facts,
                                    )
                                    .map_err(view_error)?;
                                    let (lease, _) = view::launch::attach(
                                        &mut candidate,
                                        &observation,
                                        workspace,
                                        &binding,
                                        &journal,
                                        &channels,
                                    )
                                    .await
                                    .map_err(launch_error)?;
                                    Ok::<_, DesktopClientError>((candidate, lease))
                                })
                                .await
                                .map_err(|_| DesktopClientError::Cancelled)??;
                            candidate = attached.0;
                            attached_lease = Some((workspace, attached.1));
                        }
                        _ => return Err(DesktopClientError::UnsupportedCapability),
                    }
                    let persisted = candidate.persisted().map_err(view_error)?;
                    background
                        .spawn(async move {
                            persistence
                                .lock()
                                .map_err(|_| DesktopClientError::LocalStorageUnavailable)?
                                .save(&persisted)
                                .map_err(view_error)
                        })
                        .await?;
                    let revision = {
                        let mut state = inner.state.borrow_mut();
                        state.view = candidate;
                        if let Some((workspace, lease)) = attached_lease {
                            state.leases.insert(workspace, lease);
                        }
                        inner.publish_frames(&mut state)?;
                        state.revision
                    };
                    inner.notifier.personal_edit_applied(id, revision).await?;
                    Ok(None)
                }))
            }
            DesktopRequest::DetachWorkspace(workspace) => {
                let plan = inner
                    .state
                    .borrow()
                    .view
                    .prepare_detach(workspace)
                    .map_err(view_error)?;
                let attachment = plan.attachment();
                let binding = inner
                    .source(&workspace.server())
                    .ok_or(DesktopClientError::TargetGone)?
                    .binding
                    .clone();
                let retained = inner
                    .pending
                    .journal()
                    .snapshot()
                    .map_err(DesktopClientError::from)?;
                inner
                    .state
                    .borrow_mut()
                    .detach_requests
                    .retain(|_, attachment| {
                        retained
                            .detaches()
                            .iter()
                            .any(|task| task.workspace == attachment.workspace())
                    });
                if inner.state.borrow().detach_requests.len() >= super::MAX_PENDING_UI_REQUESTS {
                    return Err(DesktopClientError::AdmissionFull);
                }
                inner
                    .state
                    .borrow_mut()
                    .detach_requests
                    .insert(id, attachment);
                let pending = inner.pending.clone();
                let detached_binding = binding.clone();
                Ok(cx.spawn(async move |_| {
                    let _edit = admission;
                    inner
                        .executor
                        .spawn(async move { pending.detach_attachment(&binding, attachment).await })
                        .await
                        .map_err(|_| DesktopClientError::Cancelled)?
                        .map_err(DesktopClientError::from)?;
                    let revision = {
                        let mut state = inner.state.borrow_mut();
                        let current = state
                            .view
                            .workspace(workspace)
                            .ok_or(DesktopClientError::TargetGone)?;
                        if current.attachment() != attachment {
                            return Err(DesktopClientError::StaleContext);
                        }
                        let removed: std::collections::HashSet<_> = current
                            .tab_order()
                            .iter()
                            .filter_map(|&tab| current.arrangement(tab))
                            .flat_map(|arrangement| arrangement.pane_ids())
                            .collect();
                        let plan = state.view.prepare_detach(workspace).map_err(view_error)?;
                        state.view.apply_detach(plan).map_err(view_error)?;
                        state.channels.retain(|pane, _| !removed.contains(pane));
                        state.measurements.retain(|pane, _| !removed.contains(pane));
                        state.leases.remove(&workspace);
                        inner.publish_frames(&mut state)?;
                        state.revision
                    };
                    inner.notifier.personal_edit_applied(id, revision).await?;
                    let connected = inner.source(&workspace.server()).is_some_and(|source| {
                        matches!(*source.state.borrow(), ConnectionState::Connected { .. })
                    });
                    if connected {
                        let pending = inner.pending.clone();
                        let operations = inner.operations.clone();
                        let settled = inner
                            .executor
                            .spawn(async move {
                                drain_original_detach(
                                    &pending,
                                    &detached_binding,
                                    workspace,
                                    operations.as_ref(),
                                )
                                .await
                            })
                            .await
                            .map_err(|_| DesktopClientError::Cancelled)?;
                        if settled == Ok(true) {
                            inner.state.borrow_mut().detach_requests.remove(&id);
                        }
                    }
                    Ok(None)
                }))
            }
            DesktopRequest::RetryDetachWorkspace {
                workspace,
                original_request,
            } => {
                let original = inner
                    .state
                    .borrow()
                    .detach_requests
                    .get(&original_request)
                    .copied()
                    .ok_or(DesktopClientError::StaleContext)?;
                if original.workspace() != workspace {
                    return Err(DesktopClientError::StaleContext);
                }
                let binding = inner
                    .source(&workspace.server())
                    .ok_or(DesktopClientError::TargetGone)?
                    .binding;
                let pending = inner.pending.clone();
                let operations = inner.operations.clone();
                Ok(cx.spawn(async move |_| {
                    let _edit = admission;
                    let settled = inner
                        .executor
                        .spawn(async move {
                            drain_original_detach(
                                &pending,
                                &binding,
                                workspace,
                                operations.as_ref(),
                            )
                            .await
                        })
                        .await
                        .map_err(|_| DesktopClientError::Cancelled)?
                        .map_err(DesktopClientError::from)?;
                    if settled {
                        inner
                            .state
                            .borrow_mut()
                            .detach_requests
                            .remove(&original_request);
                    }
                    Ok(None)
                }))
            }
            DesktopRequest::ServerAction(_) => Err(DesktopClientError::UnsupportedCapability),
        }
    }
    fn save_view(&self, cx: &mut App) -> Task<Result<(), DesktopClientError>> {
        self.save(cx)
    }
    fn prepare_quit(&self, cx: &mut App) -> Task<Result<(), DesktopClientError>> {
        let save = self.save(cx);
        let inner = self.inner.clone();
        cx.spawn(async move |_| {
            save.await?;
            let runtime = inner.runtime.borrow_mut().take();
            if let Some(runtime) = runtime {
                inner
                    .executor
                    .spawn(runtime.shutdown())
                    .await
                    .map_err(|_| DesktopClientError::Cancelled)?
                    .map_err(DesktopClientError::from)?;
            }
            Ok(())
        })
    }
}

/// Resume the journaled task only. Each pass uses its original grants and durable operation IDs.
pub(crate) async fn drain_original_detach(
    pending: &PendingTasks,
    binding: &PairedServerBinding,
    workspace: WorkspaceId,
    peer: &dyn OperationPeer,
) -> Result<bool, spaceterm_client::failure::ClientFailure> {
    let key = binding.target().local_pairing()?;
    let state = pending.journal().snapshot()?;
    let Some(task) = state
        .detaches()
        .iter()
        .find(|task| task.local_pairing == key && task.workspace == workspace)
    else {
        return Ok(true);
    };
    let passes = task
        .grants
        .len()
        .saturating_add(task.dependencies.len())
        .saturating_mul(2)
        .saturating_add(2);
    for _ in 0..passes {
        let before = pending.journal().snapshot()?.revision();
        if pending.drain_detach(binding, workspace, peer).await? {
            return Ok(true);
        }
        if pending.journal().snapshot()?.revision() == before {
            return Ok(false);
        }
    }
    Ok(false)
}

fn apply_edit(
    inner: &Inner,
    view: &mut ClientViewState,
    edit: ViewEdit,
) -> Result<(), DesktopClientError> {
    match edit {
        ViewEdit::ActivateWorkspace(workspace) => view.activate_workspace(workspace),
        ViewEdit::ActivateTab { workspace, tab } => view.activate_tab(workspace, tab),
        ViewEdit::ActivateAdjacentTab { workspace, step } => view.activate_adjacent_tab(
            workspace,
            step,
            &inner.snapshot(workspace.server())?.organization,
        ),
        ViewEdit::ActivateWorktree {
            workspace,
            worktree,
        } => view.activate_worktree(
            workspace,
            worktree,
            &inner.snapshot(workspace.server())?.organization,
        ),
        ViewEdit::MoveTab {
            workspace,
            tab,
            position,
        } => view.move_tab(
            workspace,
            tab,
            position,
            &inner.snapshot(workspace.server())?.organization,
        ),
        ViewEdit::StepTab {
            workspace,
            tab,
            step,
        } => view.step_tab(
            workspace,
            tab,
            step,
            &inner.snapshot(workspace.server())?.organization,
        ),
        ViewEdit::Arrange {
            workspace,
            tab,
            edit,
        } => view.arrange(workspace, tab, edit),
    }
    .map(|_| ())
    .map_err(view_error)
}

fn launch_error(error: view::launch::LaunchFailure) -> DesktopClientError {
    match error {
        view::launch::LaunchFailure::Journal(error) => error.into(),
        view::launch::LaunchFailure::View(error) => view_error(error),
    }
}

fn view_error(error: view::ViewError) -> DesktopClientError {
    use view::ViewError as E;
    match error {
        E::TargetMissing => DesktopClientError::TargetGone,
        E::WrongServer | E::WrongMembership => DesktopClientError::WrongServer,
        E::NotReady => DesktopClientError::Reconciling,
        E::InvalidGeometry | E::InvalidRatio | E::InsufficientSpace => {
            DesktopClientError::InvalidGeometry
        }
        E::ResourceLimit { .. } => DesktopClientError::LimitReached,
        E::IdentityExhausted { .. } => DesktopClientError::CounterExhausted,
        E::StaleView | E::StaleObservation | E::StaleIntent => DesktopClientError::StaleContext,
        _ => DesktopClientError::LocalStorageUnavailable,
    }
}

fn offline_cause(
    connection: &ConnectionState,
    name: DisplayName,
    blocked: Option<(BlockedLocation, IntendedAction)>,
) -> OfflineCause {
    match connection {
        ConnectionState::Stopped => OfflineCause::Stopped(name),
        ConnectionState::Incompatible { .. } => OfflineCause::UpdateRequired,
        _ => blocked.map_or(OfflineCause::Unreachable, |(location, intended)| {
            OfflineCause::Blocked {
                server: name,
                location,
                intended,
            }
        }),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_git_tests;
