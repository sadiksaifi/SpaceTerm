//! Workspace attachments are client presentation, never shared organization or Session ownership.

use super::*;
use crate::domain::ServerWorkspaceCollection;
use crate::ui::empty_workspace_window::{EmptyWorkspaceAction, EmptyWorkspaceWindow};
use crate::ui::pane_presentation::DisplayName;
use crate::ui::server_client_runtime::{DesktopClientEvent, DesktopClientRuntime};
use crate::ui::server_desktop_contracts::{
    Command, ConnectionState, DesktopClientError, DesktopRequest, DesktopServerFrame,
    DesktopViewRevision, OperationId, Outcome, ReplicaStatus, ServerId, UiRequestId, ViewEdit,
    WorkspaceId as QualifiedWorkspaceId,
};
use crate::ui::tab_manager::ServerTabManagerEvent;
use crate::ui::tab_view::{ServerPaneFactory, ServerPublication};
use crate::ui::workspace_sidebar::server_group::{ServerConnection, UpdateRequired};
use crate::ui::workspace_sidebar::{ServerSidebarEvent, ServerSidebarSection, ServerWorkspaceRow};
use gpui::Focusable;
use spaceterm_core::operations::{
    AssessmentTarget, CloseAssessment, CloseTarget as ServerCloseTarget, OperationFailure,
};

pub(super) struct ServerWorkspaces {
    runtime: Entity<DesktopClientRuntime>,
    pub(super) collection: ServerWorkspaceCollection<Entity<TabManager>>,
    window: gpui::AnyWindowHandle,
    frames: BTreeMap<ServerId, Arc<DesktopServerFrame>>,
    panes: Rc<dyn ServerPaneFactory>,
    empty: Entity<EmptyWorkspaceWindow>,
    switcher: ComboBoxHandle<QualifiedWorkspaceId, ServerId>,
    scope: Option<ServerId>,
    close: Option<PendingClose>,
    confirmation: Option<CloseAssessment>,
    modal: Option<spaceterm_ui::ModalPresentationHandle>,
    closing: bool,
    close_task: Option<Task<()>>,
    save_error: Option<DesktopClientError>,
    requests: BTreeMap<UiRequestId, WorkspaceRequest>,
    paused_detaches: BTreeMap<QualifiedWorkspaceId, PausedDetach>,
    presentation_errors: BTreeMap<QualifiedWorkspaceId, DesktopClientError>,
    navigation: u64,
}

struct WorkspaceRequest {
    request: DesktopRequest,
    admitted: DesktopViewRevision,
    completed: bool,
    applied: Option<DesktopViewRevision>,
    operation: Option<OperationId>,
    navigation: u64,
    origin: Option<QualifiedWorkspaceId>,
    attachment: Option<spaceterm_client::view::WorkspaceAttachment>,
}

struct PausedDetach {
    original_request: UiRequestId,
    attachment: Option<spaceterm_client::view::WorkspaceAttachment>,
}

impl WorkspaceRequest {
    fn personal_workspace(&self) -> Option<QualifiedWorkspaceId> {
        match self.request {
            DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(workspace))
            | DesktopRequest::AttachWorkspace(workspace)
            | DesktopRequest::DetachWorkspace(workspace)
            | DesktopRequest::RetryDetachWorkspace { workspace, .. } => Some(workspace),
            _ => None,
        }
    }

    fn settled(&self, frames: &BTreeMap<ServerId, Arc<DesktopServerFrame>>) -> bool {
        self.completed
            && !matches!(
                self.request,
                DesktopRequest::Shared {
                    command: Command::CreateWorkspace { .. },
                    ..
                }
            )
            && self.personal_workspace().is_none_or(|workspace| {
                self.applied.is_some_and(|revision| {
                    revision > self.admitted
                        && frames
                            .get(&workspace.server())
                            .is_some_and(|frame| frame.view_revision >= revision)
                })
            })
    }
}

struct PendingClose {
    target: AssessmentTarget,
    request: UiRequestId,
    operation: Option<OperationId>,
    confirming: bool,
    changed: bool,
}

impl ServerWorkspaces {
    pub(super) fn sidebar_order(&self, cx: &App) -> Vec<QualifiedWorkspaceId> {
        self.runtime
            .read(cx)
            .servers()
            .iter()
            .flat_map(|info| {
                self.collection
                    .order()
                    .filter(|workspace| workspace.server() == info.server)
            })
            .collect()
    }

    pub(super) fn active_manager(&self) -> Option<&Entity<TabManager>> {
        let workspace = self.collection.active()?;
        if self.paused_detaches.contains_key(&workspace) {
            return None;
        }
        self.collection.payload(workspace)
    }

    pub(super) fn restore_focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(manager) = self.active_manager() {
            manager.update(cx, |manager, cx| manager.restore_focus(window, cx));
        } else {
            self.empty.read(cx).focus_handle(cx).focus(window, cx);
        }
    }
}

impl WorkspaceManager {
    pub(crate) fn new_server(
        runtime: Entity<DesktopClientRuntime>,
        adapters: WorkspaceManagerAdapters,
        default_sidebar: SidebarState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_source(
            WorkspaceSource::Server(runtime),
            adapters,
            default_sidebar,
            window,
            cx,
        )
    }

    pub(super) fn initialize_server(
        &mut self,
        runtime: Entity<DesktopClientRuntime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let empty = cx.new(|cx| EmptyWorkspaceWindow::new(false, false, cx));
        cx.subscribe_in(
            &empty,
            window,
            |manager, _, event: &EmptyWorkspaceAction, window, cx| match event {
                EmptyWorkspaceAction::AttachWorkspace => {
                    manager.open_server_workspace_switcher(None, window, cx)
                }
                EmptyWorkspaceAction::NewWorkspace => {
                    manager.create_server_workspace(None, String::new(), window, cx)
                }
            },
        )
        .detach();
        cx.subscribe_in(
            &runtime,
            window,
            |manager, _, event: &DesktopClientEvent, window, cx| {
                manager.server_client_event(event, window, cx);
            },
        )
        .detach();
        self.server = Some(ServerWorkspaces {
            window: window.window_handle(),
            runtime: runtime.clone(),
            collection: ServerWorkspaceCollection::new(),
            frames: BTreeMap::new(),
            panes: Rc::new(self.pane_construction.clone()),
            empty,
            switcher: ComboBoxHandle::default(),
            scope: None,
            close: None,
            confirmation: None,
            modal: None,
            closing: false,
            close_task: None,
            save_error: None,
            requests: BTreeMap::new(),
            paused_detaches: BTreeMap::new(),
            presentation_errors: BTreeMap::new(),
            navigation: 0,
        });
        let frames: Vec<_> = runtime
            .read(cx)
            .servers()
            .iter()
            .filter_map(|info| runtime.read(cx).frame(info.server))
            .collect();
        for frame in frames {
            self.apply_server_frame_in(frame, window, cx);
        }
        self.refresh_server_sidebar(cx);
    }

    pub(crate) fn is_server_backed(&self) -> bool {
        self.server.is_some()
    }

    pub(crate) fn active_server_workspace(&self) -> Option<QualifiedWorkspaceId> {
        self.server.as_ref()?.collection.active()
    }

    /// Publication applies inactive Workspaces too. It cannot issue explicit use or change membership.
    pub(crate) fn apply_server_frame(
        &mut self,
        frame: Arc<DesktopServerFrame>,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        let Some(server) = &self.server else {
            return;
        };
        let window = server.window;
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = owner.update(cx, |manager, cx| {
                    manager.apply_server_frame_in(frame, window, cx)
                });
            });
        });
    }

    fn apply_server_frame_in(
        &mut self,
        frame: Arc<DesktopServerFrame>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = &mut self.server else {
            return;
        };
        if frame.server != frame.organization.state().server
            || server
                .frames
                .get(&frame.server)
                .is_some_and(|previous| frame.view_revision < previous.view_revision)
        {
            return;
        }
        let previous = server.active_manager().map(Entity::entity_id);
        let removed = server
            .collection
            .apply(&frame.organization, &frame.personal);
        server.paused_detaches.retain(|workspace, paused| {
            server.collection.record(*workspace).is_some()
                && (workspace.server() != frame.server
                    || paused.attachment.is_none_or(|original| {
                        frame
                            .personal
                            .workspace(*workspace)
                            .is_some_and(|view| view.attachment() == original)
                    }))
        });
        for workspace in server.collection.order() {
            if let Some(manager) = server.collection.payload(workspace) {
                manager.update(cx, |manager, cx| {
                    if Some(workspace) == server.collection.active()
                        && !server.paused_detaches.contains_key(&workspace)
                    {
                        manager.activate_without_focus(cx);
                    } else {
                        manager.deactivate(cx);
                    }
                });
            }
        }
        for manager in removed {
            manager.update(cx, |manager, cx| manager.deactivate(cx));
        }
        server.frames.insert(frame.server, Arc::clone(&frame));
        let workspaces: Vec<_> = server
            .collection
            .order()
            .filter(|workspace| workspace.server() == frame.server)
            .collect();
        for workspace in workspaces {
            let Some(record) = server.collection.record(workspace).cloned() else {
                continue;
            };
            let Some(view) = frame.personal.workspace(workspace) else {
                continue;
            };
            if let Some(manager) = server.collection.payload(workspace) {
                if let Err(error) = manager.update(cx, |manager, cx| {
                    manager.apply_workspace(&frame, &record, view, cx)
                }) {
                    server.presentation_errors.insert(workspace, error);
                } else {
                    server.presentation_errors.remove(&workspace);
                }
            } else {
                let runtime = server.runtime.clone();
                let panes = Rc::clone(&server.panes);
                let construction = self.pane_construction.clone();
                let drag = Rc::clone(&self.operating_system_window_drag_platform);
                match TabManager::prepare_server_workspace(
                    &ServerPublication::of(&frame),
                    workspace,
                    runtime,
                    panes,
                    window,
                    cx,
                ) {
                    Ok(prepared) => {
                        let manager = cx.new(|cx| {
                            TabManager::from_prepared_server(
                                prepared,
                                construction,
                                drag,
                                window,
                                cx,
                            )
                        });
                        cx.subscribe_in(
                            &manager,
                            window,
                            |owner, _, event: &ServerTabManagerEvent, window, cx| {
                                owner.server_tab_event(*event, window, cx);
                            },
                        )
                        .detach();
                        server.presentation_errors.remove(&workspace);
                        if server.collection.active() != Some(workspace)
                            || server.paused_detaches.contains_key(&workspace)
                        {
                            manager.update(cx, |manager, cx| manager.deactivate(cx));
                        }
                        let _ = server.collection.insert(workspace, manager);
                    }
                    Err(error) => {
                        server.presentation_errors.insert(workspace, error);
                    }
                }
            }
        }

        server
            .presentation_errors
            .retain(|workspace, _| server.collection.record(*workspace).is_some());
        let next = server.active_manager().map(Entity::entity_id);
        self.refresh_server_sidebar(cx);
        self.sync_terminal_focus_blocker(window, cx);
        self.advance_workspace_requests(window, cx);
        if next != previous {
            let owner = cx.weak_entity();
            let handle = window.window_handle();
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ =
                        owner.update(cx, |manager, cx| {
                            if window.is_window_active()
                                && manager.terminal_focus_blocker(window, cx).is_none()
                                && manager.server.as_ref().and_then(|server| {
                                    server.active_manager().map(Entity::entity_id)
                                }) == next
                            {
                                manager.focus(window, cx);
                            }
                        });
                });
            });
        }
        cx.notify();
    }

    fn refresh_server_sidebar(&mut self, cx: &mut Context<Self>) {
        let Some(server) = &mut self.server else {
            return;
        };
        let infos = server.runtime.read(cx).servers();
        let available = infos.iter().map(|info| info.server).collect();
        let removed = server.collection.retain_servers(&available);
        for manager in removed {
            manager.update(cx, |manager, cx| manager.deactivate(cx));
        }
        server.frames.retain(|id, _| available.contains(id));
        let sections = infos
            .into_iter()
            .map(|info| ServerSidebarSection {
                server: info.server,
                name: info.name,
                connection: server
                    .frames
                    .get(&info.server)
                    .map_or(ServerConnection::Connecting, |frame| {
                        connection(&frame.connection)
                    }),
                rows: server
                    .collection
                    .order()
                    .filter(|workspace| workspace.server() == info.server)
                    .filter_map(|workspace| {
                        let record = server.collection.record(workspace)?;
                        Some(ServerWorkspaceRow {
                            workspace,
                            name: workspace_name(record),
                            pinned: record.pinned_directory.value.is_some(),
                            active: server.collection.active() == Some(workspace),
                            paused: server.paused_detaches.contains_key(&workspace),
                        })
                    })
                    .collect(),
            })
            .collect();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_server_sections(sections, cx));
        let attachable = server.frames.values().any(|frame| {
            shared_ready(frame)
                && frame
                    .organization
                    .state()
                    .workspaces
                    .iter()
                    .any(|record| server.collection.record(record.id).is_none())
        });
        let creatable = server.frames.values().any(|frame| shared_ready(frame));
        server.empty.update(cx, |empty, cx| {
            empty.set_availability(attachable, creatable, cx)
        });
    }

    fn server_client_event(
        &mut self,
        event: &DesktopClientEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = &mut self.server else {
            return;
        };
        match event {
            DesktopClientEvent::FrameChanged(id) => {
                let frame = server.runtime.read(cx).frame(*id);
                if let Some(frame) = frame {
                    self.apply_server_frame(frame, cx);
                } else {
                    self.refresh_server_sidebar(cx);
                    cx.notify();
                }
            }
            DesktopClientEvent::PersonalEditApplied { request, revision } => {
                if let Some(pending) = server.requests.get_mut(request)
                    && *revision > pending.admitted
                    && pending.applied.is_none()
                {
                    pending.applied = Some(*revision);
                }

                let managers: Vec<_> = server.collection.payloads().cloned().collect();
                for manager in managers {
                    manager.update(cx, |manager, cx| {
                        manager.personal_edit_applied(*request, *revision, cx)
                    });
                }
            }
            DesktopClientEvent::RequestCompleted { request, operation } => {
                if let Some(pending) = server.requests.get_mut(request) {
                    pending.completed = true;
                    pending.operation = *operation;
                }

                if let Some(close) = &mut server.close
                    && close.request == *request
                {
                    close.operation = *operation;
                    if let Some(outcome) =
                        operation.and_then(|operation| server.runtime.read(cx).outcome(operation))
                    {
                        self.server_close_outcome(&outcome, window, cx);
                    }
                }
            }
            DesktopClientEvent::RequestFailed { request, error } => {
                if let Some(pending) = server.requests.remove(request) {
                    let detach = match pending.request {
                        DesktopRequest::DetachWorkspace(workspace) => Some((workspace, *request)),
                        DesktopRequest::RetryDetachWorkspace {
                            workspace,
                            original_request,
                        } => Some((workspace, original_request)),
                        _ => None,
                    };
                    if let Some((workspace, original_request)) = detach {
                        server.paused_detaches.insert(
                            workspace,
                            PausedDetach {
                                original_request,
                                attachment: pending.attachment,
                            },
                        );
                        if let Some(tabs) = server.collection.payload(workspace) {
                            tabs.update(cx, |tabs, cx| tabs.deactivate(cx));
                        }
                        self.refresh_server_sidebar(cx);
                        self.show_detach_error(workspace, window, cx);
                    } else {
                        self.show_server_error(*error, window, cx);
                    }
                    cx.notify();
                    return;
                }
                if server
                    .close
                    .as_ref()
                    .is_some_and(|close| close.request == *request)
                {
                    server.close = None;
                    self.show_server_error(*error, window, cx);
                }
            }
            DesktopClientEvent::OperationOutcome {
                operation, outcome, ..
            } => {
                if server
                    .close
                    .as_ref()
                    .is_some_and(|close| close.operation == Some(*operation))
                {
                    self.server_close_outcome(outcome, window, cx);
                }
            }
        }
        self.advance_workspace_requests(window, cx);
        cx.notify();
    }

    fn advance_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(server) = &mut self.server else {
            return false;
        };
        if server.closing {
            return false;
        }
        let Some(next) = server.navigation.checked_add(1) else {
            self.show_server_error(DesktopClientError::CounterExhausted, window, cx);
            return false;
        };
        server.navigation = next;
        true
    }

    /// Creation attention uses the original result revision. Attach attention requires the exact
    /// local application receipt, completion, and presentation, in any delivery order.
    fn advance_workspace_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = &mut self.server else {
            return;
        };
        let mut finished = Vec::new();
        let mut attach = Vec::new();
        let mut activate = Vec::new();
        let mut errors = Vec::new();
        for (id, pending) in &server.requests {
            let current = |workspace| {
                !server.closing
                    && pending.navigation == server.navigation
                    && (pending.origin == server.collection.active()
                        || server.collection.active() == Some(workspace))
            };
            if let DesktopRequest::Shared {
                server: target,
                command: Command::CreateWorkspace { .. },
            } = pending.request
            {
                let Some(operation) = pending.operation else {
                    continue;
                };
                let Some(outcome) = server.runtime.read(cx).outcome(operation) else {
                    continue;
                };
                let workspace = match outcome.as_ref() {
                    Outcome::CreatedWorkspace { workspace, .. } => *workspace,
                    Outcome::CreatedWithControl(created) => match created.entities() {
                        spaceterm_core::operations::CreatedEntities::Workspace {
                            workspace,
                            ..
                        } => workspace,
                        _ => {
                            finished.push(*id);
                            errors.push(DesktopClientError::StaleContext);
                            continue;
                        }
                    },
                    Outcome::Failed(_) | Outcome::Conflict(_) => {
                        finished.push(*id);
                        errors.push(DesktopClientError::UnsupportedCapability);
                        continue;
                    }
                    _ => {
                        finished.push(*id);
                        errors.push(DesktopClientError::StaleContext);
                        continue;
                    }
                };
                if workspace.server() != target {
                    finished.push(*id);
                    continue;
                }
                if !current(workspace) {
                    finished.push(*id);
                    continue;
                }
                let Some(revision) = server.runtime.read(cx).outcome_revision(operation) else {
                    continue;
                };
                let Some(frame) = server.frames.get(&target) else {
                    continue;
                };
                if !shared_ready(frame) || frame.organization.state().revision < revision {
                    continue;
                }
                finished.push(*id);
                if frame
                    .organization
                    .state()
                    .workspaces
                    .iter()
                    .any(|record| record.id == workspace)
                {
                    attach.push(workspace);
                }
            } else if pending.settled(&server.frames) {
                finished.push(*id);
                if let DesktopRequest::AttachWorkspace(workspace) = pending.request
                    && current(workspace)
                    && server.collection.record(workspace).is_some()
                    && server.collection.active() != Some(workspace)
                {
                    activate.push(workspace);
                }
            }
        }
        for request in finished {
            server.requests.remove(&request);
        }
        for workspace in attach {
            self.submit_server(DesktopRequest::AttachWorkspace(workspace), window, cx);
        }
        for workspace in activate {
            self.submit_server(
                DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(workspace)),
                window,
                cx,
            );
        }
        for error in errors {
            self.show_server_error(error, window, cx);
        }
        cx.notify();
    }

    fn submit_server(
        &mut self,
        request: DesktopRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UiRequestId> {
        let server = self.server.as_ref()?;
        let tracked = matches!(
            request,
            DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(_))
                | DesktopRequest::AttachWorkspace(_)
                | DesktopRequest::DetachWorkspace(_)
                | DesktopRequest::RetryDetachWorkspace { .. }
                | DesktopRequest::Shared {
                    command: Command::CreateWorkspace { .. },
                    ..
                }
        );
        if tracked
            && server.requests.len() >= crate::ui::server_client_runtime::MAX_PENDING_UI_REQUESTS
        {
            self.show_server_error(DesktopClientError::AdmissionFull, window, cx);
            return None;
        }
        if let DesktopRequest::RetryDetachWorkspace {
            workspace,
            original_request,
        } = request
            && !server
                .paused_detaches
                .get(&workspace)
                .is_some_and(|paused| paused.original_request == original_request)
        {
            return None;
        }
        let workspace = match &request {
            DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(workspace))
            | DesktopRequest::AttachWorkspace(workspace)
            | DesktopRequest::DetachWorkspace(workspace)
            | DesktopRequest::RetryDetachWorkspace { workspace, .. } => Some(*workspace),
            _ => None,
        };
        let frame = workspace.and_then(|workspace| server.frames.get(&workspace.server()));
        let admitted = frame.map_or(DesktopViewRevision::INITIAL, |frame| frame.view_revision);
        let attachment = workspace
            .and_then(|workspace| frame?.personal.workspace(workspace))
            .map(|view| view.attachment());
        let runtime = server.runtime.clone();
        match runtime.update(cx, |runtime, cx| runtime.request(request.clone(), cx)) {
            Ok(id) => {
                if tracked && let Some(server) = &mut self.server {
                    server.requests.insert(
                        id,
                        WorkspaceRequest {
                            request,
                            admitted,
                            completed: false,
                            applied: None,
                            operation: None,
                            navigation: server.navigation,
                            origin: server.collection.active(),
                            attachment,
                        },
                    );
                }
                Some(id)
            }
            Err(error) => {
                self.show_server_error(error, window, cx);
                None
            }
        }
    }

    pub(super) fn select_server_workspace(
        &mut self,
        workspace: QualifiedWorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.advance_navigation(window, cx) {
            return;
        }
        let server = self.server.as_ref().unwrap();
        let request = if server.collection.record(workspace).is_some() {
            DesktopRequest::PersonalEdit(ViewEdit::ActivateWorkspace(workspace))
        } else {
            if !server
                .frames
                .get(&workspace.server())
                .is_some_and(|frame| shared_ready(frame))
            {
                return;
            }
            DesktopRequest::AttachWorkspace(workspace)
        };
        self.submit_server(request, window, cx);
    }

    pub(super) fn handle_server_sidebar_event(
        &mut self,
        event: ServerSidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ServerSidebarEvent::Activate(workspace) => {
                self.select_server_workspace(workspace, window, cx)
            }
            ServerSidebarEvent::Attach(server) => {
                self.open_server_workspace_switcher(Some(server), window, cx)
            }
            ServerSidebarEvent::Detach(workspace) => {
                if !self.advance_navigation(window, cx) {
                    return;
                }
                let request = self
                    .server
                    .as_ref()
                    .and_then(|server| server.paused_detaches.get(&workspace))
                    .map_or(DesktopRequest::DetachWorkspace(workspace), |paused| {
                        DesktopRequest::RetryDetachWorkspace {
                            workspace,
                            original_request: paused.original_request,
                        }
                    });
                self.submit_server(request, window, cx);
            }
            ServerSidebarEvent::Kill(workspace) => {
                self.request_server_close(AssessmentTarget::KillWorkspace(workspace), window, cx)
            }
        }
    }

    fn server_tab_event(
        &mut self,
        event: ServerTabManagerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = match event {
            ServerTabManagerEvent::SelectPane { pane, source } => {
                DesktopRequest::SelectPane { pane, source }
            }
            ServerTabManagerEvent::OpenTab { tab, source } => {
                DesktopRequest::OpenTab { tab, source }
            }
            ServerTabManagerEvent::ClosePane { pane } => {
                self.request_server_close(
                    AssessmentTarget::Close(ServerCloseTarget::Pane(pane)),
                    window,
                    cx,
                );
                return;
            }
            ServerTabManagerEvent::CloseTab { tab } => {
                self.request_server_close(
                    AssessmentTarget::Close(ServerCloseTarget::Tab(tab)),
                    window,
                    cx,
                );
                return;
            }
            ServerTabManagerEvent::PaneAction { pane, action } => match action.route(pane) {
                crate::ui::server_desktop_contracts::PaneActionRoute::Request(request) => request,
                _ => {
                    self.show_server_error(DesktopClientError::UnsupportedCapability, window, cx);
                    return;
                }
            },
        };
        self.submit_server(request, window, cx);
    }

    pub(super) fn open_server_workspace_switcher(
        &mut self,
        scope: Option<ServerId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window_modal_is_open(window, cx) {
            return;
        }
        if let Some(server) = &mut self.server {
            server.scope = scope;
            server.switcher.open(window, cx);
        }
        self.sync_terminal_focus_blocker(window, cx);
        cx.notify();
    }

    fn server_switcher_items(&self, cx: &App) -> Vec<ComboBoxItem<QualifiedWorkspaceId>> {
        let Some(server) = &self.server else {
            return Vec::new();
        };
        server
            .runtime
            .read(cx)
            .servers()
            .iter()
            .filter(|info| server.scope.is_none_or(|scope| scope == info.server))
            .flat_map(|info| {
                server
                    .frames
                    .get(&info.server)
                    .into_iter()
                    .flat_map(move |frame| {
                        frame
                            .organization
                            .state()
                            .workspaces
                            .iter()
                            .filter_map(move |record| {
                                let attached = server.collection.record(record.id).is_some();
                                if !attached && !shared_ready(frame) {
                                    return None;
                                }
                                let name = workspace_name(record)
                                    .get()
                                    .cloned()
                                    .unwrap_or_else(|| "Unnamed Workspace".into());
                                let server_name = info
                                    .name
                                    .get()
                                    .cloned()
                                    .unwrap_or_else(|| "Unnamed Server".into());
                                let item = ComboBoxItem::new(record.id, name)
                                    .description(server_name.clone())
                                    .keywords([server_name]);
                                Some(if attached {
                                    item
                                } else {
                                    item.trailing(ComboBoxAccessory::Status("Not attached".into()))
                                })
                            })
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub(super) fn create_server_workspace(
        &mut self,
        target: Option<ServerId>,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = &self.server else {
            return;
        };
        let target = target
            .or_else(|| {
                server
                    .collection
                    .active()
                    .map(|workspace| workspace.server())
            })
            .or_else(|| {
                server
                    .runtime
                    .read(cx)
                    .servers()
                    .iter()
                    .find(|info| {
                        server
                            .frames
                            .get(&info.server)
                            .is_some_and(|frame| shared_ready(frame))
                    })
                    .map(|info| info.server)
            });
        let Some(target) = target else {
            self.show_server_error(DesktopClientError::Offline, window, cx);
            return;
        };
        let name = if name.trim().is_empty() {
            spaceterm_core::organization::NameDraft::Automatic
        } else {
            match spaceterm_core::organization::NameText::new(name) {
                Ok(name) => spaceterm_core::organization::NameDraft::Custom(name),
                Err(_) => {
                    self.show_server_error(DesktopClientError::UnsupportedCapability, window, cx);
                    return;
                }
            }
        };
        if !self.advance_navigation(window, cx) {
            return;
        }
        self.submit_server(
            DesktopRequest::Shared {
                server: target,
                command: Command::CreateWorkspace {
                    server: target,
                    name,
                    source: spaceterm_core::operations::WorkspaceCreationSource::ServerHome {
                        server: target,
                    },
                },
            },
            window,
            cx,
        );
    }

    pub(super) fn request_server_close(
        &mut self,
        target: AssessmentTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .server
            .as_ref()
            .is_none_or(|server| server.close.is_some() || server.confirmation.is_some())
        {
            return;
        }
        self.assess_server_close(target, false, window, cx);
    }

    fn assess_server_close(
        &mut self,
        target: AssessmentTarget,
        changed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(frame) = self
            .server
            .as_ref()
            .and_then(|server| server.frames.get(&target.server()))
            && !shared_ready(frame)
        {
            self.show_server_error(frame_refusal(frame), window, cx);
            return;
        }
        let command = match target {
            AssessmentTarget::KillWorkspace(workspace) => {
                Command::AssessKillWorkspace { workspace }
            }
            AssessmentTarget::Close(target) => Command::AssessClose { target },
            _ => return,
        };
        if let Some(request) = self.submit_server(
            DesktopRequest::Shared {
                server: target.server(),
                command,
            },
            window,
            cx,
        ) && let Some(server) = &mut self.server
        {
            server.close = Some(PendingClose {
                target,
                request,
                operation: None,
                confirming: false,
                changed,
            });
        }
    }

    fn server_close_outcome(
        &mut self,
        outcome: &Outcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = &mut self.server else {
            return;
        };
        let Some(pending) = server.close.take() else {
            return;
        };
        if server.closing {
            return;
        }
        match outcome {
            Outcome::Assessment(assessment)
                if !pending.confirming && assessment.summary().target() == pending.target =>
            {
                if !assessment.summary().requires_confirmation() && !pending.changed {
                    self.confirm_server_close(**assessment, window, cx);
                    return;
                }
                server.confirmation = Some(**assessment);
                let assessment = **assessment;
                let copy = close_copy(assessment, server, cx);
                let message = if pending.changed {
                    format!(
                        "This changed after you confirmed. Review what closes now. {}",
                        copy.1
                    )
                } else {
                    copy.1
                };
                let owner = cx.weak_entity();
                let handle = window.window_handle();
                let presentation = Alert::new(
                    ModalId::new("close-confirmation"),
                    copy.0,
                    copy.0,
                    message,
                    vec![
                        ModalAction::new(
                            CloseConfirmationAction::Confirm,
                            copy.2,
                            ModalActionRole::Affirmative,
                            "close-confirmation-confirm",
                        )
                        .with_intent(ModalActionIntent::Destructive)
                        .with_emphasis(ModalActionEmphasis::Prominent),
                        ModalAction::new(
                            CloseConfirmationAction::Cancel,
                            "Cancel",
                            ModalActionRole::Cancel,
                            "close-confirmation-cancel",
                        ),
                    ],
                )
                .intent(AlertIntent::Critical)
                .present(window, cx, move |outcome, cx| {
                    let confirmed = matches!(
                        outcome,
                        AlertOutcome::Activated {
                            action_id: CloseConfirmationAction::Confirm,
                            ..
                        }
                    );
                    let _ = owner.update(cx, |manager, cx| {
                        let current = manager
                            .server
                            .as_mut()
                            .and_then(|server| server.confirmation.take());
                        if confirmed && current == Some(assessment) {
                            let owner = cx.weak_entity();
                            cx.defer(move |cx| {
                                let _ = handle.update(cx, |_, window, cx| {
                                    let _ = owner.update(cx, |manager, cx| {
                                        manager.confirm_server_close(assessment, window, cx)
                                    });
                                });
                            });
                        }
                    });
                });
                if let Some(server) = &mut self.server {
                    match presentation {
                        Ok(handle) => server.modal = Some(handle),
                        Err(_) => {
                            server.confirmation = None;
                            server.save_error = Some(DesktopClientError::UnsupportedCapability);
                        }
                    }
                }
            }
            Outcome::Failed(OperationFailure::StaleAssessment) if pending.confirming => {
                self.assess_server_close(pending.target, true, window, cx)
            }
            Outcome::Applied => {}
            _ => self.show_server_error(DesktopClientError::StaleContext, window, cx),
        }
    }

    fn confirm_server_close(
        &mut self,
        assessment: CloseAssessment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = assessment.summary().target();
        let command = match target {
            AssessmentTarget::KillWorkspace(workspace) => Command::KillWorkspaceConfirmed {
                workspace,
                assessment: assessment.reference(),
            },
            AssessmentTarget::Close(target) => Command::CloseConfirmed {
                target,
                assessment: assessment.reference(),
            },
            _ => return,
        };
        if let Some(request) = self.submit_server(
            DesktopRequest::Shared {
                server: target.server(),
                command,
            },
            window,
            cx,
        ) && let Some(server) = &mut self.server
        {
            server.close = Some(PendingClose {
                target,
                request,
                operation: None,
                confirming: true,
                changed: false,
            });
        }
    }

    fn show_detach_error(
        &mut self,
        workspace: QualifiedWorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(original_request) = self
            .server
            .as_ref()
            .and_then(|server| server.paused_detaches.get(&workspace))
            .map(|paused| paused.original_request)
        else {
            return;
        };
        let owner = cx.weak_entity();
        let handle = window.window_handle();
        let _ = Alert::new(
            ModalId::new("server-detach-failed"),
            "Detach Workspace",
            "Couldn't Detach Workspace",
            "SpaceTerm couldn't save this change on this device. The Workspace stays paused here until the change is saved.",
            vec![
                ModalAction::new(false, "OK", ModalActionRole::Cancel, "server-detach-failed-ok"),
                ModalAction::new(true, "Try Again", ModalActionRole::Affirmative, "server-detach-failed-retry"),
            ],
        ).intent(AlertIntent::Warning).present(window, cx, move |retry, cx| {
            if matches!(retry, spaceterm_ui::AlertOutcome::Activated { action_id: true, .. }) {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = owner.update(cx, |manager, cx| {
                        manager.submit_server(DesktopRequest::RetryDetachWorkspace { workspace, original_request }, window, cx);
                    });
                });
            }
        });
    }

    pub(super) fn show_server_error(
        &mut self,
        error: DesktopClientError,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            error,
            DesktopClientError::TargetGone
                | DesktopClientError::WrongServer
                | DesktopClientError::StaleContext
                | DesktopClientError::InvalidGeometry
                | DesktopClientError::Cancelled
        ) {
            return;
        }
        let message = server_error_message(error);
        let _ = Alert::new(
            ModalId::new("server-request-failed"),
            "Server request",
            "Couldn't Complete Request",
            message,
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                "server-request-failed-ok",
            )],
        )
        .intent(AlertIntent::Informational)
        .present(window, cx, |_, _| {});
    }

    /// Saving while a divider edit is still waiting would persist an older provider view.
    pub(crate) fn save_server_view(
        &mut self,
        quitting: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), DesktopClientError>> {
        let Some(server) = &mut self.server else {
            return Task::ready(Ok(()));
        };
        server.closing = true;
        // Closing presentation cancels attention, not the client-core operation identity.

        server.confirmation = None;
        if let Some(modal) = server.modal.take() {
            let window = server.window;
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, cx| {
                    let _ = modal.dismiss(window, cx);
                });
            });
        }
        let runtime = server.runtime.clone();
        let (wakeup, wakeups) = async_channel::bounded(1);
        let signal = wakeup.clone();
        let runtime_observer = cx.observe(&runtime, move |_, _, _| {
            signal.try_send(()).ok();
        });
        let signal = wakeup.clone();
        let owner_observer = cx.observe(&cx.entity(), move |_, _, _| {
            signal.try_send(()).ok();
        });
        let observers: Vec<_> = server
            .collection
            .payloads()
            .map(|manager| {
                let signal = wakeup.clone();
                cx.observe(manager, move |_, _, _| {
                    signal.try_send(()).ok();
                })
            })
            .collect();
        cx.spawn(async move |owner, cx| {
            let _observers = (runtime_observer, owner_observer, observers);
            loop {
                let ready = owner
                    .update(cx, |manager, cx| {
                        let waiting = manager.server.as_ref().is_some_and(|server| {
                            server
                                .requests
                                .values()
                                .any(|request| request.personal_workspace().is_some())
                                || server.collection.payloads().any(|tabs| {
                                    tabs.read(cx).has_waiting_personal_edits(cx)
                                        || tabs.read(cx).has_unsettled_personal_edits(cx)
                                })
                        });
                        if manager
                            .server
                            .as_ref()
                            .is_some_and(|server| !server.paused_detaches.is_empty())
                        {
                            if let Some(server) = &mut manager.server {
                                server.closing = false;
                            }
                            return Err(DesktopClientError::LocalStorageUnavailable);
                        }
                        Ok(!waiting && !runtime.read(cx).has_pending_local_requests())
                    })
                    .map_err(|_| DesktopClientError::Cancelled)??;
                if ready {
                    let save = runtime.update(cx, |runtime, cx| {
                        if quitting {
                            runtime.prepare_client_quit(cx)
                        } else {
                            runtime.save_view(cx)
                        }
                    });
                    let result = save.await;
                    let _ = owner.update(cx, |manager, _| {
                        if let Some(server) = &mut manager.server {
                            server.closing = false;
                        }
                    });
                    return result;
                }
                wakeups
                    .recv()
                    .await
                    .map_err(|_| DesktopClientError::Cancelled)?;
            }
        })
    }

    pub(super) fn close_server_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(server) = &mut self.server else {
            return true;
        };
        if server.close_task.is_some() {
            return false;
        }
        server.confirmation = None;
        let save = self.save_server_view(false, cx);
        let window_handle = window.window_handle();
        let task = cx.spawn(async move |owner, cx| {
            let result = save.await;
            let _ = owner.update(cx, |manager, cx| {
                if let Some(server) = &mut manager.server {
                    server.close_task = None;
                    server.save_error = result.err();
                }
                match result {
                    Ok(()) => {
                        let _ =
                            cx.update_window(window_handle, |_, window, _| window.remove_window());
                    }
                    Err(error) => {
                        let owner = cx.weak_entity();
                        cx.defer(move |cx| {
                            let _ = window_handle.update(cx, |_, window, cx| {
                                let _ = owner.update(cx, |manager, cx| {
                                    manager.show_server_error(error, window, cx)
                                });
                            });
                        });
                    }
                }
                cx.notify();
            });
        });
        if let Some(server) = &mut self.server {
            server.close_task = Some(task);
        }
        false
    }

    pub(super) fn render_server(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(server) = &self.server else {
            return div().into_any_element();
        };
        let owner = cx.weak_entity();
        let accept = owner.clone();
        let command = owner.clone();
        let items = self.server_switcher_items(cx);
        let infos = server.runtime.read(cx).servers();
        let commands: Vec<_> = infos
            .iter()
            .filter(|info| server.scope.is_none_or(|scope| scope == info.server))
            .map(|info| {
                let label = info
                    .name
                    .get()
                    .map_or("Unnamed Server", |name| name.as_ref());
                ComboBoxCommand::new(info.server, "New Workspace")
                    .description(format!("On {label}"))
                    .disabled(
                        !server
                            .frames
                            .get(&info.server)
                            .is_some_and(|frame| shared_ready(frame)),
                    )
            })
            .collect();
        let switcher = ComboBox::with_commands(
            "workspace-switcher",
            "Switch Workspace",
            server.collection.active(),
            "Switch Workspace",
            items,
            move |_| commands.clone(),
        )
        .handle(server.switcher.clone())
        .copy(ComboBoxCopy::new(
            "Workspace name",
            "Filter or create...",
            "No Workspaces",
            "No matching Workspaces",
        ))
        .on_accept(move |acceptance, window, cx| {
            let _ = accept.update(cx, |manager, cx| {
                manager.select_server_workspace(*acceptance.item_id(), window, cx)
            });
        })
        .on_command(move |activation, window, cx| {
            let _ = command.update(cx, |manager, cx| {
                manager.create_server_workspace(
                    Some(*activation.command()),
                    activation.query().trim().to_owned(),
                    window,
                    cx,
                )
            });
        });
        let content = if !server.collection.has_attachments() {
            server.empty.clone().into_any_element()
        } else if let Some(manager) = server.active_manager() {
            manager.clone().into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .track_focus(&server.empty.read(cx).focus_handle(cx))
                .child(
                    crate::ui::pane_presentation::ContentUnavailable::new(
                        "unavailable-workspace",
                        "unavailable-workspace",
                        spaceterm_ui::IconName::TriangleAlert,
                        "Workspace Unavailable",
                        if server.collection.active().is_some_and(|workspace| {
                            server.paused_detaches.contains_key(&workspace)
                        }) {
                            "The Workspace stays paused here until the change is saved."
                        } else {
                            server
                                .collection
                                .active()
                                .and_then(|workspace| server.presentation_errors.get(&workspace))
                                .map_or(
                                    "The server is catching up. Try again in a moment.",
                                    |error| server_error_message(*error),
                                )
                        },
                    )
                    .prominent(),
                )
                .into_any_element()
        };
        let sidebar_visible = self.sidebar.read(cx).layout().visible;
        let close_owner = owner.clone();
        let close: spaceterm_ui::WindowCloseHandler = Rc::new(move |window, cx| {
            let _ = close_owner.update(cx, |manager, cx| {
                manager.close_server_window(window, cx);
            });
        });
        let detach = owner.clone();
        let new = owner.clone();
        let toggle = owner.clone();
        let drag = owner.clone();
        let appearance = super::super::appearance::chrome(cx).clone();
        let sidebar = self.sidebar.read(cx).layout();
        let identity = server
            .collection
            .active()
            .and_then(|workspace| server.collection.record(workspace))
            .map_or(
                WorkspaceChromeIdentity {
                    name: String::new(),
                    worktree: None,
                    pinned: false,
                    status: None,
                },
                |record| WorkspaceChromeIdentity {
                    name: workspace_name(record)
                        .get()
                        .map_or(String::new(), |name| name.to_string()),
                    worktree: None,
                    pinned: record.pinned_directory.value.is_some(),
                    status: None,
                },
            );
        let chrome = WorkspaceChromeLayout::resolve(sidebar, &identity, window, cx);
        let frame = super::super::workspace_frame::WorkspaceFrame::for_appearance(&appearance, cx);
        if let Some(tabs) = server.active_manager() {
            let controls = gpui::AnyView::from(self.update_control.clone());
            let close = close.clone();
            tabs.update(cx, |tabs, cx| {
                tabs.set_sidebar_layout(sidebar.visible, sidebar.width, chrome.width, cx);
                tabs.set_trailing_accessory(Some(controls), cx);
                tabs.set_window_close_handler(close);
            });
        }
        let (toggle_icon, toggle_label) = sidebar_toggle_presentation(sidebar.visible);
        let icon_size = appearance.icons.metrics(IconRole::Chrome).glyph_size;
        let toggle_button =
            IconButton::new("toggle-sidebar-button", toggle_label, move |foreground| {
                Icon::custom(toggle_icon, icon_size, foreground).into_any_element()
            })
            .variant(ButtonVariant::Ghost)
            .size(TOGGLE_SIZE)
            .debug_selector("toggle-sidebar-button")
            .on_activate(move |_, window, cx| {
                let _ = toggle.update(cx, |manager, cx| manager.toggle_sidebar(window, cx));
            });
        let controls = spaceterm_ui::ClientWindowControls::new(close)
            .side(spaceterm_ui::WindowControlSide::Left)
            .surface_color(gpui_color(appearance.colors.title_bar_background));
        let top = WindowDragRegion::new(
            "workspace-top-chrome-drag-region",
            "Move Operating-System Window from Workspace chrome",
            chrome.render_controls(toggle_button, switcher, controls, cx),
        )
        .status(self.window_drag_status.clone())
        .on_event(move |event, window, cx| {
            drag.update(cx, |manager, cx| {
                manager.handle_operating_system_window_drag_event(*event, window, cx)
            })
            .unwrap_or_default()
        });
        let root = div()
            .id("workspace-manager")
            .debug_selector(|| "workspace-manager".into())
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .key_context(TERMINAL_KEY_CONTEXT)
            .on_action(cx.listener(Self::on_switch_workspace))
            .on_action(cx.listener(Self::on_close_workspace))
            .on_action(move |_: &crate::ui::DetachWorkspace, window, cx| {
                let _ = detach.update(cx, |manager, cx| {
                    if let Some(workspace) = manager.active_server_workspace() {
                        manager.handle_server_sidebar_event(
                            ServerSidebarEvent::Detach(workspace),
                            window,
                            cx,
                        );
                    }
                });
            })
            .on_action(move |_: &crate::ui::NewWorkspace, window, cx| {
                let _ = new.update(cx, |manager, cx| {
                    manager.create_server_workspace(None, String::new(), window, cx)
                });
            })
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_sidebar_focus))
            .on_action(cx.listener(Self::on_activate_workspace_1))
            .on_action(cx.listener(Self::on_activate_workspace_2))
            .on_action(cx.listener(Self::on_activate_workspace_3))
            .on_action(cx.listener(Self::on_activate_workspace_4))
            .on_action(cx.listener(Self::on_activate_workspace_5))
            .on_action(cx.listener(Self::on_activate_workspace_6))
            .on_action(cx.listener(Self::on_activate_workspace_7))
            .on_action(cx.listener(Self::on_activate_workspace_8))
            .on_action(cx.listener(Self::on_activate_workspace_9))
            .child(Self::render_top_left_chrome_surface(chrome, window, cx))
            .child(div().relative().size_full().child(content))
            .when(sidebar_visible, |root| root.child(self.sidebar.clone()))
            .child(self.sidebar.read(cx).render_resize_handle(
                self.sidebar.downgrade(),
                chrome.width,
                frame.top_chrome_height(appearance.top_height()),
            ))
            .child(
                spaceterm_ui::ControlHost::TitleBar.mount(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(chrome.width)
                        .h(frame.top_chrome_height(appearance.top_height()))
                        .child(top),
                ),
            );
        ModalLayer::new(super::super::window_shell::render(root, window, cx)).into_any_element()
    }
}

fn shared_ready(frame: &DesktopServerFrame) -> bool {
    frame.replica_status == ReplicaStatus::Ready
        && matches!(frame.connection, ConnectionState::Connected { .. })
}

fn connection(state: &ConnectionState) -> ServerConnection {
    match state {
        ConnectionState::Connected { .. } => ServerConnection::Connected,
        ConnectionState::Connecting { .. } => ServerConnection::Connecting,
        ConnectionState::Offline { .. } => ServerConnection::Offline,
        ConnectionState::Stopped => ServerConnection::Stopped,
        ConnectionState::Incompatible { .. } => {
            ServerConnection::Incompatible(UpdateRequired::ThisComputer)
        }
    }
}

fn workspace_name(record: &spaceterm_core::organization::WorkspaceRecord) -> DisplayName {
    DisplayName::reported(record.name.value.display.as_str())
}

fn close_copy(
    assessment: CloseAssessment,
    server: &ServerWorkspaces,
    cx: &App,
) -> (&'static str, String, &'static str) {
    let summary = assessment.summary();
    let sessions = summary.live_session_count();
    let ending = if sessions == 0 {
        String::new()
    } else {
        format!(
            " {sessions} Terminal Sessions end for every client. Any command running in them stops."
        )
    };
    match summary.target() {
        AssessmentTarget::KillWorkspace(workspace) => {
            let name = server
                .collection
                .record(workspace)
                .map(workspace_name)
                .and_then(|name| name.get().cloned())
                .unwrap_or_else(|| "This Workspace".into());
            let server_name = server
                .runtime
                .read(cx)
                .servers()
                .iter()
                .find(|info| info.server == workspace.server())
                .and_then(|info| info.name.get().cloned())
                .unwrap_or_else(|| "the server".into());
            (
                "Kill Workspace?",
                format!(
                    "{name} and its {} Tabs are deleted from {server_name} for every client.{ending} This can't be undone. Repository files and Git Worktree directories stay on disk.",
                    summary.tab_count()
                ),
                "Kill Workspace",
            )
        }
        AssessmentTarget::Close(ServerCloseTarget::Pane(_)) => (
            "Close Pane?",
            format!("It closes for every client.{ending}"),
            "Close Pane",
        ),
        AssessmentTarget::Close(ServerCloseTarget::Tab(_)) => (
            "Close Tab?",
            format!(
                "Its {} Panes close for every client.{ending}",
                summary.pane_count()
            ),
            "Close Tab",
        ),
        _ => (
            "Close Tabs?",
            format!(
                "{} Tabs with {} Panes close for every client.{ending}",
                summary.tab_count(),
                summary.pane_count()
            ),
            "Close Tabs",
        ),
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;

fn frame_refusal(frame: &DesktopServerFrame) -> DesktopClientError {
    match frame.connection {
        ConnectionState::Offline { .. } | ConnectionState::Connecting { .. } => {
            DesktopClientError::Offline
        }
        ConnectionState::Stopped => DesktopClientError::Stopped,
        ConnectionState::Incompatible { .. } => DesktopClientError::Incompatible,
        ConnectionState::Connected { .. } => DesktopClientError::Reconciling,
    }
}

fn server_error_message(error: DesktopClientError) -> &'static str {
    match error {
        DesktopClientError::Offline => "The server is offline. Nothing changed.",
        DesktopClientError::Stopped => "The server is stopped. Nothing changed.",
        DesktopClientError::Incompatible => "The server needs an update first. Nothing changed.",
        DesktopClientError::Reconciling => "The server is catching up. Try again in a moment.",
        DesktopClientError::AdmissionFull => {
            "SpaceTerm is still finishing earlier requests. Try again in a moment."
        }
        DesktopClientError::LocalStorageUnavailable => {
            "SpaceTerm can't save changes on this device right now. Nothing changed."
        }
        DesktopClientError::AuthorizationUnavailable => {
            "SpaceTerm can't get authorization for this right now. Nothing changed."
        }
        _ => "The server can't complete this action right now.",
    }
}
