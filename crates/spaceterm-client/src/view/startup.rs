//! Explicit-use choreography for control acquisition and measured startup.
//!
//! Only four deliberate local sources begin a use: Pane selection, Tab opening,
//! Start Session and Retry. Retry is a local action that becomes the core Start
//! Session source. Reconciliation, rendering, restored focus, window focus,
//! Settings closure, reconnect and measurement have no entry point that begins
//! a use, so none of them can acquire control or start a Terminal Session.
//!
//! A [`UseTicket`] correlates the view's own callbacks. Client-core owns the
//! server intent token and every operation identity; this module never builds a
//! wire identity, retries a submitted operation or keeps an outcome history. It
//! holds at most one unsubmitted use and, per Pane, the latest use handed to
//! client-core while the Pane still shows the state it was handed off for.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use spaceterm_core::authority::{
    ConnectionBinding, ContextGeneration, ControlGeneration, ControlGrant, EffectFence, SessionKey,
};
use spaceterm_core::control::StartupSource;
use spaceterm_core::geometry::GridSize;
use spaceterm_core::identity::{ClientId, PaneId, TabId, WorkspaceId};
use spaceterm_core::operations::OperationId;
use spaceterm_core::organization::{ControlRecord, EndReason, SessionPhase};

use super::arrangement::PaneArrangement;
use super::replica_bridge::{ReplicaObservation, ReplicaReadiness};
use super::state::{ClientViewState, ViewAvailability, WorkspaceAttachment};
use super::types::{LayoutRevision, UseTicket, ViewCounter, ViewError, ViewResource};

/// The core startup intent identity, as returned by `StartupIntent::id()`.
pub type StartupIntentId = OperationId;

/// The core wire startup source. Local Retry maps to Start Session.
pub type ExplicitUseKind = StartupSource;

/// The deliberate input that produced an explicit use.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SelectionSource {
    Pointer,
    Command,
    AccessibilityPress,
}

/// The only local actions that may request control or startup.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExplicitUseSource {
    PaneSelection(SelectionSource),
    /// Opening a Tab uses only that Tab's remembered Focused Pane.
    TabOpening(SelectionSource),
    StartSession(SelectionSource),
    /// A deliberate retry after a failure. It is sent as Start Session.
    Retry(SelectionSource),
}

impl ExplicitUseSource {
    pub const fn selection(self) -> SelectionSource {
        match self {
            Self::PaneSelection(source)
            | Self::TabOpening(source)
            | Self::StartSession(source)
            | Self::Retry(source) => source,
        }
    }

    /// The canonical core startup source for a use of a Pane in `tab`.
    pub const fn kind(self, tab: TabId) -> ExplicitUseKind {
        match self {
            Self::PaneSelection(_) => StartupSource::ExplicitSelection,
            Self::TabOpening(_) => StartupSource::TabOpening { tab },
            Self::StartSession(_) | Self::Retry(_) => StartupSource::StartSession,
        }
    }

    /// Whether the source deliberately starts a Pane whose last start failed
    /// or found its directory unavailable. Selection and Tab opening never do,
    /// so such a Pane cannot enter a render- or focus-driven retry loop; they
    /// only compare-acquire it when it is unclaimed.
    pub const fn restarts_failed(self) -> bool {
        matches!(self, Self::StartSession(_) | Self::Retry(_))
    }
}

/// The exact Pane an explicit use targets.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SelectionTarget {
    pub workspace: WorkspaceId,
    pub tab: TabId,
    pub pane: PaneId,
}

impl SelectionTarget {
    /// The remembered Focused Pane of a Workspace's Active Tab: the only Pane
    /// that opening that Tab may use.
    pub fn focused(view: &ClientViewState, workspace: WorkspaceId) -> Option<Self> {
        let workspace_view = view.workspace(workspace)?;
        let tab = workspace_view.active_tab()?;
        let pane = workspace_view.arrangement(tab)?.focused_pane()?;
        Some(Self {
            workspace,
            tab,
            pane,
        })
    }
}

/// The local view, the replica of the use's Server and the current native
/// context generation of the use's Pane. Every reduction rereads them; no
/// caller-built authority is trusted.
#[derive(Clone, Copy)]
pub struct UseFacts<'a> {
    pub view: &'a ClientViewState,
    pub replica: &'a ReplicaObservation<'a>,
    pub context: ContextGeneration,
}

/// The facts one explicit use was checked against: the selected target, its
/// attachment, the reconciled Session, phase and control of its Pane, the
/// authenticated connection, the native context and the arrangement layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UseContext {
    target: SelectionTarget,
    attachment: WorkspaceAttachment,
    session: SessionKey,
    phase: SessionPhase,
    control: ControlRecord,
    control_generation: ControlGeneration,
    connection: ConnectionBinding,
    context: ContextGeneration,
    layout: LayoutRevision,
}

impl UseContext {
    /// Observes a target of the Active Tab of the active Workspace in a Ready
    /// replica. A Retained or Synchronizing replica permits navigation only.
    pub fn observe(
        view: &ClientViewState,
        replica: &ReplicaObservation<'_>,
        target: SelectionTarget,
        context: ContextGeneration,
    ) -> Result<Self, ViewError> {
        observe_target(view, replica, target)
            .map_err(Unobserved::error)?
            .context(target, context)
            .map_err(Unobserved::error)
    }

    pub const fn target(&self) -> SelectionTarget {
        self.target
    }

    pub const fn attachment(&self) -> WorkspaceAttachment {
        self.attachment
    }

    pub const fn session(&self) -> SessionKey {
        self.session
    }

    pub const fn phase(&self) -> SessionPhase {
        self.phase
    }

    pub const fn control(&self) -> ControlRecord {
        self.control
    }

    /// The retained control generation, also when unclaimed, so a
    /// compare-acquire detects a release race.
    pub const fn control_generation(&self) -> ControlGeneration {
        self.control_generation
    }

    pub const fn connection(&self) -> ConnectionBinding {
        self.connection
    }

    /// This client's stable Client identity on the use's Server.
    pub const fn client(&self) -> ClientId {
        self.connection.client
    }

    pub const fn context(&self) -> ContextGeneration {
        self.context
    }

    pub const fn layout(&self) -> LayoutRevision {
        self.layout
    }
}

/// Actual dimensions for one requested measurement. They supply size only and
/// never begin or authorize a use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Measurement {
    pub ticket: UseTicket,
    pub context: ContextGeneration,
    pub layout: LayoutRevision,
    pub grid: GridSize,
}

/// Client-core's report that the server admitted a startup intent for a use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorizedUse {
    pub ticket: UseTicket,
    pub intent: StartupIntentId,
    pub fence: EffectFence,
}

/// What an explicit use asks client-core for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UseKind {
    /// A server startup intent, completed only with measured dimensions.
    Startup,
    /// Compare-acquire of an unclaimed Pane without a startup: its Session
    /// runs or starts, or its last start failed and only a deliberate Start
    /// Session or Retry starts it again.
    Acquisition,
}

/// Work for client-core and the desktop. Effects carry identities, stamps and
/// geometry only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupEffect {
    /// Request the server's control-checked explicit-use intent: preserve this
    /// Client's exact grant, or compare-acquire an unclaimed Pane. Never take
    /// over.
    RequestUse {
        ticket: UseTicket,
        target: SelectionTarget,
        source: ExplicitUseSource,
        kind: ExplicitUseKind,
        context: UseContext,
    },
    /// Compare-acquire an unclaimed Pane. It never starts a Session and
    /// requests no measurement.
    AcquireControl {
        ticket: UseTicket,
        target: SelectionTarget,
        context: UseContext,
    },
    /// Measure the target Pane and report it with exactly these tags, even
    /// when its grid equals an earlier measurement.
    RequestMeasurement {
        ticket: UseTicket,
        target: SelectionTarget,
        context: ContextGeneration,
        layout: LayoutRevision,
    },
    /// Complete the exact server intent once with measured dimensions.
    CompleteUse {
        ticket: UseTicket,
        intent: StartupIntentId,
        grid: GridSize,
        fence: EffectFence,
    },
    /// Cancel a server intent that must not start.
    CancelUse {
        ticket: UseTicket,
        intent: StartupIntentId,
    },
}

/// Progress of the current explicit use.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StartupState {
    Idle,
    /// Waiting for the server intent. `measured` reports whether dimensions
    /// already arrived.
    AwaitingAuthorization {
        measured: bool,
    },
    AwaitingMeasurement,
    /// Authorized and measured, waiting for the replica to show the exact
    /// grant as current.
    ReadyToComplete,
    /// No use is unsubmitted, and a completion or acquisition handed to
    /// client-core is still in flight for at least one Pane.
    Submitted,
}

/// Why an unsubmitted use was canceled.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IntentCancelReason {
    /// A newer explicit selection replaced it.
    Superseded,
    /// The Workspace attachment left the view or changed incarnation:
    /// detach, self-unpair, deletion or suppression.
    Detached,
    PairingRemoved,
    /// The Pane or Tab was removed or is no longer represented.
    TargetRemoved,
    /// The view left the selected Tab or Workspace.
    LeftSelection,
    ControlLost,
    /// The connection was lost, replaced, or is not reconciled.
    ConnectionLost,
    EpochChanged,
    SessionChanged,
    EndAllSessions,
    ContextChanged,
    /// The server definitively rejected the use.
    Rejected,
}

/// Effects of one reduction, in order, and the resulting state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupReduction {
    pub effects: Vec<StartupEffect>,
    pub state: StartupState,
    /// Set when this reduction canceled the unsubmitted use.
    pub canceled: Option<IntentCancelReason>,
}

/// An explicit use of one Pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneUse {
    pub ticket: UseTicket,
    pub kind: UseKind,
    /// The facts the use began with.
    pub context: UseContext,
    pub state: StartupState,
}

#[derive(Clone, Copy, Debug)]
struct Record {
    ticket: UseTicket,
    kind: UseKind,
    begun: UseContext,
    /// Layout of the outstanding measurement request.
    requested: LayoutRevision,
    authorization: Option<(StartupIntentId, EffectFence)>,
    measurement: Option<GridSize>,
}

impl Record {
    fn new(ticket: UseTicket, kind: UseKind, begun: UseContext) -> Self {
        Self {
            ticket,
            kind,
            begun,
            requested: begun.layout,
            authorization: None,
            measurement: None,
        }
    }

    fn pending_state(&self) -> StartupState {
        match (self.authorization, self.measurement) {
            (None, measurement) => StartupState::AwaitingAuthorization {
                measured: measurement.is_some(),
            },
            (Some(_), None) => StartupState::AwaitingMeasurement,
            (Some(_), Some(_)) => StartupState::ReadyToComplete,
        }
    }

    fn pane_use(&self, state: StartupState) -> PaneUse {
        PaneUse {
            ticket: self.ticket,
            kind: self.kind,
            context: self.begun,
            state,
        }
    }

    fn server_matches(&self, replica: &ReplicaObservation<'_>) -> bool {
        self.begun.target.pane.server() == replica.server()
    }

    /// Whether a handed-off use still describes the observed Pane, so a new
    /// selection of it must not repeat the request. A use belongs to the
    /// connection it was requested on: after a reconnect the reconciled
    /// replica shows whether it took effect, and a new explicit use may
    /// request again.
    fn in_flight(&self, observed: &Observed) -> bool {
        let begun = &self.begun;
        if observed.attachment != begun.attachment
            || observed.session != begun.session
            || observed.phase != begun.phase
            || observed.connection != Some(begun.connection)
        {
            return false;
        }
        match self.kind {
            UseKind::Acquisition => {
                observed.control == begun.control
                    && observed.control_generation == begun.control_generation
            }
            UseKind::Startup => {
                let granted = self
                    .authorization
                    .map_or(begun.control_generation, |(_, fence)| {
                        fence.grant().generation()
                    });
                !held_by_other(observed.control, begun.client())
                    && observed.control_generation <= granted
            }
        }
    }

    /// Checks an unsubmitted use against current facts. `Ok(true)` means the
    /// exact authorized grant is current.
    fn check(&self, current: &UseContext) -> Result<bool, IntentCancelReason> {
        let begun = &self.begun;
        if current.attachment != begun.attachment {
            return Err(IntentCancelReason::Detached);
        }
        session_change(begun.session, current.session)?;
        if current.phase != begun.phase {
            return Err(match current.phase {
                SessionPhase::Ended {
                    reason: EndReason::EndAllSessions,
                } => IntentCancelReason::EndAllSessions,
                _ => IntentCancelReason::SessionChanged,
            });
        }
        if current.connection != begun.connection {
            return Err(IntentCancelReason::ConnectionLost);
        }
        if current.context != begun.context {
            return Err(IntentCancelReason::ContextChanged);
        }
        let ours = begun.client();
        if held_by_other(current.control, ours) {
            return Err(IntentCancelReason::ControlLost);
        }
        let Some((_, fence)) = self.authorization else {
            let unchanged = current.control == begun.control
                && current.control_generation == begun.control_generation;
            // The replica may show this Client's compare-acquire before
            // client-core reports the intent.
            let acquired = begun.control == ControlRecord::Unclaimed
                && matches!(current.control, ControlRecord::Held { .. })
                && current.control_generation > begun.control_generation;
            return if unchanged || acquired {
                Ok(false)
            } else {
                Err(IntentCancelReason::ControlLost)
            };
        };
        session_change(fence.session(), current.session)?;
        if fence.connection() != current.connection {
            return Err(IntentCancelReason::ConnectionLost);
        }
        if fence.context() != current.context {
            return Err(IntentCancelReason::ContextChanged);
        }
        let granted = fence.grant().generation();
        match current.control_generation.cmp(&granted) {
            // The replica has not yet published the grant.
            Ordering::Less => Ok(false),
            Ordering::Equal => {
                let expected = ControlGrant::new(current.target.pane, ours, granted)
                    .and_then(|grant| {
                        EffectFence::new(
                            current.session,
                            grant,
                            current.connection,
                            current.context,
                            None,
                        )
                    })
                    .map_err(|_| IntentCancelReason::ControlLost)?;
                let held = current.control
                    == ControlRecord::Held {
                        client: ours,
                        generation: granted,
                    };
                if held && fence.matches(&expected) {
                    Ok(true)
                } else {
                    Err(IntentCancelReason::ControlLost)
                }
            }
            Ordering::Greater => Err(IntentCancelReason::ControlLost),
        }
    }

    fn cancel_effect(&self) -> Option<StartupEffect> {
        self.authorization
            .map(|(intent, _)| StartupEffect::CancelUse {
                ticket: self.ticket,
                intent,
            })
    }
}

fn held_by_other(control: ControlRecord, ours: ClientId) -> bool {
    matches!(control, ControlRecord::Held { client, .. } if client != ours)
}

fn session_change(expected: SessionKey, current: SessionKey) -> Result<(), IntentCancelReason> {
    if expected.epoch != current.epoch {
        Err(IntentCancelReason::EpochChanged)
    } else if expected != current {
        Err(IntentCancelReason::SessionChanged)
    } else {
        Ok(())
    }
}

/// What an explicit use of a Pane in this state asks for, if anything.
fn decide(source: ExplicitUseSource, current: &UseContext) -> Option<UseKind> {
    let ours = current.client();
    // Control held by another Client needs the separate Take Over action.
    if held_by_other(current.control, ours) {
        return None;
    }
    // Every deliberate use compare-acquires an unclaimed Pane (ADR 0022),
    // whether or not its Session may start.
    let acquire = (current.control == ControlRecord::Unclaimed).then_some(UseKind::Acquisition);
    match current.phase {
        // An existing Session is reused, never duplicated.
        SessionPhase::Running { .. } | SessionPhase::Starting { .. } => acquire,
        SessionPhase::NotStarted | SessionPhase::Ended { .. } => Some(UseKind::Startup),
        SessionPhase::Failed { .. } | SessionPhase::DirectoryUnavailable => {
            if source.restarts_failed() {
                Some(UseKind::Startup)
            } else {
                acquire
            }
        }
    }
}

/// Why a target cannot be observed for use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Unobserved {
    WrongServer,
    NotAttached,
    WrongPairing,
    ResourceLimited(ViewResource),
    Removed,
    NotReady,
    NotSelected,
}

impl Unobserved {
    fn error(self) -> ViewError {
        match self {
            Self::WrongServer => ViewError::WrongServer,
            Self::NotAttached | Self::Removed => ViewError::TargetMissing,
            Self::WrongPairing => ViewError::WrongMembership,
            Self::ResourceLimited(resource) => ViewError::ResourceLimit { resource },
            Self::NotReady => ViewError::NotReady,
            Self::NotSelected => ViewError::StaleIntent,
        }
    }

    fn reason(self) -> IntentCancelReason {
        match self {
            Self::WrongServer | Self::NotAttached => IntentCancelReason::Detached,
            Self::WrongPairing => IntentCancelReason::PairingRemoved,
            Self::ResourceLimited(_) | Self::Removed => IntentCancelReason::TargetRemoved,
            Self::NotReady => IntentCancelReason::ConnectionLost,
            Self::NotSelected => IntentCancelReason::LeftSelection,
        }
    }
}

/// A represented target's current facts, before readiness and selection are
/// required.
#[derive(Clone, Copy, Debug)]
struct Observed {
    attachment: WorkspaceAttachment,
    session: SessionKey,
    phase: SessionPhase,
    control: ControlRecord,
    control_generation: ControlGeneration,
    connection: Option<ConnectionBinding>,
    ready: bool,
    selected: bool,
    layout: LayoutRevision,
}

impl Observed {
    fn context(
        self,
        target: SelectionTarget,
        context: ContextGeneration,
    ) -> Result<UseContext, Unobserved> {
        let connection = self
            .connection
            .filter(|_| self.ready)
            .ok_or(Unobserved::NotReady)?;
        if !self.selected {
            return Err(Unobserved::NotSelected);
        }
        Ok(UseContext {
            target,
            attachment: self.attachment,
            session: self.session,
            phase: self.phase,
            control: self.control,
            control_generation: self.control_generation,
            connection,
            context,
            layout: self.layout,
        })
    }
}

fn observe_target(
    view: &ClientViewState,
    replica: &ReplicaObservation<'_>,
    target: SelectionTarget,
) -> Result<Observed, Unobserved> {
    let server = replica.server();
    if target.workspace.server() != server
        || target.tab.server() != server
        || target.pane.server() != server
    {
        return Err(Unobserved::WrongServer);
    }
    let tab = TabObservation::new(view, replica, target.workspace, target.tab)?;
    if !tab.arrangement.contains_pane(target.pane) {
        return Err(Unobserved::Removed);
    }
    tab.pane(replica, target.pane)
}

/// A target's Tab as the view and the replica show it, read once for every
/// Pane of the Tab that one reduction observes.
struct TabObservation<'v> {
    tab: TabId,
    attachment: WorkspaceAttachment,
    arrangement: &'v PaneArrangement,
    selected: bool,
}

impl<'v> TabObservation<'v> {
    fn new(
        view: &'v ClientViewState,
        replica: &ReplicaObservation<'_>,
        workspace: WorkspaceId,
        tab: TabId,
    ) -> Result<Self, Unobserved> {
        let workspace_view = view.workspace(workspace).ok_or(Unobserved::NotAttached)?;
        let attachment = workspace_view.attachment();
        if attachment.pairing() != replica.pairing() || attachment.client() != replica.client() {
            return Err(Unobserved::WrongPairing);
        }
        if let ViewAvailability::ResourceLimited { resource } = workspace_view.availability() {
            return Err(Unobserved::ResourceLimited(resource));
        }
        let arrangement = workspace_view.arrangement(tab).ok_or(Unobserved::Removed)?;
        let in_workspace = replica
            .tab_record(tab)
            .is_some_and(|record| record.workspace == workspace);
        if !in_workspace {
            return Err(Unobserved::Removed);
        }
        Ok(Self {
            tab,
            attachment,
            arrangement,
            selected: view.active_workspace() == Some(workspace)
                && workspace_view.active_tab() == Some(tab),
        })
    }

    /// The facts of a Pane this Tab's arrangement holds.
    fn pane(&self, replica: &ReplicaObservation<'_>, pane: PaneId) -> Result<Observed, Unobserved> {
        if replica.pane_tab(pane) != Some(self.tab) {
            return Err(Unobserved::Removed);
        }
        let record = replica.pane_runtime(pane).ok_or(Unobserved::Removed)?;
        Ok(Observed {
            attachment: self.attachment,
            session: SessionKey {
                epoch: replica.epoch(),
                pane,
                generation: record.session_generation,
            },
            phase: record.phase,
            control: record.control,
            control_generation: record.control_generation,
            connection: replica.connection(),
            ready: replica.readiness() == ReplicaReadiness::Ready,
            selected: self.selected,
            layout: self.arrangement.layout_revision(),
        })
    }
}

/// The explicit-use state of the single active Workspace Window.
#[derive(Clone, Debug, Default)]
pub struct StartupIntents {
    /// The last issued ticket. Tickets are never reused.
    issued: Option<UseTicket>,
    /// The current unsubmitted use.
    pending: Option<Record>,
    /// Per Pane, the latest use handed to client-core, kept only to suppress
    /// repeats and to show its progress. Each `begin` and `revalidate` drops
    /// every record of its Server whose Pane no longer shows the state the
    /// use was handed off for, so the records of a Server never outnumber its
    /// represented Panes. `cancel` drops them all.
    submitted: BTreeMap<PaneId, Record>,
}

impl StartupIntents {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> StartupState {
        match &self.pending {
            Some(record) => record.pending_state(),
            None if self.submitted.is_empty() => StartupState::Idle,
            None => StartupState::Submitted,
        }
    }

    /// The current unsubmitted use.
    pub fn pending(&self) -> Option<PaneUse> {
        self.pending
            .as_ref()
            .map(|record| record.pane_use(record.pending_state()))
    }

    /// The unsubmitted or in-flight use of one Pane.
    pub fn use_for(&self, pane: PaneId) -> Option<PaneUse> {
        if let Some(record) = self
            .pending
            .as_ref()
            .filter(|record| record.begun.target.pane == pane)
        {
            return Some(record.pane_use(record.pending_state()));
        }
        self.submitted
            .get(&pane)
            .map(|record| record.pane_use(StartupState::Submitted))
    }

    /// Begins an explicit use of the remembered Focused Pane of the Active Tab
    /// of the active Workspace.
    ///
    /// Control held by another Client and a Session this Client already runs
    /// request nothing. An unclaimed running or starting Pane requests
    /// compare-acquire only. A Failed or Directory Unavailable Pane starts
    /// only from Start Session or Retry; selecting it or opening its Tab
    /// compare-acquires it when unclaimed. A use already in flight for the
    /// Pane on the current connection is not repeated, but an acquisition in
    /// flight does not absorb a deliberate start. A new selection cancels an
    /// older unsubmitted use; a selection of the Pane already being started
    /// continues that use. Every failure, including ticket exhaustion, is
    /// returned before any effect or state change.
    pub fn begin(
        &mut self,
        facts: UseFacts<'_>,
        target: SelectionTarget,
        source: ExplicitUseSource,
    ) -> Result<StartupReduction, ViewError> {
        let observed =
            observe_target(facts.view, facts.replica, target).map_err(Unobserved::error)?;
        let current = observed
            .context(target, facts.context)
            .map_err(Unobserved::error)?;
        let focused = facts
            .view
            .workspace(target.workspace)
            .and_then(|workspace| workspace.arrangement(target.tab))
            .and_then(|arrangement| arrangement.focused_pane());
        if focused != Some(target.pane) {
            return Err(ViewError::StaleIntent);
        }
        // A use already handed off for this exact Pane state is not repeated.
        // A startup includes its compare-acquire; an acquisition starts nothing.
        let in_flight = self
            .submitted
            .get(&target.pane)
            .filter(|record| record.begun.target == target && record.in_flight(&observed))
            .map(|record| record.kind);
        let kind = decide(source, &current).filter(|&kind| match in_flight {
            None => true,
            Some(UseKind::Startup) => false,
            Some(UseKind::Acquisition) => kind == UseKind::Startup,
        });
        let ticket = match kind {
            Some(_) => Some(self.next_ticket()?),
            None => None,
        };
        self.prune_submitted(facts);
        let mut effects = Vec::new();
        let mut canceled = None;
        if self
            .pending
            .as_ref()
            .is_some_and(|record| record.begun.target == target)
        {
            canceled = self.advance(Ok(current), &mut effects);
            if canceled.is_none() {
                return Ok(self.reduction(effects, None));
            }
        }
        if let Some(older) = self.pending.take() {
            effects.extend(older.cancel_effect());
            canceled = Some(IntentCancelReason::Superseded);
        }
        match (kind, ticket) {
            (Some(UseKind::Startup), Some(ticket)) => {
                self.issued = Some(ticket);
                effects.push(StartupEffect::RequestUse {
                    ticket,
                    target,
                    source,
                    kind: source.kind(target.tab),
                    context: current,
                });
                effects.push(StartupEffect::RequestMeasurement {
                    ticket,
                    target,
                    context: current.context,
                    layout: current.layout,
                });
                self.pending = Some(Record::new(ticket, UseKind::Startup, current));
            }
            (Some(UseKind::Acquisition), Some(ticket)) => {
                self.issued = Some(ticket);
                effects.push(StartupEffect::AcquireControl {
                    ticket,
                    target,
                    context: current,
                });
                self.submitted.insert(
                    target.pane,
                    Record::new(ticket, UseKind::Acquisition, current),
                );
            }
            _ => {}
        }
        Ok(self.reduction(effects, canceled))
    }

    /// Records the server intent for a use. The use completes once it is
    /// measured and the exact grant is current.
    ///
    /// An intent for a canceled or superseded ticket, or for an acquisition,
    /// is canceled and starts nothing. A repeat for the submitted ticket is
    /// ignored. A ticket this state never issued, or an intent bound to
    /// another Pane, Client or content revision, is rejected without change.
    pub fn authorized(
        &mut self,
        facts: UseFacts<'_>,
        authorization: AuthorizedUse,
    ) -> Result<StartupReduction, ViewError> {
        let AuthorizedUse {
            ticket,
            intent,
            fence,
        } = authorization;
        if let Some(record) = self
            .pending
            .as_mut()
            .filter(|record| record.ticket == ticket)
        {
            if !record.server_matches(facts.replica) {
                return Err(ViewError::WrongServer);
            }
            let ours = record.begun.client();
            if fence.session().pane != record.begun.target.pane
                || fence.grant().client() != ours
                || intent.stream().client() != ours
                || intent.stream().pairing() != record.begun.attachment.pairing()
                || fence.content().is_some()
            {
                return Err(ViewError::WrongMembership);
            }
            let mut effects = Vec::new();
            match record.authorization {
                Some((known, _)) if known == intent => {}
                Some(_) => effects.push(StartupEffect::CancelUse { ticket, intent }),
                None => {
                    record.authorization = Some((intent, fence));
                    let canceled = self.revalidate_pending(facts, &mut effects);
                    return Ok(self.reduction(effects, canceled));
                }
            }
            return Ok(self.reduction(effects, None));
        }
        let effects = match self
            .submitted
            .values()
            .find(|record| record.ticket == ticket)
        {
            Some(record) => match record.kind {
                UseKind::Startup => Vec::new(),
                UseKind::Acquisition => vec![StartupEffect::CancelUse { ticket, intent }],
            },
            None if self.issued.is_some_and(|issued| ticket <= issued) => {
                vec![StartupEffect::CancelUse { ticket, intent }]
            }
            None => return Err(ViewError::StaleIntent),
        };
        Ok(self.reduction(effects, None))
    }

    /// Records measured dimensions for the current request. They complete an
    /// authorized use at most once and never begin one. A measurement for an
    /// older layout requests a current one; a stale, repeated or canceled
    /// measurement changes nothing.
    pub fn measured(
        &mut self,
        facts: UseFacts<'_>,
        measurement: Measurement,
    ) -> Result<StartupReduction, ViewError> {
        let Some(record) = self
            .pending
            .as_mut()
            .filter(|record| record.ticket == measurement.ticket)
        else {
            return if self
                .issued
                .is_some_and(|issued| measurement.ticket <= issued)
            {
                Ok(self.reduction(Vec::new(), None))
            } else {
                Err(ViewError::StaleIntent)
            };
        };
        if !record.server_matches(facts.replica) {
            return Err(ViewError::WrongServer);
        }
        if record.measurement.is_none()
            && measurement.context == record.begun.context
            && measurement.layout == record.requested
        {
            record.measurement = Some(measurement.grid);
        }
        let mut effects = Vec::new();
        let canceled = self.revalidate_pending(facts, &mut effects);
        Ok(self.reduction(effects, canceled))
    }

    /// Rechecks the uses of the replica's Server after any change of the view,
    /// replica, connection or native context. It cancels an unsubmitted use
    /// that lost its target, selection, control, connection, epoch, Session or
    /// context, requests a current measurement after a layout change, and
    /// completes a use whose grant became current. It never begins a use, so
    /// repaint, window focus, Settings closure and reconnect start nothing.
    pub fn revalidate(&mut self, facts: UseFacts<'_>) -> StartupReduction {
        self.prune_submitted(facts);
        let mut effects = Vec::new();
        let canceled = self.revalidate_pending(facts, &mut effects);
        self.reduction(effects, canceled)
    }

    /// Cancels the unsubmitted use, such as for detach or self-unpair, and
    /// forgets every in-flight one. Client-core settles submitted operations.
    pub fn cancel(&mut self, reason: IntentCancelReason) -> StartupReduction {
        self.submitted.clear();
        let Some(record) = self.pending.take() else {
            return self.reduction(Vec::new(), None);
        };
        let effects = record.cancel_effect().into_iter().collect();
        self.reduction(effects, Some(reason))
    }

    /// Applies a definitive server rejection of one ticket's request or
    /// completion. The server already holds no intent to cancel for it.
    pub fn rejected(&mut self, ticket: UseTicket) -> StartupReduction {
        self.submitted.retain(|_, record| record.ticket != ticket);
        if self
            .pending
            .as_ref()
            .is_some_and(|record| record.ticket == ticket)
        {
            self.pending = None;
            return self.reduction(Vec::new(), Some(IntentCancelReason::Rejected));
        }
        self.reduction(Vec::new(), None)
    }

    /// Drops the in-flight uses of the replica's Server whose Pane is gone or
    /// no longer shows the state the use was handed off for.
    ///
    /// Each Tab's view facts and arrangement are read once for all of its
    /// uses, and each Pane is found through the replica's index. One prune
    /// costs a few indexed lookups per use plus one sort per Tab that has
    /// uses, never a scan of the Server's Panes per use.
    fn prune_submitted(&mut self, facts: UseFacts<'_>) {
        let UseFacts { view, replica, .. } = facts;
        let mut tabs = BTreeMap::new();
        self.submitted.retain(|_, record| {
            if !record.server_matches(replica) {
                return true;
            }
            let target = record.begun.target;
            let cached = tabs
                .entry((target.workspace, target.tab))
                .or_insert_with(|| {
                    let observed =
                        TabObservation::new(view, replica, target.workspace, target.tab).ok()?;
                    let mut members = observed.arrangement.pane_ids();
                    members.sort_unstable();
                    Some((observed, members))
                });
            cached.as_ref().is_some_and(|(observed, members)| {
                members.binary_search(&target.pane).is_ok()
                    && observed
                        .pane(replica, target.pane)
                        .is_ok_and(|current| record.in_flight(&current))
            })
        });
    }

    fn next_ticket(&self) -> Result<UseTicket, ViewError> {
        match self.issued {
            None => Ok(UseTicket::FIRST),
            Some(issued) => issued
                .checked_successor()
                .ok_or(ViewCounter::UseTicket.exhausted()),
        }
    }

    fn revalidate_pending(
        &mut self,
        facts: UseFacts<'_>,
        effects: &mut Vec<StartupEffect>,
    ) -> Option<IntentCancelReason> {
        let record = self
            .pending
            .as_ref()
            .filter(|record| record.server_matches(facts.replica))?;
        let target = record.begun.target;
        let current = observe_target(facts.view, facts.replica, target)
            .and_then(|observed| observed.context(target, facts.context))
            .map_err(Unobserved::reason);
        self.advance(current, effects)
    }

    /// Cancels, re-requests measurement for, or completes the pending use.
    fn advance(
        &mut self,
        current: Result<UseContext, IntentCancelReason>,
        effects: &mut Vec<StartupEffect>,
    ) -> Option<IntentCancelReason> {
        let record = self.pending.as_mut()?;
        let checked = current.and_then(|current| Ok((current, record.check(&current)?)));
        let (current, grant_current) = match checked {
            Ok(checked) => checked,
            Err(reason) => {
                effects.extend(record.cancel_effect());
                self.pending = None;
                return Some(reason);
            }
        };
        if current.layout != record.requested {
            record.requested = current.layout;
            record.measurement = None;
            effects.push(StartupEffect::RequestMeasurement {
                ticket: record.ticket,
                target: current.target,
                context: current.context,
                layout: current.layout,
            });
        }
        if grant_current
            && let (Some((intent, fence)), Some(grid)) = (record.authorization, record.measurement)
        {
            effects.push(StartupEffect::CompleteUse {
                ticket: record.ticket,
                intent,
                grid,
                fence,
            });
            if let Some(record) = self.pending.take() {
                self.submitted.insert(record.begun.target.pane, record);
            }
        }
        None
    }

    fn reduction(
        &self,
        effects: Vec<StartupEffect>,
        canceled: Option<IntentCancelReason>,
    ) -> StartupReduction {
        StartupReduction {
            effects,
            state: self.state(),
            canceled,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_issued_for_test(&mut self, ticket: UseTicket) {
        self.issued = Some(ticket);
    }
}
