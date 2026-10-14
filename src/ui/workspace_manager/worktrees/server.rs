//! Qualified Worktree consumers. Only the organization replica changes Tab membership.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gpui::{App, Context, Entity, Subscription, Task, Window};
use spaceterm_client::git::{
    replica::GitFreshness,
    rpc::{GitOperationState, TrackedGitOperation},
};
use spaceterm_client::view::WorkspaceAttachment;
use spaceterm_core::{
    authority::OrganizationRevision,
    identity::{WorkspaceId, WorktreeId},
    operations::{Command, OperationFailure, OperationId, TabCreationSource},
    organization::OrganizationSnapshot,
    worktrees::{
        ApprovedRemoval, GitCommand, GitMutationOutcomeParts, GitStatusTarget, MAX_GIT_PANES,
        MAX_GIT_TABS, queries::GitAssessmentPage,
    },
};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId, ModalPresentationHandle,
};

use super::super::WorkspaceManager;
use crate::repository_status::RepositoryView;
use crate::repository_status::presentation::{MAXIMUM_PATH_CHARS, plural, sanitize_for_display};
use crate::ui::server_desktop_contracts::{
    DesktopClientError, DesktopRequest, UiRequestId, ViewEdit,
};
use crate::ui::server_git::{ServerGitChanged, ServerGitInterest, ServerGitStore};
use crate::ui::worktree_form::{ServerWorktreeFormContext, WorktreeForm, WorktreeFormEvent};
use crate::ui::worktree_store::InstalledWorktrees;

#[derive(Default)]
pub(in crate::ui::workspace_manager) struct ServerWorktreeConsumers {
    store: Option<Entity<ServerGitStore>>,
    inventories: BTreeMap<WorkspaceId, ServerGitInterest>,
    statuses: BTreeMap<StatusTarget, ServerGitInterest>,
    changes: Option<Subscription>,
    form: Option<(WorkspaceId, Entity<WorktreeForm>)>,
    form_context: Option<(spaceterm_core::authority::FieldVersion, bool)>,
    original_creations: BTreeMap<WorkspaceId, OperationId>,
    created: Option<CreatedWorktree>,
    openings: BTreeMap<UiRequestId, OpeningWorktree>,
    deferred_open: Option<(WorkspaceId, WorktreeId, u64)>,
    organizations: BTreeMap<spaceterm_core::identity::ServerId, Arc<OrganizationSnapshot>>,
    removal: Option<Removal>,
    read: Option<Task<()>>,
    generation: u64,
}

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
enum StatusTarget {
    Workspace(WorkspaceId),
    Worktree(WorktreeId),
}
impl StatusTarget {
    fn git(self) -> GitStatusTarget {
        match self {
            Self::Workspace(id) => GitStatusTarget::Workspace(id),
            Self::Worktree(id) => GitStatusTarget::Worktree(id),
        }
    }
    fn of(target: GitStatusTarget) -> Option<Self> {
        match target {
            GitStatusTarget::Workspace(id) => Some(Self::Workspace(id)),
            GitStatusTarget::Worktree(id) => Some(Self::Worktree(id)),
            _ => None,
        }
    }
}

struct CreatedWorktree {
    workspace: WorkspaceId,
    worktree: WorktreeId,
    revision: OrganizationRevision,
    attachment: WorkspaceAttachment,
    navigation: u64,
}

struct OpeningWorktree {
    workspace: WorkspaceId,
    worktree: WorktreeId,
    attachment: WorkspaceAttachment,
    navigation: u64,
    operation: Option<OperationId>,
}

struct Removal {
    worktree: WorktreeId,
    generation: u64,
    assessed: Arc<OrganizationSnapshot>,
    assessment: Option<GitAssessmentPage>,
    modal: Option<ModalPresentationHandle>,
    operation: Option<OperationId>,
    submitted: bool,
    committed: Option<OrganizationRevision>,
    observer: Option<Task<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemovalAction {
    Remove,
    Cancel,
}

impl WorkspaceManager {
    /// Interests include unattached and inactive Workspaces. A publication cannot run local Git.
    pub(in crate::ui::workspace_manager) fn sync_server_git(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = InstalledWorktrees::server_store(cx) else {
            return;
        };
        let Some(server) = &self.server else { return };
        let frames = server.frames.values().cloned().collect::<Vec<_>>();
        let consumers = &mut self.sidebar_worktrees.server;
        consumers.store = Some(store.clone());
        if consumers.changes.is_none() {
            consumers.changes = Some(cx.subscribe_in(
                &store,
                window,
                |manager, _, _: &ServerGitChanged, window, cx| {
                    manager.sync_server_git(window, cx);
                    manager.refresh_server_sidebar(cx);
                    cx.notify();
                },
            ));
        }
        // The application provider delivers organization changes, including unattached
        // Workspaces. Window reconciliation only updates its local read model.
        for frame in &frames {
            consumers
                .organizations
                .insert(frame.server, frame.organization.clone());
        }
        consumers
            .organizations
            .retain(|server, _| frames.iter().any(|frame| frame.server == *server));
        let wanted = frames
            .iter()
            .flat_map(|frame| {
                frame
                    .organization
                    .state()
                    .workspaces
                    .iter()
                    .map(|record| record.id)
            })
            .collect::<BTreeSet<_>>();
        let gone = consumers
            .inventories
            .keys()
            .filter(|id| !wanted.contains(id))
            .copied()
            .collect::<Vec<_>>();
        for workspace in gone {
            if let Some(id) = consumers.inventories.remove(&workspace) {
                store.update(cx, |store, cx| store.unregister(id, cx));
            }
        }
        consumers
            .inventories
            .retain(|_, id| store.read(cx).contains_interest(*id));
        for workspace in wanted {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                consumers.inventories.entry(workspace)
                && let Ok(id) =
                    store.update(cx, |store, cx| store.register_inventory(workspace, cx))
            {
                entry.insert(id);
            }
        }
        let mut targets = BTreeMap::new();
        for frame in &frames {
            let state = frame.organization.state();
            for workspace in &state.workspaces {
                targets.insert(StatusTarget::Workspace(workspace.id), workspace.id);
                for tab in state
                    .tabs
                    .iter()
                    .filter(|tab| tab.workspace == workspace.id)
                {
                    if let Some(worktree) = tab.worktree {
                        targets
                            .entry(StatusTarget::Worktree(worktree))
                            .or_insert(workspace.id);
                    }
                }
                if let Some(id) = consumers.inventories.get(&workspace.id)
                    && let Some(inventory) = store.read(cx).inventory(*id)
                {
                    for record in inventory.records {
                        targets
                            .entry(StatusTarget::Worktree(record.worktree))
                            .or_insert(workspace.id);
                    }
                }
            }
        }
        let gone = consumers
            .statuses
            .keys()
            .filter(|target| !targets.contains_key(target))
            .copied()
            .collect::<Vec<_>>();
        for target in gone {
            if let Some(id) = consumers.statuses.remove(&target) {
                store.update(cx, |store, cx| store.unregister(id, cx));
            }
        }
        consumers
            .statuses
            .retain(|_, id| store.read(cx).contains_interest(*id));
        for (target, workspace) in targets {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                consumers.statuses.entry(target)
                && let Ok(id) = store.update(cx, |store, cx| {
                    store.register_status(workspace, target.git(), cx)
                })
            {
                entry.insert(id);
            }
        }
        if let Some((workspace, form)) = &consumers.form
            && let Some(frame) = frames
                .iter()
                .find(|frame| frame.server == workspace.server())
        {
            let context = (
                frame
                    .organization
                    .state()
                    .settings
                    .worktree_location
                    .version,
                super::super::server::shared_ready(frame),
            );
            if consumers.form_context != Some(context) {
                consumers.form_context = Some(context);
                form.update(cx, |form, cx| {
                    form.server_context_changed(context.0, context.1, cx)
                });
            }
        }
        let deferred = self.sidebar_worktrees.server.deferred_open;
        if let Some((workspace, worktree, navigation)) = deferred
            && let Some(server) = &self.server
        {
            if server.navigation != navigation {
                self.sidebar_worktrees.server.deferred_open = None;
            } else if server.collection.active() == Some(workspace) {
                self.sidebar_worktrees.server.deferred_open = None;
                self.open_server_worktree(workspace, worktree, window, cx);
            }
        }
        if let Some(removal) = &self.sidebar_worktrees.server.removal
            && let Some(revision) = removal.committed
            && frames.iter().any(|frame| {
                frame.server == removal.worktree.server()
                    && frame.organization.state().revision >= revision
                    && !frame
                        .organization
                        .state()
                        .worktrees
                        .iter()
                        .any(|record| record.id == removal.worktree)
            })
        {
            self.sidebar_worktrees.server.removal = None;
        }
        self.advance_server_created_worktree(window, cx);
        self.advance_server_worktree_openings(window, cx);
    }

    pub(in crate::ui::workspace_manager) fn server_repository_view(
        &self,
        target: GitStatusTarget,
        cx: &App,
    ) -> RepositoryView {
        let consumers = &self.sidebar_worktrees.server;
        let Some(store) = &consumers.store else {
            return RepositoryView::Hidden;
        };
        let Some(key) = StatusTarget::of(target) else {
            return RepositoryView::Hidden;
        };
        let Some(id) = consumers.statuses.get(&key) else {
            return RepositoryView::Hidden;
        };
        let Some(mut view) = store.read(cx).status(*id) else {
            return RepositoryView::Hidden;
        };
        let Some(frame) = self
            .server
            .as_ref()
            .and_then(|server| server.frames.get(&target.server()))
        else {
            return RepositoryView::Hidden;
        };
        if !super::super::server::shared_ready(frame) {
            view.freshness = GitFreshness::LastKnown;
        }
        let directory = match target {
            GitStatusTarget::Worktree(worktree) => frame
                .organization
                .state()
                .worktrees
                .iter()
                .find(|record| record.id == worktree)
                .map(|record| &record.directory),
            GitStatusTarget::Workspace(workspace) => frame
                .organization
                .state()
                .workspaces
                .iter()
                .find(|record| record.id == workspace)
                .and_then(|record| {
                    record.pinned_directory.value.as_ref().or_else(|| {
                        let tab = frame
                            .organization
                            .state()
                            .tabs
                            .iter()
                            .find(|tab| tab.id == record.root_tab)?;
                        frame
                            .organization
                            .state()
                            .panes
                            .iter()
                            .find(|pane| pane.id == tab.root_pane)
                            .map(|pane| pane.restoration_directory())
                    })
                }),
            GitStatusTarget::Pane(_) => None,
        };
        directory.map_or(RepositoryView::Hidden, |directory| {
            view.repository_view(directory)
        })
    }

    pub(in crate::ui::workspace_manager) fn server_creates_worktrees(
        &self,
        workspace: WorkspaceId,
        cx: &App,
    ) -> bool {
        let consumers = &self.sidebar_worktrees.server;
        self.server
            .as_ref()
            .and_then(|server| server.frames.get(&workspace.server()))
            .is_some_and(|frame| super::super::server::shared_ready(frame))
            && consumers
                .store
                .as_ref()
                .and_then(|store| {
                    consumers
                        .inventories
                        .get(&workspace)
                        .and_then(|id| store.read(cx).inventory(*id))
                })
                .is_some_and(|view| view.scope.is_some() && view.freshness == GitFreshness::Current)
    }

    pub(in crate::ui::workspace_manager) fn server_worktree_rows(
        &self,
        workspace: WorkspaceId,
        cx: &App,
    ) -> Vec<crate::ui::workspace_sidebar::ServerWorktreeRow> {
        let Some(frame) = self
            .server
            .as_ref()
            .and_then(|server| server.frames.get(&workspace.server()))
        else {
            return Vec::new();
        };
        let consumers = &self.sidebar_worktrees.server;
        let inventory = consumers.store.as_ref().and_then(|store| {
            consumers
                .inventories
                .get(&workspace)
                .and_then(|id| store.read(cx).inventory(*id))
        });
        let Some(inventory) = inventory else {
            return Vec::new();
        };
        let mut records = inventory.records;
        // Keep formerly listed Worktrees with Tabs. Only the replica determines membership.
        for tab in frame
            .organization
            .state()
            .tabs
            .iter()
            .filter(|tab| tab.workspace == workspace)
        {
            if let Some(worktree) = tab.worktree
                && !records.iter().any(|record| record.worktree == worktree)
                && let Some(record) = frame
                    .organization
                    .state()
                    .worktrees
                    .iter()
                    .find(|record| record.id == worktree)
            {
                records.push(spaceterm_core::worktrees::queries::GitInventoryRecord {
                    worktree,
                    directory: record.directory.clone(),
                    main: record.main,
                    missing: record.missing,
                    listed: false,
                    version: spaceterm_core::authority::FieldVersion::new(0),
                });
            }
        }
        if records.len() < 2 {
            return Vec::new();
        }
        records
            .into_iter()
            .map(|record| {
                let path = sanitize_for_display(
                    &String::from_utf8_lossy(record.directory.bytes()),
                    MAXIMUM_PATH_CHARS,
                );
                let name = path
                    .rsplit('/')
                    .find(|part| !part.is_empty())
                    .unwrap_or("Worktree")
                    .to_owned();
                crate::ui::workspace_sidebar::ServerWorktreeRow {
                    worktree: record.worktree,
                    name: name.into(),
                    path: path.into(),
                    repository: crate::repository_status::presentation::SidebarBadge::from_view(
                        &self
                            .server_repository_view(GitStatusTarget::Worktree(record.worktree), cx),
                    ),
                    active: frame.personal.workspace(workspace).is_some_and(|view| {
                        view.active_worktree(&frame.organization) == Some(record.worktree)
                    }),
                    has_tabs: frame.organization.state().tabs.iter().any(|tab| {
                        tab.workspace == workspace && tab.worktree == Some(record.worktree)
                    }),
                    removable: record.listed
                        && !record.main
                        && inventory.freshness == GitFreshness::Current,
                }
            })
            .collect()
    }

    pub(in crate::ui::workspace_manager) fn new_server_worktree(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let consumers = &self.sidebar_worktrees.server;
        if consumers
            .form
            .as_ref()
            .is_some_and(|(_, form)| form.read(cx).is_open())
        {
            return false;
        }
        let Some(server) = &self.server else {
            return false;
        };
        let Some(frame) = server.frames.get(&workspace.server()) else {
            return false;
        };
        if !super::super::server::shared_ready(frame) {
            return false;
        }
        let Some(attachment) = frame
            .personal
            .workspace(workspace)
            .map(|view| view.attachment())
        else {
            return false;
        };
        let Some(store) = &consumers.store else {
            return false;
        };
        let Some(inventory) = consumers
            .inventories
            .get(&workspace)
            .and_then(|id| store.read(cx).inventory(*id))
        else {
            return false;
        };
        if inventory.freshness != GitFreshness::Current {
            return false;
        }
        let Some(repository) = inventory.scope else {
            return false;
        };
        let Ok(requests) = store.read(cx).requests(workspace.server()) else {
            return false;
        };
        let Some(record) = frame
            .organization
            .state()
            .repositories
            .iter()
            .find(|record| record.id == repository.repository)
        else {
            return false;
        };
        let name = String::from_utf8_lossy(record.directory.bytes())
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or("repository")
            .to_owned();
        let default_base = consumers
            .statuses
            .get(&StatusTarget::Workspace(workspace))
            .and_then(|id| store.read(cx).status(*id))
            .and_then(|view| view.facts)
            .and_then(|facts| facts.branch)
            .map(|branch| branch.as_str().to_owned());
        let context = ServerWorktreeFormContext {
            workspace,
            repository,
            repository_name: name,
            location_version: frame
                .organization
                .state()
                .settings
                .worktree_location
                .version,
            checked_out: Vec::new(),
            default_base,
            online: true,
            original_operation: consumers.original_creations.get(&workspace).copied(),
        };
        let navigation = server.navigation;
        let location_version = frame
            .organization
            .state()
            .settings
            .worktree_location
            .version;
        let Ok(form) = WorktreeForm::new_server_entity(requests, context, window, cx) else {
            return false;
        };
        cx.subscribe_in(
            &form,
            window,
            move |manager, _, event, window, cx| match event {
                WorktreeFormEvent::ServerCreated {
                    workspace,
                    worktree,
                    revision,
                    ..
                } => {
                    manager
                        .sidebar_worktrees
                        .server
                        .original_creations
                        .remove(workspace);
                    manager.sidebar_worktrees.server.created = Some(CreatedWorktree {
                        workspace: *workspace,
                        worktree: *worktree,
                        revision: *revision,
                        attachment,
                        navigation,
                    });
                    manager.advance_server_created_worktree(window, cx);
                }
                WorktreeFormEvent::ServerOperation {
                    workspace,
                    operation,
                    state,
                } => {
                    if matches!(state, GitOperationState::Rejected(_)) {
                        manager
                            .sidebar_worktrees
                            .server
                            .original_creations
                            .remove(workspace);
                    } else {
                        manager
                            .sidebar_worktrees
                            .server
                            .original_creations
                            .insert(*workspace, *operation);
                    }
                }
                _ => {}
            },
        )
        .detach();
        let presented = form.update(cx, |form, cx| form.present(window, cx));
        self.sidebar_worktrees.server.form_context = Some((location_version, true));
        self.sidebar_worktrees.server.form = Some((workspace, form));
        presented
    }

    fn advance_server_created_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(created) = &self.sidebar_worktrees.server.created else {
            return;
        };
        let Some(server) = &self.server else { return };
        let Some(frame) = server.frames.get(&created.workspace.server()) else {
            return;
        };
        // A late result is still committed, but cannot attach or steal selection after navigation.
        if server.navigation != created.navigation
            || frame
                .personal
                .workspace(created.workspace)
                .is_none_or(|view| view.attachment() != created.attachment)
            || server.collection.active() != Some(created.workspace)
        {
            self.sidebar_worktrees.server.created = None;
            return;
        }
        if !super::super::server::shared_ready(frame)
            || frame.organization.state().revision < created.revision
        {
            return;
        }
        if !frame
            .organization
            .state()
            .worktrees
            .iter()
            .any(|record| record.id == created.worktree)
        {
            return;
        }
        let Some(created) = self.sidebar_worktrees.server.created.take() else {
            return;
        };
        self.open_server_worktree(created.workspace, created.worktree, window, cx);
    }

    pub(in crate::ui::workspace_manager) fn open_server_worktree(
        &mut self,
        workspace: WorkspaceId,
        worktree: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_server_workspace() != Some(workspace) {
            self.select_server_workspace(workspace, window, cx);
            if let Some(server) = &self.server {
                self.sidebar_worktrees.server.deferred_open =
                    Some((workspace, worktree, server.navigation));
            }
            return;
        }
        if !self.advance_navigation(window, cx) {
            return;
        }
        let Some(server) = &self.server else { return };
        let Some(frame) = server.frames.get(&workspace.server()) else {
            return;
        };
        if frame.personal.workspace(workspace).is_none() || worktree.server() != workspace.server()
        {
            return;
        }
        let attachment = frame
            .personal
            .workspace(workspace)
            .map(|view| view.attachment());
        let navigation = server.navigation;
        let tabs = frame
            .organization
            .state()
            .tabs
            .iter()
            .any(|tab| tab.workspace == workspace && tab.worktree == Some(worktree));
        let request = if tabs {
            DesktopRequest::PersonalEdit(ViewEdit::ActivateWorktree {
                workspace,
                worktree: Some(worktree),
            })
        } else {
            if !super::super::server::shared_ready(frame)
                || !frame
                    .organization
                    .state()
                    .worktrees
                    .iter()
                    .any(|record| record.id == worktree && !record.missing)
            {
                return;
            }
            DesktopRequest::Shared {
                server: workspace.server(),
                command: Command::CreateTab {
                    workspace,
                    source: TabCreationSource::Worktree(worktree),
                },
            }
        };
        if let Some(request) = self.submit_server(request, window, cx)
            && !tabs
            && let Some(attachment) = attachment
        {
            self.sidebar_worktrees.server.openings.insert(
                request,
                OpeningWorktree {
                    workspace,
                    worktree,
                    attachment,
                    navigation,
                    operation: None,
                },
            );
        }
    }

    pub(in crate::ui::workspace_manager) fn server_worktree_event(
        &mut self,
        event: &crate::ui::server_client_runtime::DesktopClientEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::server_client_runtime::DesktopClientEvent;
        match event {
            DesktopClientEvent::RequestCompleted { request, operation } => {
                if let Some(opening) = self.sidebar_worktrees.server.openings.get_mut(request) {
                    opening.operation = *operation;
                }
                if operation.is_none() {
                    self.sidebar_worktrees.server.openings.remove(request);
                }
            }
            DesktopClientEvent::RequestFailed { request, .. } => {
                self.sidebar_worktrees.server.openings.remove(request);
            }
            _ => {}
        }
        self.advance_server_worktree_openings(window, cx);
    }

    fn advance_server_worktree_openings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = &self.server else { return };
        let mut finished = Vec::new();
        let mut activate = Vec::new();
        for (request, opening) in &self.sidebar_worktrees.server.openings {
            let Some(frame) = server.frames.get(&opening.workspace.server()) else {
                continue;
            };
            if server.navigation != opening.navigation
                || server.collection.active() != Some(opening.workspace)
                || frame
                    .personal
                    .workspace(opening.workspace)
                    .is_none_or(|view| view.attachment() != opening.attachment)
            {
                finished.push(*request);
                continue;
            }
            let Some(operation) = opening.operation else {
                continue;
            };
            let Some(outcome) = server.runtime.read(cx).outcome(operation) else {
                continue;
            };
            let tab = match outcome.as_ref() {
                spaceterm_core::operations::Outcome::CreatedTab { tab, .. } => Some(*tab),
                spaceterm_core::operations::Outcome::CreatedWithControl(created) => {
                    match created.entities() {
                        spaceterm_core::operations::CreatedEntities::Tab { tab, .. } => Some(tab),
                        _ => None,
                    }
                }
                _ => None,
            };
            let Some(tab) = tab else {
                finished.push(*request);
                continue;
            };
            let Some(revision) = server.runtime.read(cx).outcome_revision(operation) else {
                continue;
            };
            if !super::super::server::shared_ready(frame)
                || frame.organization.state().revision < revision
            {
                continue;
            }
            if frame.organization.state().tabs.iter().any(|record| {
                record.id == tab
                    && record.workspace == opening.workspace
                    && record.worktree == Some(opening.worktree)
            }) {
                activate.push((opening.workspace, tab));
            }
            finished.push(*request);
        }
        for request in finished {
            self.sidebar_worktrees.server.openings.remove(&request);
        }
        for (workspace, tab) in activate {
            self.submit_server(
                DesktopRequest::PersonalEdit(ViewEdit::ActivateTab { workspace, tab }),
                window,
                cx,
            );
        }
    }

    pub(in crate::ui::workspace_manager) fn server_git_window_activated(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.active_server_workspace() else {
            return;
        };
        let consumers = &self.sidebar_worktrees.server;
        let interests = consumers
            .statuses
            .iter()
            .filter(|(target, _)| matches!(target, StatusTarget::Workspace(id) if *id == workspace))
            .map(|(_, id)| *id)
            .collect::<Vec<_>>();
        if let Some(store) = &consumers.store {
            store.update(cx, |store, cx| {
                let _ = store.window_activated(interests, cx);
            });
        }
    }

    pub(in crate::ui::workspace_manager) fn release_server_git(&mut self, cx: &mut App) {
        let consumers = std::mem::take(&mut self.sidebar_worktrees.server);
        if let Some(store) = consumers.store {
            for id in consumers
                .inventories
                .into_values()
                .chain(consumers.statuses.into_values())
            {
                store.update(cx, |store, cx| store.unregister(id, cx));
            }
        }
    }

    pub(in crate::ui::workspace_manager) fn step_server_worktree(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.advance_navigation(window, cx) {
            return;
        }
        let Some(workspace) = self.active_server_workspace() else {
            return;
        };
        let Some(frame) = self
            .server
            .as_ref()
            .and_then(|server| server.frames.get(&workspace.server()))
        else {
            return;
        };
        let Some(view) = frame.personal.workspace(workspace) else {
            return;
        };
        let present = frame
            .organization
            .state()
            .tabs
            .iter()
            .filter(|tab| tab.workspace == workspace)
            .map(|tab| tab.worktree)
            .collect::<BTreeSet<_>>();
        let mut order = Vec::new();
        if present.contains(&None) {
            order.push(None);
        }
        if let Some(store) = &self.sidebar_worktrees.server.store
            && let Some(inventory) = self
                .sidebar_worktrees
                .server
                .inventories
                .get(&workspace)
                .and_then(|id| store.read(cx).inventory(*id))
        {
            order.extend(
                inventory
                    .records
                    .iter()
                    .map(|record| Some(record.worktree))
                    .filter(|worktree| present.contains(worktree)),
            );
        }
        for worktree in present {
            if !order.contains(&worktree) {
                order.push(worktree);
            }
        }
        if order.len() < 2 {
            return;
        }
        let position = order
            .iter()
            .position(|worktree| *worktree == view.active_worktree(&frame.organization));
        let index = match (position, forward) {
            (Some(index), true) => (index + 1) % order.len(),
            (Some(index), false) => (index + order.len() - 1) % order.len(),
            (None, true) => 0,
            (None, false) => order.len() - 1,
        };
        self.submit_server(
            DesktopRequest::PersonalEdit(ViewEdit::ActivateWorktree {
                workspace,
                worktree: order[index],
            }),
            window,
            cx,
        );
    }

    pub(in crate::ui::workspace_manager) fn remove_server_worktree(
        &mut self,
        worktree: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(removal) = &self.sidebar_worktrees.server.removal
            && removal.submitted
        {
            if removal.worktree != worktree {
                return;
            }
            let Some(operation) = removal.operation else {
                return;
            };
            let generation = removal.generation;
            let Some(store) = &self.sidebar_worktrees.server.store else {
                return;
            };
            let Ok(requests) = store.read(cx).requests(worktree.server()) else {
                return;
            };
            let handle = window.window_handle();
            cx.spawn(async move |manager, cx| {
                if let Ok(tracked) = requests.resume(operation).await {
                    let _ = handle.update(cx, |_, window, cx| {
                        let _ = manager.update(cx, |manager, cx| {
                            manager.observe_server_removal(generation, tracked, window, cx)
                        });
                    });
                }
            })
            .detach();
            return;
        }
        let Some(frame) = self
            .server
            .as_ref()
            .and_then(|server| server.frames.get(&worktree.server()))
        else {
            return;
        };
        if !super::super::server::shared_ready(frame) {
            return;
        }
        let Some(store) = &self.sidebar_worktrees.server.store else {
            return;
        };
        let Ok(requests) = store.read(cx).requests(worktree.server()) else {
            return;
        };
        self.sidebar_worktrees.server.generation =
            self.sidebar_worktrees.server.generation.wrapping_add(1);
        let generation = self.sidebar_worktrees.server.generation;
        self.sidebar_worktrees.server.removal = Some(Removal {
            worktree,
            generation,
            assessed: frame.organization.clone(),
            assessment: None,
            modal: None,
            operation: None,
            submitted: false,
            committed: None,
            observer: None,
        });
        let handle = window.window_handle();
        self.sidebar_worktrees.server.read = Some(cx.spawn(async move |manager, cx| {
            let result = complete_assessment(&requests, worktree).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = manager.update(cx, |manager, cx| {
                    manager.present_server_removal(generation, result, window, cx)
                });
            });
        }));
    }

    fn present_server_removal(
        &mut self,
        generation: u64,
        result: Result<GitAssessmentPage, spaceterm_client::git::rpc::ClientGitFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(removal) = &mut self.sidebar_worktrees.server.removal else {
            return;
        };
        if removal.generation != generation {
            return;
        }
        let assessment = match result {
            Ok(assessment) => assessment,
            Err(_) => {
                self.sidebar_worktrees.server.removal = None;
                self.show_server_error(DesktopClientError::StaleContext, window, cx);
                return;
            }
        };
        let Some(summary) = &assessment.removal else {
            return;
        };
        let worktree = removal.worktree;
        let detail = sanitize_for_display(
            &String::from_utf8_lossy(summary.directory.bytes()),
            MAXIMUM_PATH_CHARS,
        );
        let server_name = self
            .server
            .as_ref()
            .and_then(|server| {
                server
                    .runtime
                    .read(cx)
                    .servers()
                    .into_iter()
                    .find(|info| info.server == worktree.server())
            })
            .and_then(|info| info.name.get().cloned())
            .unwrap_or_else(|| "the selected server".into());
        let message = format!(
            "On {server_name}. {}",
            removal_message(&assessment, &removal.assessed)
        );
        removal.assessment = Some(assessment.clone());
        let owner = cx.weak_entity();
        let handle = window.window_handle();
        let result = Alert::new(
            ModalId::new("server-worktree-removal"),
            "Remove Worktree?",
            "Remove Worktree?",
            message,
            vec![
                ModalAction::new(
                    RemovalAction::Remove,
                    "Remove",
                    ModalActionRole::Affirmative,
                    "server-worktree-removal-confirm",
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    RemovalAction::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "server-worktree-removal-cancel",
                ),
            ],
        )
        .detail(detail)
        .intent(AlertIntent::Critical)
        .present(window, cx, move |outcome, cx| {
            let confirmed = matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: RemovalAction::Remove,
                    ..
                }
            );
            let _ = handle.update(cx, |_, window, cx| {
                let _ = owner.update(cx, |manager, cx| {
                    if confirmed {
                        manager.confirm_server_removal(generation, worktree, window, cx);
                    } else if manager
                        .sidebar_worktrees
                        .server
                        .removal
                        .as_ref()
                        .is_some_and(|removal| removal.generation == generation)
                    {
                        manager.sidebar_worktrees.server.removal = None;
                    }
                });
            });
        });
        if let Some(removal) = &mut self.sidebar_worktrees.server.removal {
            removal.modal = result.ok();
        }
    }

    fn confirm_server_removal(
        &mut self,
        generation: u64,
        worktree: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(removal) = &self.sidebar_worktrees.server.removal else {
            return;
        };
        if removal.generation != generation || removal.submitted {
            return;
        }
        let Some(frame) = self
            .server
            .as_ref()
            .and_then(|server| server.frames.get(&worktree.server()))
        else {
            return;
        };
        if !super::super::server::shared_ready(frame) {
            return;
        }
        if frame.organization != removal.assessed {
            self.remove_server_worktree(worktree, window, cx);
            return;
        }
        let Some(assessment) = removal
            .assessment
            .as_ref()
            .map(|assessment| assessment.token.clone())
        else {
            return;
        };
        let Some(store) = &self.sidebar_worktrees.server.store else {
            return;
        };
        let Ok(requests) = store.read(cx).requests(worktree.server()) else {
            return;
        };
        if let Some(removal) = &mut self.sidebar_worktrees.server.removal {
            removal.submitted = true;
        }
        let handle = window.window_handle();
        // Submission outlives this observer and the window. The native tracker owns its write.
        cx.spawn(async move |manager, cx| {
            let result = requests
                .submit(GitCommand::RemoveConfirmed { assessment })
                .await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = manager.update(cx, |manager, cx| match result {
                    Ok(tracked) => manager.observe_server_removal(generation, tracked, window, cx),
                    Err(_) => {
                        manager.show_server_error(DesktopClientError::Reconciling, window, cx)
                    }
                });
            });
        })
        .detach();
    }

    fn observe_server_removal(
        &mut self,
        generation: u64,
        tracked: TrackedGitOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(removal) = &mut self.sidebar_worktrees.server.removal else {
            return;
        };
        if removal.generation != generation {
            return;
        }
        removal.operation = Some(tracked.id());
        let handle = window.window_handle();
        removal.observer = Some(cx.spawn(async move |manager, cx| {
            loop {
                let state = tracked
                    .state()
                    .await
                    .unwrap_or(GitOperationState::Unresolved);
                if state == GitOperationState::Pending {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(100))
                        .await;
                    continue;
                }
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = manager.update(cx, |manager, cx| {
                        let Some(removal) = &manager.sidebar_worktrees.server.removal else {
                            return;
                        };
                        if removal.generation != generation {
                            return;
                        }
                        let worktree = removal.worktree;
                        match state {
                            GitOperationState::Rejected(
                                OperationFailure::StaleAssessment
                                | OperationFailure::AssessmentExpired
                                | OperationFailure::StaleValidation,
                            ) => {
                                manager.sidebar_worktrees.server.removal = None;
                                manager.remove_server_worktree(worktree, window, cx);
                            }
                            GitOperationState::Completed(ref outcome)
                                if matches!(
                                    outcome.parts(),
                                    GitMutationOutcomeParts::Removed { .. }
                                ) =>
                            {
                                // No local Tab closure. Publication is the only membership owner.
                                if let GitMutationOutcomeParts::Removed { revision, .. } =
                                    outcome.parts()
                                    && let Some(removal) =
                                        &mut manager.sidebar_worktrees.server.removal
                                {
                                    removal.committed = Some(*revision);
                                }
                                manager.sync_server_git(window, cx);
                            }
                            GitOperationState::Rejected(_) => {
                                manager.sidebar_worktrees.server.removal = None;
                            }
                            _ => manager.show_server_error(
                                DesktopClientError::Reconciling,
                                window,
                                cx,
                            ),
                        }
                    });
                });
                return;
            }
        }));
    }
}

async fn complete_assessment(
    requests: &crate::ui::server_git::ServerGitRequests,
    worktree: WorktreeId,
) -> Result<GitAssessmentPage, spaceterm_client::git::rpc::ClientGitFailure> {
    use spaceterm_client::git::rpc::ClientGitFailure;
    let mut assessment = requests.assess_remove(worktree).await?;
    if assessment
        .removal
        .as_ref()
        .is_none_or(|summary| summary.worktree != worktree)
    {
        return Err(ClientGitFailure::InvalidResponse);
    }
    let mut cursors = Vec::new();
    while let Some(cursor) = assessment.next.take() {
        // A peer repeating a page cannot keep a confirmation read alive indefinitely.
        if cursors.contains(&cursor) || cursors.len() > MAX_GIT_TABS + MAX_GIT_PANES {
            return Err(ClientGitFailure::Capacity);
        }
        cursors.push(cursor.clone());
        let page = requests
            .assessment_page(assessment.token.clone(), Some(cursor))
            .await?;
        if page.token != assessment.token
            || page.removal != assessment.removal
            || page.creation_preview.is_some()
        {
            return Err(ClientGitFailure::InvalidResponse);
        }
        assessment.tabs.extend(page.tabs);
        assessment.panes.extend(page.panes);
        assessment.fallbacks.extend(page.fallbacks);
        assessment.next = page.next;
        assessment.requires_confirmation |= page.requires_confirmation;
        if assessment.tabs.len() > MAX_GIT_TABS
            || assessment.panes.len() > MAX_GIT_PANES
            || assessment.fallbacks.len() > MAX_GIT_TABS
        {
            return Err(ClientGitFailure::Capacity);
        }
    }
    if assessment
        .tabs
        .iter()
        .map(|record| record.tab)
        .collect::<BTreeSet<_>>()
        .len()
        != assessment.tabs.len()
        || assessment
            .panes
            .iter()
            .map(|record| record.pane)
            .collect::<BTreeSet<_>>()
            .len()
            != assessment.panes.len()
        || assessment
            .fallbacks
            .iter()
            .map(|record| record.workspace)
            .collect::<BTreeSet<_>>()
            .len()
            != assessment.fallbacks.len()
    {
        return Err(ClientGitFailure::InvalidResponse);
    }
    Ok(assessment)
}

/// Workspaces a confirmation names before it summarizes the rest as a count.
const MAXIMUM_LISTED_WORKSPACES: usize = 3;

/// Lists the first named Workspaces and counts the rest, so the confirmation stays short.
fn bounded_names(names: impl ExactSizeIterator<Item = Option<String>>) -> String {
    let total = names.len();
    let listed = names
        .take(MAXIMUM_LISTED_WORKSPACES)
        .map(|name| name.unwrap_or_else(|| "an unnamed Workspace".to_owned()))
        .collect::<Vec<_>>();
    let more = total - listed.len();
    match (listed.as_slice(), more) {
        ([], _) => plural(more as u64, "Workspace"),
        ([only], 0) => only.clone(),
        ([init @ .., last], 0) => format!("{} and {last}", init.join(", ")),
        (listed, more) => format!("{} and {more} more", listed.join(", ")),
    }
}

fn removal_message(assessment: &GitAssessmentPage, organization: &OrganizationSnapshot) -> String {
    let risk = assessment.removal.as_ref().map(|summary| summary.risk);
    let mut message = Vec::new();
    if matches!(
        risk,
        Some(
            ApprovedRemoval::DiscardChanges
                | ApprovedRemoval::DiscardChangesAndDeleteSubmoduleCheckouts
        )
    ) {
        message.push("Its uncommitted changes and untracked files will be deleted.".to_owned());
    }
    if matches!(
        risk,
        Some(
            ApprovedRemoval::DeleteSubmoduleCheckouts
                | ApprovedRemoval::DiscardChangesAndDeleteSubmoduleCheckouts
        )
    ) {
        message.push("Its submodule checkouts will be deleted.".to_owned());
    }
    let name = |workspace: WorkspaceId| {
        organization
            .state()
            .workspaces
            .iter()
            .find(|record| record.id == workspace)
            .map(|record| record.name.value.display.as_str().to_owned())
    };
    let workspaces = assessment
        .tabs
        .iter()
        .map(|tab| tab.workspace)
        .collect::<BTreeSet<_>>();
    if !assessment.tabs.is_empty() {
        let tabs = plural(assessment.tabs.len() as u64, "Tab");
        let names = bounded_names(workspaces.iter().map(|workspace| name(*workspace)));
        message.push(match workspaces.len() {
            1 => format!("Closes {tabs} in {names} for every client."),
            count => format!(
                "Closes {tabs} in {} for every client: {names}.",
                plural(count as u64, "Workspace")
            ),
        });
    }
    if !assessment.panes.is_empty() {
        message.push(match assessment.panes.len() {
            1 => "The Terminal Session in 1 Pane ends. Any command running in it stops.".to_owned(),
            panes => format!(
                "The Terminal Sessions in {panes} Panes end. Any commands running in them stop."
            ),
        });
    }
    for fallback in assessment.fallbacks.iter().take(MAXIMUM_LISTED_WORKSPACES) {
        message.push(format!(
            "{} gets a new Tab in {}.",
            name(fallback.workspace).unwrap_or_else(|| "A Workspace".to_owned()),
            sanitize_for_display(
                &String::from_utf8_lossy(fallback.directory.bytes()),
                MAXIMUM_PATH_CHARS
            )
        ));
    }
    match assessment
        .fallbacks
        .len()
        .saturating_sub(MAXIMUM_LISTED_WORKSPACES)
    {
        0 => {}
        1 => message.push("1 more Workspace gets a new Tab in another directory.".to_owned()),
        more => message.push(format!(
            "{more} more Workspaces each get a new Tab in another directory."
        )),
    }
    message.push("The branch and its commits stay in the repository.".to_owned());
    message.join(" ")
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
