//! Disposable Pane presentation handles. Neither drop nor connection loss releases control.

mod effects;
mod events;
pub use effects::{EffectAdmission, EffectCorrelation, PaneRuntimeEffect};
mod reads;
pub use crate::projection::FrameLease;
pub use events::{NativeRequestLease, NativeTiming, PaneEvent, PaneEvents};
pub use reads::{PaneReadItem, PaneReadRequest, PaneReadSource, PaneReadStream};
pub use spaceterm_core::control::StartupSource as ExplicitUseSource;
pub use spaceterm_terminal::runtime_contracts::{
    FencedRuntimeCommand, RuntimeCommand as RuntimeEffect, RuntimeMetadataSnapshot,
};

use crate::{
    api::ConnectedInfo,
    failure::{ClientFailure, ClientFailureClass, exhausted, invalid_peer, resource_limit},
    operations::OperationTicket,
    ports::{ClientFuture, ConnectionInvalidation},
    projection::{ProjectionBudgets, TerminalProjection, stale},
    replica::ReplicaSnapshot,
    view::AttachmentIncarnation,
};
use spaceterm_core::{
    authority::*,
    content::ContentBase,
    content_queries::{ContentRequestId, QueryStamp, RequestGeneration},
    control::{MeasuredStartupMeasurement, StartupIntent, TakeOverRequest},
    geometry::MeasuredGrid,
    identity::{PaneId, ServerId, TabId, WorkspaceId},
    native_requests::{NativeCompletion, NativeRequest},
    organization::{ControlRecord, PaneRuntimeRecord, SessionPhase},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::{Semaphore, watch};

/// The authenticated worker owns codecs, operation tracking and final dequeue.
/// No native or production fallback is provided here. It must retain the exact
/// permit with queued effects, rechecking it immediately before transmission.
/// Correlated request numbers are nonzero and monotonic per connection. Reply
/// delivery must not call back synchronously from an admission method.
pub trait PaneConnectionPort: Send + Sync {
    /// Wake the connection-owned presentation worker after local demand changes.
    fn presentation_changed(&self) {}

    fn grid_limits(
        &self,
        info: ConnectedInfo,
    ) -> Result<spaceterm_core::geometry::GridLimits, ClientFailure>;
    fn explicit_use(&self, request: PaneUseRequest) -> ClientFuture<'_, PaneUseReply>;
    fn complete_intent(
        &self,
        workspace: WorkspaceId,
        measurement: MeasuredStartupMeasurement,
    ) -> ClientFuture<'_, OperationTicket>;
    fn cancel_intent(&self, intent: StartupIntent);
    fn take_over(
        &self,
        workspace: WorkspaceId,
        request: TakeOverRequest,
    ) -> ClientFuture<'_, OperationTicket>;
    fn try_effect(
        &self,
        permit: EffectPermit,
        command: FencedRuntimeCommand,
    ) -> Result<EffectAdmission, ClientFailure>;
    /// No ordinary key fallback is permitted. The provider must preserve the
    /// enhanced-only condition until the owning runtime encodes the chord.
    fn try_copy(
        &self,
        _permit: EffectPermit,
        _modifiers: spaceterm_terminal::InputModifiers,
    ) -> Result<EffectAdmission, ClientFailure> {
        Err(ClientFailure::new(ClientFailureClass::MissingCapability))
    }
    fn read(
        &self,
        permit: EffectPermit,
        request: PaneReadRequest,
    ) -> ClientFuture<'_, Box<dyn PaneReadSource>>;
}
#[derive(Clone, Copy, Debug)]
pub enum PaneUseReply {
    AwaitingMeasurement(StartupIntent),
    ControlReady(GrantedControl),
}
#[derive(Clone)]
pub enum ExplicitUseOutcome {
    AwaitingMeasurement(Box<IntentToken>),
    ControlReady(GrantedControl),
}
impl ExplicitUseOutcome {
    pub fn startup(self) -> Option<IntentToken> {
        match self {
            Self::AwaitingMeasurement(token) => Some(*token),
            Self::ControlReady(_) => None,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct PaneUseRequest {
    pub workspace: WorkspaceId,
    pub tab: TabId,
    pub runtime: PaneRuntimeRecord,
    pub info: ConnectedInfo,
    pub context: ContextGeneration,
    pub source: ExplicitUseSource,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectKind {
    Key,
    ImeCommit,
    Pointer,
    Resize,
    FocusReport,
    Appearance,
    Paste,
    Read,
    NativeRead,
    NativeCompletion,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PaneContextFacts {
    pub focused_pane: bool,
    pub terminal_input_focus: bool,
    pub presentable: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneReadiness {
    Reconciling,
    Live,
    Offline,
    Takeover,
    Detached,
    Missing,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneChannelState {
    pub readiness: PaneReadiness,
    pub runtime: Option<PaneRuntimeRecord>,
    pub connection: Option<ConnectedInfo>,
    pub context: ContextGeneration,
    pub facts: PaneContextFacts,
    pub content: Option<ContentBase>,
}
#[derive(Clone, Copy, Eq, PartialEq)]
struct PaneAuthority {
    server: ServerId,
    pairing: spaceterm_core::identity::PairingId,
    client: spaceterm_core::identity::ClientId,
    incarnation: Option<spaceterm_core::pairing::LocalPairingGeneration>,
}
impl From<ConnectedInfo> for PaneAuthority {
    fn from(info: ConnectedInfo) -> Self {
        Self {
            server: info.server,
            pairing: info.pairing,
            client: info.client,
            incarnation: None,
        }
    }
}
struct ChannelInner {
    authority: PaneAuthority,
    pane: PaneId,
    tab: TabId,
    lease: Weak<LeaseInner>,
    shared: Arc<Shared>,
    state: Mutex<ChannelState>,
    published: watch::Sender<PaneChannelState>,
    projection: Arc<Mutex<TerminalProjection>>,
    effect_admission: Mutex<()>,
    effect_slots: Arc<Semaphore>,
    _slot: tokio::sync::OwnedSemaphorePermit,
}
#[derive(Clone, Copy)]
struct AuthorizedIntent {
    intent: StartupIntent,
    attempt: u64,
}
struct ChannelState {
    facts: PaneChannelState,
    content_confirmed: bool,
    intent: Option<AuthorizedIntent>,
    intent_pending: Option<Arc<()>>,
    events: Option<events::Subscription>,
    native_high_water: u64,
    native_session: Option<SessionKey>,
    native: BTreeMap<u64, events::PendingNative>,
    metadata: Option<Arc<RuntimeMetadataSnapshot>>,
    effects: BTreeMap<u64, effects::PendingEffect>,
    effect_high_water: u64,
}
struct LeaseInner {
    workspace: WorkspaceId,
    incarnation: AttachmentIncarnation,
    active: AtomicBool,
    channels: Mutex<BTreeMap<PaneId, Weak<ChannelInner>>>,
    shared: Arc<Shared>,
}
struct ServerState {
    active: bool,
    info: ConnectedInfo,
    snapshot: Arc<ReplicaSnapshot>,
    transfers: Arc<Semaphore>,
    requests: Arc<AtomicU64>,
}
struct RetainedServer {
    authority: PaneAuthority,
    snapshot: Arc<ReplicaSnapshot>,
}
struct Shared {
    admission: Mutex<Option<crate::journal::PaneJournalAdmission>>,
    peer: Arc<dyn PaneConnectionPort>,
    budgets: ProjectionBudgets,
    native_residency: Arc<crate::projection::budget::Residency>,
    slots: Arc<Semaphore>,
    // One checked sequence keeps each connection/Pane monotonic across disposable
    // channels without retaining comparison entries for deleted Panes.
    context_high_water: AtomicU64,
    servers: Mutex<BTreeMap<ServerId, ServerState>>,
    retained: Mutex<BTreeMap<ServerId, RetainedServer>>,
    leases: Mutex<BTreeMap<(WorkspaceId, AttachmentIncarnation), Weak<LeaseInner>>>,
}
impl Shared {
    fn admitted_identity(&self, authority: PaneAuthority) -> bool {
        self.admission.lock().is_ok_and(|gate| {
            gate.as_ref().is_none_or(|gate| {
                gate.allows_identity(
                    authority.server,
                    authority.pairing,
                    authority.client,
                    authority.incarnation,
                )
            })
        })
    }
    fn admitted_binding(&self, binding: &crate::api::PairedServerBinding) -> bool {
        let Ok(gate) = self.admission.lock().map(|gate| gate.clone()) else {
            return false;
        };
        gate.as_ref()
            .is_none_or(|gate| gate.allows_binding(binding))
    }
    fn publication_admitted(&self, authority: PaneAuthority) -> bool {
        let Ok(gate) = self.admission.lock().map(|gate| gate.clone()) else {
            return false;
        };
        gate.as_ref().is_none_or(|gate| {
            gate.allows_identity_publication(
                authority.server,
                authority.pairing,
                authority.client,
                authority.incarnation,
            )
        })
    }
    fn next_context(&self) -> Result<ContextGeneration, ClientFailure> {
        self.context_high_water
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map(|previous| ContextGeneration::new(previous + 1))
            .map_err(|_| exhausted())
    }
}
#[derive(Clone)]
pub struct PaneChannels {
    shared: Arc<Shared>,
}
impl PaneChannels {
    pub fn new(
        peer: Arc<dyn PaneConnectionPort>,
        budgets: ProjectionBudgets,
    ) -> Result<Self, ClientFailure> {
        budgets.validate()?;
        Ok(Self {
            shared: Arc::new(Shared {
                admission: Mutex::new(None),
                peer,
                budgets,
                native_residency: crate::projection::budget::Residency::new(
                    spaceterm_protocol::limits::MAX_EGRESS_BYTES as u64,
                    spaceterm_protocol::limits::MAX_EGRESS_BYTES as u64,
                )?,
                slots: Arc::new(Semaphore::new(4096)),
                context_high_water: AtomicU64::new(1),
                servers: Mutex::new(BTreeMap::new()),
                retained: Mutex::new(BTreeMap::new()),
                leases: Mutex::new(BTreeMap::new()),
            }),
        })
    }
    pub(crate) fn bind_journal_admission(
        &self,
        gate: crate::journal::PaneJournalAdmission,
    ) -> Result<(), ClientFailure> {
        let mut owner = self.shared.admission.lock().map_err(|_| stale())?;
        if owner.is_some() {
            return Err(resource_limit());
        }
        *owner = Some(gate);
        Ok(())
    }
    /// Only the local attachment owner calls this after its suppression gate.
    /// Server publication can never create an attachment or its incarnation.
    pub fn attach(
        &self,
        workspace: WorkspaceId,
        incarnation: AttachmentIncarnation,
    ) -> Result<WorkspaceLease, ClientFailure> {
        let mut leases = self.shared.leases.lock().map_err(|_| stale())?;
        leases.retain(|_, lease| lease.strong_count() != 0);
        if let Some(lease) = leases
            .get(&(workspace, incarnation))
            .and_then(Weak::upgrade)
        {
            return Ok(WorkspaceLease(lease));
        }
        if leases.len() >= 4096 {
            return Err(resource_limit());
        }
        let lease = Arc::new(LeaseInner {
            workspace,
            incarnation,
            active: AtomicBool::new(true),
            channels: Mutex::new(BTreeMap::new()),
            shared: Arc::clone(&self.shared),
        });
        leases.insert((workspace, incarnation), Arc::downgrade(&lease));
        Ok(WorkspaceLease(lease))
    }
    /// Materialize readonly membership from an exact saved pairing before a cold reconnect.
    /// This retains the replica's residency charge and creates no connection, grant or intent.
    pub fn retain(
        &self,
        binding: &crate::api::PairedServerBinding,
        snapshot: Arc<ReplicaSnapshot>,
    ) -> Result<(), ClientFailure> {
        let target = binding.target();
        let authority = PaneAuthority {
            server: target.server(),
            pairing: target.pairing(),
            client: target.client(),
            incarnation: Some(target.incarnation()),
        };
        if snapshot.cursor.server != authority.server || !self.shared.admitted_binding(binding) {
            return Err(stale());
        }
        let servers = self.shared.servers.lock().map_err(|_| stale())?;
        if servers.contains_key(&authority.server) {
            return Ok(());
        }
        let mut retained = self.shared.retained.lock().map_err(|_| stale())?;
        if retained.len() >= crate::limits::ClientLimits::default().saved_servers
            && !retained.contains_key(&authority.server)
        {
            return Err(resource_limit());
        }
        retained.insert(
            authority.server,
            RetainedServer {
                authority,
                snapshot,
            },
        );
        Ok(())
    }
    /// Called only with the authenticated, reconciled replica. The allocation is
    /// retained by Arc, keeping the replica owner's residency charge intact.
    pub fn publish(
        &self,
        info: ConnectedInfo,
        snapshot: Arc<ReplicaSnapshot>,
    ) -> Result<(), ClientFailure> {
        if !self.shared.publication_admitted(info.into()) {
            return Err(stale());
        }
        if snapshot.cursor.server != info.server
            || snapshot.cursor.epoch != info.epoch
            || snapshot.cursor.revision != info.organization_revision
            || info.binding.client != info.client
        {
            return Err(invalid_peer());
        }
        {
            let mut servers = self.shared.servers.lock().map_err(|_| stale())?;
            let previous = servers.get(&info.server);
            if previous.is_some_and(|old| {
                old.info.attempt > info.attempt
                    || (old.info.attempt == info.attempt
                        && old.info.organization_revision > info.organization_revision)
            }) {
                return Err(stale());
            }
            let same = previous.filter(|old| {
                old.info.binding == info.binding
                    && old.info.attempt == info.attempt
                    && old.info.epoch == info.epoch
            });
            let transfers = same
                .map(|old| Arc::clone(&old.transfers))
                .unwrap_or_else(|| {
                    Arc::new(Semaphore::new(
                        info.limits
                            .concurrent_transfers
                            .min(spaceterm_protocol::limits::MAX_CONCURRENT_TRANSFERS),
                    ))
                });
            let requests = same
                .map(|old| Arc::clone(&old.requests))
                .unwrap_or_else(|| Arc::new(AtomicU64::new(0)));
            servers.insert(
                info.server,
                ServerState {
                    active: true,
                    info,
                    snapshot: Arc::clone(&snapshot),
                    transfers,
                    requests,
                },
            );
        }
        self.shared
            .retained
            .lock()
            .map_err(|_| stale())?
            .remove(&info.server);
        for channel in self.channels(info.server)? {
            // Cold channels retain their exact pairing generation across reconnect.
            if !self.shared.publication_admitted(channel.authority) {
                return Err(stale());
            }
            channel.apply(info, &snapshot)?;
        }
        self.shared.peer.presentation_changed();
        Ok(())
    }
    /// Routes display metadata only to existing live attachments. A remote
    /// publication cannot create a channel, attachment or local authority.
    /// Delayed publications for retired fences or source sequences are ignored.
    pub fn receive_metadata(
        &self,
        fence: EffectFence,
        snapshot: RuntimeMetadataSnapshot,
    ) -> Result<bool, ClientFailure> {
        let mut delivered = false;
        for inner in self.channels(fence.session().pane.server())? {
            if inner.pane != fence.session().pane {
                continue;
            }
            let Some(lease) = inner.lease.upgrade() else {
                continue;
            };
            let channel = PaneChannel { inner, lease };
            match channel.publish_metadata(fence, snapshot.clone()) {
                Ok(()) => delivered = true,
                Err(error) if error.class() == ClientFailureClass::StaleAuthority => {}
                Err(error) => return Err(error),
            }
        }
        Ok(delivered)
    }
    /// The attachment owner invalidates and captures grants under the exact
    /// incarnation. A delayed task cannot touch a later live attachment.
    pub fn invalidate_attachment(
        &self,
        pairing: &spaceterm_core::pairing::LocalPairingKey,
        attachment: crate::view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        if pairing.parts().pairing != attachment.pairing()
            || pairing.parts().server != attachment.workspace().server()
        {
            return Err(stale());
        }
        let leases = self.shared.leases.lock().map_err(|_| stale())?;
        let current = leases
            .iter()
            .filter(|((workspace, _), _)| *workspace == attachment.workspace())
            .filter_map(|(_, lease)| lease.upgrade())
            .filter(|lease| lease.active.load(Ordering::Acquire))
            .max_by_key(|lease| lease.incarnation)
            .ok_or_else(stale)?;
        if current.incarnation != attachment.incarnation() {
            return Err(stale());
        }
        let channels: Vec<_> = current
            .channels
            .lock()
            .map_err(|_| stale())?
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        if channels.iter().any(|channel| {
            channel.authority.pairing != attachment.pairing()
                || channel.authority.client != attachment.client()
        }) {
            return Err(stale());
        }
        current.active.store(false, Ordering::Release);
        let mut grants = Vec::new();
        for channel in channels {
            let state = channel.state.lock().map_err(|_| stale())?;
            if let Some(runtime) = state.facts.runtime
                && let Some(info) = state.facts.connection
                && let ControlRecord::Held { client, generation } = runtime.control
                && client == attachment.client()
            {
                grants.push(
                    GrantedControl::new(
                        info.epoch,
                        ControlGrant::new(channel.pane, client, generation)
                            .map_err(|_| invalid_peer())?,
                    )
                    .map_err(|_| invalid_peer())?,
                );
            }
        }
        WorkspaceLease(current).invalidate()?;
        Ok(grants)
    }
    pub fn invalidate(
        &self,
        server: ServerId,
        reason: ConnectionInvalidation,
    ) -> Result<(), ClientFailure> {
        {
            let mut servers = self.shared.servers.lock().map_err(|_| stale())?;
            if matches!(
                reason,
                ConnectionInvalidation::Shutdown | ConnectionInvalidation::BindingRemoved
            ) {
                servers.remove(&server);
                self.shared
                    .retained
                    .lock()
                    .map_err(|_| stale())?
                    .remove(&server);
            } else if let Some(retained) = servers.get_mut(&server) {
                // Retained membership serves personal navigation only. It grants no
                // connection, content permit, startup intent or runtime effect.
                retained.active = false;
            }
        }
        for channel in self.channels(server)? {
            let mut state = channel.state.lock().map_err(|_| stale())?;
            // Pending and authorized intents are comparison state, not effect
            // authority. Publication disables admission until their fences are
            // revalidated against the installed graph and connection attempt.
            if reason != ConnectionInvalidation::Publication {
                state.intent = None;
                state.intent_pending = None;
            }
            state.native.clear();
            state.effects.clear();
            state.facts.connection = None;
            state.content_confirmed = false;
            let freeze = matches!(
                reason,
                ConnectionInvalidation::ConnectionLost
                    | ConnectionInvalidation::Reconciliation
                    | ConnectionInvalidation::Publication
            );
            let mut projection = channel.projection.lock().map_err(|_| stale())?;
            if freeze && state.facts.readiness != PaneReadiness::Takeover {
                projection.freeze()?;
                state.facts.readiness = PaneReadiness::Offline;
            } else {
                projection.revoke()?;
                if state.facts.readiness != PaneReadiness::Takeover {
                    state.facts.readiness = PaneReadiness::Reconciling;
                }
                state.facts.content = None;
                state.metadata = None;
            }
            drop(projection);
            events::publish(&mut state, PaneEvent::Invalidated, true);
            channel.published.send_replace(state.facts);
        }
        Ok(())
    }
    /// Disposable local presentation demand. The caller must not retain these
    /// handles between worker turns because they keep the attachment alive.
    pub fn presentation_channels(
        &self,
        server: ServerId,
    ) -> Result<Vec<PaneChannel>, ClientFailure> {
        Ok(self
            .channels(server)?
            .into_iter()
            .filter_map(|inner| {
                let lease = inner.lease.upgrade()?;
                lease
                    .active
                    .load(Ordering::Acquire)
                    .then_some(PaneChannel { inner, lease })
            })
            .collect())
    }
    /// All Panes on one authenticated connection use its acknowledged context.
    /// Moving the connection context invalidates earlier input and native work.
    pub fn align_connection_context(
        &self,
        server: ServerId,
        context: ContextGeneration,
    ) -> Result<(), ClientFailure> {
        if context.get() > self.shared.context_high_water.load(Ordering::Acquire) {
            return Err(stale());
        }
        for channel in self.channels(server)? {
            let mut state = channel.state.lock().map_err(|_| stale())?;
            if state.facts.context >= context {
                continue;
            }
            state.facts.context = context;
            state.intent = None;
            state.intent_pending = None;
            state.native.clear();
            state.effects.clear();
            channel.update_projection(&state)?;
            events::refresh_metadata(&mut state);
            channel.published.send_replace(state.facts);
        }
        Ok(())
    }
    /// Allocate a local generation when the focused Pane disappears.
    pub fn next_connection_context(&self) -> Result<ContextGeneration, ClientFailure> {
        self.shared.next_context()
    }
    #[cfg(feature = "test-support")]
    pub(crate) fn contexts(
        &self,
        server: ServerId,
    ) -> Result<Vec<(PaneId, ContextGeneration)>, ClientFailure> {
        self.channels(server)?
            .iter()
            .filter(|channel| {
                channel
                    .lease
                    .upgrade()
                    .is_some_and(|lease| lease.active.load(Ordering::Acquire))
            })
            .map(|channel| {
                Ok((
                    channel.pane,
                    channel.state.lock().map_err(|_| stale())?.facts.context,
                ))
            })
            .collect()
    }
    fn channels(&self, server: ServerId) -> Result<Vec<Arc<ChannelInner>>, ClientFailure> {
        let leases = self
            .shared
            .leases
            .lock()
            .map_err(|_| stale())?
            .values()
            .filter_map(Weak::upgrade)
            .filter(|lease| lease.workspace.server() == server)
            .collect::<Vec<_>>();
        let mut channels = vec![];
        for lease in leases {
            channels.extend(
                lease
                    .channels
                    .lock()
                    .map_err(|_| stale())?
                    .values()
                    .filter_map(Weak::upgrade),
            );
        }
        Ok(channels)
    }
}
#[derive(Clone)]
pub struct WorkspaceLease(Arc<LeaseInner>);
impl WorkspaceLease {
    pub fn workspace(&self) -> WorkspaceId {
        self.0.workspace
    }
    pub fn incarnation(&self) -> AttachmentIncarnation {
        self.0.incarnation
    }
    pub fn pane(&self, pane: PaneId) -> Result<PaneChannel, ClientFailure> {
        if !self.0.active.load(Ordering::Acquire) {
            return Err(stale());
        }
        let admitted_authority = {
            let servers = self.0.shared.servers.lock().map_err(|_| stale())?;
            if let Some(server) = servers.get(&self.0.workspace.server()) {
                server.info.into()
            } else {
                let retained = self.0.shared.retained.lock().map_err(|_| stale())?;
                retained
                    .get(&self.0.workspace.server())
                    .ok_or_else(stale)?
                    .authority
            }
        };
        if !self.0.shared.publication_admitted(admitted_authority) {
            return Err(stale());
        }
        let mut channels = self.0.channels.lock().map_err(|_| stale())?;
        channels.retain(|_, channel| channel.strong_count() != 0);
        if let Some(channel) = channels.get(&pane).and_then(Weak::upgrade) {
            return Ok(PaneChannel {
                inner: channel,
                lease: Arc::clone(&self.0),
            });
        }
        // A publication can finish while admission or construction is waiting.
        // Capture its current graph under the channel insertion ordering.
        let (authority, info, snapshot, active) = {
            let servers = self.0.shared.servers.lock().map_err(|_| stale())?;
            if let Some(server) = servers.get(&self.0.workspace.server()) {
                (
                    server.info.into(),
                    Some(server.info),
                    Arc::clone(&server.snapshot),
                    server.active,
                )
            } else {
                let retained = self.0.shared.retained.lock().map_err(|_| stale())?;
                let server = retained.get(&self.0.workspace.server()).ok_or_else(stale)?;
                (server.authority, None, Arc::clone(&server.snapshot), false)
            }
        };
        if authority != admitted_authority {
            return Err(stale());
        }
        let record = snapshot
            .organization
            .state()
            .panes
            .iter()
            .find(|record| record.id == pane)
            .ok_or_else(missing)?;
        let tab = snapshot
            .organization
            .state()
            .tabs
            .iter()
            .find(|tab| tab.id == record.tab && tab.workspace == self.0.workspace)
            .ok_or_else(missing)?
            .id;
        let facts = PaneChannelState {
            readiness: if active {
                PaneReadiness::Reconciling
            } else {
                PaneReadiness::Offline
            },
            runtime: if active {
                None
            } else {
                snapshot
                    .organization
                    .runtime()
                    .iter()
                    .find(|runtime| runtime.pane == pane)
                    .copied()
            },
            connection: None,
            context: self.0.shared.next_context()?,
            facts: PaneContextFacts::default(),
            content: None,
        };
        let projection = TerminalProjection::new(self.0.shared.budgets.clone())?;
        let shared = Arc::clone(&self.0.shared);
        projection.bind_admission(Arc::new(move || shared.admitted_identity(authority)));
        let channel = Arc::new(ChannelInner {
            authority,
            pane,
            tab,
            lease: Arc::downgrade(&self.0),
            shared: Arc::clone(&self.0.shared),
            state: Mutex::new(ChannelState {
                facts,
                content_confirmed: false,
                intent: None,
                intent_pending: None,
                events: None,
                native_high_water: 0,
                native_session: None,
                native: BTreeMap::new(),
                metadata: None,
                effects: BTreeMap::new(),
                effect_high_water: 0,
            }),
            published: watch::channel(facts).0,
            projection: Arc::new(Mutex::new(projection)),
            effect_admission: Mutex::new(()),
            effect_slots: Arc::new(Semaphore::new(
                spaceterm_protocol::limits::MAX_EGRESS_ENVELOPES,
            )),
            _slot: Arc::clone(&self.0.shared.slots)
                .try_acquire_owned()
                .map_err(|_| resource_limit())?,
        });
        channels.insert(pane, Arc::downgrade(&channel));
        if let Some(info) = info.filter(|_| active) {
            channel.apply(info, &snapshot)?;
        }
        drop(channels);
        self.0.shared.peer.presentation_changed();
        Ok(PaneChannel {
            inner: channel,
            lease: Arc::clone(&self.0),
        })
    }
    /// The journal/attachment owner invalidates before recording deferred release.
    pub fn invalidate(&self) -> Result<(), ClientFailure> {
        self.0.active.store(false, Ordering::Release);
        for channel in self
            .0
            .channels
            .lock()
            .map_err(|_| stale())?
            .values()
            .filter_map(Weak::upgrade)
        {
            let mut state = channel.state.lock().map_err(|_| stale())?;
            channel.projection.lock().map_err(|_| stale())?.revoke()?;
            state.facts.readiness = PaneReadiness::Detached;
            state.facts.content = None;
            state.intent = None;
            state.intent_pending = None;
            state.native.clear();
            state.effects.clear();
            state.metadata = None;
            events::publish(&mut state, PaneEvent::Invalidated, true);
            channel.published.send_replace(state.facts);
        }
        self.0.shared.peer.presentation_changed();
        Ok(())
    }
}
#[derive(Clone)]
pub struct PaneChannel {
    inner: Arc<ChannelInner>,
    lease: Arc<LeaseInner>,
}
impl PaneChannel {
    pub fn pane(&self) -> PaneId {
        self.inner.pane
    }
    pub fn state(&self) -> watch::Receiver<PaneChannelState> {
        self.inner.published.subscribe()
    }
    /// A local projection lock must not span PaneChannel calls, native effects or await.
    pub fn projection(&self) -> Arc<Mutex<TerminalProjection>> {
        Arc::clone(&self.inner.projection)
    }
    pub fn current_frame(&self) -> Option<FrameLease> {
        self.inner.projection.lock().ok()?.current_frame()
    }
    /// Metadata is retained independently of semantic pages and subscriptions.
    /// It is presentation data, never native or restoration-directory authority.
    pub fn metadata_snapshot(&self) -> Option<Arc<RuntimeMetadataSnapshot>> {
        if !self.inner.admitted() || !self.lease.active.load(Ordering::Acquire) {
            return None;
        }
        self.inner
            .state
            .lock()
            .ok()?
            .metadata
            .as_ref()
            .map(Arc::clone)
    }
    pub fn publish_metadata(
        &self,
        fence: EffectFence,
        snapshot: RuntimeMetadataSnapshot,
    ) -> Result<(), ClientFailure> {
        if !self.inner.shared.publication_admitted(self.inner.authority)
            || !self.lease.active.load(Ordering::Acquire)
        {
            return Err(stale());
        }
        let mut state = self.inner.state.lock().map_err(|_| stale())?;
        let current = bound_fence(&state.facts, self.inner.pane, fence.content().is_some())?;
        if !matches!(
            state.facts.readiness,
            PaneReadiness::Live | PaneReadiness::Reconciling
        ) || !matches!(
            state.facts.runtime.map(|runtime| runtime.phase),
            Some(SessionPhase::Running { .. })
        ) || !fence.is_current(&current)
            || snapshot.session() != fence.session()
            || state.metadata.as_ref().is_some_and(|old| {
                old.parts().sequence >= snapshot.parts().sequence
                    || old.parts().observed_at > snapshot.parts().observed_at
            })
        {
            return Err(stale());
        }
        state.metadata = Some(Arc::new(snapshot));
        events::refresh_metadata(&mut state);
        Ok(())
    }
    pub fn update_context(
        &self,
        facts: PaneContextFacts,
    ) -> Result<ContextGeneration, ClientFailure> {
        if !self.inner.admitted() {
            return Err(stale());
        }
        let mut state = self.inner.state.lock().map_err(|_| stale())?;
        if !self.lease.active.load(Ordering::Acquire) {
            return Err(stale());
        }
        if state.facts.facts != facts {
            state.facts.context = self.inner.shared.next_context()?;
            state.facts.facts = facts;
            state.intent = None;
            state.intent_pending = None;
            state.native.clear();
            state.effects.clear();
            self.inner.update_projection(&state)?;
            events::refresh_metadata(&mut state);
            self.inner.published.send_replace(state.facts);
        }
        let context = state.facts.context;
        drop(state);
        self.inner.shared.peer.presentation_changed();
        Ok(context)
    }
    pub fn publish_content_base(
        &self,
        fence: EffectFence,
        base: ContentBase,
    ) -> Result<(), ClientFailure> {
        if !self.inner.shared.publication_admitted(self.inner.authority) {
            return Err(stale());
        }
        let mut state = self.inner.state.lock().map_err(|_| stale())?;
        if !matches!(
            state.facts.readiness,
            PaneReadiness::Live | PaneReadiness::Reconciling
        ) || !matches!(
            state.facts.runtime.map(|runtime| runtime.phase),
            Some(SessionPhase::Running { .. })
        ) {
            return Err(stale());
        }
        let current = bound_fence(&state.facts, self.inner.pane, false)?;
        if !fence.is_current(&current)
            || base.session != fence.session()
            || state
                .facts
                .content
                .is_some_and(|old| old.session == base.session && old.revision > base.revision)
        {
            return Err(stale());
        }
        state.facts.content = Some(base);
        state.content_confirmed = true;
        state.facts.readiness = PaneReadiness::Live;
        self.inner.update_projection(&state)?;
        self.inner.published.send_replace(state.facts);
        Ok(())
    }
    pub fn permit(&self, kind: EffectKind) -> Result<EffectPermit, ClientFailure> {
        if !self.inner.admitted() {
            return Err(stale());
        }
        let state = self.inner.state.lock().map_err(|_| stale())?;
        let fence = current_fence(
            &state.facts,
            self.inner.pane,
            matches!(
                kind,
                EffectKind::Read | EffectKind::NativeRead | EffectKind::Pointer
            ),
        )?;
        if !allows(kind, state.facts.facts)
            || !matches!(
                state.facts.runtime.map(|r| r.phase),
                Some(SessionPhase::Running { .. })
            )
        {
            return Err(stale());
        }
        Ok(EffectPermit {
            channel: Arc::downgrade(&self.inner),
            lease: Arc::downgrade(&self.lease),
            fence,
            kind,
            native: None,
        })
    }
    pub fn try_effect(
        &self,
        permit: &EffectPermit,
        effect: impl Into<PaneRuntimeEffect>,
    ) -> Result<EffectAdmission, ClientFailure> {
        let effect = effect.into();
        let kind = match &effect {
            PaneRuntimeEffect::Command(command) => effect_kind(command),
            PaneRuntimeEffect::Copy(_) => EffectKind::Key,
        };
        if !Weak::ptr_eq(&permit.channel, &Arc::downgrade(&self.inner))
            || permit.kind != kind
            || !permit.is_current()
        {
            return Err(stale());
        }
        let effect = match effect {
            PaneRuntimeEffect::Command(command) => command,
            PaneRuntimeEffect::Copy(modifiers) => {
                let _admission = self.inner.effect_admission.lock().map_err(|_| stale())?;
                let admission = self.inner.shared.peer.try_copy(permit.clone(), modifiers)?;
                return self.track_effect(permit, admission, None);
            }
        };
        if let RuntimeEffect::Resize(grid) = &effect {
            let info = self
                .inner
                .state
                .lock()
                .map_err(|_| stale())?
                .facts
                .connection
                .ok_or_else(stale)?;
            grid.validate(self.inner.shared.peer.grid_limits(info)?)
                .map_err(|_| resource_limit())?;
        }
        match &effect {
            RuntimeEffect::Pointer(value)
                if value.stamp.fence() != permit.fence
                    || value.cell.session != permit.fence.session() =>
            {
                return Err(stale());
            }
            RuntimeEffect::Wheel(value)
                if value.stamp.fence() != permit.fence
                    || value.cell.session != permit.fence.session() =>
            {
                return Err(stale());
            }
            RuntimeEffect::CompleteNativeRequest(value) => {
                return self.complete_native(permit, value.clone());
            }
            _ => {}
        }
        if let RuntimeEffect::ResolvePaste(approval, decision) = &effect {
            let request = self.inner.state.lock().map_err(|_| stale())?.native.values()
                .find(|pending| matches!(pending.request.kind(), spaceterm_core::native_requests::NativeRequestKind::UnsafePaste { approval: pending, .. } if pending == approval))
                .map(|pending| pending.request.clone()).ok_or_else(stale)?;
            let completion = NativeCompletion::new(
                &request,
                spaceterm_core::native_requests::NativeCompletionKind::Paste {
                    approval: *approval,
                    decision: *decision,
                },
            )
            .map_err(|_| stale())?;
            // Keep ResolvePaste's cancellation disposition. Reuse native request
            // validation and its captured deadline, not a different runtime command.
            return self.complete_native_command(
                &self.permit(EffectKind::NativeCompletion)?,
                completion,
                |_| effect,
            );
        }
        let _admission = self.inner.effect_admission.lock().map_err(|_| stale())?;
        let pending = if matches!(effect, RuntimeEffect::RequestPaste(_)) {
            Some((effects::PendingEffectKind::PasteWrite, self.effect_slot()?))
        } else {
            None
        };
        let admission = self.inner.shared.peer.try_effect(
            permit.clone(),
            FencedRuntimeCommand {
                fence: permit.fence,
                command: effect,
            },
        )?;
        self.track_effect(permit, admission, pending)
    }
    /// Reject a delayed view action before it can rebuild current authority.
    pub fn validate_use_context(
        &self,
        context: &crate::view::UseContext,
    ) -> Result<(), ClientFailure> {
        if !self.inner.admitted() {
            return Err(stale());
        }
        let state = self.inner.state.lock().map_err(|_| stale())?;
        self.check_use_context(&state.facts, context)
    }
    fn check_use_context(
        &self,
        state: &PaneChannelState,
        context: &crate::view::UseContext,
    ) -> Result<(), ClientFailure> {
        let target = context.target();
        if !self.lease.active.load(Ordering::Acquire)
            || target.workspace != self.lease.workspace
            || target.tab != self.inner.tab
            || target.pane != self.inner.pane
            || context.attachment().workspace() != self.lease.workspace
            || context.attachment().incarnation() != self.lease.incarnation
            || context.attachment().client() != context.connection().client
            || state.context != context.context()
            || state.connection.is_none_or(|info| {
                info.binding != context.connection() || info.epoch != context.session().epoch
            })
            || state.runtime.is_none_or(|runtime| {
                runtime.pane != context.session().pane
                    || runtime.session_generation != context.session().generation
                    || runtime.phase != context.phase()
                    || runtime.control != context.control()
                    || runtime.control_generation != context.control_generation()
            })
        {
            return Err(stale());
        }
        Ok(())
    }
    pub async fn explicit_use_context(
        &self,
        source: ExplicitUseSource,
        context: crate::view::UseContext,
    ) -> Result<ExplicitUseOutcome, ClientFailure> {
        self.explicit_use_inner(source, Some(context)).await
    }
    pub async fn explicit_use(
        &self,
        source: ExplicitUseSource,
    ) -> Result<ExplicitUseOutcome, ClientFailure> {
        self.explicit_use_inner(source, None).await
    }
    async fn explicit_use_inner(
        &self,
        source: ExplicitUseSource,
        captured: Option<crate::view::UseContext>,
    ) -> Result<ExplicitUseOutcome, ClientFailure> {
        if !self.inner.admitted() {
            return Err(stale());
        }
        let marker = Arc::new(());
        let request = {
            let mut state = self.inner.state.lock().map_err(|_| stale())?;
            let info = state.facts.connection.ok_or_else(stale)?;
            let runtime = state.facts.runtime.ok_or_else(missing)?;
            if let Some(context) = captured {
                self.check_use_context(&state.facts, &context)?;
            }
            if !self.lease.active.load(Ordering::Acquire)
                || !state.facts.facts.focused_pane
                || !state.facts.facts.presentable
                || state.facts.readiness == PaneReadiness::Takeover
                || state.intent_pending.is_some()
                || state.intent.is_some()
                || matches!(source, ExplicitUseSource::TabOpening { tab } if tab != self.inner.tab)
            {
                return Err(stale());
            }
            state.intent_pending = Some(Arc::clone(&marker));
            PaneUseRequest {
                workspace: self.lease.workspace,
                tab: self.inner.tab,
                runtime,
                info,
                context: state.facts.context,
                source,
            }
        };
        let pending = PendingIntent {
            channel: Arc::downgrade(&self.inner),
            marker,
        };
        let reply = self.inner.shared.peer.explicit_use(request).await?;
        let mut state = self.inner.state.lock().map_err(|_| stale())?;
        let owns_pending = state
            .intent_pending
            .as_ref()
            .is_some_and(|marker| Arc::ptr_eq(marker, &pending.marker));
        let current = bound_fence(&state.facts, self.inner.pane, false).ok();
        let existing_session = matches!(
            request.runtime.phase,
            SessionPhase::Running { .. } | SessionPhase::Starting { .. }
        );
        let valid = self.lease.active.load(Ordering::Acquire)
            && owns_pending
            && state.facts.context == request.context
            && state
                .facts
                .connection
                .is_some_and(|info| same_attempt(info, request.info))
            && current.is_some_and(|fence| {
                fence.session().generation == request.runtime.session_generation
            })
            && match reply {
                PaneUseReply::AwaitingMeasurement(intent) => {
                    !existing_session
                        && intent.source() == source
                        && current.is_some_and(|fence| intent.fence().is_current(&fence))
                }
                PaneUseReply::ControlReady(grant) => {
                    existing_session
                        && grant.epoch() == request.info.epoch
                        && current.is_some_and(|fence| grant.grant() == fence.grant())
                }
            };
        if owns_pending {
            state.intent_pending = None;
        }
        if !valid {
            drop(state);
            if let PaneUseReply::AwaitingMeasurement(intent) = reply {
                self.inner.shared.peer.cancel_intent(intent);
            }
            return Err(stale());
        }
        let outcome = match reply {
            PaneUseReply::AwaitingMeasurement(intent) => {
                state.intent = Some(AuthorizedIntent {
                    intent,
                    attempt: request.info.attempt,
                });
                ExplicitUseOutcome::AwaitingMeasurement(Box::new(IntentToken {
                    intent,
                    channel: Arc::downgrade(&self.inner),
                    incarnation: self.lease.incarnation,
                }))
            }
            PaneUseReply::ControlReady(grant) => ExplicitUseOutcome::ControlReady(grant),
        };
        drop(state);
        drop(pending);
        Ok(outcome)
    }
    pub async fn complete_intent(
        &self,
        token: &IntentToken,
        grid: impl Into<MeasuredGrid>,
    ) -> Result<OperationTicket, ClientFailure> {
        if !self.inner.admitted() {
            return Err(stale());
        }
        let measurement = {
            let mut state = self.inner.state.lock().map_err(|_| stale())?;
            if !Weak::ptr_eq(&token.channel, &Arc::downgrade(&self.inner))
                || token.incarnation != self.lease.incarnation
                || !self.lease.active.load(Ordering::Acquire)
                || !state.intent.is_some_and(|saved| {
                    saved.intent == token.intent
                        && state
                            .facts
                            .connection
                            .is_some_and(|info| info.attempt == saved.attempt)
                })
                || !token.intent.fence().is_current(&current_fence(
                    &state.facts,
                    self.inner.pane,
                    false,
                )?)
            {
                return Err(stale());
            }
            let info = state.facts.connection.ok_or_else(stale)?;
            let grid = grid
                .into()
                .validate(self.inner.shared.peer.grid_limits(info)?)
                .map_err(|_| resource_limit())?;
            state.intent = None;
            MeasuredStartupMeasurement {
                intent: token.intent.id(),
                fence: token.intent.fence(),
                grid,
            }
        };
        self.inner
            .shared
            .peer
            .complete_intent(self.lease.workspace, measurement)
            .await
    }
    pub fn cancel_intent(&self, token: &IntentToken) {
        if !Weak::ptr_eq(&token.channel, &Arc::downgrade(&self.inner)) {
            return;
        }
        let removed = self.inner.state.lock().is_ok_and(|mut state| {
            if state
                .intent
                .is_some_and(|saved| saved.intent == token.intent)
            {
                state.intent = None;
                true
            } else {
                false
            }
        });
        if removed {
            self.inner.shared.peer.cancel_intent(token.intent);
        }
    }
    pub async fn take_over(&self) -> Result<OperationTicket, ClientFailure> {
        if !self.inner.admitted() {
            return Err(stale());
        }
        let state = self.inner.state.lock().map_err(|_| stale())?.facts;
        let info = state.connection.ok_or_else(stale)?;
        if !self.lease.active.load(Ordering::Acquire)
            || state.readiness != PaneReadiness::Takeover
            || !state.facts.focused_pane
            || !state.facts.presentable
        {
            return Err(stale());
        }
        self.inner
            .shared
            .peer
            .take_over(
                self.lease.workspace,
                TakeOverRequest {
                    epoch: info.epoch,
                    pane: self.inner.pane,
                    client: info.client,
                },
            )
            .await
    }
    pub fn query_stamp(
        &self,
        permit: &EffectPermit,
        generation: RequestGeneration,
    ) -> Result<QueryStamp, ClientFailure> {
        if !matches!(permit.kind, EffectKind::Read | EffectKind::NativeRead)
            || !permit.is_current()
            || !Weak::ptr_eq(&permit.channel, &Arc::downgrade(&self.inner))
        {
            return Err(stale());
        }
        let base = self
            .inner
            .state
            .lock()
            .map_err(|_| stale())?
            .facts
            .content
            .ok_or_else(stale)?;
        let requests = {
            let servers = self.inner.shared.servers.lock().map_err(|_| stale())?;
            Arc::clone(
                &servers
                    .get(&self.inner.pane.server())
                    .ok_or_else(stale)?
                    .requests,
            )
        };
        let request = requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| exhausted())?
            .checked_add(1)
            .ok_or_else(exhausted)?;
        QueryStamp::new(
            permit.fence,
            base,
            ContentRequestId::new(request).map_err(|_| exhausted())?,
            generation,
        )
        .map_err(|_| invalid_peer())
    }
}
#[derive(Clone)]
pub struct EffectPermit {
    channel: Weak<ChannelInner>,
    lease: Weak<LeaseInner>,
    fence: EffectFence,
    kind: EffectKind,
    native: Option<events::NativeGuard>,
}
impl EffectPermit {
    pub fn fence(&self) -> EffectFence {
        self.fence
    }
    pub fn kind(&self) -> EffectKind {
        self.kind
    }
    pub fn is_current(&self) -> bool {
        let Some(lease) = self
            .lease
            .upgrade()
            .filter(|lease| lease.active.load(Ordering::Acquire))
        else {
            return false;
        };
        let Some(channel) = self.channel.upgrade() else {
            return false;
        };
        let _keep_lease = lease;
        if !channel.admitted() {
            return false;
        }
        channel.state.lock().is_ok_and(|state| {
            allows(self.kind, state.facts.facts)
                && self.native.is_none_or(|guard| guard.allows(&channel))
                && current_fence(&state.facts, channel.pane, self.fence.content().is_some())
                    .is_ok_and(|current| self.fence.is_current(&current))
                && matches!(
                    state.facts.runtime.map(|r| r.phase),
                    Some(SessionPhase::Running { .. })
                )
        })
    }
}
#[derive(Clone)]
pub struct IntentToken {
    intent: StartupIntent,
    channel: Weak<ChannelInner>,
    incarnation: AttachmentIncarnation,
}
impl IntentToken {
    pub fn intent(&self) -> StartupIntent {
        self.intent
    }
}
struct PendingIntent {
    channel: Weak<ChannelInner>,
    marker: Arc<()>,
}
impl Drop for PendingIntent {
    fn drop(&mut self) {
        if let Some(channel) = self.channel.upgrade()
            && let Ok(mut state) = channel.state.lock()
            && state
                .intent_pending
                .as_ref()
                .is_some_and(|marker| Arc::ptr_eq(marker, &self.marker))
        {
            state.intent_pending = None;
        }
    }
}
impl ChannelInner {
    fn admitted(&self) -> bool {
        self.shared.admitted_identity(self.authority)
    }
    fn apply(&self, info: ConnectedInfo, snapshot: &ReplicaSnapshot) -> Result<(), ClientFailure> {
        // Publication and pane construction validate journal authority before
        // taking pane locks. Native effects retain their nonblocking gate.
        if info.pairing != self.authority.pairing || info.client != self.authority.client {
            return Err(stale());
        }
        let mut state = self.state.lock().map_err(|_| stale())?;
        if !self
            .shared
            .servers
            .lock()
            .map_err(|_| stale())?
            .get(&info.server)
            .is_some_and(|server| {
                server.active
                    && same_attempt(server.info, info)
                    && server.snapshot.cursor == snapshot.cursor
            })
        {
            return Err(stale());
        }
        let present = snapshot
            .organization
            .state()
            .panes
            .iter()
            .any(|pane| pane.id == self.pane && pane.tab == self.tab);
        let runtime = snapshot
            .organization
            .runtime()
            .iter()
            .find(|runtime| runtime.pane == self.pane)
            .copied()
            .filter(|_| present);
        let same_runtime = state.facts.runtime == runtime;
        let changed = !same_runtime
            || state
                .facts
                .connection
                .is_none_or(|old| !same_attempt(old, info));
        if changed {
            state.native.clear();
            state.effects.clear();
            let session = runtime.map(|runtime| SessionKey {
                pane: self.pane,
                epoch: info.epoch,
                generation: runtime.session_generation,
            });
            let same_session = state.native_session == session;
            state.native_session = session;
            let keep_content = same_session
                && state.facts.runtime.zip(runtime).is_some_and(|(old, new)| {
                    old.control == new.control && matches!(new.phase, SessionPhase::Running { .. })
                });
            if !keep_content {
                state.metadata = None;
                state.facts.content = None;
                self.projection.lock().map_err(|_| stale())?.revoke()?;
            }
            if !keep_content
                || state
                    .facts
                    .connection
                    .is_none_or(|old| !same_attempt(old, info))
            {
                state.content_confirmed = false;
            }
            if state
                .facts
                .connection
                .is_none_or(|old| !same_attempt(old, info))
            {
                state.effect_high_water = 0;
            }
            if !same_session {
                state.native_high_water = 0;
            }
        }
        state.facts.runtime = runtime;
        state.facts.connection = Some(info);
        state.facts.readiness = if self
            .lease
            .upgrade()
            .is_none_or(|lease| !lease.active.load(Ordering::Acquire))
        {
            PaneReadiness::Detached
        } else if runtime.is_none() {
            PaneReadiness::Missing
        } else if matches!(runtime.map(|r| r.control), Some(ControlRecord::Held { client, .. }) if client != info.client)
        {
            PaneReadiness::Takeover
        } else if matches!(runtime.map(|r| (r.phase, r.control)), Some((SessionPhase::Running { .. }, ControlRecord::Held { client, .. })) if client == info.client)
            && !state.content_confirmed
        {
            PaneReadiness::Reconciling
        } else {
            PaneReadiness::Live
        };
        if state.intent.is_some_and(|saved| {
            !same_runtime
                || saved.attempt != info.attempt
                || !bound_fence(&state.facts, self.pane, false)
                    .is_ok_and(|current| saved.intent.fence().is_current(&current))
        }) {
            state.intent = None;
        }
        self.update_projection(&state)?;
        if changed {
            events::publish(&mut state, PaneEvent::Invalidated, true);
            events::refresh_metadata(&mut state);
        }
        self.published.send_replace(state.facts);
        Ok(())
    }
    fn update_projection(&self, state: &ChannelState) -> Result<(), ClientFailure> {
        let mut projection = self.projection.lock().map_err(|_| stale())?;
        if matches!(
            state.facts.readiness,
            PaneReadiness::Offline | PaneReadiness::Reconciling
        ) && matches!(
            state.facts.runtime.map(|runtime| runtime.phase),
            Some(SessionPhase::Running { .. })
        ) {
            return projection.freeze();
        }
        if matches!(
            state.facts.runtime.map(|r| r.phase),
            Some(SessionPhase::Running { .. })
        ) && let Ok(fence) =
            current_fence(&state.facts, self.pane, state.facts.content.is_some())
        {
            projection.authority(
                fence,
                state.facts.content,
                state.facts.facts.presentable,
                state.facts.facts.focused_pane && state.facts.facts.presentable,
            )
        } else {
            projection.revoke()
        }
    }
}
fn current_fence(
    state: &PaneChannelState,
    pane: PaneId,
    content: bool,
) -> Result<EffectFence, ClientFailure> {
    if state.readiness != PaneReadiness::Live {
        return Err(stale());
    }
    bound_fence(state, pane, content)
}
fn bound_fence(
    state: &PaneChannelState,
    pane: PaneId,
    content: bool,
) -> Result<EffectFence, ClientFailure> {
    let info = state.connection.ok_or_else(stale)?;
    let runtime = state.runtime.ok_or_else(missing)?;
    let ControlRecord::Held { client, generation } = runtime.control else {
        return Err(stale());
    };
    if client != info.client {
        return Err(stale());
    }
    let grant = ControlGrant::new(pane, client, generation).map_err(|_| invalid_peer())?;
    let revision = if content {
        Some(state.content.ok_or_else(stale)?.revision)
    } else {
        None
    };
    EffectFence::new(
        SessionKey {
            pane,
            epoch: info.epoch,
            generation: runtime.session_generation,
        },
        grant,
        info.binding,
        state.context,
        revision,
    )
    .map_err(|_| invalid_peer())
}
fn same_attempt(a: ConnectedInfo, b: ConnectedInfo) -> bool {
    a.server == b.server
        && a.client == b.client
        && a.pairing == b.pairing
        && a.epoch == b.epoch
        && a.binding == b.binding
        && a.attempt == b.attempt
}
fn missing() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::TargetNotFound)
}
fn allows(kind: EffectKind, facts: PaneContextFacts) -> bool {
    facts.presentable
        && match kind {
            EffectKind::ImeCommit | EffectKind::NativeRead => facts.focused_pane,
            EffectKind::Key
            | EffectKind::Pointer
            | EffectKind::Paste
            | EffectKind::NativeCompletion => facts.focused_pane && facts.terminal_input_focus,
            EffectKind::Read
            | EffectKind::Resize
            | EffectKind::FocusReport
            | EffectKind::Appearance => true,
        }
}
fn effect_kind(effect: &RuntimeEffect) -> EffectKind {
    match effect {
        RuntimeEffect::Key(_) | RuntimeEffect::Copy(_) => EffectKind::Key,
        RuntimeEffect::InputMethodCommit(_) => EffectKind::ImeCommit,
        RuntimeEffect::Pointer(_) | RuntimeEffect::Wheel(_) => EffectKind::Pointer,
        RuntimeEffect::Resize(_) | RuntimeEffect::CellPixelMetrics(_) => EffectKind::Resize,
        RuntimeEffect::FocusReport(_) => EffectKind::FocusReport,
        RuntimeEffect::Appearance(_) => EffectKind::Appearance,
        RuntimeEffect::RequestPaste(_)
        | RuntimeEffect::RequestCorrelatedPaste(..)
        | RuntimeEffect::ResolvePaste(_, _) => EffectKind::Paste,
        RuntimeEffect::CompleteNativeRequest(_) => EffectKind::NativeCompletion,
        RuntimeEffect::ClearScreenAndScrollback => EffectKind::Key,
    }
}

impl Drop for ChannelInner {
    fn drop(&mut self) {
        self.shared.peer.presentation_changed();
        if let Ok(mut projection) = self.projection.lock() {
            let _ = projection.revoke();
        }
    }
}
