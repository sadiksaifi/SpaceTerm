//! Each Workspace sidebar row's Repository Status Interest. A pinned row reads its Pinned
//! Directory; an unpinned row reads its Root Pane's Repository Source Directory, else home. Focus
//! changes never change a row's directory.

use std::collections::BTreeMap;

use gpui::{App, Context, Entity, Subscription};

use super::WorkspaceManager;
use crate::domain::{
    DirectoryAvailability, PinnedDirectory, RemoteConnectionPhase, RemoteConnectionState,
    WorkspaceEntry, WorkspaceId, WorkspaceLocation,
};
use crate::repository_status::presentation::{SidebarBadge, chip_tooltip_line};
use crate::repository_status::scheduler::{Interest, InterestId, SourceDirectory};
use crate::repository_status::{RemoteMachineKey, RepositoryMachine, RepositoryView};
use crate::ui::TabManager;
use crate::ui::repository_status_store::{
    InstalledRepositoryStatus, RepositoryStatusStore, RepositoryViewsChanged,
};

#[derive(Default)]
pub(super) struct SidebarRepositories {
    rows: BTreeMap<WorkspaceId, RowInterest>,
    views: BTreeMap<WorkspaceId, RepositoryView>,
}

struct RowInterest {
    store: Entity<RepositoryStatusStore>,
    id: InterestId,
    reported: RowFacts,
    _changes: Subscription,
}

/// Everything a sidebar row tells the store. Rows are always visible.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RowFacts {
    machine: RepositoryMachine,
    directory: SourceDirectory,
    available: bool,
}

impl WorkspaceManager {
    /// Registers, updates, and releases row Interests to match the current Workspaces.
    pub(super) fn sync_sidebar_repositories(&mut self, cx: &mut Context<Self>) {
        let Some(store) = InstalledRepositoryStatus::store(cx) else {
            return;
        };
        let wanted: BTreeMap<WorkspaceId, RowFacts> = self
            .workspaces
            .iter()
            .filter_map(|workspace| Some((workspace.id(), self.row_facts(workspace, cx)?)))
            .collect();
        let gone: Vec<WorkspaceId> = self
            .sidebar_repositories
            .rows
            .iter()
            .filter(|(workspace_id, row)| {
                wanted
                    .get(workspace_id)
                    .is_none_or(|facts| facts.machine != row.reported.machine)
            })
            .map(|(workspace_id, _)| *workspace_id)
            .collect();
        for workspace_id in gone {
            if let Some(row) = self.sidebar_repositories.rows.remove(&workspace_id) {
                row.store
                    .update(cx, |store, cx| store.unregister(row.id, cx));
                self.sidebar_repositories.views.remove(&workspace_id);
                cx.notify();
            }
        }
        for (workspace_id, facts) in wanted {
            let Some(row) = self.sidebar_repositories.rows.get_mut(&workspace_id) else {
                self.register_row(workspace_id, facts, &store, cx);
                continue;
            };
            let previous = std::mem::replace(&mut row.reported, facts.clone());
            if previous == facts {
                continue;
            }
            let id = row.id;
            row.store.update(cx, |store, cx| {
                if previous.available != facts.available {
                    store.set_available(id, facts.available, cx);
                }
                if previous.directory != facts.directory {
                    store.set_source(id, facts.directory.clone(), cx);
                }
            });
        }
    }

    pub(super) fn release_sidebar_repositories(&mut self, cx: &mut App) {
        self.sidebar_repositories.views.clear();
        for (_, row) in std::mem::take(&mut self.sidebar_repositories.rows) {
            row.store
                .update(cx, |store, cx| store.unregister(row.id, cx));
        }
    }

    /// What a row's second line shows before its directory.
    pub(super) fn sidebar_badge(
        &self,
        workspace_id: WorkspaceId,
        cx: &App,
    ) -> Option<SidebarBadge> {
        SidebarBadge::from_view(&self.row_view(workspace_id, cx)?)
    }

    /// The collapsed title-bar chip's tooltip line for a Workspace.
    pub(super) fn chip_repository_line(
        &self,
        workspace_id: WorkspaceId,
        cx: &App,
    ) -> Option<String> {
        chip_tooltip_line(&self.row_view(workspace_id, cx)?)
    }

    /// A row's view, or the Developer Workbench fixture in its place.
    fn row_view(&self, workspace_id: WorkspaceId, cx: &App) -> Option<RepositoryView> {
        #[cfg(feature = "developer-tools")]
        if let Some(view) = crate::ui::developer_workbench::repository_view_fixture(cx) {
            return Some(view);
        }
        let _ = cx;
        self.sidebar_repositories.views.get(&workspace_id).cloned()
    }

    #[cfg(test)]
    pub(super) fn present_sidebar_repository(
        &mut self,
        workspace_id: WorkspaceId,
        view: RepositoryView,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_repositories.views.insert(workspace_id, view);
        cx.notify();
    }

    fn register_row(
        &mut self,
        workspace_id: WorkspaceId,
        facts: RowFacts,
        store: &Entity<RepositoryStatusStore>,
        cx: &mut Context<Self>,
    ) {
        let id = store.update(cx, |store, cx| {
            store.register(
                Interest {
                    machine: facts.machine.clone(),
                    directory: facts.directory.clone(),
                    visible: true,
                    available: facts.available,
                    finished_commands: 0,
                },
                cx,
            )
        });
        let changes = cx.subscribe(
            store,
            move |manager, store, event: &RepositoryViewsChanged, cx| {
                let Some(row) = manager.sidebar_repositories.rows.get(&workspace_id) else {
                    return;
                };
                if !event.contains(row.id) {
                    return;
                }
                let view = store.read(cx).view(row.id);
                let views = &mut manager.sidebar_repositories.views;
                if views.get(&workspace_id) != Some(&view) {
                    views.insert(workspace_id, view);
                    cx.notify();
                }
            },
        );
        let view = store.read(cx).view(id);
        self.sidebar_repositories.views.insert(workspace_id, view);
        self.sidebar_repositories.rows.insert(
            workspace_id,
            RowInterest {
                store: store.clone(),
                id,
                reported: facts,
                _changes: changes,
            },
        );
        cx.notify();
    }

    fn row_facts(
        &self,
        workspace: &WorkspaceEntry<Entity<TabManager>>,
        cx: &App,
    ) -> Option<RowFacts> {
        let (machine, directory) = self.row_source(workspace, cx)?;
        let connected = workspace
            .remote_connection_state()
            .map(RemoteConnectionState::phase)
            .is_none_or(|phase| phase == RemoteConnectionPhase::Connected);
        Some(RowFacts {
            machine,
            directory,
            available: connected
                && matches!(workspace.availability(), DirectoryAvailability::Available),
        })
    }

    /// The directory a row reads and its machine: the Pinned Directory, else the Root Pane's
    /// Repository Source Directory, else home.
    pub(super) fn row_source(
        &self,
        workspace: &WorkspaceEntry<Entity<TabManager>>,
        cx: &App,
    ) -> Option<(RepositoryMachine, SourceDirectory)> {
        let location_machine = match workspace.location() {
            WorkspaceLocation::Local => RepositoryMachine::Local,
            WorkspaceLocation::Remote { key, .. } => {
                RepositoryMachine::Remote(RemoteMachineKey::new(key.destination().as_str()))
            }
        };
        let (machine, directory) = match workspace.pinned_directory() {
            Some(PinnedDirectory::Local(directory)) => (
                RepositoryMachine::Local,
                SourceDirectory::Local(directory.path().to_path_buf()),
            ),
            Some(PinnedDirectory::Remote { directory, .. }) => (
                location_machine,
                SourceDirectory::Remote(directory.as_str().into()),
            ),
            None => workspace
                .payload()
                .read(cx)
                .repository_source(cx)
                .or_else(|| {
                    let home = match workspace.location() {
                        WorkspaceLocation::Local => {
                            SourceDirectory::Local(self.local_home_directory_path.clone())
                        }
                        WorkspaceLocation::Remote { .. } => SourceDirectory::Remote(
                            workspace.remote_display_directory()?.as_str().into(),
                        ),
                    };
                    Some((location_machine, home))
                })?,
        };
        Some((machine, directory))
    }
}
