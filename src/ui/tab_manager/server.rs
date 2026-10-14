//! Server-backed Tab Managers.
//!
//! Membership, the Root Tab and each Tab's Worktree come from the server's replica. Tab order and
//! the Active Tab come from this client's personal view and change only through client-view
//! operations, which never change a server Root. A Tab Manager never creates, closes or reorders
//! a shared Tab itself: Create Tab asks the server, and a Tab leaves only when a publication no
//! longer represents it.

use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyWindowHandle, AppContext as _, Context, Entity, FocusHandle, Subscription, Window};
use spaceterm_client::view::{ViewError, ViewUpdate};
use spaceterm_core::operations::{CreatedEntities, TabCreationSource};

use super::{ServerEditRefused, ServerTabManagerEvent, TabKey, TabManager, TabManagerEvent, Tabs};
use crate::domain::{ServerTabCollection, SplitAxis, TabStep};
use crate::platform::window_movement::OperatingSystemWindowDragPlatform;
use crate::ui::pane_lifecycle::PaneConstruction;
use crate::ui::server_client_runtime::{
    DesktopClientEvent, DesktopClientRuntime, MAX_PENDING_UI_REQUESTS,
};
use crate::ui::server_desktop_contracts::{
    ClientViewState, Command, DesktopClientError, DesktopRequest, DesktopServerFrame,
    DesktopViewRevision, OrganizationSnapshot, SelectionSource, TabId, TabStep as ViewTabStep,
    UiRequestId, ViewEdit, WorkspaceId, WorkspaceRecord, WorkspaceView, WorktreeId,
};
use crate::ui::tab_view::{
    CreationIntent, IntentProgress, ServerPaneFactory, ServerPublication, ServerTabParts,
    ServerTabPresentationChanged, shared_unavailable,
};
use crate::ui::{RemoteChildLaunchUnavailable, TabView};

/// Validated presentation resources prepared before allocating a GPUI Tab Manager.
pub(crate) struct PreparedServerTabManager {
    record: WorkspaceRecord,
    collection: ServerTabCollection<Entity<TabView>>,
    parts: Vec<ServerTabParts>,
    publication: ServerPublication,
    runtime: Entity<DesktopClientRuntime>,
    factory: Rc<dyn ServerPaneFactory>,
}

/// One server Workspace's Tabs and the state that keeps them consistent with the replica.
pub(super) struct ServerTabs {
    record: WorkspaceRecord,
    collection: ServerTabCollection<Entity<TabView>>,
    /// The latest applied publication, beneath admitted personal edits.
    publication: ServerPublication,
    /// The personal view presented: the publication's, with each personal edit this Tab Manager
    /// sent since then applied.
    personal: Arc<ClientViewState>,
    runtime: Entity<DesktopClientRuntime>,
    factory: Rc<dyn ServerPaneFactory>,
    window: AnyWindowHandle,
    /// Admitted edits retained until completion and the receipted frame revision both arrive.
    edits: VecDeque<PendingTabEdit>,
    /// A Tab this Tab Manager asked for, activated once the server names it and a publication
    /// holds it.
    creation: Option<CreationIntent>,
    materialize_scheduled: bool,
    _runtime: Subscription,
}

struct PendingTabEdit {
    request: UiRequestId,
    admitted_after: DesktopViewRevision,
    replay: ViewEdit,
    /// Cumulative order checkpoints avoid replaying a represented composition of absolute moves.
    /// They are presentation optimizations, never application receipts or retirement evidence.
    order: Option<Vec<TabId>>,
    rebased_order: Option<Vec<TabId>>,
    completed: bool,
    applied: Option<DesktopViewRevision>,
}

impl ServerTabs {
    pub(super) fn cancel_creation(&mut self) {
        self.creation = None;
    }

    const fn workspace(&self) -> WorkspaceId {
        self.collection.workspace()
    }

    pub(super) const fn active(&self) -> Option<TabId> {
        self.collection.active_tab()
    }

    pub(super) fn active_view(&self) -> Option<&Entity<TabView>> {
        self.collection.tab(self.collection.active_tab()?)
    }

    pub(super) fn view(&self, tab: TabId) -> Option<&Entity<TabView>> {
        self.collection.tab(tab)
    }

    fn presented_in(&self, scope: Option<WorktreeId>) -> Vec<TabId> {
        self.collection
            .tabs_in(scope)
            .into_iter()
            .filter(|tab| self.view(*tab).is_some())
            .collect()
    }

    /// Converts a drop among presented Tabs to client-view's position among all represented Tabs.
    fn personal_position(&self, tab: TabId, position: usize) -> Option<usize> {
        self.view(tab)?;
        let scope = self.collection.scope_of(tab).ok()?;
        let sequence: Vec<_> = self
            .collection
            .tabs_in(scope)
            .into_iter()
            .filter(|candidate| *candidate != tab)
            .collect();
        let presented: Vec<_> = sequence
            .iter()
            .copied()
            .filter(|candidate| self.view(*candidate).is_some())
            .collect();
        if let Some(next) = presented.get(position) {
            sequence.iter().position(|candidate| candidate == next)
        } else if position == presented.len() {
            Some(
                presented
                    .last()
                    .and_then(|last| sequence.iter().position(|candidate| candidate == last))
                    .map_or(0, |index| index + 1),
            )
        } else {
            None
        }
    }

    /// Presented Tabs with views, in personal order.
    pub(super) fn views(&self) -> Vec<(TabKey, &Entity<TabView>)> {
        self.collection
            .iter()
            .map(|(tab, view)| (TabKey::Server(tab), view))
            .collect()
    }

    /// The Active Worktree's presented Tabs with views, in personal order.
    pub(super) fn visible(&self) -> Vec<(TabKey, &Entity<TabView>)> {
        self.collection
            .visible()
            .into_iter()
            .filter_map(|tab| {
                self.collection
                    .tab(tab)
                    .map(|view| (TabKey::Server(tab), view))
            })
            .collect()
    }

    pub(super) fn all_views(&self) -> Vec<&Entity<TabView>> {
        self.collection.payloads().map(|(_, view)| view).collect()
    }

    pub(super) fn has_unscoped_tabs(&self) -> bool {
        !self.collection.tabs_in(None).is_empty()
    }

    /// Whether the presented frame is the server's current publication, which shared commands
    /// need.
    pub(super) const fn accepts_shared_commands(&self) -> bool {
        self.shared_action_refusal().is_none()
    }

    pub(super) const fn shared_action_refusal(&self) -> Option<DesktopClientError> {
        shared_unavailable(self.publication.status)
    }

    fn retire_edits(&mut self, revision: DesktopViewRevision) {
        self.edits.retain(|pending| {
            !pending.completed || pending.applied.is_none_or(|applied| applied > revision)
        });
    }

    fn replay_edits(&mut self, publication: &ServerPublication) -> Arc<ClientViewState> {
        let workspace = self.workspace();
        let mut personal = ClientViewState::clone(&publication.personal);
        let represented_order = personal.workspace(workspace).and_then(|view| {
            let published = view.tab_order();
            let published_tabs: BTreeSet<_> = published.iter().copied().collect();
            let matches_order = |expected: &[TabId]| {
                let known: BTreeSet<_> = expected.iter().copied().collect();
                expected
                    .iter()
                    .filter(|tab| published_tabs.contains(*tab))
                    .eq(published.iter().filter(|tab| known.contains(*tab)))
            };
            self.edits.iter().rposition(|pending| {
                pending.order.as_deref().is_some_and(matches_order)
                    || pending.rebased_order.as_deref().is_some_and(matches_order)
            })
        });
        for (index, pending) in self.edits.iter_mut().enumerate() {
            if pending
                .applied
                .is_some_and(|applied| applied <= publication.view_revision)
                || (pending.order.is_some() && represented_order.is_some_and(|last| index <= last))
            {
                continue;
            }
            let _ = apply_view_edit(&mut personal, pending.replay, &publication.organization);
            if pending.order.is_some() {
                pending.rebased_order = personal
                    .workspace(workspace)
                    .map(|view| view.tab_order().to_vec());
            }
        }
        Arc::new(personal)
    }

    /// Presents `personal`'s order and Active Tab over the latest publication's records. It
    /// returns the views of Tabs `personal` no longer represents.
    fn present(
        &mut self,
        personal: Arc<ClientViewState>,
    ) -> Result<Vec<(TabId, Entity<TabView>)>, DesktopClientError> {
        let view = personal
            .workspace(self.workspace())
            .ok_or(DesktopClientError::TargetGone)?;
        let released = self
            .collection
            .apply(
                &self.record,
                &self.publication.organization.state().tabs,
                view,
            )
            .map_err(|_| DesktopClientError::StaleContext)?;
        self.personal = personal;
        Ok(released)
    }
}

/// Applies one personal edit with the client-view operation the provider runs for it.
fn apply_view_edit(
    state: &mut ClientViewState,
    edit: ViewEdit,
    organization: &OrganizationSnapshot,
) -> Result<ViewUpdate, ViewError> {
    match edit {
        ViewEdit::ActivateWorkspace(workspace) => state.activate_workspace(workspace),
        ViewEdit::ActivateTab { workspace, tab } => state.activate_tab(workspace, tab),
        ViewEdit::ActivateAdjacentTab { workspace, step } => {
            state.activate_adjacent_tab(workspace, step, organization)
        }
        ViewEdit::ActivateWorktree {
            workspace,
            worktree,
        } => state.activate_worktree(workspace, worktree, organization),
        ViewEdit::MoveTab {
            workspace,
            tab,
            position,
        } => state.move_tab(workspace, tab, position, organization),
        ViewEdit::StepTab {
            workspace,
            tab,
            step,
        } => state.step_tab(workspace, tab, step, organization),
        ViewEdit::Arrange {
            workspace,
            tab,
            edit,
        } => state.arrange(workspace, tab, edit),
    }
}

const fn view_error(error: ViewError) -> DesktopClientError {
    match error {
        ViewError::TargetMissing => DesktopClientError::TargetGone,
        ViewError::WrongServer => DesktopClientError::WrongServer,
        ViewError::NotReady => DesktopClientError::Reconciling,
        _ => DesktopClientError::StaleContext,
    }
}

const fn view_step(step: TabStep) -> ViewTabStep {
    match step {
        TabStep::Previous => ViewTabStep::Previous,
        TabStep::Next => ViewTabStep::Next,
    }
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the Workspace Manager presents server Workspaces once Panes host server sessions"
    )
)]
impl TabManager {
    /// Presents `workspace` from `frame`. Every presented Tab's Panes need presentations from
    /// `panes`; one that cannot be created fails the whole Tab Manager. This build has no server
    /// Pane channel, so production construction fails closed.
    pub(crate) fn new_server(
        frame: &DesktopServerFrame,
        workspace: WorkspaceId,
        runtime: Entity<DesktopClientRuntime>,
        panes: PaneConstruction,
        window_drag: Rc<dyn OperatingSystemWindowDragPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Self, DesktopClientError> {
        let factory: Rc<dyn ServerPaneFactory> = Rc::new(panes.clone());
        Self::new_server_with(
            &ServerPublication::of(frame),
            workspace,
            runtime,
            factory,
            panes,
            window_drag,
            window,
            cx,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "construction needs the publication, its target, the runtime, both Pane sources, the window drag platform and the window"
    )]
    pub(crate) fn new_server_with(
        publication: &ServerPublication,
        workspace: WorkspaceId,
        runtime: Entity<DesktopClientRuntime>,
        factory: Rc<dyn ServerPaneFactory>,
        pane_construction: PaneConstruction,
        window_drag: Rc<dyn OperatingSystemWindowDragPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Self, DesktopClientError> {
        let prepared =
            Self::prepare_server_workspace(publication, workspace, runtime, factory, window, cx)?;
        Ok(Self::from_prepared_server(
            prepared,
            pane_construction,
            window_drag,
            window,
            cx,
        ))
    }

    pub(crate) fn prepare_server_workspace(
        publication: &ServerPublication,
        workspace: WorkspaceId,
        runtime: Entity<DesktopClientRuntime>,
        factory: Rc<dyn ServerPaneFactory>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> Result<PreparedServerTabManager, DesktopClientError> {
        if workspace.server() != publication.server {
            return Err(DesktopClientError::WrongServer);
        }
        let state = publication.organization.state();
        let record = state
            .workspaces
            .iter()
            .find(|record| record.id == workspace)
            .ok_or(DesktopClientError::TargetGone)?;
        let view = publication
            .personal
            .workspace(workspace)
            .ok_or(DesktopClientError::TargetGone)?;
        let collection = ServerTabCollection::new(record, &state.tabs, view)
            .map_err(|_| DesktopClientError::StaleContext)?;
        // Every Pane is presented before any Tab View exists, so a failure presents nothing.
        let parts = collection
            .missing()
            .into_iter()
            .map(|tab| {
                TabView::prepare_server(publication, tab, &runtime, factory.as_ref(), window, cx)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PreparedServerTabManager {
            record: record.clone(),
            collection,
            parts,
            publication: publication.clone(),
            runtime,
            factory,
        })
    }

    pub(crate) fn from_prepared_server(
        prepared: PreparedServerTabManager,
        pane_construction: PaneConstruction,
        window_drag: Rc<dyn OperatingSystemWindowDragPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let PreparedServerTabManager {
            record,
            mut collection,
            parts,
            publication,
            runtime,
            factory,
        } = prepared;
        let active = collection.active_tab();
        for parts in parts {
            let (tab, view) = Self::create_server_tab_view(
                parts,
                runtime.clone(),
                Rc::clone(&factory),
                window,
                cx,
            );
            if Some(tab) != active {
                view.update(cx, |view, cx| view.deactivate(cx));
            }
            collection.insert(tab, view).ok();
        }
        let subscription = cx.subscribe(&runtime, |manager: &mut Self, _, event, cx| {
            manager.on_server_runtime_event(event, cx);
        });
        let tabs = Tabs::Server(Box::new(ServerTabs {
            record,
            collection,
            publication: publication.clone(),
            personal: Arc::clone(&publication.personal),
            runtime,
            factory,
            window: window.window_handle(),
            edits: VecDeque::new(),
            creation: None,
            materialize_scheduled: false,
            _runtime: subscription,
        }));
        Self::with_tabs(tabs, pane_construction, window_drag, window)
    }

    /// Creates a server Tab's view and forwards what it reports.
    fn create_server_tab_view(
        parts: ServerTabParts,
        runtime: Entity<DesktopClientRuntime>,
        factory: Rc<dyn ServerPaneFactory>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (TabId, Entity<TabView>) {
        let tab = parts.id();
        let view = cx.new(|cx| TabView::from_server_parts(parts, runtime, factory, window, cx));
        debug_assert_eq!(view.read(cx).server_tab_id(), Some(tab));
        cx.subscribe(&view, |_, _, event: &ServerTabManagerEvent, cx| {
            cx.emit(*event);
        })
        .detach();
        cx.subscribe(&view, |_, _, event: &ServerEditRefused, cx| {
            cx.emit(*event);
        })
        .detach();
        cx.subscribe(&view, |_, _, _: &ServerTabPresentationChanged, cx| {
            cx.emit(TabManagerEvent::PresentationChanged);
            cx.notify();
        })
        .detach();
        cx.subscribe(&view, |_, _, event: &RemoteChildLaunchUnavailable, cx| {
            cx.emit(*event);
        })
        .detach();
        (tab, view)
    }

    /// Applies one coherent publication of this Workspace. It never acquires control, starts a
    /// Session or opens a Tab. A Tab Manager presenting a local or SSH Workspace refuses every
    /// server publication.
    pub(crate) fn apply_workspace(
        &mut self,
        frame: &DesktopServerFrame,
        record: &WorkspaceRecord,
        view: &WorkspaceView,
        cx: &mut Context<Self>,
    ) -> Result<(), DesktopClientError> {
        self.apply_server_workspace(ServerPublication::of(frame), record, view, cx)
    }

    pub(crate) fn apply_server_workspace(
        &mut self,
        publication: ServerPublication,
        record: &WorkspaceRecord,
        view: &WorkspaceView,
        cx: &mut Context<Self>,
    ) -> Result<(), DesktopClientError> {
        let Tabs::Server(server) = &mut self.tabs else {
            return Err(DesktopClientError::WrongServer);
        };
        if publication.server != server.workspace().server()
            || record.id.server() != publication.server
        {
            return Err(DesktopClientError::WrongServer);
        }
        if record.id != server.workspace() || view.workspace() != server.workspace() {
            return Err(DesktopClientError::StaleContext);
        }
        // The record, records and view must all come from the same frame.
        match publication.personal.workspace(server.workspace()) {
            Some(published) if std::ptr::eq(published, view) || published == view => {}
            _ => return Err(DesktopClientError::StaleContext),
        }
        if publication.view_revision < server.publication.view_revision {
            return Err(DesktopClientError::StaleContext);
        }
        let previous = server.active_view().cloned();
        if server
            .active()
            .is_some_and(|tab| view.arrangement(tab).is_none())
        {
            server.creation = None;
        }
        server.retire_edits(publication.view_revision);
        let personal = server.replay_edits(&publication);
        let presented = personal
            .workspace(server.workspace())
            .ok_or(DesktopClientError::TargetGone)?;
        let released = server
            .collection
            .apply(record, &publication.organization.state().tabs, presented)
            .map_err(|_| DesktopClientError::StaleContext)?;
        server.record = record.clone();
        server.personal = personal;
        server.publication = publication;

        let status = server.publication.status;
        let server_id = server.publication.server;
        let organization = Arc::clone(&server.publication.organization);
        let records = &organization.state().tabs;
        let mut refused = None;
        for (tab, tab_view) in server.collection.iter() {
            let (Some(tab_record), Some(arrangement)) = (
                records.iter().find(|candidate| candidate.id == tab),
                view.arrangement(tab),
            ) else {
                continue;
            };
            if let Err(error) = tab_view.update(cx, |tab_view, cx| {
                tab_view.apply_server_tab(
                    status,
                    server_id,
                    server.publication.view_revision,
                    tab_record,
                    arrangement,
                    cx,
                )
            }) {
                refused.get_or_insert(error);
            }
        }
        self.release_server_tabs(released, cx);
        self.schedule_server_materialize(cx);
        self.present_published_activation(previous, cx);
        self.complete_server_creation(cx);
        self.sync_terminal_focus_blocker(cx);
        self.scroll_active_tab_into_view();
        cx.emit(TabManagerEvent::PresentationChanged);
        cx.notify();
        refused.map_or(Ok(()), Err)
    }

    /// Records the provider's successful application of this exact personal request in `revision`.
    /// C3 delivers this receipt separately from request completion. Equality of view contents and
    /// layout revisions cannot supply it. Every frame at or after `revision` must include the edit.
    pub(crate) fn personal_edit_applied(
        &mut self,
        request: UiRequestId,
        revision: DesktopViewRevision,
        cx: &mut Context<Self>,
    ) {
        let Tabs::Server(server) = &mut self.tabs else {
            return;
        };
        for (_, view) in server.collection.iter() {
            view.update(cx, |view, cx| {
                view.personal_edit_applied(request, revision, cx)
            });
        }
        let Some(pending) = server
            .edits
            .iter_mut()
            .find(|pending| pending.request == request)
        else {
            return;
        };
        if revision <= pending.admitted_after {
            cx.emit(ServerEditRefused(DesktopClientError::StaleContext));
            return;
        }
        pending.applied.get_or_insert(revision);
        if revision <= server.publication.view_revision {
            let publication = server.publication.clone();
            let record = server.record.clone();
            if let Some(view) = publication.personal.workspace(record.id) {
                let view = view.clone();
                if let Err(error) = self.apply_server_workspace(publication, &record, &view, cx) {
                    cx.emit(ServerEditRefused(error));
                }
            }
        }
    }

    /// Quit/save composition must flush local Split edits before saving the provider's view.
    pub(crate) fn has_waiting_personal_edits(&self, cx: &gpui::App) -> bool {
        match &self.tabs {
            Tabs::Server(server) => server
                .all_views()
                .into_iter()
                .any(|view| view.read(cx).has_waiting_personal_edits()),
            Tabs::Local(_) => false,
        }
    }

    /// Saving cannot discard an optimistic edit before its exact application receipt is presented.
    pub(crate) fn has_unsettled_personal_edits(&self, cx: &gpui::App) -> bool {
        match &self.tabs {
            Tabs::Server(server) => {
                !server.edits.is_empty()
                    || server
                        .all_views()
                        .into_iter()
                        .any(|view| view.read(cx).has_unsettled_personal_edits())
            }
            Tabs::Local(_) => false,
        }
    }

    /// Returns native focus to the active Tab's Focused Pane. This is not a selection.
    pub(crate) fn restore_focus(&self, window: &mut Window, cx: &mut gpui::App) {
        self.focus(window, cx);
    }

    /// The Active Tab's server Worktree, or None for the Workspace's own Tabs.
    pub(crate) fn active_server_worktree(&self) -> Option<WorktreeId> {
        match &self.tabs {
            Tabs::Server(server) => server.collection.active_scope(),
            Tabs::Local(_) => None,
        }
    }

    /// Shows a Worktree's most recently active Tab, or asks the server for its first Tab when it
    /// has none. A Tab a person chose with `source` is also opened for explicit use.
    pub(crate) fn open_server_worktree(
        &mut self,
        worktree: Option<WorktreeId>,
        source: Option<SelectionSource>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Tabs::Server(server) = &self.tabs else {
            return false;
        };
        let mut scratch = ClientViewState::clone(&server.personal);
        let target = scratch
            .activate_worktree(
                server.workspace(),
                worktree,
                &server.publication.organization,
            )
            .ok()
            .and_then(|_| scratch.workspace(server.workspace()))
            .and_then(WorkspaceView::active_tab);
        let target = target.and_then(|preferred| {
            server
                .view(preferred)
                .map(|_| preferred)
                .or_else(|| server.presented_in(worktree).first().copied())
        });
        match (target, worktree) {
            (Some(tab), _) => self.activate_server_tab(tab, source, focus, window, cx),
            (None, Some(worktree)) if server.collection.tabs_in(Some(worktree)).is_empty() => {
                self.request_server_tab(TabCreationSource::Worktree(worktree), cx);
                false
            }
            (None, _) => false,
        }
    }

    /// Activates a server Tab with native focus. A Tab a person chose with `source` is also
    /// opened for explicit use.
    pub(super) fn open_server_tab(
        &mut self,
        tab: TabId,
        source: Option<SelectionSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.activate_server_tab(tab, source, true, window, cx)
    }

    fn activate_server_tab(
        &mut self,
        tab: TabId,
        source: Option<SelectionSource>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Tabs::Server(server) = &mut self.tabs else {
            return false;
        };
        let Some(next) = server.view(tab).cloned() else {
            return false;
        };
        let navigated = Some(tab) != server.active();
        let previous = server.active_view().cloned();
        let workspace = server.workspace();
        if let Err(error) = self.personal_edit(ViewEdit::ActivateTab { workspace, tab }, cx) {
            cx.emit(ServerEditRefused(error));
            return false;
        }
        if (navigated || source.is_some())
            && let Tabs::Server(server) = &mut self.tabs
        {
            // Navigation and explicit use are independent. Either cancels post-create attention.
            server.creation = None;
        }
        let previous = previous.filter(|previous| previous.entity_id() != next.entity_id());
        self.present_activation(previous, next, focus, window, cx);
        if let Some(source) = source {
            cx.emit(ServerTabManagerEvent::OpenTab { tab, source });
        }
        true
    }

    /// Opens the Tab one step from the Active Tab within its Worktree, as a keyboard command.
    pub(super) fn open_adjacent_server_tab(
        &mut self,
        step: TabStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Tabs::Server(server) = &self.tabs else {
            return;
        };
        // Missing presentations retain membership but do not trap keyboard navigation.
        let presented = server.presented_in(server.collection.active_scope());
        let current = presented
            .iter()
            .position(|tab| Some(*tab) == server.active());
        let target = match (current, step) {
            (Some(index), TabStep::Next) => presented.get((index + 1) % presented.len()),
            (Some(index), TabStep::Previous) => {
                presented.get((index + presented.len() - 1) % presented.len())
            }
            (None, TabStep::Next) => presented.first(),
            (None, TabStep::Previous) => presented.last(),
        }
        .copied();
        if let Some(tab) = target
            && Some(tab) != server.active()
        {
            self.open_server_tab(tab, Some(SelectionSource::Command), window, cx);
        }
    }

    /// Moves a Tab to `position` among its Worktree's Tabs in this client's order.
    pub(super) fn move_server_tab(&mut self, tab: TabId, position: usize, cx: &mut Context<Self>) {
        let Tabs::Server(server) = &self.tabs else {
            return;
        };
        let Some(position) = server.personal_position(tab, position) else {
            cx.emit(ServerEditRefused(DesktopClientError::TargetGone));
            return;
        };
        let edit = ViewEdit::MoveTab {
            workspace: server.workspace(),
            tab,
            position,
        };
        self.present_tab_order_edit(edit, cx);
    }

    /// Moves a Tab one step among its Worktree's Tabs in this client's order.
    pub(super) fn step_server_tab(&mut self, tab: TabId, step: TabStep, cx: &mut Context<Self>) {
        let Tabs::Server(server) = &self.tabs else {
            return;
        };
        let Ok(scope) = server.collection.scope_of(tab) else {
            return;
        };
        let presented = server.presented_in(scope);
        if presented.len() != server.collection.tabs_in(scope).len() {
            let Some(index) = presented.iter().position(|candidate| *candidate == tab) else {
                return;
            };
            let position = match step {
                TabStep::Previous => index.checked_sub(1),
                TabStep::Next => Some(index + 1).filter(|position| *position < presented.len()),
            };
            if let Some(position) = position {
                self.move_server_tab(tab, position, cx);
            }
            return;
        }
        let edit = ViewEdit::StepTab {
            workspace: server.workspace(),
            tab,
            step: view_step(step),
        };
        self.present_tab_order_edit(edit, cx);
    }

    fn present_tab_order_edit(&mut self, edit: ViewEdit, cx: &mut Context<Self>) {
        match self.personal_edit(edit, cx) {
            Ok(true) => {
                self.scroll_active_tab_into_view();
                cx.emit(TabManagerEvent::PresentationChanged);
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => cx.emit(ServerEditRefused(error)),
        }
    }

    /// Asks the server for a Tab beside the Active Tab's Focused Pane. The server places it; this
    /// client activates it once the outcome names it and a publication holds it.
    pub(super) fn create_server_tab(&mut self, cx: &mut Context<Self>) {
        let Tabs::Server(server) = &self.tabs else {
            return;
        };
        let pane = server
            .active_view()
            .and_then(|view| view.read(cx).focused_server_pane());
        let source = match (pane, server.collection.active_scope()) {
            (Some(pane), _) => TabCreationSource::Pane(pane),
            (None, Some(worktree)) => TabCreationSource::Worktree(worktree),
            (None, None) => {
                cx.emit(ServerEditRefused(DesktopClientError::TargetGone));
                return;
            }
        };
        self.request_server_tab(source, cx);
    }

    fn request_server_tab(&mut self, source: TabCreationSource, cx: &mut Context<Self>) {
        let Tabs::Server(server) = &mut self.tabs else {
            return;
        };
        if let Some(error) = shared_unavailable(server.publication.status) {
            cx.emit(ServerEditRefused(error));
            return;
        }
        let request = DesktopRequest::Shared {
            server: server.workspace().server(),
            command: Command::CreateTab {
                workspace: server.workspace(),
                source,
            },
        };
        match server
            .runtime
            .update(cx, |runtime, cx| runtime.request(request, cx))
        {
            Ok(request) => server.creation = Some(CreationIntent::new(request)),
            Err(error) => cx.emit(ServerEditRefused(error)),
        }
    }

    /// Splits a server Tab's Focused Pane. The Tab becomes active first so the new Pane is
    /// visible; choosing a menu command opens no Tab for explicit use.
    pub(super) fn split_server_tab(
        &mut self,
        tab: TabId,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.open_server_tab(tab, None, window, cx) {
            return;
        }
        if let Some(view) = self.tabs.active_view().cloned() {
            view.update(cx, |view, cx| view.split_focused(axis, window, cx));
        }
    }

    /// Applies one personal edit here with the client-view operation the provider runs, then
    /// sends it. It returns whether the presentation changed. A refused edit changes nothing.
    fn personal_edit(
        &mut self,
        edit: ViewEdit,
        cx: &mut Context<Self>,
    ) -> Result<bool, DesktopClientError> {
        let Tabs::Server(server) = &mut self.tabs else {
            return Err(DesktopClientError::WrongServer);
        };
        let mut next = ClientViewState::clone(&server.personal);
        let update = apply_view_edit(&mut next, edit, &server.publication.organization)
            .map_err(view_error)?;
        if update.is_empty() {
            return Ok(false);
        }
        // A StepTab request is replayed at its accepted position, not stepped twice when a
        // publication already reflects it.
        let replay = match edit {
            ViewEdit::StepTab { workspace, tab, .. } => {
                let scope = server
                    .collection
                    .scope_of(tab)
                    .map_err(|_| DesktopClientError::TargetGone)?;
                let position = next
                    .workspace(workspace)
                    .ok_or(DesktopClientError::TargetGone)?
                    .tab_order()
                    .iter()
                    .filter(|candidate| {
                        server
                            .publication
                            .organization
                            .state()
                            .tabs
                            .iter()
                            .any(|record| record.id == **candidate && record.worktree == scope)
                    })
                    .position(|candidate| *candidate == tab)
                    .ok_or(DesktopClientError::TargetGone)?;
                ViewEdit::MoveTab {
                    workspace,
                    tab,
                    position,
                }
            }
            _ => edit,
        };
        if server.edits.len() >= MAX_PENDING_UI_REQUESTS {
            return Err(DesktopClientError::AdmissionFull);
        }
        let request = server.runtime.update(cx, |runtime, cx| {
            runtime.request(DesktopRequest::PersonalEdit(edit), cx)
        })?;
        server.edits.push_back(PendingTabEdit {
            request,
            admitted_after: server.publication.view_revision,
            replay,
            order: if matches!(replay, ViewEdit::MoveTab { .. }) {
                next.workspace(server.workspace())
                    .map(|view| view.tab_order().to_vec())
            } else {
                None
            },
            rebased_order: None,
            completed: false,
            applied: None,
        });
        let released = server.present(Arc::new(next))?;
        self.release_server_tabs(released, cx);
        Ok(true)
    }

    fn on_server_runtime_event(&mut self, event: &DesktopClientEvent, cx: &mut Context<Self>) {
        let Tabs::Server(server) = &mut self.tabs else {
            return;
        };
        match event {
            DesktopClientEvent::RequestFailed { request, error }
                if server
                    .edits
                    .iter()
                    .any(|pending| pending.request == *request) =>
            {
                server.edits.retain(|pending| pending.request != *request);
                let previous = server.active_view().cloned();
                let publication = server.publication.clone();
                let personal = server.replay_edits(&publication);
                if let Ok(released) = server.present(personal) {
                    self.release_server_tabs(released, cx);
                }
                self.present_published_activation(previous, cx);
                self.sync_terminal_focus_blocker(cx);
                self.scroll_active_tab_into_view();
                cx.emit(ServerEditRefused(*error));
                cx.emit(TabManagerEvent::PresentationChanged);
                cx.notify();
                return;
            }
            DesktopClientEvent::RequestCompleted { request, .. } => {
                if let Some(pending) = server
                    .edits
                    .iter_mut()
                    .find(|pending| pending.request == *request)
                {
                    pending.completed = true;
                }
                server.retire_edits(server.publication.view_revision);
            }
            _ => {}
        }
        let server_id = server.workspace().server();
        let Some(intent) = &mut server.creation else {
            return;
        };
        match intent.observe(server_id, event) {
            IntentProgress::Waiting => {}
            IntentProgress::Cancelled(error) => {
                server.creation = None;
                if let Some(error) = error {
                    cx.emit(ServerEditRefused(error));
                }
            }
            IntentProgress::Named(CreatedEntities::Tab { .. }) => self.complete_server_creation(cx),
            IntentProgress::Named(_) => server.creation = None,
        }
    }

    /// Activates the Tab this Tab Manager asked for once a publication holds it and its view
    /// exists. The outcome and the publication may arrive in either order.
    fn complete_server_creation(&mut self, cx: &mut Context<Self>) {
        let Tabs::Server(server) = &mut self.tabs else {
            return;
        };
        let Some(CreatedEntities::Tab { tab, .. }) =
            server.creation.as_ref().and_then(CreationIntent::target)
        else {
            return;
        };
        if server.view(tab).is_none() {
            return;
        }
        server.creation = None;
        let previous = server.active_view().cloned();
        let workspace = server.workspace();
        match self.personal_edit(ViewEdit::ActivateTab { workspace, tab }, cx) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                cx.emit(ServerEditRefused(error));
                return;
            }
        }
        self.present_published_activation(previous, cx);
        self.sync_terminal_focus_blocker(cx);
        self.scroll_active_tab_into_view();
        cx.emit(TabManagerEvent::PresentationChanged);
        cx.notify();
    }

    /// Presents a change of Active Tab that this Tab Manager did not choose with a window, such
    /// as a remote close or a refused edit. Native focus follows after the current update when
    /// this Tab Manager is active. That is restoration, never selection.
    fn present_published_activation(
        &mut self,
        previous: Option<Entity<TabView>>,
        cx: &mut Context<Self>,
    ) {
        let next = self.tabs.active_view().cloned();
        if previous.as_ref().map(Entity::entity_id) == next.as_ref().map(Entity::entity_id) {
            return;
        }
        if let Some(previous) = previous {
            previous.update(cx, |view, cx| view.deactivate(cx));
        }
        let Some(next) = next else {
            return;
        };
        let blocker = self.terminal_focus_blocker();
        next.update(cx, |view, cx| {
            view.set_focus_branch(self.active, blocker, cx)
        });
        if self.active {
            self.defer_restore_focus(None, cx);
        }
    }

    fn defer_restore_focus(&self, removed: Option<Vec<FocusHandle>>, cx: &mut Context<Self>) {
        let Tabs::Server(server) = &self.tabs else {
            return;
        };
        let window = server.window;
        let manager = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = manager.update(cx, |manager, cx| {
                    if manager.terminal_focus_blocker().is_none()
                        && removed.as_ref().is_none_or(|handles| {
                            handles
                                .iter()
                                .any(|handle| handle.contains_focused(window, cx))
                        })
                    {
                        manager.restore_focus(window, cx);
                    }
                });
            });
        });
    }

    /// Releases the views of Tabs the personal view no longer represents. Their Sessions belong
    /// to the server, so only presentation ends.
    fn release_server_tabs(
        &mut self,
        released: Vec<(TabId, Entity<TabView>)>,
        cx: &mut Context<Self>,
    ) {
        // Keep the old focus branches until a deferred window update can inspect them. The
        // publication can itself arrive inside a window update, which cannot reborrow the window.
        let removed: Vec<_> = released
            .iter()
            .map(|(_, view)| view.read(cx).tab_focus().clone())
            .collect();
        for (tab, view) in released {
            view.update(cx, |view, cx| view.close_all(cx));
            let key = TabKey::Server(tab);
            if self.tab_menu == Some(key) {
                self.tab_menu = None;
            }
            if self.tab_selector_pressed == Some(key) {
                self.tab_selector_pressed = None;
            }
            if self.tab_reorder.dragged() == Some(key) {
                self.tab_reorder.cancel();
            }
        }
        if !removed.is_empty() {
            self.defer_restore_focus(Some(removed), cx);
        }
    }

    /// Creates views for Tabs a publication added, once the window is free.
    fn schedule_server_materialize(&mut self, cx: &mut Context<Self>) {
        let Tabs::Server(server) = &mut self.tabs else {
            return;
        };
        if server.materialize_scheduled || server.collection.missing().is_empty() {
            return;
        }
        server.materialize_scheduled = true;
        let window = server.window;
        let manager = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = manager.update(cx, |manager, cx| {
                    manager.materialize_server_tabs(window, cx)
                });
            });
        });
    }

    fn materialize_server_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Tabs::Server(server) = &mut self.tabs else {
            return;
        };
        server.materialize_scheduled = false;
        let publication = server.publication.clone();
        let runtime = server.runtime.clone();
        let factory = Rc::clone(&server.factory);
        let previous = server.active_view().cloned();
        let mut created = Vec::new();
        for tab in server.collection.missing() {
            // A Tab whose Panes cannot all be presented stays out of the strip, and the next
            // publication tries again.
            let Ok(parts) =
                TabView::prepare_server(&publication, tab, &runtime, factory.as_ref(), window, cx)
            else {
                continue;
            };
            created.push(Self::create_server_tab_view(
                parts,
                runtime.clone(),
                Rc::clone(&factory),
                window,
                cx,
            ));
        }
        if created.is_empty() {
            return;
        }
        let Tabs::Server(server) = &mut self.tabs else {
            return;
        };
        for (tab, view) in created {
            view.update(cx, |view, cx| view.deactivate(cx));
            // A view whose Tab left meanwhile is dropped with its Panes' presentations.
            if server.collection.insert(tab, view.clone()).is_err() {
                view.update(cx, |view, cx| view.close_all(cx));
            }
        }
        // A newly presented Active Tab restores focus only after checking the parent's blocker.
        self.present_published_activation(previous, cx);
        self.complete_server_creation(cx);
        self.sync_terminal_focus_blocker(cx);
        cx.emit(TabManagerEvent::PresentationChanged);
        cx.notify();
    }
}

/// Server-backed Tab Manager state, for tests that inspect it directly.
#[cfg(test)]
impl TabManager {
    /// Presented server Tabs with views, in personal order.
    pub(crate) fn server_tab_order(&self) -> Vec<TabId> {
        match &self.tabs {
            Tabs::Server(server) => server.collection.iter().map(|(tab, _)| tab).collect(),
            Tabs::Local(_) => Vec::new(),
        }
    }

    /// The server Tabs the strip shows, in strip order.
    pub(crate) fn visible_server_tabs(&self) -> Vec<TabId> {
        self.visible_tabs()
            .into_iter()
            .filter_map(|(key, _)| match key {
                TabKey::Server(tab) => Some(tab),
                TabKey::Local(_) => None,
            })
            .collect()
    }

    pub(crate) fn active_server_tab(&self) -> Option<TabId> {
        match &self.tabs {
            Tabs::Server(server) => server.active(),
            Tabs::Local(_) => None,
        }
    }

    /// The Terminal Pane presenting `pane` in any of this Workspace's Tabs.
    pub(crate) fn server_terminal(
        &self,
        pane: crate::ui::server_desktop_contracts::PaneId,
        cx: &gpui::App,
    ) -> Option<Entity<crate::ui::TerminalPane>> {
        self.tabs
            .all_views()
            .into_iter()
            .find_map(|view| view.read(cx).server_terminal(pane))
    }

    pub(crate) fn server_tab(&self, tab: TabId) -> Option<Entity<TabView>> {
        self.tabs.view(TabKey::Server(tab)).cloned()
    }

    pub(crate) fn server_root_tab(&self) -> Option<TabId> {
        match &self.tabs {
            Tabs::Server(server) => Some(server.collection.root_tab()),
            Tabs::Local(_) => None,
        }
    }

    pub(crate) fn press_server_tab_for_accessibility(
        &mut self,
        tab: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_tab(
            TabKey::Server(tab),
            SelectionSource::AccessibilityPress,
            window,
            cx,
        );
    }

    pub(crate) fn drop_server_tab_at(
        &mut self,
        tab: TabId,
        position: usize,
        cx: &mut Context<Self>,
    ) {
        self.move_tab(TabKey::Server(tab), position, cx);
    }

    pub(crate) fn request_close_server_tab(&mut self, tab: TabId, cx: &mut Context<Self>) {
        self.request_close_tab(TabKey::Server(tab), cx);
    }
}
