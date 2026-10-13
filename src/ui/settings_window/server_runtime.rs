//! Settings shares the recovered application's registry, operation tracker and pending-task owner.
//! A presentation key never supplies native authority or chooses a later selected server.

use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, time::Instant};

use gpui::{App, Entity, Global, SharedString, Task, WeakEntity};
use spaceterm_client::{
    api::{ConnectionState, PairedServerBinding, RouteParts, ServerQueries},
    failure::{ClientFailure, ClientFailureClass},
    journal::{RetainedOperationState, RevocationPhase},
    management_cli::{
        AssessmentEvidence, AssessmentToken, ManagementAction, ManagementAssessment,
        ManagementFailure, ManagementFuture,
    },
    operations::{OperationPeer, OperationStatus, OperationTicket, OperationTracker},
    pending::PendingTasks,
    registry::ServerHandle,
    replica::ReplicaState,
};
use spaceterm_client_native::native::{
    NativeActionPrompt, NativeServiceManagement, classify_service as classify_native,
};
use spaceterm_core::{
    authority::FieldVersion,
    identity::{PairingId, ServerId},
    observations::MonotonicTick,
    operation_ledger_contracts::CanonicalOperationRequest,
    operations::{
        Command, FieldConflict, OperationFailure, OperationId, OperationSequence, Outcome,
    },
    organization::WorktreeLocationTemplate,
    pairing::{
        PairingCommand, PairingOutcome, PairingTarget, PendingParts, PendingRevocationTask,
        PendingRevocationTaskParts, PendingSelfRevocation,
    },
};
use spaceterm_host::account_service::ServiceAction;
use tokio::sync::watch;

use super::servers::model::{self, *};
pub(crate) use super::servers::model::{
    BindScope, ServerPermission, ServerPermissionKind, ServerPermissionState, ServerPermissions,
    ServerServiceLifecycle,
};
use crate::ui::server_desktop_contracts::DesktopServerInfo;

/// The inventory wire adapter must share this connection's worker. No default or test provider
/// is installed in production when that adapter is unavailable.
pub(crate) trait SettingsInventory: Send + Sync {
    fn page(
        &self,
        after: Option<PairingId>,
        version: Option<FieldVersion>,
    ) -> ManagementFuture<'_, PairedDevices>;
}

#[derive(Clone)]
pub(crate) struct SettingsConnection {
    pub(crate) binding: PairedServerBinding,
    pub(crate) name: DesktopServerInfo,
    pub(crate) state: watch::Receiver<ConnectionState>,
    pub(crate) replica: watch::Receiver<ReplicaState>,
    pub(crate) queries: Option<ServerQueries>,
    /// Explicit native composition only. An IPC pathname or an SSH route grants none.
    pub(crate) native: Option<Arc<NativeServiceManagement>>,
    pub(crate) lifecycle: Option<ServerServiceLifecycle>,
    pub(crate) inventory: Option<Arc<dyn SettingsInventory>>,
    pub(crate) machine: Option<SettingsMachineFacts>,
}

impl SettingsConnection {
    pub(crate) fn from_handle(
        binding: PairedServerBinding,
        name: DesktopServerInfo,
        handle: &ServerHandle,
        tracker: &OperationTracker,
    ) -> Result<Self, ClientFailure> {
        if name.server != binding.target().server() || handle.identity() != name.server {
            return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
        }
        Ok(Self {
            binding,
            name,
            state: handle.state(),
            replica: handle.replica(),
            queries: Some(handle.queries(tracker.journal().clone())?),
            native: None,
            lifecycle: None,
            inventory: None,
            machine: None,
        })
    }
}

/// Recovered foreground native capabilities and authenticated inventory adapters, scoped to
/// the exact original local pairing. Defaults refuse effects rather than inventing providers.
#[derive(Clone, Default)]
pub(crate) struct SettingsServices {
    pub(crate) lifecycles: Vec<(
        spaceterm_core::pairing::LocalPairingKey,
        ServerServiceLifecycle,
    )>,
    pub(crate) hosts: Vec<(
        spaceterm_core::pairing::LocalPairingKey,
        Arc<NativeServiceManagement>,
    )>,
    pub(crate) machines: Vec<(
        spaceterm_core::pairing::LocalPairingKey,
        SettingsMachineFacts,
    )>,
    pub(crate) inventories: Vec<(
        spaceterm_core::pairing::LocalPairingKey,
        Arc<dyn SettingsInventory>,
    )>,
}
/// Native host facts are admitted only by an exact local registration/pin match.
#[derive(Clone)]
pub(crate) struct SettingsMachineFacts {
    pub(crate) permissions: Option<ServerPermissions>,
    pub(crate) scopes: Vec<BindScope>,
}

struct AuthenticatedInventory(ServerQueries);
impl SettingsInventory for AuthenticatedInventory {
    fn page(
        &self,
        after: Option<PairingId>,
        version: Option<FieldVersion>,
    ) -> ManagementFuture<'_, PairedDevices> {
        Box::pin(async move {
            let page = self.0.paired_devices(after, version).await?;
            Ok(PairedDevices {
                version: page.version,
                this_device: page.this_device,
                total: page.total,
                devices: page
                    .devices
                    .into_iter()
                    .map(|target| PairedDevice {
                        name: format!("Device {}", target.parts().pairing.local()).into(),
                        target,
                    })
                    .collect(),
                next: page.next,
            })
        })
    }
}

impl SettingsServices {
    /// Recovers capabilities from the canonical journal without enrolling, installing or starting.
    /// Remote routes get authenticated inventory only, never this computer's native host authority.
    pub(crate) fn recover(
        registry: &spaceterm_client::registry::ClientRegistry,
        journal: &Arc<spaceterm_client::journal::JournalController>,
        native: Option<&spaceterm_client_native::NativeClientProvider>,
    ) -> Result<Self, ClientFailure> {
        let mut result = Self::default();
        let registration = native
            .map(|provider| provider.management().supervisor().discover())
            .transpose()
            .map_err(|_| ClientFailure::new(ClientFailureClass::Unavailable))?
            .flatten();
        for saved in journal.snapshot()?.bindings() {
            let binding = saved.binding()?;
            if !journal.ordinary_use_allowed(&binding)? {
                continue;
            }
            let key = binding.target().local_pairing()?;
            if let Some(handle) = registry.server(binding.target().server()) {
                result.inventories.push((
                    key.clone(),
                    Arc::new(AuthenticatedInventory(handle.queries(journal.clone())?)),
                ));
            }
            if let (Some(provider), Some(registration)) = (native, registration.as_ref()) {
                result.admit_native(provider, registration, &binding)?;
            }
        }
        Ok(result)
    }
    pub(crate) fn admit_native(
        &mut self,
        provider: &spaceterm_client_native::NativeClientProvider,
        registration: &spaceterm_host::account_service::RegisteredServer,
        binding: &PairedServerBinding,
    ) -> Result<(), ClientFailure> {
        if registration.key() != provider.key()
            || registration.server_pin() != binding.target().pin()
            || !matches!(binding.target().route().parts(), RouteParts::Ipc { .. })
        {
            return Ok(());
        }
        let route = spaceterm_client_native::route::LocalRouteAuthority::new(
            registration
                .endpoint()
                .map_err(|_| ClientFailure::new(ClientFailureClass::Unavailable))?,
        )?;
        if route.description() != binding.target().route() {
            return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
        }
        let key = binding.target().local_pairing()?;
        self.hosts.push((key.clone(), provider.management()));
        self.lifecycles.push((
            key.clone(),
            crate::platform::server_settings_services::lifecycle(),
        ));
        self.machines.push((
            key,
            crate::platform::server_settings_services::machine_facts(provider.key().build_identity),
        ));
        Ok(())
    }

    pub(crate) fn bind(&self, source: &mut SettingsConnection) -> Result<(), ClientFailure> {
        let key = source.binding.target().local_pairing()?;
        source.lifecycle = self
            .lifecycles
            .iter()
            .find(|(pairing, _)| pairing == &key)
            .map(|(_, lifecycle)| *lifecycle);
        source.native = self
            .hosts
            .iter()
            .find(|(pairing, _)| pairing == &key)
            .map(|(_, provider)| provider.clone());
        source.inventory = self
            .inventories
            .iter()
            .find(|(pairing, _)| pairing == &key)
            .map(|(_, provider)| provider.clone());
        source.machine = self
            .machines
            .iter()
            .find(|(pairing, _)| pairing == &key)
            .map(|(_, facts)| facts.clone());
        Ok(())
    }
}

pub(crate) struct SettingsComposition {
    pub(crate) connections: Vec<SettingsConnection>,
    pub(crate) tracker: OperationTracker,
    pub(crate) operations: Arc<dyn OperationPeer>,
    pub(crate) pending: PendingTasks,
    pub(crate) executor: tokio::runtime::Handle,
}

#[derive(Default)]
struct ServerState {
    native: Option<NativePresentation>,
    edit: Option<SettingEdit>,
    applied_revision: Option<spaceterm_core::authority::OrganizationRevision>,
    /// A conflict reply can arrive before its organization publication.
    conflict: Option<(SharedString, u64)>,
    remote_edit: Option<RemoteAccessEdit>,
    remote_conflict: Option<(
        spaceterm_core::remote_access::RemoteAccessPolicy,
        FieldVersion,
    )>,
    remote_applied_revision: Option<spaceterm_core::authority::OrganizationRevision>,
    status: Option<spaceterm_core::management::ServerStatus>,
    status_connection: Option<spaceterm_core::authority::ConnectionBinding>,
    devices: Option<PairedDevices>,
    unpair: Option<ManagementAssessment>,
    management: ServerManagement,
    busy: bool,
}

struct NativePresentation {
    generation: std::num::NonZeroU64,
    desired: spaceterm_host::startup::DesiredServerState,
    approved: Option<ServerInstallation>,
    staged: Option<ServerInstallation>,
    pending: Option<spaceterm_host::native_deployment::contracts::NativeRequestId>,
}
struct NativeRead {
    desired: spaceterm_host::startup::DesiredServerState,
    status: spaceterm_host::account_service::ServiceStatus,
    generation: std::num::NonZeroU64,
    recovered: Option<ResultPublication>,
}

pub(crate) struct SettingsRuntime {
    connections: RefCell<Vec<SettingsConnection>>,
    tracker: OperationTracker,
    operations: Arc<dyn OperationPeer>,
    pending: PendingTasks,
    executor: tokio::runtime::Handle,
    state: RefCell<HashMap<ServerId, ServerState>>,
    publication: RefCell<Option<WeakEntity<Servers>>>,
    tasks: RefCell<Vec<Task<()>>>,
    origin: Instant,
}

#[derive(Clone)]
struct InstalledSettingsRuntime(Rc<SettingsRuntime>);
impl Global for InstalledSettingsRuntime {}

/// Called once by desktop client composition. Settings window lifetimes own no connection.
pub(crate) fn install(composition: SettingsComposition, cx: &mut App) -> Entity<Servers> {
    if let Some(installed) = cx.try_global::<InstalledSettingsRuntime>()
        && let Some(source) = installed
            .0
            .publication
            .borrow()
            .as_ref()
            .and_then(WeakEntity::upgrade)
    {
        return source;
    }
    let runtime = Rc::new(SettingsRuntime {
        connections: RefCell::new(composition.connections),
        tracker: composition.tracker,
        operations: composition.operations,
        pending: composition.pending,
        executor: composition.executor,
        state: RefCell::new(HashMap::new()),
        publication: RefCell::new(None),
        tasks: RefCell::new(Vec::new()),
        origin: Instant::now(),
    });
    let source = model::install(Rc::new(RuntimeRequests(runtime.clone())), cx);
    *runtime.publication.borrow_mut() = Some(source.downgrade());
    runtime.recover_edits();
    runtime.publish(cx);
    runtime.follow(cx);
    runtime.follow_recovered_tickets(cx);
    cx.set_global(InstalledSettingsRuntime(runtime));
    source
}

/// Adds an already registered, durably active binding to the existing Settings owner.
pub(crate) fn add_connection(
    source: SettingsConnection,
    cx: &mut App,
) -> Result<(), ClientFailure> {
    let runtime = cx
        .try_global::<InstalledSettingsRuntime>()
        .ok_or(ClientFailure::new(ClientFailureClass::Unavailable))?
        .0
        .clone();
    check_ordinary(&runtime.tracker, &source.binding)
        .map_err(|_| ClientFailure::new(ClientFailureClass::AuthorizationDenied))?;
    let previous = runtime
        .connections
        .borrow()
        .iter()
        .position(|old| old.name.server == source.name.server);
    if let Some(index) = previous {
        let old = runtime.connections.borrow()[index].binding.clone();
        if old == source.binding {
            return Ok(());
        }
        if runtime.tracker.journal().ordinary_use_allowed(&old)? {
            return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
        }
        runtime.state.borrow_mut().remove(&source.name.server);
    }
    runtime.follow_connections(std::slice::from_ref(&source), cx);
    if let Some(index) = previous {
        runtime.connections.borrow_mut()[index] = source;
    } else {
        runtime.connections.borrow_mut().push(source);
    }
    runtime.recover_edits();
    runtime.publish(cx);
    Ok(())
}

impl SettingsRuntime {
    fn connection(&self, key: &ServerKey) -> Option<SettingsConnection> {
        self.connections
            .borrow()
            .iter()
            .find(|source| server_key(source.name.server) == *key)
            .cloned()
    }

    fn snapshot(&self) -> ServersSnapshot {
        let Ok(journal) = self.tracker.journal().snapshot() else {
            return ServersSnapshot::default();
        };
        // A mismatched pending owner is never allowed to publish or mutate another journal.
        if !Arc::ptr_eq(self.tracker.journal(), self.pending.journal()) {
            return ServersSnapshot::default();
        }
        let state = self.state.borrow();
        let mut servers = Vec::new();
        for source in self.connections.borrow().iter() {
            if !self
                .tracker
                .journal()
                .ordinary_use_allowed(&source.binding)
                .unwrap_or(false)
            {
                continue;
            }
            let Ok(pairing) = source.binding.target().local_pairing() else {
                continue;
            };
            let connection = source.state.borrow().clone();
            let replica = source.replica.borrow();
            let ready = match (&connection, replica.retained()) {
                (ConnectionState::Connected { info }, Some(snapshot)) => {
                    replica.is_ready()
                        && info.server == source.name.server
                        && info.pairing == source.binding.target().pairing()
                        && info.client == source.binding.target().client()
                        && info.epoch == snapshot.cursor.epoch
                        && info.organization_revision == snapshot.cursor.revision
                }
                _ => false,
            };
            let (status, runtime_epoch, compatibility) = match connection {
                ConnectionState::Connected { info } if ready => (
                    ServerStatus::Running,
                    Some(info.epoch),
                    Some(VerifiedCompatibility::Compatible { major: info.major }),
                ),
                ConnectionState::Connected { .. } | ConnectionState::Connecting { .. } => {
                    (ServerStatus::Connecting, None, None)
                }
                ConnectionState::Stopped => (ServerStatus::Stopped, None, None),
                // NoCommonMajor alone does not prove which release side is older.
                ConnectionState::Incompatible { .. } => {
                    (ServerStatus::UnsupportedProfiles, None, None)
                }
                ConnectionState::Offline { .. } => (ServerStatus::Offline, None, None),
            };
            let personal = state.get(&source.name.server);
            let native = personal.and_then(|state| state.native.as_ref());
            let worktree_location = replica.retained().map(|snapshot| {
                let setting = &snapshot.organization.state().settings.worktree_location;
                let (value, revision) = personal
                    .and_then(|state| state.conflict.as_ref())
                    .filter(|(_, revision)| *revision >= setting.version.get())
                    .cloned()
                    .unwrap_or_else(|| (setting.value.as_str().into(), setting.version.get()));
                ServerTextSetting {
                    value,
                    revision,
                    edit: personal.and_then(|state| {
                        state.edit.clone().filter(|_| {
                            !state
                                .applied_revision
                                .is_some_and(|revision| snapshot.cursor.revision >= revision)
                        })
                    }),
                }
            });
            let verified_status = personal.and_then(|state| state.status.as_ref()).filter(|status| {
                let same_connection = matches!(*source.state.borrow(), ConnectionState::Connected { info } if personal.and_then(|state| state.status_connection) == Some(info.binding));
                ready && same_connection && status.parts().server == source.name.server && Some(status.parts().epoch) == runtime_epoch
            });
            // Both status and organization frames arrive through the authenticated worker.
            // Retained policy is display data while disconnected, never mutation authority.
            let retained_remote = replica
                .retained()
                .and_then(|snapshot| snapshot.organization.state().retained.as_ref())
                .map(|facts| facts.remote_access().clone());
            let published_remote = verified_status
                .map(|status| status.parts().remote_access.clone())
                .filter(|status| {
                    retained_remote
                        .as_ref()
                        .is_none_or(|retained| status.version.get() >= retained.version.get())
                })
                .or(retained_remote);
            let current_remote = personal
                .and_then(|state| state.remote_conflict.as_ref())
                .filter(|(_, version)| {
                    published_remote
                        .as_ref()
                        .is_none_or(|value| version.get() >= value.version.get())
                })
                .map(|(value, version)| spaceterm_core::organization::Versioned {
                    value: value.clone(),
                    version: *version,
                })
                .or(published_remote);
            let remote_access = current_remote.map(|current| {
                let edit = personal.and_then(|state| {
                    state.remote_edit.clone().filter(|edit| {
                        !state.remote_applied_revision.is_some_and(|revision| {
                            replica
                                .retained()
                                .is_some_and(|snapshot| snapshot.cursor.revision >= revision)
                                || current.version.get() > edit.base_version.get()
                        })
                    })
                });
                RemoteAccess {
                    version: current.version,
                    enabled: current.value.is_enabled(),
                    // A policy is not an inventory of selectable machine scopes.
                    scope: source.machine.as_ref().and_then(|machine| {
                        machine
                            .scopes
                            .iter()
                            .find(|scope| scope.endpoints == current.value.endpoints())
                            .map(|scope| scope.id.clone())
                    }),
                    scopes: source
                        .machine
                        .as_ref()
                        .map_or_else(Vec::new, |machine| machine.scopes.clone()),
                    pending: personal.is_some_and(|state| state.busy)
                        || edit
                            .as_ref()
                            .is_some_and(|edit| edit.state == SettingEditState::Saving),
                    endpoints: current.value.endpoints().to_vec(),
                    edit,
                }
            });
            let mut management = personal
                .map(|state| state.management.clone())
                .unwrap_or_default();
            management.pending = personal.is_some_and(|state| state.busy);
            if let Some(request) = native.and_then(|native| native.pending) {
                management.native_request = Some(request);
            }
            servers.push(ServerSummary {
                key: server_key(source.name.server),
                pairing,
                runtime_epoch,
                service_lifecycle: source.lifecycle.filter(|_| native.is_some()),
                compatibility,
                installations: native
                    .map(|native| ServerInstallations {
                        selection_generation: Some(native.generation),
                        approved: native.approved.clone(),
                        staged: native.staged.clone(),
                        running: None,
                        activation: None,
                    })
                    .unwrap_or_default(),
                paired_devices: personal.and_then(|state| state.devices.clone()),
                unpair_assessment: personal.and_then(|state| state.unpair.clone()),
                management,
                name: source
                    .name
                    .name
                    .get()
                    .cloned()
                    .unwrap_or_else(|| "SpaceTerm Server".into()),
                route: route(source),
                status: if matches!(status, ServerStatus::Offline | ServerStatus::Connecting)
                    && native.is_some_and(|native| {
                        native.desired == spaceterm_host::startup::DesiredServerState::Stopped
                    }) {
                    ServerStatus::Stopped
                } else {
                    status
                },
                version: verified_status.and_then(|status| {
                    status
                        .parts()
                        .running
                        .as_ref()
                        .map(|value| value.as_str().into())
                }),
                staged_version: verified_status.and_then(|status| {
                    status
                        .parts()
                        .staged
                        .as_ref()
                        .map(|value| value.as_str().into())
                }),
                worktree_location,
                permissions: source
                    .machine
                    .as_ref()
                    .and_then(|machine| machine.permissions.clone()),
                remote_access,
            });
        }
        servers.sort_by_key(|server| !server.route.is_local());
        let pending_unpairs = journal
            .revocations()
            .iter()
            .filter(|record| !record.notice_dismissed)
            .map(|record| {
                let parts = record.task.parts();
                let operation = parts.original.operation();
                let name = self
                    .connections
                    .borrow()
                    .iter()
                    .find(|source| {
                        source
                            .binding
                            .target()
                            .local_pairing()
                            .is_ok_and(|key| key == parts.local_pairing)
                    })
                    .and_then(|source| source.name.name.get().cloned())
                    .unwrap_or_else(|| "SpaceTerm Server".into());
                PendingUnpair {
                    id: notice_key(operation),
                    server_name: name,
                    original_pairing: parts.local_pairing.clone(),
                    operation,
                    phase: match record.phase {
                        RevocationPhase::AwaitingConfirmation => {
                            PendingUnpairPhase::AwaitingConfirmation(
                                PendingUnpairReason::OutcomeUnknown,
                            )
                        }
                        RevocationPhase::CleanupPending(_) => PendingUnpairPhase::CredentialCleanup,
                    },
                }
            })
            .collect();
        ServersSnapshot {
            servers,
            pending_unpairs,
        }
    }

    fn publish(&self, cx: &mut App) {
        if let Some(source) = self
            .publication
            .borrow()
            .as_ref()
            .and_then(WeakEntity::upgrade)
        {
            let snapshot = self.snapshot();
            source.update(cx, |source, cx| source.publish(snapshot, cx));
        }
    }

    fn follow_connections(self: &Rc<Self>, sources: &[SettingsConnection], cx: &mut App) {
        for source in sources {
            let mut connection = source.state.clone();
            let mut replica = source.replica.clone();
            let weak = Rc::downgrade(self);
            self.tasks.borrow_mut().push(cx.spawn(async move |cx| {
                loop {
                    let alive = tokio::select! { result = connection.changed() => result.is_ok(), result = replica.changed() => result.is_ok() };
                    if !alive { return; }
                    let Some(runtime) = weak.upgrade() else { return; };
                    cx.update(|cx| runtime.publish(cx));
                }
            }));
            if let Some(queries) = source.queries.clone() {
                let mut connection = source.state.clone();
                let weak = Rc::downgrade(self);
                let executor = self.executor.clone();
                let server = source.name.server;
                self.tasks.borrow_mut().push(cx.spawn(async move |cx| {
                    loop {
                        if connection.has_changed().is_err() { return; }
                        if matches!(*connection.borrow(), ConnectionState::Connected { .. }) {
                            let query = queries.clone();
                            let subscription = executor.spawn(async move { query.subscribe_status().await }).await;
                            if let Ok(Ok(mut subscription)) = subscription {
                                loop {
                                    let Some(runtime) = weak.upgrade() else { return; };
                                    if let Ok(latest) = subscription.latest() {
                                        {
                                            let mut states = runtime.state.borrow_mut();
                                            let state = states.entry(server).or_default();
                                            state.status = latest;
                                            state.status_connection = Some(subscription.connection());
                                        }
                                        cx.update(|cx| runtime.publish(cx));
                                    }
                                    drop(runtime);
                                    let changed = tokio::select! { result = subscription.changed() => result.is_ok(), _ = connection.changed() => false };
                                    if !changed { break; }
                                }
                                // Re-establish subscription immediately after a connection generation changes.
                                continue;
                            }
                        }
                        if connection.changed().await.is_err() { return; }
                    }
                }));
            }
        }
        for source in sources {
            let Some(native) = source.native.clone() else {
                continue;
            };
            let weak = Rc::downgrade(self);
            let executor = self.executor.clone();
            let binding = source.binding.clone();
            let server = source.name.server;
            self.tasks.borrow_mut().push(cx.spawn(async move |cx| {
                loop {
                    let Some(runtime) = weak.upgrade() else {
                        return;
                    };
                    if check_ordinary(&runtime.tracker, &binding).is_err() {
                        return;
                    }
                    drop(runtime);
                    let provider = native.clone();
                    let target = binding.clone();
                    let observation = executor
                        .spawn(async move { recover_native(&provider, &target).await })
                        .await;
                    let Some(runtime) = weak.upgrade() else {
                        return;
                    };
                    if check_ordinary(&runtime.tracker, &binding).is_err() {
                        return;
                    }
                    {
                        let mut states = runtime.state.borrow_mut();
                        let state = states.entry(server).or_default();
                        match observation {
                            Ok(Ok(read)) => {
                                let previous_pending =
                                    state.native.as_ref().and_then(|native| native.pending);
                                state.native = Some(NativePresentation {
                                    generation: read.generation,
                                    desired: read.desired,
                                    approved: read.status.approved.as_ref().map(installation),
                                    staged: read.status.staged.as_ref().map(installation),
                                    pending: read.status.pending,
                                });
                                if let Some(ResultPublication::Native {
                                    request,
                                    unresolved: _,
                                    failure,
                                }) = read.recovered
                                    && (state.management.native_request != Some(request)
                                        || previous_pending == Some(request))
                                {
                                    state.management.native_request = Some(request);
                                    state.management.failure = failure;
                                }
                            }
                            Ok(Err(failure)) => {
                                state.native = None;
                                state.management.failure = Some(failure);
                            }
                            Err(_) => {
                                state.native = None;
                                state.management.failure =
                                    Some(ManagementFailure::NativeUnavailable);
                            }
                        }
                    }
                    cx.update(|cx| runtime.publish(cx));
                    drop(runtime);
                    if executor
                        .spawn(async {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }));
        }
    }

    fn follow(self: &Rc<Self>, cx: &mut App) {
        self.follow_connections(&self.connections.borrow(), cx);
        let mut revisions = self.tracker.journal().revisions();
        let mut health = self.tracker.journal().health();
        let weak = Rc::downgrade(self);
        self.tasks.borrow_mut().push(cx.spawn(async move |cx| {
            loop {
                let alive = tokio::select! { result = revisions.changed() => result.is_ok(), result = health.changed() => result.is_ok() };
                if !alive { return; }
                let Some(runtime) = weak.upgrade() else { return; };
                cx.update(|cx| runtime.publish(cx));
            }
        }));
    }

    fn recover_edits(&self) {
        let Ok(journal) = self.tracker.journal().snapshot() else {
            return;
        };
        for operation in journal.operations() {
            if !self.connections.borrow().iter().any(|source| {
                source.binding.target().server() == operation.binding.id.stream().server()
                    && source.binding.target().pairing() == operation.binding.id.stream().pairing()
                    && source.binding.target().client() == operation.binding.id.stream().client()
                    && self
                        .tracker
                        .journal()
                        .ordinary_use_allowed(&source.binding)
                        .unwrap_or(false)
            }) {
                continue;
            }
            let Ok(request) = CanonicalOperationRequest::decode(
                &operation.request,
                self.tracker.journal().codec(),
            ) else {
                continue;
            };
            if let Command::SetRemoteAccess {
                server,
                expected_version,
                value,
            } = request.command()
            {
                let (edit_state, outcome, revision) = match &operation.state {
                    RetainedOperationState::Pending => (SettingEditState::Saving, None, None),
                    RetainedOperationState::Completed {
                        outcome,
                        publication_revision,
                        ..
                    } => {
                        let Ok(outcome) = outcome.outcome(self.tracker.journal().codec()) else {
                            continue;
                        };
                        let state = match &outcome {
                            Outcome::Conflict(FieldConflict::RemoteAccess { .. }) => {
                                SettingEditState::Conflict
                            }
                            Outcome::Failed(_) => {
                                SettingEditState::Rejected(SettingRejection::NotSaved)
                            }
                            Outcome::Applied => SettingEditState::Saving,
                            _ => continue,
                        };
                        (state, Some(outcome), Some(*publication_revision))
                    }
                    _ => continue,
                };
                let mut states = self.state.borrow_mut();
                let state = states.entry(*server).or_default();
                state.remote_edit = Some(RemoteAccessEdit {
                    draft: value.clone(),
                    base_version: *expected_version,
                    state: edit_state,
                });
                state.remote_applied_revision = None;
                if let Some(outcome) = outcome {
                    apply_outcome(state, &outcome, SettingOperation::Remote, revision);
                }
            }
            if let Command::SetWorktreeLocation {
                server, template, ..
            } = request.command()
            {
                let state = match &operation.state {
                    RetainedOperationState::Pending => SettingEditState::Saving,
                    RetainedOperationState::Completed { outcome, .. } => {
                        match outcome.outcome(self.tracker.journal().codec()) {
                            Ok(Outcome::Conflict(FieldConflict::WorktreeLocation { .. })) => {
                                SettingEditState::Conflict
                            }
                            Ok(Outcome::Failed(_)) => {
                                SettingEditState::Rejected(SettingRejection::NotSaved)
                            }
                            _ => continue,
                        }
                    }
                    _ => continue,
                };
                self.state.borrow_mut().entry(*server).or_default().edit = Some(SettingEdit {
                    draft: template.as_str().into(),
                    state,
                });
            }
        }
    }

    fn failure(&self, server: ServerId, failure: ManagementFailure, cx: &mut App) {
        self.state
            .borrow_mut()
            .entry(server)
            .or_default()
            .management
            .failure = Some(failure);
        self.publish(cx);
    }

    fn follow_recovered_tickets(self: &Rc<Self>, cx: &mut App) {
        let Ok(journal) = self.tracker.journal().snapshot() else {
            return;
        };
        for record in journal
            .operations()
            .iter()
            .filter(|record| matches!(record.state, RetainedOperationState::Pending))
        {
            let Ok(request) =
                CanonicalOperationRequest::decode(&record.request, self.tracker.journal().codec())
            else {
                continue;
            };
            if matches!(
                request.command(),
                Command::SetWorktreeLocation { .. }
                    | Command::AssessEndAllSessions { .. }
                    | Command::EndAllSessions { .. }
                    | Command::Pairing(
                        PairingCommand::AssessUnpair { .. }
                            | PairingCommand::UnpairConfirmed { .. }
                    )
                    | Command::SetRemoteAccess { .. }
            ) && let Ok(ticket) = self.tracker.ticket(record.binding.id)
            {
                let server = ticket.id().stream().server();
                self.state.borrow_mut().entry(server).or_default().busy = true;
                self.state
                    .borrow_mut()
                    .entry(server)
                    .or_default()
                    .management
                    .operation = Some(ticket.id());
                // Restored comparisons cannot refresh a destructive confirmation's expiry.
                self.follow_ticket(server, ticket, 0, true, cx);
            }
        }
    }

    fn follow_ticket(
        self: &Rc<Self>,
        server: ServerId,
        ticket: OperationTicket,
        issued: u64,
        restored: bool,
        cx: &mut App,
    ) {
        let mut status = ticket.status();
        let operation = ticket.id();
        let weak = Rc::downgrade(self);
        self.tasks.borrow_mut().push(cx.spawn(async move |cx| {
            loop {
                let observed = status.borrow().clone();
                if matches!(
                    observed,
                    OperationStatus::Completed { .. } | OperationStatus::Retired
                ) {
                    let Some(runtime) = weak.upgrade() else {
                        return;
                    };
                    {
                        let mut states = runtime.state.borrow_mut();
                        let state = states.entry(server).or_default();
                        state.busy = false;
                        match observed {
                            OperationStatus::Completed {
                                outcome,
                                publication_revision,
                            } => apply_received_outcome(
                                state,
                                operation,
                                outcome,
                                issued,
                                restored,
                                operation_setting(&runtime.tracker, operation),
                                Some(publication_revision),
                            ),
                            _ => state.management.failure = Some(ManagementFailure::Indeterminate),
                        }
                    }
                    cx.update(|cx| runtime.publish(cx));
                    return;
                }
                if status.changed().await.is_err() {
                    return;
                }
            }
        }));
    }

    fn begin(self: &Rc<Self>, bound: BoundServerRequest, cx: &mut App) {
        self.tasks.borrow_mut().retain(|task| !task.is_ready());
        if !bound.still_matches(&self.snapshot()) {
            return;
        }
        if let ServerRequestTarget::PendingUnpair { operation, .. } = bound.target {
            let pending = self.pending.clone();
            let future = self
                .executor
                .spawn(async move { pending.dismiss_revocation(operation).await });
            let weak = Rc::downgrade(self);
            self.tasks.borrow_mut().push(cx.spawn(async move |cx| {
                let _ = future.await;
                if let Some(runtime) = weak.upgrade() {
                    cx.update(|cx| runtime.publish(cx));
                }
            }));
            return;
        }
        let Some(key) = bound.request.server() else {
            return;
        };
        let Some(source) = self.connection(key) else {
            return;
        };
        let server = source.name.server;
        if matches!(bound.request, ServerRequest::DiscardWorktreeLocationEdit(_)) {
            self.state.borrow_mut().entry(server).or_default().edit = None;
            self.publish(cx);
            return;
        }
        if matches!(bound.request, ServerRequest::DiscardRemoteAccessEdit(_)) {
            self.state
                .borrow_mut()
                .entry(server)
                .or_default()
                .remote_edit = None;
            self.publish(cx);
            return;
        }
        if self
            .state
            .borrow()
            .get(&server)
            .is_some_and(|state| state.busy)
        {
            self.failure(server, ManagementFailure::OutcomePending, cx);
            return;
        }
        if let ServerRequest::SetWorktreeLocation { template, .. } = &bound.request
            && let Err(error) = crate::worktrees::path_template::validate(template.as_ref())
        {
            self.state.borrow_mut().entry(server).or_default().edit = Some(SettingEdit {
                draft: template.clone(),
                state: SettingEditState::Rejected(SettingRejection::InvalidTemplate(error)),
            });
            self.publish(cx);
            return;
        }
        if let ServerRequestTarget::Server {
            remote_policy: Some((version, policy)),
            ..
        } = &bound.target
        {
            let mut states = self.state.borrow_mut();
            let state = states.entry(server).or_default();
            state.remote_edit = Some(RemoteAccessEdit {
                draft: policy.clone(),
                base_version: *version,
                state: SettingEditState::Saving,
            });
            state.remote_applied_revision = None;
        }
        match &bound.request {
            ServerRequest::SetWorktreeLocation { template, .. } => {
                let mut state = self.state.borrow_mut();
                state.entry(server).or_default().applied_revision = None;
                state.entry(server).or_default().edit = Some(SettingEdit {
                    draft: template.clone(),
                    state: SettingEditState::Saving,
                });
            }
            ServerRequest::UnpairDevice { target, .. }
                if target.parts().pairing == source.binding.target().pairing() => {}
            _ => {}
        }
        self.state.borrow_mut().entry(server).or_default().busy = true;
        self.state
            .borrow_mut()
            .entry(server)
            .or_default()
            .management
            .failure = None;
        self.publish(cx);
        let editing = matches!(bound.request, ServerRequest::SetWorktreeLocation { .. });
        let remote_editing = matches!(
            bound.request,
            ServerRequest::EnableRemoteAccess { .. } | ServerRequest::DisableRemoteAccess(_)
        );
        let binding = source.binding.clone();
        let tracker = self.tracker.clone();
        let peer = self.operations.clone();
        let pending = self.pending.clone();
        let native = source.native.clone();
        let inventory = source.inventory.clone();
        let snapshot = self.snapshot();
        let summary = snapshot
            .servers
            .iter()
            .find(|summary| summary.key == *key)
            .cloned();
        let issued = self.origin.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let task = self.executor.spawn(async move {
            execute(
                bound,
                binding,
                summary.ok_or(ManagementFailure::Revoked)?,
                tracker,
                peer,
                pending,
                native,
                inventory,
                issued,
            )
            .await
        });
        let weak = Rc::downgrade(self);
        self.tasks.borrow_mut().push(cx.spawn(async move |cx| {
            let result = task
                .await
                .unwrap_or(Err(ManagementFailure::NativeUnavailable));
            let Some(runtime) = weak.upgrade() else {
                return;
            };
            {
                let mut states = runtime.state.borrow_mut();
                let state = states.entry(server).or_default();
                state.busy = false;
                match result {
                    Ok(ResultPublication::Operation {
                        ticket,
                        outcome,
                        failure,
                    }) => {
                        state.management.failure = failure;
                        let operation = ticket.id();
                        state.management.operation = Some(operation);
                        if outcome.is_none() {
                            state.busy = true;
                            let runtime = runtime.clone();
                            cx.update(|cx| {
                                runtime.follow_ticket(server, ticket, issued, false, cx)
                            });
                        }
                        if let Some(outcome) = outcome {
                            apply_outcome(
                                state,
                                &outcome,
                                operation_setting(&runtime.tracker, operation),
                                operation_publication_revision(&runtime.tracker, operation),
                            );
                        }
                    }
                    Ok(ResultPublication::Assessment(assessment)) => {
                        state.management.end_all = Some(assessment)
                    }
                    Ok(ResultPublication::UnpairAssessment(assessment)) => {
                        state.unpair = Some(assessment)
                    }
                    Ok(ResultPublication::Devices(page)) => {
                        state.devices = Some(page);
                        state.unpair = None;
                    }
                    Ok(ResultPublication::NativeAssessment(prompt)) => match prompt.action {
                        ServiceAction::Stop => state.management.stop = Some(prompt),
                        ServiceAction::Restart => state.management.restart = Some(prompt),
                        _ => {}
                    },
                    Ok(ResultPublication::Native {
                        request,
                        unresolved,
                        failure,
                    }) => {
                        state.management.native_request = Some(request);
                        state.management.failure = failure;
                        state.busy = unresolved;
                    }
                    Ok(ResultPublication::Removed) => {}
                    Err(failure) => {
                        state.management.failure = Some(failure);
                        if editing && let Some(edit) = &mut state.edit {
                            edit.state = SettingEditState::Rejected(SettingRejection::NotSaved);
                        }
                        if remote_editing && let Some(edit) = &mut state.remote_edit {
                            edit.state = SettingEditState::Rejected(SettingRejection::NotSaved);
                        }
                        state.unpair = None;
                        state.management.end_all = None;
                        state.management.stop = None;
                        state.management.restart = None;
                    }
                }
            }
            cx.update(|cx| runtime.publish(cx));
        }));
    }
}

struct RuntimeRequests(Rc<SettingsRuntime>);
impl ServerRequests for RuntimeRequests {
    fn send(&self, request: BoundServerRequest, cx: &mut App) {
        self.0.begin(request, cx);
    }
}

enum ResultPublication {
    Operation {
        ticket: OperationTicket,
        outcome: Option<Outcome>,
        failure: Option<ManagementFailure>,
    },
    Assessment(ManagementAssessment),
    UnpairAssessment(ManagementAssessment),
    Devices(PairedDevices),
    NativeAssessment(HostAssessment),
    Native {
        request: spaceterm_host::native_deployment::contracts::NativeRequestId,
        unresolved: bool,
        failure: Option<ManagementFailure>,
    },
    Removed,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the operation uses the application's existing owners and an exact captured target"
)]
async fn execute(
    bound: BoundServerRequest,
    binding: PairedServerBinding,
    summary: ServerSummary,
    tracker: OperationTracker,
    peer: Arc<dyn OperationPeer>,
    pending: PendingTasks,
    native: Option<Arc<NativeServiceManagement>>,
    inventory: Option<Arc<dyn SettingsInventory>>,
    issued: u64,
) -> Result<ResultPublication, ManagementFailure> {
    if !tracker
        .journal()
        .ordinary_use_allowed(&binding)
        .map_err(classify)?
    {
        return Err(ManagementFailure::Revoked);
    }
    let server = binding.target().server();
    let command = match bound.request {
        ServerRequest::SetWorktreeLocation {
            template,
            base_revision,
            ..
        } => {
            if !summary.connected() {
                return Err(ManagementFailure::Offline);
            }
            crate::worktrees::path_template::validate(template.as_ref())
                .map_err(|_| ManagementFailure::Usage)?;
            Command::SetWorktreeLocation {
                server,
                expected_version: FieldVersion::new(base_revision),
                template: WorktreeLocationTemplate::new(template.to_string())
                    .map_err(|_| ManagementFailure::Usage)?,
            }
        }
        ServerRequest::AssessEndAllSessions(_) => Command::AssessEndAllSessions { server },
        ServerRequest::EndAllSessions(_) => {
            let assessment = summary
                .management
                .end_all
                .as_ref()
                .ok_or(ManagementFailure::ConfirmationRequired)?;
            check_assessment(&bound.target, assessment)?;
            assessment.validate_at(MonotonicTick::new(issued))?;
            assessment.command()
        }
        ServerRequest::AssessUnpairDevice { target, .. } => {
            Command::Pairing(PairingCommand::AssessUnpair { target })
        }
        ServerRequest::UnpairDevice {
            target, assessment, ..
        } => {
            assessment.validate_at(MonotonicTick::new(issued))?;
            if summary.unpair_assessment.as_ref() != Some(assessment.as_ref()) {
                return Err(ManagementFailure::StaleAssessment);
            }
            if target.parts().pairing == binding.target().pairing() {
                return remove_self(binding, tracker, pending).await;
            }
            assessment.command()
        }
        ServerRequest::UnpairThisDevice(_) => return remove_self(binding, tracker, pending).await,
        ServerRequest::ReadPairedDevices { after, version, .. } => {
            let provider = inventory.ok_or(ManagementFailure::NativeUnavailable)?;
            let page = tokio::time::timeout(
                spaceterm_client::management_cli::bindings::STATUS_READ_DEADLINE,
                provider.page(after, version),
            )
            .await
            .map_err(|_| ManagementFailure::Offline)??;
            check_ordinary(&tracker, &binding)?;
            if !page.valid_for(&summary.pairing)
                || page.this_device != binding.target().device()
                || version.is_some_and(|version| page.version != version)
                || after.is_some_and(|after| {
                    page.devices
                        .first()
                        .is_none_or(|device| device.target.parts().pairing <= after)
                })
                || version.is_some()
                    && summary.paired_devices.as_ref().is_some_and(|previous| {
                        previous.total != page.total || previous.this_device != page.this_device
                    })
            {
                return Err(ManagementFailure::StaleAssessment);
            }
            return Ok(ResultPublication::Devices(page));
        }
        ServerRequest::AssessStop(_) | ServerRequest::AssessRestart(_) => {
            let action = if matches!(bound.request, ServerRequest::AssessStop(_)) {
                ServiceAction::Stop
            } else {
                ServiceAction::Restart
            };
            if !summary.route.manages_host() {
                return Err(ManagementFailure::NativeUnavailable);
            }
            let native = native.ok_or(ManagementFailure::NativeUnavailable)?;
            let native_status = check_native(&native, &binding).await?;
            check_ordinary(&tracker, &binding)?;
            if let Some(request) = native_status.status.pending {
                return Ok(native_result(request, native.resolve(request).await));
            }
            let assessment = native.assess(action).await.map_err(classify_native)?;
            let prompt = NativeActionPrompt::from_assessment(&assessment)?;
            return Ok(ResultPublication::NativeAssessment(HostAssessment {
                reference: prompt.token,
                request: prompt.request,
                action: prompt.action,
                affected_sessions: prompt.affected_sessions,
            }));
        }
        ServerRequest::Start(_) => {
            if !summary.route.manages_host() {
                return Err(ManagementFailure::NativeUnavailable);
            }
            let native = native.ok_or(ManagementFailure::NativeUnavailable)?;
            let native_status = check_native(&native, &binding).await?;
            check_ordinary(&tracker, &binding)?;
            if let Some(request) = native_status.status.pending {
                return Ok(native_result(request, native.resolve(request).await));
            }
            let request = native
                .supervisor()
                .allocate_request()
                .map_err(classify_native)?;
            return Ok(native_result(request, native.start(request).await));
        }
        ServerRequest::Stop(_) | ServerRequest::Restart(_) => {
            let action = if matches!(bound.request, ServerRequest::Stop(_)) {
                ServiceAction::Stop
            } else {
                ServiceAction::Restart
            };
            if !summary.route.manages_host() {
                return Err(ManagementFailure::NativeUnavailable);
            }
            let native = native.ok_or(ManagementFailure::NativeUnavailable)?;
            let native_status = check_native(&native, &binding).await?;
            check_ordinary(&tracker, &binding)?;
            if let Some(request) = native_status.status.pending {
                return Ok(native_result(request, native.resolve(request).await));
            }
            let prompt = if action == ServiceAction::Stop {
                summary.management.stop.as_ref()
            } else {
                summary.management.restart.as_ref()
            }
            .ok_or(ManagementFailure::ConfirmationRequired)?;
            let ServerRequestTarget::Server {
                assessment: Some(reference),
                ..
            } = &bound.target
            else {
                return Err(ManagementFailure::ConfirmationRequired);
            };
            if reference != &prompt.reference || prompt.action != action {
                return Err(ManagementFailure::StaleAssessment);
            }
            let original = native.restore_assessment(action, reference.clone()).await?;
            if original.request() != prompt.request {
                return Err(ManagementFailure::StaleAssessment);
            }
            check_ordinary(&tracker, &binding)?;
            let confirmed = original.confirm().map_err(classify_native)?;
            let progress = if action == ServiceAction::Stop {
                native.stop(confirmed).await
            } else {
                native.restart(confirmed).await
            };
            return Ok(native_result(prompt.request, progress));
        }
        ServerRequest::InstallUpdate { assessment, .. } => {
            if !summary.activation_matches(&assessment) {
                return Err(ManagementFailure::StaleAssessment);
            }
            if !summary.route.manages_host() {
                return Err(ManagementFailure::NativeUnavailable);
            }
            let native = native.ok_or(ManagementFailure::NativeUnavailable)?;
            let native_status = check_native(&native, &binding).await?;
            check_ordinary(&tracker, &binding)?;
            if let Some(request) = native_status.status.pending {
                return Ok(native_result(request, native.resolve(request).await));
            }
            let action = ServiceAction::Activate(assessment.candidate.installation);
            let original = native
                .restore_assessment(action, assessment.reference)
                .await?;
            if original.request() != assessment.native_request {
                return Err(ManagementFailure::StaleAssessment);
            }
            check_ordinary(&tracker, &binding)?;
            let progress = native
                .supervisor()
                .activate(original.confirm().map_err(classify_native)?)
                .await;
            return Ok(native_result(assessment.native_request, progress));
        }
        ServerRequest::DisableRemoteAccess(_) | ServerRequest::EnableRemoteAccess { .. } => {
            let ServerRequestTarget::Server {
                remote_policy: Some((version, policy)),
                ..
            } = &bound.target
            else {
                return Err(ManagementFailure::NativeUnavailable);
            };
            Command::SetRemoteAccess {
                server,
                expected_version: *version,
                value: policy.clone(),
            }
        }
        ServerRequest::DiscardWorktreeLocationEdit(_)
        | ServerRequest::DiscardRemoteAccessEdit(_)
        | ServerRequest::DismissUnpairNotice(_) => {
            return Err(ManagementFailure::Usage);
        }
    };
    if !summary.connected() {
        return Err(ManagementFailure::Offline);
    }
    let ticket = tracker
        .submit(&binding, command, None, peer.as_ref())
        .await
        .map_err(classify)?;
    let operation = ticket.id();
    let failure = if let Ok(lookup) = peer.lookup(ticket.binding()).await {
        tracker
            .resolve_bound(ticket.binding(), lookup, peer.as_ref())
            .await
            .err()
            .map(classify)
    } else {
        None
    };
    let outcome = match &*ticket.status().borrow() {
        OperationStatus::Completed { outcome, .. } => Some(outcome.clone()),
        _ => None,
    };
    match outcome {
        Some(Outcome::Assessment(assessment))
            if matches!(
                assessment.summary().target(),
                spaceterm_core::operations::AssessmentTarget::EndAllSessions(_)
            ) =>
        {
            let value = assessment_value(
                operation,
                ManagementAction::EndAllSessions { server },
                AssessmentEvidence::EndAllSessions(*assessment),
                issued,
            )?;
            Ok(ResultPublication::Assessment(value))
        }
        Some(Outcome::Pairing(outcome)) => match *outcome {
            PairingOutcome::UnpairAssessed(assessment) => {
                let target = assessment.parts().target.clone();
                Ok(ResultPublication::UnpairAssessment(assessment_value(
                    operation,
                    ManagementAction::Unpair { target },
                    AssessmentEvidence::Unpair(assessment),
                    issued,
                )?))
            }
            other => Ok(ResultPublication::Operation {
                ticket,
                outcome: Some(Outcome::Pairing(Box::new(other))),
                failure,
            }),
        },
        outcome => Ok(ResultPublication::Operation {
            ticket,
            outcome,
            failure,
        }),
    }
}

fn check_assessment(
    target: &ServerRequestTarget,
    assessment: &ManagementAssessment,
) -> Result<(), ManagementFailure> {
    match target {
        ServerRequestTarget::Server {
            assessment: Some(token),
            runtime_epoch,
            ..
        } if token == assessment.token() && *runtime_epoch == assessment.epoch() => Ok(()),
        _ => Err(ManagementFailure::StaleAssessment),
    }
}

fn check_ordinary(
    tracker: &OperationTracker,
    binding: &PairedServerBinding,
) -> Result<(), ManagementFailure> {
    if !tracker
        .journal()
        .ordinary_use_allowed(binding)
        .map_err(classify)?
    {
        return Err(ManagementFailure::Revoked);
    }
    Ok(())
}

async fn check_native(
    native: &NativeServiceManagement,
    binding: &PairedServerBinding,
) -> Result<NativeRead, ManagementFailure> {
    let status = native.status().await.map_err(classify_native)?;
    let registration = native
        .supervisor()
        .discover()
        .map_err(classify_native)?
        .ok_or(ManagementFailure::AuthorizationDenied)?;
    if status.key != native.key()
        || status.server != Some(binding.target().server())
        || registration.server_pin() != binding.target().pin()
    {
        return Err(ManagementFailure::AuthorizationDenied);
    }
    if status.desired != Some(registration.desired_state())
        || status.approved.as_ref() != registration.approved_installation()
        || status.staged.as_ref() != registration.staged_installation()
    {
        return Err(ManagementFailure::StaleAssessment);
    }
    let generation = std::num::NonZeroU64::new(registration.generation())
        .ok_or(ManagementFailure::UnsafeNativeState)?;
    Ok(NativeRead {
        desired: registration.desired_state(),
        status,
        generation,
        recovered: None,
    })
}

async fn recover_native(
    native: &NativeServiceManagement,
    binding: &PairedServerBinding,
) -> Result<NativeRead, ManagementFailure> {
    let mut read = check_native(native, binding).await?;
    let originals = native
        .supervisor()
        .retained_service_requests()
        .map_err(classify_native)?;
    let request = read
        .status
        .pending
        .or_else(|| originals.last().map(|progress| progress.request));
    if let Some(request) = request {
        let progress = native.resolve(request).await;
        read = check_native(native, binding).await?;
        read.recovered = Some(native_result(request, progress));
    }
    Ok(read)
}

fn installation(
    status: &spaceterm_host::account_service::InstallationStatus,
) -> ServerInstallation {
    ServerInstallation {
        installation: status.installation,
        artifact_sha256: status.artifact_sha256,
        version: status.version.as_str().into(),
    }
}

async fn remove_self(
    binding: PairedServerBinding,
    tracker: OperationTracker,
    pending: PendingTasks,
) -> Result<ResultPublication, ManagementFailure> {
    let journal = tracker.journal().snapshot().map_err(classify)?;
    let saved = journal
        .bindings()
        .iter()
        .find(|saved| saved.binding().is_ok_and(|saved| saved == binding))
        .ok_or(ManagementFailure::Revoked)?;
    let target = binding.target();
    let route = journal
        .pairing()
        .slot(target.credential())
        .and_then(|slot| slot.route_reference)
        .ok_or(ManagementFailure::CredentialUnavailable)?;
    let operation = OperationId::new(
        saved.streams().parts().self_revocation,
        OperationSequence::new(1).map_err(|_| ManagementFailure::UnsafeNativeState)?,
    );
    let pairing = target.local_pairing().map_err(classify)?;
    let exact = PairingTarget::new(spaceterm_core::pairing::PairingTargetParts {
        server: target.server(),
        pairing: target.pairing(),
        device: target.device(),
    })
    .map_err(|_| ManagementFailure::UnsafeNativeState)?;
    let request = CanonicalOperationRequest::new(
        operation,
        Command::Pairing(PairingCommand::SelfRevoke { target: exact }),
        tracker.journal().codec(),
    )
    .map_err(|_| ManagementFailure::UnsafeNativeState)?;
    let original = PendingSelfRevocation::new(PendingParts {
        original_server: target.server(),
        original_pairing: target.pairing(),
        device: target.device(),
        client: target.client(),
        operation,
        route,
        credential: target.credential(),
        notice_dismissed: false,
    })
    .map_err(|_| ManagementFailure::UnsafeNativeState)?;
    let task = PendingRevocationTask::new(PendingRevocationTaskParts {
        original,
        local_pairing: pairing,
        pinned_server_public_key: target.pin().public_key,
        request_digest: request.binding().digest,
    })
    .map_err(|_| ManagementFailure::UnsafeNativeState)?;
    pending
        .record_self_revocation(&binding, task)
        .await
        .map_err(classify)?;
    Ok(ResultPublication::Removed)
}

fn assessment_value(
    operation: OperationId,
    action: ManagementAction,
    evidence: AssessmentEvidence,
    issued: u64,
) -> Result<ManagementAssessment, ManagementFailure> {
    ManagementAssessment::new(
        AssessmentToken::new(format!("settings_{}", notice_key(operation)))?,
        action,
        evidence,
        MonotonicTick::new(issued),
        MonotonicTick::new(issued.saturating_add(30_000)),
    )
}

fn native_result(
    request: spaceterm_host::native_deployment::contracts::NativeRequestId,
    progress: Result<
        spaceterm_host::account_service::ServiceProgress,
        spaceterm_host::account_service::ServiceFailure,
    >,
) -> ResultPublication {
    match progress {
        Ok(progress) if progress.request == request => ResultPublication::Native {
            request,
            unresolved: progress.phase != spaceterm_host::account_service::ServicePhase::Completed,
            failure: match progress.state {
                spaceterm_host::account_service::ServiceRequestState::NotApplied(failure) => {
                    Some(classify_native(failure))
                }
                spaceterm_host::account_service::ServiceRequestState::Indeterminate => {
                    Some(ManagementFailure::Indeterminate)
                }
                _ => None,
            },
        },
        Ok(_) => ResultPublication::Native {
            request,
            unresolved: true,
            failure: Some(ManagementFailure::UnsafeNativeState),
        },
        Err(failure) => ResultPublication::Native {
            request,
            unresolved: true,
            failure: Some(classify_native(failure)),
        },
    }
}

fn apply_received_outcome(
    state: &mut ServerState,
    operation: OperationId,
    outcome: Outcome,
    issued: u64,
    restored: bool,
    editing: SettingOperation,
    revision: Option<spaceterm_core::authority::OrganizationRevision>,
) {
    if restored
        && (matches!(&outcome, Outcome::Assessment(_))
            || matches!(&outcome, Outcome::Pairing(value) if matches!(value.as_ref(), PairingOutcome::UnpairAssessed(_))))
    {
        state.management.failure = Some(ManagementFailure::StaleAssessment);
        return;
    }
    let assessed = match outcome {
        Outcome::Assessment(assessment)
            if matches!(
                assessment.summary().target(),
                spaceterm_core::operations::AssessmentTarget::EndAllSessions(_)
            ) =>
        {
            assessment_value(
                operation,
                ManagementAction::EndAllSessions {
                    server: operation.stream().server(),
                },
                AssessmentEvidence::EndAllSessions(*assessment),
                issued,
            )
            .map(|assessment| {
                state.management.end_all = Some(assessment);
            })
        }
        Outcome::Pairing(outcome) => match *outcome {
            PairingOutcome::UnpairAssessed(assessment) => {
                let target = assessment.parts().target.clone();
                assessment_value(
                    operation,
                    ManagementAction::Unpair { target },
                    AssessmentEvidence::Unpair(assessment),
                    issued,
                )
                .map(|assessment| {
                    state.unpair = Some(assessment);
                })
            }
            _ => Ok(()),
        },
        outcome => {
            apply_outcome(state, &outcome, editing, revision);
            Ok(())
        }
    };
    if let Err(failure) = assessed {
        state.management.failure = Some(failure);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SettingOperation {
    Other,
    Worktree,
    Remote,
}

fn operation_setting(tracker: &OperationTracker, operation: OperationId) -> SettingOperation {
    tracker
        .journal()
        .snapshot()
        .ok()
        .and_then(|journal| journal.operation(operation).cloned())
        .and_then(|record| {
            CanonicalOperationRequest::decode(&record.request, tracker.journal().codec()).ok()
        })
        .map_or(SettingOperation::Other, |request| match request.command() {
            Command::SetWorktreeLocation { .. } => SettingOperation::Worktree,
            Command::SetRemoteAccess { .. } => SettingOperation::Remote,
            _ => SettingOperation::Other,
        })
}

fn operation_publication_revision(
    tracker: &OperationTracker,
    operation: OperationId,
) -> Option<spaceterm_core::authority::OrganizationRevision> {
    let journal = tracker.journal().snapshot().ok()?;
    match &journal.operation(operation)?.state {
        RetainedOperationState::Completed {
            publication_revision,
            ..
        } => Some(*publication_revision),
        _ => None,
    }
}

fn apply_outcome(
    state: &mut ServerState,
    outcome: &Outcome,
    editing: SettingOperation,
    revision: Option<spaceterm_core::authority::OrganizationRevision>,
) {
    if editing == SettingOperation::Remote {
        match outcome {
            Outcome::Applied => state.remote_applied_revision = revision,
            Outcome::Conflict(FieldConflict::RemoteAccess {
                current_value,
                current_version,
                ..
            }) => {
                state.remote_conflict = Some((current_value.clone(), *current_version));
                if let Some(edit) = &mut state.remote_edit {
                    edit.state = SettingEditState::Conflict;
                }
            }
            Outcome::Failed(_) => {
                if let Some(edit) = &mut state.remote_edit {
                    edit.state = SettingEditState::Rejected(SettingRejection::NotSaved);
                }
            }
            _ => {}
        }
        return;
    }
    if editing == SettingOperation::Other {
        if let Outcome::Failed(failure) = outcome {
            state.management.failure = Some(match failure {
                OperationFailure::StaleAssessment
                | OperationFailure::AssessmentExpired
                | OperationFailure::StaleAuthority => ManagementFailure::StaleAssessment,
                OperationFailure::Capacity | OperationFailure::Exhausted => {
                    ManagementFailure::LimitExceeded
                }
                OperationFailure::StorageUnavailable => ManagementFailure::StorageUnavailable,
                _ => ManagementFailure::NativeUnavailable,
            });
        }
        return;
    }
    match outcome {
        Outcome::Applied => {
            state.applied_revision = revision;
            state.conflict = None;
        }
        Outcome::Conflict(FieldConflict::WorktreeLocation {
            current_value,
            current_version,
            ..
        }) => {
            state.conflict = Some((current_value.as_str().into(), current_version.get()));
            if let Some(edit) = &mut state.edit {
                edit.state = SettingEditState::Conflict;
            }
        }
        Outcome::Failed(failure) => {
            if let Some(edit) = &mut state.edit {
                edit.state = SettingEditState::Rejected(
                    if *failure == OperationFailure::DirectoryUnavailable {
                        SettingRejection::Unwritable
                    } else {
                        SettingRejection::NotSaved
                    },
                );
            }
        }
        _ => {}
    }
}

fn route(source: &SettingsConnection) -> ServerRoute {
    match source.binding.target().route().parts() {
        RouteParts::Ipc { .. } => ServerRoute::Local,
        RouteParts::Ssh { destination, .. } => ServerRoute::Ssh {
            destination: destination.clone().into(),
        },
        RouteParts::Wss { address, .. } => ServerRoute::Wss {
            authority: address.to_string().into(),
        },
    }
}
fn server_key(server: ServerId) -> ServerKey {
    ServerKey::new(
        server
            .bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )
}
fn notice_key(operation: OperationId) -> SharedString {
    format!(
        "{}_{}_{}_{}",
        server_key(operation.stream().server()).0,
        operation.stream().pairing().local(),
        operation.stream().key().local(),
        operation.sequence().get()
    )
    .into()
}
fn classify(failure: ClientFailure) -> ManagementFailure {
    match failure.class() {
        ClientFailureClass::AuthorizationDenied | ClientFailureClass::PinMismatch => {
            ManagementFailure::AuthorizationDenied
        }
        ClientFailureClass::Incompatible => ManagementFailure::Incompatible,
        ClientFailureClass::MissingCapability => ManagementFailure::NativeUnavailable,
        ClientFailureClass::ResourceLimit | ClientFailureClass::CounterExhausted => {
            ManagementFailure::LimitExceeded
        }
        ClientFailureClass::InvalidStorage
        | ClientFailureClass::UnknownSchema
        | ClientFailureClass::UnsafeStorage
        | ClientFailureClass::UnsafeEndpoint
        | ClientFailureClass::InvalidPeer => ManagementFailure::UnsafeNativeState,
        ClientFailureClass::StaleAuthority => ManagementFailure::StaleAssessment,
        ClientFailureClass::StorageUnavailable => ManagementFailure::StorageUnavailable,
        ClientFailureClass::RecoveryRequired => ManagementFailure::Indeterminate,
        _ => ManagementFailure::Offline,
    }
}

#[cfg(test)]
#[path = "server_runtime_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../../platform/server_settings_services_tests.rs"]
mod native_tests;
