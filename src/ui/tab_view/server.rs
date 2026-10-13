//! Server-backed Tab Views.
//!
//! Membership and the Root Pane come from the server's replica. Layout, focus and zoom come from
//! this client's personal arrangement and change only through client-view operations. A Tab View
//! never creates, closes or reorders a shared Pane itself: a split asks the server, and a Pane
//! leaves only when a publication no longer holds it.

use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyWindowHandle, App, Context, Entity, Subscription, Window};
use spaceterm_client::view::{
    ArrangementMetrics, ArrangementNode, FocusDirection as ViewFocusDirection, PaneNodeRef,
    PaneTreeRef, SplitId, ZoomState,
};
use spaceterm_core::operations::CreatedEntities;
use spaceterm_core::organization::{SplitAxis as ServerSplitAxis, SplitEdge};

use super::{PaneCaptionText, PaneKey, Panes, ServerPaneId, ServerTabId, TabView, view_size};
use crate::domain::{FocusDirection, PaneSize, ServerTab, SplitAxis};
use crate::ui::pane_lifecycle::{PaneConstruction, RemoteHierarchyLifecycle};
use crate::ui::server_client_runtime::{
    DesktopClientEvent, DesktopClientRuntime, MAX_PENDING_UI_REQUESTS,
};
use crate::ui::server_desktop_contracts::{
    ArrangementEdit, ClientViewState, Command, DesktopClientError, DesktopRequest,
    DesktopServerFrame, DesktopViewRevision, OperationId, OrganizationSnapshot, Outcome,
    PaneArrangement, ReplicaStatus, SelectionSource, ServerId, TabRecord, UiRequestId, ViewEdit,
    WorkspaceId,
};
use crate::ui::tab_manager::{ServerEditRefused, ServerTabManagerEvent};
use crate::ui::{TerminalPane, TerminalPaneEvent};

/// Operation outcomes a creation intent keeps while its request has not named its operation.
const MAX_EARLY_OUTCOMES: usize = 128;

/// Creates the presentation of one server Pane.
pub(crate) trait ServerPaneFactory {
    fn create(
        &self,
        pane: ServerPaneId,
        runtime: &Entity<DesktopClientRuntime>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<TerminalPane>, DesktopClientError>;
}

/// A native presentation wraps only the runtime's authenticated Pane channel.
impl ServerPaneFactory for PaneConstruction {
    fn create(
        &self,
        pane: ServerPaneId,
        runtime: &Entity<DesktopClientRuntime>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<TerminalPane>, DesktopClientError> {
        use gpui::AppContext as _;
        let channel = runtime.read(cx).pane_channel(pane)?;
        let delivery = runtime.read(cx).pane_presentation(pane, None)?;
        let presentation = crate::ui::pane_presentation::PanePresentation::from_portable(
            delivery.decision.presentation,
            delivery.device.clone(),
            delivery.cause.clone(),
        );
        let session =
            crate::ui::server_pane_session::ServerPaneSession::bind(channel, delivery.token)?;
        let events = session
            .channel()
            .events()
            .map_err(DesktopClientError::from)?;
        let terminal = cx.new(|cx| {
            let mut terminal = self.create_bound_server(session, events, presentation, window, cx);
            terminal.bind_server_client(runtime.clone(), cx);
            terminal
        });
        Ok(terminal)
    }
}

/// A Tab View's presentation, layout or attention changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ServerTabPresentationChanged;

/// The parts of one frame a Tab View or Tab Manager reads. Records and arrangements always come
/// from the same frame.
#[derive(Clone)]
pub(crate) struct ServerPublication {
    pub(crate) server: ServerId,
    pub(crate) status: ReplicaStatus,
    pub(crate) organization: Arc<OrganizationSnapshot>,
    pub(crate) personal: Arc<ClientViewState>,
    pub(crate) view_revision: DesktopViewRevision,
}

impl ServerPublication {
    pub(crate) fn of(frame: &DesktopServerFrame) -> Self {
        Self {
            server: frame.server,
            status: frame.replica_status,
            organization: Arc::clone(&frame.organization),
            personal: Arc::clone(&frame.personal),
            view_revision: frame.view_revision,
        }
    }

    /// `tab`'s Workspace, record and personal arrangement.
    pub(crate) fn tab(
        &self,
        tab: ServerTabId,
    ) -> Result<(WorkspaceId, &TabRecord, &PaneArrangement), DesktopClientError> {
        if tab.server() != self.server {
            return Err(DesktopClientError::WrongServer);
        }
        let record = self
            .organization
            .state()
            .tabs
            .iter()
            .find(|record| record.id == tab)
            .ok_or(DesktopClientError::TargetGone)?;
        let arrangement = self
            .personal
            .workspace(record.workspace)
            .and_then(|view| view.arrangement(tab))
            .ok_or(DesktopClientError::TargetGone)?;
        Ok((record.workspace, record, arrangement))
    }
}

/// A server Tab whose Panes have their presentations, ready to become a Tab View.
pub(crate) struct ServerTabParts {
    workspace: WorkspaceId,
    status: ReplicaStatus,
    view_revision: DesktopViewRevision,
    tab: ServerTab<Entity<TerminalPane>>,
}

impl ServerTabParts {
    pub(crate) const fn id(&self) -> ServerTabId {
        self.tab.id()
    }
}

/// A server Tab's Panes and the state that keeps them consistent with the replica.
pub(super) struct ServerPanes {
    workspace: WorkspaceId,
    pub(super) tab: ServerTab<Entity<TerminalPane>>,
    /// The latest applied publication's arrangement, beneath admitted personal edits.
    published: PaneArrangement,
    published_revision: DesktopViewRevision,
    status: ReplicaStatus,
    pub(super) minimum_pane_size: PaneSize,
    runtime: Entity<DesktopClientRuntime>,
    factory: Rc<dyn ServerPaneFactory>,
    window: AnyWindowHandle,
    /// Local edits waiting for an allocation, or admitted edits awaiting completion and a receipted frame.
    edits: VecDeque<PendingArrangementEdit>,
    /// A resize applied here and sent when the interaction ends.
    live: Option<TargetedArrangementEdit>,
    /// A split this Tab View asked for, focused once the server names it and publishes it.
    creation: Option<CreationIntent>,
    materialize_scheduled: bool,
    _runtime: Subscription,
}

/// Relative attention edits retain their accepted result so an already-reflected publication
/// cannot apply a second toggle or keyboard step.
struct PendingArrangementEdit {
    /// Split-dependent edits wait here until preceding Moves have receipted allocations.
    request: Option<UiRequestId>,
    admitted_after: DesktopViewRevision,
    replay: ArrangementReplay,
    /// Keep the admitted checkpoint even when an older publication requires a different replay.
    result: PaneArrangement,
    rebased: Option<PaneArrangement>,
    completed: bool,
    applied: Option<DesktopViewRevision>,
}

/// A replay optimization, never an application acknowledgement. Remote
/// insertion can wrap a known Pane in a new Split; remote deletion can collapse an old Split.
/// Compare only branches that still separate known Panes, preserving their IDs, axes and ratios.
fn arrangement_matches(expected: &PaneArrangement, published: &PaneArrangement) -> bool {
    #[derive(PartialEq)]
    enum Fact {
        Pane(ServerPaneId),
        Split(SplitId, ServerSplitAxis, f32),
    }

    fn facts(tree: PaneTreeRef<'_>, known: &BTreeSet<ServerPaneId>, out: &mut Vec<Fact>) -> usize {
        match tree.node() {
            PaneNodeRef::Pane(pane) if known.contains(&pane) => {
                out.push(Fact::Pane(pane));
                1
            }
            PaneNodeRef::Pane(_) => 0,
            PaneNodeRef::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => {
                let first = facts(first, known, out);
                let second = facts(second, known, out);
                if first > 0 && second > 0 {
                    out.push(Fact::Split(id, axis, ratio));
                }
                first + second
            }
        }
    }

    let known: BTreeSet<_> = expected
        .pane_ids()
        .into_iter()
        .filter(|pane| published.contains_pane(*pane))
        .collect();
    if known.is_empty() {
        return true;
    }
    if expected
        .focused_pane()
        .is_some_and(|pane| known.contains(&pane))
        && expected.focused_pane() != published.focused_pane()
    {
        return false;
    }
    let surviving_zoom = match expected.zoom() {
        ZoomState::Zoomed(pane) if !known.contains(&pane) => ZoomState::Restored,
        zoom => zoom,
    };
    if surviving_zoom != published.zoom() {
        return false;
    }
    // When the old Focused Pane is gone, its published fallback owns the history change.
    if expected
        .focused_pane()
        .is_some_and(|pane| known.contains(&pane))
        && !expected
            .focus_history()
            .iter()
            .filter(|pane| known.contains(pane))
            .eq(published
                .focus_history()
                .iter()
                .filter(|pane| known.contains(pane)))
    {
        return false;
    }
    let mut before = Vec::new();
    let mut after = Vec::new();
    facts(expected.tree(), &known, &mut before);
    facts(published.tree(), &known, &mut after);
    before == after
}

/// A provider-allocated Split, or its Pane partition while allocation is pending.
/// Remote insertion can wrap either child; deletion of a whole child removes the pending target.
/// Once bound, a deleted Split cannot be replaced by a later Move with the same partition.
enum SplitTarget {
    Allocated(SplitId),
    Pending {
        first: BTreeSet<ServerPaneId>,
        second: BTreeSet<ServerPaneId>,
    },
}

impl SplitTarget {
    fn capture(arrangement: &PaneArrangement, split: SplitId) -> Option<Self> {
        let record = arrangement.export();
        let (first, second) = record.nodes.iter().find_map(|node| match node {
            ArrangementNode::Split {
                id, first, second, ..
            } if *id == split => Some((*first, *second)),
            _ => None,
        })?;
        let panes = |root| {
            let mut panes = BTreeSet::new();
            let mut stack = vec![root];
            while let Some(index) = stack.pop() {
                match record.nodes[index as usize] {
                    ArrangementNode::Pane(pane) => {
                        panes.insert(pane);
                    }
                    ArrangementNode::Split { first, second, .. } => {
                        stack.extend([first, second]);
                    }
                }
            }
            panes
        };
        Some(Self::Pending {
            first: panes(first),
            second: panes(second),
        })
    }

    fn resolve(&self, arrangement: &PaneArrangement) -> Option<SplitId> {
        let record = arrangement.export();
        let (first, second) = match self {
            Self::Pending { first, second } => (first, second),
            Self::Allocated(split) => {
                return record
                    .nodes
                    .iter()
                    .any(|node| matches!(node, ArrangementNode::Split { id, .. } if id == split))
                    .then_some(*split);
            }
        };
        let mut counts = vec![(0, 0); record.nodes.len()];
        // The validated flat table is pre-order, so children are counted before parents.
        for (index, node) in record.nodes.iter().enumerate().rev() {
            counts[index] = match *node {
                ArrangementNode::Pane(pane) => (
                    usize::from(first.contains(&pane)),
                    usize::from(second.contains(&pane)),
                ),
                ArrangementNode::Split { first, second, .. } => (
                    counts[first as usize].0 + counts[second as usize].0,
                    counts[first as usize].1 + counts[second as usize].1,
                ),
            };
        }
        let (first_count, second_count) = *counts.first()?;
        if first_count == 0 || second_count == 0 {
            return None;
        }
        record.nodes.iter().find_map(|node| match *node {
            ArrangementNode::Split {
                id, first, second, ..
            } if counts[first as usize] == (first_count, 0)
                && counts[second as usize] == (0, second_count) =>
            {
                Some(id)
            }
            _ => None,
        })
    }
}

struct TargetedArrangementEdit {
    edit: ArrangementEdit,
    split: Option<SplitTarget>,
}

impl TargetedArrangementEdit {
    fn capture(
        edit: ArrangementEdit,
        arrangement: &PaneArrangement,
        published: &PaneArrangement,
    ) -> Self {
        let split = match edit {
            ArrangementEdit::Resize { split, .. } | ArrangementEdit::SetSplitAxis { split, .. } => {
                let allocated = SplitTarget::Allocated(split);
                if allocated.resolve(published).is_some() {
                    Some(allocated)
                } else {
                    SplitTarget::capture(arrangement, split)
                }
            }
            _ => None,
        };
        Self { edit, split }
    }

    fn resolve(&self, arrangement: &PaneArrangement) -> Option<ArrangementEdit> {
        let edit = match self.edit {
            ArrangementEdit::Resize {
                available,
                metrics,
                ratio,
                ..
            } => ArrangementEdit::Resize {
                split: self.split.as_ref()?.resolve(arrangement)?,
                available,
                metrics,
                ratio,
            },
            ArrangementEdit::SetSplitAxis {
                axis,
                available,
                metrics,
                ..
            } => ArrangementEdit::SetSplitAxis {
                split: self.split.as_ref()?.resolve(arrangement)?,
                axis,
                available,
                metrics,
            },
            edit => edit,
        };
        Some(edit)
    }

    fn bind_allocation(&mut self, edit: ArrangementEdit) {
        if let ArrangementEdit::Resize { split, .. } | ArrangementEdit::SetSplitAxis { split, .. } =
            edit
        {
            self.edit = edit;
            self.split = Some(SplitTarget::Allocated(split));
        }
    }

    fn needs_allocation(&self) -> bool {
        self.split.is_some() || matches!(self.edit, ArrangementEdit::Move { .. })
    }
}

enum ArrangementReplay {
    Edit(TargetedArrangementEdit),
    CycleFocus(ServerPaneId),
    Zoom(spaceterm_client::view::ZoomState),
}

impl ServerPanes {
    pub(super) fn cancel_creation(&mut self) {
        self.creation = None;
    }

    pub(super) const fn shared_action_refusal(&self) -> Option<DesktopClientError> {
        shared_unavailable(self.status)
    }

    pub(super) fn metrics(&self, gap: f32) -> Option<ArrangementMetrics> {
        Some(ArrangementMetrics {
            minimum_leaf: view_size(self.minimum_pane_size)?,
            divider: gap,
        })
    }

    fn retire_edits(&mut self) {
        self.edits.retain(|pending| {
            !pending.completed
                || pending
                    .applied
                    .is_none_or(|applied| applied > self.published_revision)
        });
    }

    /// Replays the optimistic overlay without acknowledging equal results. Deleted targets stay gone.
    fn reapply_edits(&mut self) {
        // Matching contents can avoid a redundant structural replay in this presentation, but
        // cannot acknowledge a request. Recompute this prefix on every frame and every rollback.
        let represented = self.edits.iter().rposition(|pending| {
            arrangement_matches(&pending.result, &self.published)
                || pending
                    .rebased
                    .as_ref()
                    .is_some_and(|rebased| arrangement_matches(rebased, &self.published))
        });
        for (index, pending) in self.edits.iter_mut().enumerate() {
            if represented.is_some_and(|last| index <= last)
                || pending
                    .applied
                    .is_some_and(|applied| applied <= self.published_revision)
            {
                continue;
            }
            match &pending.replay {
                ArrangementReplay::Edit(edit) => {
                    if let Some(edit) = edit.resolve(self.tab.arrangement()) {
                        let _ = self.tab.arrange(edit);
                    }
                }
                ArrangementReplay::CycleFocus(pane) => {
                    self.tab.focus_in_cycle(*pane);
                }
                ArrangementReplay::Zoom(zoom) => {
                    if let spaceterm_client::view::ZoomState::Zoomed(pane) = zoom
                        && !self.tab.arrangement().contains_pane(*pane)
                    {
                        continue;
                    }
                    if matches!(
                        self.tab.zoom(),
                        spaceterm_client::view::ZoomState::Zoomed(_)
                    ) != matches!(zoom, spaceterm_client::view::ZoomState::Zoomed(_))
                    {
                        let _ = self.tab.arrange(ArrangementEdit::ToggleZoom);
                    }
                }
            }
            pending.rebased = Some(self.tab.arrangement().clone());
        }
        if let Some(live) = &self.live
            && live
                .resolve(self.tab.arrangement())
                .is_none_or(|edit| self.tab.arrange(edit).is_err())
        {
            self.live = None;
        }
    }

    fn has_unpublished_move(&self) -> bool {
        self.edits.iter().any(|pending| {
            matches!(&pending.replay, ArrangementReplay::Edit(edit) if matches!(edit.edit, ArrangementEdit::Move { .. }))
                && pending.applied.is_none_or(|revision| revision > self.published_revision)
        })
    }

    /// Observe a Move's actual allocation before submitting another Move or a Split edit.
    /// A predicted counter is not a provider identity, even when an equal checkpoint is visible.
    fn submit_waiting_edits(&mut self, cx: &mut Context<TabView>) {
        let mut waiting_for_move = false;
        let mut refused = Vec::new();
        for pending in &mut self.edits {
            let ArrangementReplay::Edit(targeted) = &mut pending.replay else {
                continue;
            };
            let is_move = matches!(targeted.edit, ArrangementEdit::Move { .. });
            if pending.request.is_some() {
                waiting_for_move |= is_move
                    && pending
                        .applied
                        .is_none_or(|revision| revision > self.published_revision);
                continue;
            }
            if waiting_for_move {
                continue;
            }
            let admission = targeted
                .resolve(&self.published)
                .ok_or(DesktopClientError::TargetGone)
                .and_then(|edit| {
                    targeted.bind_allocation(edit);
                    self.runtime.update(cx, |runtime, cx| {
                        runtime.request(
                            DesktopRequest::PersonalEdit(ViewEdit::Arrange {
                                workspace: self.workspace,
                                tab: self.tab.id(),
                                edit,
                            }),
                            cx,
                        )
                    })
                });
            match admission {
                Ok(request) => {
                    pending.request = Some(request);
                    pending.admitted_after = self.published_revision;
                    waiting_for_move |= is_move;
                }
                Err(error) => {
                    refused.push(error);
                    // This unsent edit never reached the provider and needs no receipt.
                    pending.completed = true;
                }
            }
        }
        self.edits
            .retain(|pending| pending.request.is_some() || !pending.completed);
        self.retire_edits();
        if !refused.is_empty() {
            self.restore_published();
            for error in refused {
                cx.emit(ServerEditRefused(error));
            }
        }
    }

    fn restore_published(&mut self) {
        let record = self.tab.record().clone();
        // Personal edits do not change membership, so rollback releases no payload.
        let _ = self.tab.apply(record, self.published.clone());
        self.reapply_edits();
    }
}

/// What a creation intent learned from one runtime event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IntentProgress {
    Waiting,
    /// The request failed, was refused, or created something else.
    Cancelled(Option<DesktopClientError>),
    /// The outcome names what the request created.
    Named(CreatedEntities),
}

/// One post-accept activation intent. It completes only after the outcome identifies the target
/// and a publication holds it, so the outcome and the publication may arrive in either order.
#[derive(Debug)]
pub(crate) struct CreationIntent {
    request: UiRequestId,
    operation: Option<OperationId>,
    /// Creation outcomes that arrived before the request named its operation, oldest first.
    early: VecDeque<(OperationId, Option<CreatedEntities>)>,
    target: Option<CreatedEntities>,
}

impl CreationIntent {
    pub(crate) const fn new(request: UiRequestId) -> Self {
        Self {
            request,
            operation: None,
            early: VecDeque::new(),
            target: None,
        }
    }

    pub(crate) const fn target(&self) -> Option<CreatedEntities> {
        self.target
    }

    /// Follows the request through one runtime event of `server`.
    pub(crate) fn observe(
        &mut self,
        server: ServerId,
        event: &DesktopClientEvent,
    ) -> IntentProgress {
        if self.target.is_some() {
            return IntentProgress::Waiting;
        }
        match event {
            DesktopClientEvent::RequestCompleted { request, operation }
                if *request == self.request =>
            {
                let Some(operation) = *operation else {
                    return IntentProgress::Cancelled(None);
                };
                self.operation = Some(operation);
                let early = std::mem::take(&mut self.early);
                match early
                    .into_iter()
                    .find(|(candidate, _)| *candidate == operation)
                {
                    Some((_, created)) => self.resolve(created),
                    None => IntentProgress::Waiting,
                }
            }
            DesktopClientEvent::RequestFailed { request, error } if *request == self.request => {
                IntentProgress::Cancelled(Some(*error))
            }
            DesktopClientEvent::OperationOutcome {
                server: from,
                operation,
                outcome,
            } if *from == server => match self.operation {
                Some(mine) if mine == *operation => self.resolve(created(outcome)),
                Some(_) => IntentProgress::Waiting,
                None => {
                    if self.early.len() == MAX_EARLY_OUTCOMES {
                        self.early.pop_front();
                    }
                    self.early.push_back((*operation, created(outcome)));
                    IntentProgress::Waiting
                }
            },
            _ => IntentProgress::Waiting,
        }
    }

    fn resolve(&mut self, created: Option<CreatedEntities>) -> IntentProgress {
        match created {
            Some(entities) => {
                self.target = Some(entities);
                IntentProgress::Named(entities)
            }
            None => IntentProgress::Cancelled(None),
        }
    }
}

/// What an outcome created, if it created organization.
fn created(outcome: &Outcome) -> Option<CreatedEntities> {
    match *outcome {
        Outcome::CreatedWorkspace {
            workspace,
            tab,
            pane,
        } => Some(CreatedEntities::Workspace {
            workspace,
            tab,
            pane,
        }),
        Outcome::CreatedTab { tab, pane } => Some(CreatedEntities::Tab { tab, pane }),
        Outcome::CreatedPane { pane } => Some(CreatedEntities::Pane(pane)),
        Outcome::CreatedWithControl(created) => Some(created.entities()),
        _ => None,
    }
}

/// Why a shared command cannot be sent now. A frame that is not the server's current publication
/// permits personal edits only, and nothing is queued for later.
pub(crate) const fn shared_unavailable(status: ReplicaStatus) -> Option<DesktopClientError> {
    match status {
        ReplicaStatus::Ready => None,
        ReplicaStatus::Resynchronizing => Some(DesktopClientError::Reconciling),
        ReplicaStatus::RetainedOffline => Some(DesktopClientError::Offline),
    }
}

pub(super) const fn view_direction(direction: FocusDirection) -> ViewFocusDirection {
    match direction {
        FocusDirection::Left => ViewFocusDirection::Left,
        FocusDirection::Right => ViewFocusDirection::Right,
        FocusDirection::Up => ViewFocusDirection::Up,
        FocusDirection::Down => ViewFocusDirection::Down,
    }
}

const fn server_axis(axis: SplitAxis) -> ServerSplitAxis {
    match axis {
        SplitAxis::Horizontal => ServerSplitAxis::Horizontal,
        SplitAxis::Vertical => ServerSplitAxis::Vertical,
    }
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the Workspace Manager presents server Tabs once Panes host server sessions"
    )
)]
impl TabView {
    /// Presents `tab` from `frame`. Every Pane needs a presentation from `panes`; one that cannot
    /// be created fails the whole Tab View.
    pub(crate) fn new_server(
        frame: &DesktopServerFrame,
        tab: ServerTabId,
        runtime: Entity<DesktopClientRuntime>,
        panes: PaneConstruction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Self, DesktopClientError> {
        Self::new_server_with(
            &ServerPublication::of(frame),
            tab,
            runtime,
            Rc::new(panes),
            window,
            cx,
        )
    }

    pub(crate) fn new_server_with(
        publication: &ServerPublication,
        tab: ServerTabId,
        runtime: Entity<DesktopClientRuntime>,
        factory: Rc<dyn ServerPaneFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Self, DesktopClientError> {
        let parts = Self::prepare_server(publication, tab, &runtime, factory.as_ref(), window, cx)?;
        Ok(Self::from_server_parts(parts, runtime, factory, window, cx))
    }

    /// Reads `tab` from `publication` and creates its Panes' presentations. Nothing is presented
    /// when it fails.
    pub(crate) fn prepare_server(
        publication: &ServerPublication,
        tab: ServerTabId,
        runtime: &Entity<DesktopClientRuntime>,
        factory: &dyn ServerPaneFactory,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<ServerTabParts, DesktopClientError> {
        let (workspace, record, arrangement) = publication.tab(tab)?;
        let mut server_tab = ServerTab::new(record.clone(), arrangement.clone())
            .map_err(|_| DesktopClientError::StaleContext)?;
        for pane in server_tab.missing() {
            let terminal = factory.create(pane, runtime, window, cx)?;
            server_tab
                .insert(pane, terminal)
                .map_err(|_| DesktopClientError::StaleContext)?;
        }
        Ok(ServerTabParts {
            workspace,
            status: publication.status,
            view_revision: publication.view_revision,
            tab: server_tab,
        })
    }

    pub(crate) fn from_server_parts(
        parts: ServerTabParts,
        runtime: Entity<DesktopClientRuntime>,
        factory: Rc<dyn ServerPaneFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let ServerTabParts {
            workspace,
            status,
            view_revision,
            tab,
        } = parts;
        let mut pane_titles = std::collections::BTreeMap::new();
        let mut pane_captions = std::collections::BTreeMap::new();
        let mut pane_attention = std::collections::BTreeMap::new();
        for (pane, terminal) in tab.panes() {
            let key = PaneKey::Server(pane);
            pane_titles.insert(key, terminal.read(cx).title());
            pane_captions.insert(key, PaneCaptionText::from_terminal(terminal.read(cx)));
            pane_attention.insert(key, 0);
            Self::observe_server_pane(pane, terminal, cx);
        }
        let subscription = cx.subscribe(&runtime, |view: &mut Self, _, event, cx| {
            view.on_runtime_event(event, cx);
        });
        let published = tab.arrangement().clone();
        Self {
            panes: Panes::Server(Box::new(ServerPanes {
                workspace,
                tab,
                published,
                published_revision: view_revision,
                status,
                minimum_pane_size: Self::initial_minimum_pane_size(cx),
                runtime,
                factory,
                window: window.window_handle(),
                edits: VecDeque::new(),
                live: None,
                creation: None,
                materialize_scheduled: false,
                _runtime: subscription,
            })),
            pane_bounds: std::collections::BTreeMap::new(),
            pane_layout_size: None,
            pending_split: None,
            split_bounds: std::collections::BTreeMap::new(),
            pane_titles,
            pane_captions,
            pane_attention,
            resizing_split_id: None,
            pane_drag: None,
            active: true,
            focus_branch_blocker: None,
            native_service_hierarchy_generation: 0,
            native_service_focus_signature: None,
            pane_layout_revision: 0,
            revealed_layout_revision: 0,
            close_tab_requested: false,
            remote_lifecycle: RemoteHierarchyLifecycle::default(),
            tab_focus: cx.focus_handle(),
        }
    }

    /// Applies one coherent publication of this Tab. It never acquires control, starts a Session
    /// or selects a Pane. A Tab View presenting a local or SSH Tab refuses every publication.
    pub(crate) fn apply_tab(
        &mut self,
        frame: &DesktopServerFrame,
        record: &TabRecord,
        arrangement: &PaneArrangement,
        cx: &mut Context<Self>,
    ) -> Result<(), DesktopClientError> {
        self.apply_server_tab(
            frame.replica_status,
            frame.server,
            frame.view_revision,
            record,
            arrangement,
            cx,
        )
    }

    pub(crate) fn apply_server_tab(
        &mut self,
        status: ReplicaStatus,
        server: ServerId,
        revision: DesktopViewRevision,
        record: &TabRecord,
        arrangement: &PaneArrangement,
        cx: &mut Context<Self>,
    ) -> Result<(), DesktopClientError> {
        let Panes::Server(panes) = &mut self.panes else {
            return Err(DesktopClientError::WrongServer);
        };
        if server != panes.tab.id().server() || record.id.server() != server {
            return Err(DesktopClientError::WrongServer);
        }
        if record.id != panes.tab.id() || arrangement.tab() != record.id {
            return Err(DesktopClientError::StaleContext);
        }
        if revision < panes.published_revision {
            return Err(DesktopClientError::StaleContext);
        }
        let focused_before = panes.tab.focused_pane();
        if !arrangement.contains_pane(focused_before) {
            panes.creation = None;
        }
        let resizing = match self.resizing_split_id {
            Some(super::SplitKey::Server(split)) => {
                SplitTarget::capture(panes.tab.arrangement(), split)
            }
            _ => None,
        };
        let layout_before = panes.tab.arrangement().layout_revision();
        panes.published_revision = revision;
        let released = panes
            .tab
            .apply(record.clone(), arrangement.clone())
            .map_err(|_| DesktopClientError::StaleContext)?;
        panes.published = arrangement.clone();
        panes.status = status;
        panes.reapply_edits();
        panes.submit_waiting_edits(cx);
        self.resizing_split_id = resizing
            .and_then(|target| target.resolve(panes.tab.arrangement()))
            .map(super::SplitKey::Server);
        let layout_changed = panes.tab.arrangement().layout_revision() != layout_before;
        let focus_moved = panes.tab.focused_pane() != focused_before;

        for (pane, terminal) in released {
            let key = PaneKey::Server(pane);
            self.pane_bounds.remove(&key);
            self.pane_titles.remove(&key);
            self.pane_captions.remove(&key);
            self.pane_attention.remove(&key);
            // Dropping the Pane releases its presentation only. Its Session belongs to the server.
            terminal.update(cx, |terminal, _| {
                terminal.set_accessibility_hierarchy(false);
                terminal.close_presentation();
            });
        }
        // A drag or resize whose Pane or Split the publication deleted is cancelled.
        if self.pane_drag.as_ref().is_some_and(|drag| {
            !self.panes.contains(drag.pane_id)
                || drag
                    .drop_target
                    .is_some_and(|(target, _)| !self.panes.contains(target))
        }) {
            self.pane_drag = None;
        }
        if let Some(split) = self.resizing_split_id
            && self.server_split_ratio_of(split).is_none()
        {
            self.resizing_split_id = None;
            if let Panes::Server(panes) = &mut self.panes {
                panes.live = None;
            }
        }
        if layout_changed {
            self.advance_native_service_hierarchy_generation(cx);
            self.pane_layout_revision += 1;
            self.split_bounds.clear();
        }
        self.schedule_materialize(cx);
        self.complete_creation(cx);
        self.sync_terminal_focus(cx);
        if focus_moved && self.active {
            // A remote close moved focus to the replacement Pane. Native focus follows it, which
            // is restoration rather than selection.
            self.defer_native_focus(cx);
        }
        self.presentation_changed(cx);
        cx.notify();
        Ok(())
    }

    /// Records the provider's successful application receipt, forwarded by the Tab Manager.
    /// Request completion or an equal arrangement is not proof that a frame includes an edit.
    pub(crate) fn personal_edit_applied(
        &mut self,
        request: UiRequestId,
        revision: DesktopViewRevision,
        cx: &mut Context<Self>,
    ) {
        let Panes::Server(panes) = &mut self.panes else {
            return;
        };
        let Some(pending) = panes
            .edits
            .iter_mut()
            .find(|pending| pending.request == Some(request))
        else {
            return;
        };
        if revision <= pending.admitted_after {
            cx.emit(ServerEditRefused(DesktopClientError::StaleContext));
            return;
        }
        pending.applied.get_or_insert(revision);
        if revision <= panes.published_revision {
            let status = panes.status;
            let server = panes.tab.id().server();
            let record = panes.tab.record().clone();
            let arrangement = panes.published.clone();
            let revision = panes.published_revision;
            if let Err(error) =
                self.apply_server_tab(status, server, revision, &record, &arrangement, cx)
            {
                cx.emit(ServerEditRefused(error));
            }
        }
    }

    /// Quit/save composition must wait for these local edits to reach the provider.
    /// Request completion alone cannot flush an edit waiting for a Move allocation.
    pub(crate) fn has_waiting_personal_edits(&self) -> bool {
        match &self.panes {
            Panes::Server(panes) => panes.edits.iter().any(|pending| pending.request.is_none()),
            Panes::Local(_) => false,
        }
    }

    /// Personal edits whose completion, application receipt and publication have not all arrived.
    pub(crate) fn has_unsettled_personal_edits(&self) -> bool {
        matches!(&self.panes, Panes::Server(panes) if !panes.edits.is_empty())
    }

    /// The server Pane that has native focus within this Tab View, if any.
    pub(crate) fn focused_server_pane(&self) -> Option<ServerPaneId> {
        match &self.panes {
            Panes::Server(panes) => Some(panes.tab.focused_pane()),
            Panes::Local(_) => None,
        }
    }

    /// The arrangement this Tab View presents, including unsent personal edits.
    #[cfg(test)]
    pub(crate) fn server_arrangement(&self) -> &PaneArrangement {
        match &self.panes {
            Panes::Server(panes) => panes.tab.arrangement(),
            Panes::Local(_) => panic!("a local Tab View has no server arrangement"),
        }
    }

    /// The server Panes that have a presentation, in visual order.
    #[cfg(test)]
    pub(crate) fn presented_server_panes(&self) -> Vec<(ServerPaneId, gpui::EntityId)> {
        match &self.panes {
            Panes::Server(panes) => panes
                .tab
                .panes()
                .map(|(pane, terminal)| (pane, terminal.entity_id()))
                .collect(),
            Panes::Local(_) => Vec::new(),
        }
    }

    /// A presented server Pane's Terminal Pane.
    #[cfg(test)]
    pub(crate) fn server_terminal(&self, pane: ServerPaneId) -> Option<Entity<TerminalPane>> {
        self.panes.pane(PaneKey::Server(pane)).cloned()
    }

    #[cfg(test)]
    pub(crate) fn server_root_pane(&self) -> Option<ServerPaneId> {
        match &self.panes {
            Panes::Server(panes) => Some(panes.tab.root_pane()),
            Panes::Local(_) => None,
        }
    }

    /// Reports a person's explicit selection of a server Pane.
    pub(super) fn select_server_pane(
        &mut self,
        pane: ServerPaneId,
        source: SelectionSource,
        cx: &mut Context<Self>,
    ) {
        // A person moved on, so a pending split no longer takes focus when it appears.
        if let Panes::Server(panes) = &mut self.panes {
            panes.creation = None;
        }
        cx.emit(ServerTabManagerEvent::SelectPane { pane, source });
    }

    /// Applies one personal edit here and sends it. A refusal keeps other admitted edits.
    pub(super) fn arrange(&mut self, edit: ArrangementEdit, cx: &mut Context<Self>) -> bool {
        let Panes::Server(panes) = &mut self.panes else {
            return false;
        };
        let focused_before = panes.tab.focused_pane();
        if !matches!(panes.tab.arrange(edit), Ok(true)) {
            return false;
        }
        let targeted =
            TargetedArrangementEdit::capture(edit, panes.tab.arrangement(), &panes.published);
        let accepted = self.send_arrangement(targeted, cx);
        if accepted
            && let Panes::Server(panes) = &mut self.panes
            && panes.tab.focused_pane() != focused_before
        {
            // Post-create attention belongs to the originating focus context, not to use intent.
            panes.cancel_creation();
        }
        accepted
    }

    /// Applies one personal edit here only, for an interaction that sends it when it ends.
    pub(super) fn arrange_live(&mut self, edit: ArrangementEdit, cx: &mut Context<Self>) {
        let Panes::Server(panes) = &mut self.panes else {
            return;
        };
        if panes.tab.arrange(edit).is_ok() {
            panes.live = Some(TargetedArrangementEdit::capture(
                edit,
                panes.tab.arrangement(),
                &panes.published,
            ));
            cx.notify();
        }
    }

    /// Sends the edit an interaction applied, once, when the interaction ends.
    pub(super) fn commit_live_edit(&mut self, cx: &mut Context<Self>) {
        let Panes::Server(panes) = &mut self.panes else {
            return;
        };
        if let Some(mut live) = panes.live.take()
            && let Some(edit) = live.resolve(panes.tab.arrangement())
        {
            live.edit = edit;
            self.send_arrangement(live, cx);
        }
    }

    fn send_arrangement(
        &mut self,
        mut targeted: TargetedArrangementEdit,
        cx: &mut Context<Self>,
    ) -> bool {
        let Panes::Server(panes) = &mut self.panes else {
            return false;
        };
        let edit = targeted.edit;
        let waits_for_allocation = targeted.needs_allocation() && panes.has_unpublished_move();
        let submission = if waits_for_allocation {
            Ok(None)
        } else {
            targeted
                .resolve(&panes.published)
                .ok_or(DesktopClientError::TargetGone)
                .map(Some)
        };
        if let Ok(Some(edit)) = submission {
            targeted.bind_allocation(edit);
        }
        let replay = match edit {
            ArrangementEdit::FocusPrevious | ArrangementEdit::FocusNext => {
                ArrangementReplay::CycleFocus(panes.tab.focused_pane())
            }
            ArrangementEdit::FocusDirection(_) => {
                ArrangementReplay::Edit(TargetedArrangementEdit::capture(
                    ArrangementEdit::Focus(panes.tab.focused_pane()),
                    panes.tab.arrangement(),
                    &panes.published,
                ))
            }
            ArrangementEdit::ToggleZoom => ArrangementReplay::Zoom(panes.tab.zoom()),
            _ => ArrangementReplay::Edit(targeted),
        };
        let focused_before = panes.tab.focused_pane();
        let admission = if panes.edits.len() >= MAX_PENDING_UI_REQUESTS {
            Err(DesktopClientError::AdmissionFull)
        } else {
            submission.and_then(|edit| {
                edit.map(|edit| {
                    panes.runtime.update(cx, |runtime, cx| {
                        runtime.request(
                            DesktopRequest::PersonalEdit(ViewEdit::Arrange {
                                workspace: panes.workspace,
                                tab: panes.tab.id(),
                                edit,
                            }),
                            cx,
                        )
                    })
                })
                .transpose()
            })
        };
        match admission {
            Ok(request) => {
                panes.edits.push_back(PendingArrangementEdit {
                    request,
                    admitted_after: panes.published_revision,
                    replay,
                    result: panes.tab.arrangement().clone(),
                    rebased: None,
                    completed: false,
                    applied: None,
                });
                true
            }
            Err(error) => {
                panes.restore_published();
                let focus_moved = panes.tab.focused_pane() != focused_before;
                self.sync_terminal_focus(cx);
                if focus_moved {
                    self.defer_native_focus(cx);
                }
                cx.emit(ServerEditRefused(error));
                cx.notify();
                false
            }
        }
    }

    /// The ratio a server Split now has.
    pub(super) fn server_split_ratio(&self, split: spaceterm_client::view::SplitId) -> Option<f32> {
        self.server_split_ratio_of(super::SplitKey::Server(split))
    }

    fn server_split_ratio_of(&self, split: super::SplitKey) -> Option<f32> {
        fn find(tree: super::LayoutRef<'_>, split: super::SplitKey) -> Option<f32> {
            match tree.node() {
                super::LayoutNode::Leaf(_) => None,
                super::LayoutNode::Split {
                    split_id,
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    if split_id == split {
                        Some(ratio)
                    } else {
                        find(first, split).or_else(|| find(second, split))
                    }
                }
            }
        }
        find(self.panes.layout(), split)
    }

    /// Asks the server for a Pane beside `source`. The server places it; this client focuses it
    /// once the outcome names it and a publication holds it.
    pub(super) fn split_server_pane(
        &mut self,
        source: ServerPaneId,
        axis: SplitAxis,
        cx: &mut Context<Self>,
    ) {
        let Panes::Server(panes) = &mut self.panes else {
            return;
        };
        if let Some(error) = shared_unavailable(panes.status) {
            cx.emit(ServerEditRefused(error));
            return;
        }
        let request = DesktopRequest::Shared {
            server: source.server(),
            command: Command::CreatePane {
                tab: panes.tab.id(),
                source,
                axis: server_axis(axis),
                edge: SplitEdge::After,
            },
        };
        match panes
            .runtime
            .update(cx, |runtime, cx| runtime.request(request, cx))
        {
            Ok(request) => panes.creation = Some(CreationIntent::new(request)),
            Err(error) => cx.emit(ServerEditRefused(error)),
        }
    }

    fn on_runtime_event(&mut self, event: &DesktopClientEvent, cx: &mut Context<Self>) {
        let Panes::Server(panes) = &mut self.panes else {
            return;
        };
        match event {
            DesktopClientEvent::RequestFailed { request, error }
                if panes
                    .edits
                    .iter()
                    .any(|pending| pending.request == Some(*request)) =>
            {
                let focused_before = panes.tab.focused_pane();
                let failed_move = panes.edits.iter().any(|pending| {
                    pending.request == Some(*request)
                        && matches!(&pending.replay, ArrangementReplay::Edit(edit) if matches!(edit.edit, ArrangementEdit::Move { .. }))
                });
                panes.edits.retain(|pending| {
                    pending.request != Some(*request) && (!failed_move || pending.request.is_some())
                });
                if failed_move {
                    panes.live = None;
                }
                panes.restore_published();
                let focus_moved = panes.tab.focused_pane() != focused_before;
                self.sync_terminal_focus(cx);
                if focus_moved {
                    self.defer_native_focus(cx);
                }
                self.presentation_changed(cx);
                cx.emit(ServerEditRefused(*error));
                cx.notify();
                return;
            }
            DesktopClientEvent::RequestCompleted { request, .. } => {
                if let Some(pending) = panes
                    .edits
                    .iter_mut()
                    .find(|pending| pending.request == Some(*request))
                {
                    pending.completed = true;
                }
                panes.retire_edits();
            }
            _ => {}
        }
        let server = panes.tab.id().server();
        let Some(intent) = &mut panes.creation else {
            return;
        };
        match intent.observe(server, event) {
            IntentProgress::Waiting => {}
            IntentProgress::Cancelled(error) => {
                panes.creation = None;
                if let Some(error) = error {
                    cx.emit(ServerEditRefused(error));
                }
            }
            IntentProgress::Named(CreatedEntities::Pane(_)) => self.complete_creation(cx),
            IntentProgress::Named(_) => panes.creation = None,
        }
    }

    /// Focuses the Pane this Tab View split off once a publication holds it.
    fn complete_creation(&mut self, cx: &mut Context<Self>) {
        let Panes::Server(panes) = &mut self.panes else {
            return;
        };
        let Some(CreatedEntities::Pane(pane)) =
            panes.creation.as_ref().and_then(CreationIntent::target)
        else {
            return;
        };
        if !panes.tab.arrangement().contains_pane(pane) {
            return;
        }
        panes.creation = None;
        self.focus_pane(PaneKey::Server(pane), cx);
        if self.active {
            self.defer_native_focus(cx);
        }
    }

    /// Creates presentations for Panes a publication added, once the window is free.
    fn schedule_materialize(&mut self, cx: &mut Context<Self>) {
        let Panes::Server(panes) = &mut self.panes else {
            return;
        };
        if panes.materialize_scheduled || panes.tab.missing().is_empty() {
            return;
        }
        panes.materialize_scheduled = true;
        let window = panes.window;
        let view = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = view.update(cx, |view, cx| view.materialize_panes(window, cx));
            });
        });
    }

    fn materialize_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Panes::Server(panes) = &mut self.panes else {
            return;
        };
        panes.materialize_scheduled = false;
        let factory = Rc::clone(&panes.factory);
        let runtime = panes.runtime.clone();
        let mut created = Vec::new();
        for pane in panes.tab.missing() {
            // A Pane whose presentation cannot be created keeps its place without one, and the
            // next publication tries again.
            if let Ok(terminal) = factory.create(pane, &runtime, window, cx) {
                created.push((pane, terminal));
            }
        }
        let Panes::Server(panes) = &mut self.panes else {
            return;
        };
        let focused = panes.tab.focused_pane();
        let focused_materialized = created.iter().any(|(pane, _)| *pane == focused);
        for (pane, terminal) in created {
            if panes.tab.insert(pane, terminal.clone()).is_err() {
                continue;
            }
            let key = PaneKey::Server(pane);
            self.pane_titles.insert(key, terminal.read(cx).title());
            self.pane_captions
                .insert(key, PaneCaptionText::from_terminal(terminal.read(cx)));
            self.pane_attention.insert(key, 0);
            Self::observe_server_pane(pane, &terminal, cx);
        }
        self.advance_native_service_hierarchy_generation(cx);
        self.sync_terminal_focus(cx);
        if focused_materialized {
            self.defer_native_focus(cx);
        }
        self.presentation_changed(cx);
        cx.notify();
    }

    /// Returns native focus to the Focused Pane after the current update. It never selects.
    fn defer_native_focus(&mut self, cx: &mut Context<Self>) {
        let Panes::Server(panes) = &self.panes else {
            return;
        };
        let window = panes.window;
        let view = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = view.update(cx, |view, cx| {
                    if view.active && view.focus_branch_blocker.is_none() {
                        view.focus(window, cx);
                    }
                });
            });
        });
    }

    fn observe_server_pane(
        pane: ServerPaneId,
        terminal: &Entity<TerminalPane>,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe(
            terminal,
            move |view: &mut Self, terminal, event: &TerminalPaneEvent, cx| {
                view.on_server_pane_event(pane, &terminal, event, cx);
            },
        )
        .detach();
    }

    fn on_server_pane_event(
        &mut self,
        pane: ServerPaneId,
        terminal: &Entity<TerminalPane>,
        event: &TerminalPaneEvent,
        cx: &mut Context<Self>,
    ) {
        let key = PaneKey::Server(pane);
        // A released Pane's late events change nothing.
        if self.panes.pane(key) != Some(terminal) {
            return;
        }
        match event {
            // Native focus reached the Pane. It moves personal focus only.
            TerminalPaneEvent::FocusRequested => self.focus_pane(key, cx),
            TerminalPaneEvent::SelectionRequested(source) => {
                self.focus_pane(key, cx);
                self.select_server_pane(pane, *source, cx);
            }
            TerminalPaneEvent::ActionRequested(action) => {
                cx.emit(ServerTabManagerEvent::PaneAction {
                    pane,
                    action: *action,
                });
            }
            TerminalPaneEvent::Measured(measurement) => {
                if measurement.token.pane != pane {
                    return;
                }
                let request = DesktopRequest::Measured(*measurement);
                let Panes::Server(panes) = &self.panes else {
                    return;
                };
                // A refused measurement leaves its startup intent waiting for the next one.
                let _ = panes
                    .runtime
                    .update(cx, |runtime, cx| runtime.request(request, cx));
            }
            // The Pane stays until the server publishes its deletion.
            TerminalPaneEvent::Exited => cx.notify(),
            event => self.present_terminal_event(key, terminal, event, cx),
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui::{App, AppContext as _, Entity, TestAppContext, Window};
    use spaceterm_client::api::ConnectingPhase;
    use spaceterm_core::identity::{OperationStreamId, OperationStreamKey, PairingId};
    use spaceterm_core::operations::OperationSequence;

    use super::{ServerPaneFactory, ServerPublication};
    use crate::domain::server_view_testing::Replica;
    use crate::terminal::WorkspaceTerminalSessionFactory;
    use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};
    use crate::ui::TerminalPane;
    use crate::ui::pane_lifecycle::PaneConstruction;
    use crate::ui::server_client_runtime::testing::RecordingClientProvider;
    use crate::ui::server_client_runtime::{
        DesktopClientDependencies, DesktopClientNotifier, DesktopClientRuntime,
        OperationOutcomeNotice, notification_channel,
    };
    use crate::ui::server_desktop_contracts::{
        ClientId, ConnectionState, DesktopClientError, DesktopRequest, DesktopServerFrame,
        DesktopViewRevision, OperationId, Outcome, PaneId, ReplicaStatus, ServerId, UiRequestId,
    };

    /// A runtime over a recording provider, and the provider's notification side.
    pub(crate) struct TestRuntime {
        pub(crate) runtime: Entity<DesktopClientRuntime>,
        pub(crate) provider: Rc<RecordingClientProvider>,
        pub(crate) notifier: DesktopClientNotifier,
    }

    impl TestRuntime {
        pub(crate) fn new(cx: &mut TestAppContext) -> Self {
            let provider = Rc::new(RecordingClientProvider::default());
            let (notifier, notifications) = notification_channel();
            let dependencies = DesktopClientDependencies {
                provider: provider.clone(),
                notifications,
            };
            let runtime = cx.new(|cx| DesktopClientRuntime::new(dependencies, cx));
            Self {
                runtime,
                provider,
                notifier,
            }
        }

        pub(crate) fn begun(&self) -> Vec<DesktopRequest> {
            self.provider.begun()
        }

        /// Delivers one operation outcome of `server` and waits until subscribers saw it.
        pub(crate) fn deliver(
            &self,
            server: ServerId,
            operation: OperationId,
            outcome: Outcome,
            cx: &mut TestAppContext,
        ) {
            let notifier = self.notifier.clone();
            let notice = OperationOutcomeNotice {
                server,
                operation,
                outcome: Arc::new(outcome),
            };
            let delivery = cx
                .foreground_executor()
                .spawn(async move { notifier.operation_outcome(notice).await });
            cx.run_until_parked();
            cx.foreground_executor()
                .block_test(delivery)
                .expect("the runtime receives the outcome");
            cx.run_until_parked();
        }
    }

    /// Operation `sequence` of Client 1 under pairing 1 of `server`.
    pub(crate) fn operation(server: ServerId, sequence: u64) -> OperationId {
        let stream = OperationStreamId::new(
            OperationStreamKey::new(server, 1).unwrap(),
            PairingId::new(server, 1).unwrap(),
            ClientId::new(server, 1).unwrap(),
        )
        .unwrap();
        OperationId::new(stream, OperationSequence::new(sequence).unwrap())
    }

    pub(crate) fn view_revision(number: usize) -> DesktopViewRevision {
        (0..number).fold(DesktopViewRevision::INITIAL, |revision, _| {
            revision.checked_successor().unwrap()
        })
    }

    pub(crate) fn request_id(index: usize) -> UiRequestId {
        (0..index).fold(UiRequestId::FIRST, |request, _| {
            request.checked_successor().unwrap()
        })
    }

    /// One coherent frame of `replica` with `status`.
    pub(crate) fn frame(replica: &Replica, status: ReplicaStatus) -> DesktopServerFrame {
        DesktopServerFrame {
            server: replica.org.server(),
            connection: ConnectionState::Connecting {
                phase: ConnectingPhase::Reconciling,
            },
            replica_status: status,
            organization: replica.snapshot(),
            personal: Arc::new(replica.personal.clone()),
            view_revision: DesktopViewRevision::INITIAL,
        }
    }

    /// One coherent Ready publication of `replica`.
    pub(crate) fn publication(replica: &Replica) -> ServerPublication {
        ServerPublication::of(&frame(replica, ReplicaStatus::Ready))
    }

    /// A local test launch policy. A server Pane's presentation in tests runs one test Terminal.
    pub(crate) fn local_session_factory() -> WorkspaceTerminalSessionFactory {
        WorkspaceTerminalSessionFactory::new_local(
            Rc::new(
                TestTerminalSessionFactory::new(TestTerminalSessionRecords::default())
                    .with_selection_copy_response(Ok(None)),
            ),
            crate::terminal::testing::test_local_directory(PathBuf::from(
                "/tmp/spaceterm-test-workspace",
            )),
        )
    }

    /// Presents server Panes with local test Terminals and records each one it creates.
    #[derive(Default)]
    pub(crate) struct TestPanes {
        created: RefCell<Vec<PaneId>>,
        failure: Cell<Option<DesktopClientError>>,
    }

    impl TestPanes {
        pub(crate) fn created(&self) -> Vec<PaneId> {
            self.created.borrow().clone()
        }

        /// Refuses every later Pane with `error`.
        pub(crate) fn fail_with(&self, error: Option<DesktopClientError>) {
            self.failure.set(error);
        }
    }

    impl ServerPaneFactory for TestPanes {
        fn create(
            &self,
            pane: PaneId,
            _runtime: &Entity<DesktopClientRuntime>,
            window: &mut Window,
            cx: &mut App,
        ) -> Result<Entity<TerminalPane>, DesktopClientError> {
            if let Some(error) = self.failure.get() {
                return Err(error);
            }
            let session_factory = local_session_factory();
            let launch = session_factory
                .prepare_child_launch()
                .map_err(|_| DesktopClientError::UnsupportedCapability)?;
            let construction = PaneConstruction::testing();
            let terminal = cx.new(|cx| construction.create(session_factory, launch, window, cx));
            self.created.borrow_mut().push(pane);
            Ok(terminal)
        }
    }
}
