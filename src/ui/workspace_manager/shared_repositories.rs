//! Repository Status and Worktrees of the shared server Workspaces this window presents.
//!
//! The owning server reads git for every directory its Workspaces show and publishes the facts
//! with its live inventory, so a Workspace nobody looks at stays as current as the Active one. This
//! window presents those facts in the views a local Workspace already has: the sidebar row's
//! branch or Pull Request, the collapsed title-bar chip, each Pane's Repository Status, and the
//! Worktree rows, with each Tab in the Worktree the server says it belongs to.
//!
//! This window reads no git for a shared Workspace. A server path is displayed and copied as
//! text; it grants no local file action. Creating and removing a shared Worktree, and opening a
//! Tab in a chosen one, stay unavailable until the server offers them.

#[cfg(feature = "developer-tools")]
pub(super) use development::SharedWorkspaceRepositories;

#[cfg(not(feature = "developer-tools"))]
mod unavailable {
    use gpui::{App, Context, Window};

    use super::super::WorkspaceManager;
    use crate::domain::{WorkspaceId, WorktreeId};
    use crate::repository_status::presentation::SidebarBadge;
    use crate::ui::workspace_sidebar::WorktreeSection;

    /// A build without shared Workspaces presents no server facts.
    impl WorkspaceManager {
        pub(in crate::ui::workspace_manager) const fn shared_sidebar_badge(
            &self,
            _: WorkspaceId,
            _: &App,
        ) -> Option<SidebarBadge> {
            None
        }

        pub(in crate::ui::workspace_manager) const fn shared_chip_line(
            &self,
            _: WorkspaceId,
            _: &App,
        ) -> Option<String> {
            None
        }

        pub(in crate::ui::workspace_manager) const fn shared_worktree_section(
            &self,
            _: WorkspaceId,
            _: &App,
        ) -> Option<WorktreeSection> {
            None
        }

        pub(in crate::ui::workspace_manager) const fn set_shared_worktrees_expanded(
            &mut self,
            _: WorkspaceId,
            _: bool,
        ) {
        }

        pub(in crate::ui::workspace_manager) const fn open_shared_worktree(
            &mut self,
            _: WorkspaceId,
            _: WorktreeId,
            _: bool,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<bool> {
            None
        }

        pub(in crate::ui::workspace_manager) const fn shared_worktree_root(
            &self,
            _: WorkspaceId,
            _: WorktreeId,
        ) -> Option<std::path::PathBuf> {
            None
        }
    }
}

#[cfg(feature = "developer-tools")]
mod development {
    use std::collections::{BTreeMap, HashMap};
    use std::path::{Path, PathBuf};

    use gpui::{App, Context, Subscription, Window};
    use spaceterm_server::hierarchy::{Id, Workspace as ServerWorkspace};
    use spaceterm_server::repository::WorktreeInventory;

    use super::super::WorkspaceManager;
    use super::super::worktrees::directory_name;
    use crate::domain::{
        RepositoryIdentity, WorkspaceId, WorktreeId, WorktreeKey, WorktreeRegistry,
    };
    use crate::repository_status::RepositoryView;
    use crate::repository_status::presentation::{
        HeadGlyph, MAXIMUM_NAME_CHARS, SidebarBadge, chip_tooltip_line, sanitize_for_display,
    };
    use crate::ui::repository_status_store::{
        InstalledRepositoryStatus, ServerPullRequestsChanged,
    };
    use crate::ui::shared_workspaces::{
        LocalPresentation, SharedRepositories, pane_directory, workspace_directory,
    };
    use crate::ui::workspace_sidebar::{
        WorktreeGroup, WorktreeRemoval, WorktreeRowViewModel, WorktreeSection,
    };
    use crate::worktrees::listing::{WorktreeHead, WorktreeRecord};

    /// What this window keeps beside the server's facts: a stable handle for each shared
    /// Worktree, and whether a person collapsed its Workspace row.
    #[derive(Default)]
    pub(in crate::ui::workspace_manager) struct SharedWorkspaceRepositories {
        worktrees: BTreeMap<WorkspaceId, SharedWorktrees>,
        _pull_requests: Option<Subscription>,
    }

    #[derive(Default)]
    struct SharedWorktrees {
        registry: WorktreeRegistry,
        /// The last listed record of each Worktree with a handle. A Worktree git no longer
        /// lists keeps its label while the server keeps Tabs in it.
        records: BTreeMap<WorktreeId, WorktreeRecord>,
        /// The person's choice, which replaces the default for the rest of the session.
        expanded: Option<bool>,
    }

    impl WorkspaceManager {
        /// Follows the Pull Requests this client's GitHub CLI finds for server branches, and
        /// presents the server's facts again whenever one changes.
        pub(in crate::ui::workspace_manager) fn follow_shared_pull_requests(
            &mut self,
            cx: &mut Context<Self>,
        ) {
            let Some(store) = InstalledRepositoryStatus::store(cx) else {
                return;
            };
            self.shared_repositories._pull_requests = Some(cx.subscribe(
                &store,
                |manager, _, _: &ServerPullRequestsChanged, cx| {
                    manager.present_shared_repositories(cx);
                },
            ));
        }

        /// Presents the server's latest Git facts in every shared Workspace, active or not: each
        /// Tab's Worktree, each Pane's Repository Status, and the sidebar.
        pub(in crate::ui::workspace_manager) fn present_shared_repositories(
            &mut self,
            cx: &mut Context<Self>,
        ) {
            let Some(shared) = self.shared.as_ref() else {
                return;
            };
            let keys = shared.repositories().pull_request_keys();
            if let Some(store) = InstalledRepositoryStatus::store(cx) {
                store.update(cx, |store, cx| store.follow_server_pull_requests(keys, cx));
            }
            let workspace_ids: Vec<WorkspaceId> = self
                .workspaces
                .iter()
                .map(|workspace| workspace.id())
                .filter(|&workspace_id| self.is_shared_workspace(workspace_id))
                .collect();
            self.shared_repositories
                .worktrees
                .retain(|workspace_id, _| workspace_ids.contains(workspace_id));
            for &workspace_id in &workspace_ids {
                self.reconcile_shared_worktrees(workspace_id, cx);
            }
            let views = self.shared_pane_views(&workspace_ids, cx);
            for workspace_id in workspace_ids {
                let Some(manager) = self
                    .workspaces
                    .workspace(workspace_id)
                    .map(|workspace| workspace.payload().clone())
                else {
                    continue;
                };
                manager.update(cx, |manager, cx| {
                    manager.present_shared_repositories(
                        &|pane| views.get(&pane).cloned().unwrap_or_default(),
                        cx,
                    );
                });
            }
            cx.notify();
        }

        /// What one server directory presents, with this client's Settings and Pull Requests.
        fn shared_repository_view(&self, directory: &Path, cx: &App) -> RepositoryView {
            let Some(shared) = self.shared.as_ref() else {
                return RepositoryView::Hidden;
            };
            let store = InstalledRepositoryStatus::store(cx);
            let store = store.as_ref().map(|store| store.read(cx));
            let lookup = |key: &_| store.and_then(|store| store.server_pull_request(key));
            shared.repositories().view(
                directory,
                &LocalPresentation {
                    preferences: store.map(|store| store.preferences()).unwrap_or_default(),
                    connected: shared.connected(),
                    pull_request: &lookup,
                },
            )
        }

        /// Each shared Pane's view, by its server identity.
        fn shared_pane_views(
            &self,
            workspace_ids: &[WorkspaceId],
            cx: &App,
        ) -> HashMap<Id, RepositoryView> {
            let Some(shared) = self.shared.as_ref() else {
                return HashMap::new();
            };
            workspace_ids
                .iter()
                .filter_map(|&workspace_id| shared.server_workspace(workspace_id))
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| &tab.panes)
                .map(|pane| {
                    (
                        pane.id,
                        self.shared_repository_view(pane_directory(pane), cx),
                    )
                })
                .collect()
        }

        /// The server Workspace one local Workspace presents, and the directory its row reads.
        fn shared_row_source(
            &self,
            workspace_id: WorkspaceId,
        ) -> Option<(&ServerWorkspace, &Path)> {
            let workspace = self.shared.as_ref()?.server_workspace(workspace_id)?;
            Some((workspace, workspace_directory(workspace)?))
        }

        /// What a shared Workspace row's second line shows after its directory.
        pub(in crate::ui::workspace_manager) fn shared_sidebar_badge(
            &self,
            workspace_id: WorkspaceId,
            cx: &App,
        ) -> Option<SidebarBadge> {
            let (_, directory) = self.shared_row_source(workspace_id)?;
            SidebarBadge::from_view(&self.shared_repository_view(directory, cx))
        }

        /// The collapsed title-bar chip's tooltip line for a shared Workspace.
        pub(in crate::ui::workspace_manager) fn shared_chip_line(
            &self,
            workspace_id: WorkspaceId,
            cx: &App,
        ) -> Option<String> {
            let (_, directory) = self.shared_row_source(workspace_id)?;
            chip_tooltip_line(&self.shared_repository_view(directory, cx))
        }

        /// The Worktrees of the repository a shared Workspace's row reads, as git on the server
        /// lists them.
        fn shared_inventory(&self, workspace_id: WorkspaceId) -> Option<&WorktreeInventory> {
            let (_, directory) = self.shared_row_source(workspace_id)?;
            self.shared.as_ref()?.repositories().worktrees(directory)
        }

        /// Names every Worktree the server lists or keeps Tabs in, and tags each Tab with the
        /// Worktree the server says it belongs to. Nothing here moves a Tab on its own: the
        /// server reconciles membership, including a Root Tab that follows its Root Pane.
        fn reconcile_shared_worktrees(
            &mut self,
            workspace_id: WorkspaceId,
            cx: &mut Context<Self>,
        ) {
            let Some(shared) = self.shared.as_ref() else {
                return;
            };
            let Some(server) = shared.server_workspace(workspace_id) else {
                return;
            };
            let Some(manager) = self
                .workspaces
                .workspace(workspace_id)
                .map(|workspace| workspace.payload().clone())
            else {
                return;
            };
            let repositories = shared.repositories();
            let inventory =
                workspace_directory(server).and_then(|directory| repositories.worktrees(directory));
            let state = self
                .shared_repositories
                .worktrees
                .entry(workspace_id)
                .or_default();
            let mut listed = Vec::new();
            if let Some(inventory) = inventory {
                let repository = RepositoryIdentity::new(inventory.main_root.clone());
                // A bare repository is listed first but has no files, so it holds no Tabs.
                for record in inventory
                    .worktrees
                    .iter()
                    .filter(|record| record.head != WorktreeHead::Bare)
                {
                    let key = WorktreeKey::new(repository.clone(), record.root.clone());
                    let Ok(id) = state.registry.id_for(&key) else {
                        return;
                    };
                    state.records.insert(id, record.clone());
                    listed.push(id);
                }
            }
            let mut scopes: HashMap<Id, (WorktreeId, PathBuf)> = HashMap::new();
            for tab in &server.tabs {
                let Some(root) = &tab.worktree else {
                    continue;
                };
                let key = membership_key(root, inventory, state, repositories);
                let Ok(id) = state.registry.id_for(&key) else {
                    continue;
                };
                state
                    .records
                    .entry(id)
                    .or_insert_with(|| unlisted_record(root, repositories));
                scopes.insert(tab.id, (id, root.clone()));
            }
            manager.update(cx, |manager, cx| {
                manager.apply_shared_worktrees(&|tab| scopes.get(&tab).cloned(), cx);
            });
            let counts = manager.read(cx).worktree_tab_counts();
            let Some(state) = self.shared_repositories.worktrees.get_mut(&workspace_id) else {
                return;
            };
            let unused: Vec<WorktreeId> = state
                .records
                .keys()
                .copied()
                .filter(|id| !counts.contains_key(id) && !listed.contains(id))
                .collect();
            for id in unused {
                state.registry.forget(id);
                state.records.remove(&id);
            }
        }

        /// The Worktrees a shared Workspace row discloses, or `None` outside a repository.
        pub(in crate::ui::workspace_manager) fn shared_worktree_section(
            &self,
            workspace_id: WorkspaceId,
            cx: &App,
        ) -> Option<WorktreeSection> {
            let (server, _) = self.shared_row_source(workspace_id)?;
            let state = self.shared_repositories.worktrees.get(&workspace_id)?;
            let manager = self.workspaces.workspace(workspace_id)?.payload().read(cx);
            let counts = manager.worktree_tab_counts();
            let active = manager.active_worktree();
            let inventory = self.shared_inventory(workspace_id);
            let current =
                inventory.map(|inventory| RepositoryIdentity::new(inventory.main_root.clone()));
            // `listed` says whether the repository the Workspace reads lists the Worktree now.
            let row = |id: WorktreeId, record: &WorktreeRecord, missing: bool, listed: bool| {
                let (label, detached) = match &record.head {
                    WorktreeHead::Branch(branch) => (branch.as_str(), false),
                    WorktreeHead::Detached(commit) => (commit.as_str(), true),
                    WorktreeHead::Bare => ("", false),
                };
                let label = sanitize_for_display(label, MAXIMUM_NAME_CHARS);
                // The row describes the Pane last used in the Worktree, else its root.
                let directory = manager
                    .shared_worktree_pane(id, cx)
                    .and_then(|pane| {
                        server
                            .tabs
                            .iter()
                            .flat_map(|tab| &tab.panes)
                            .find(|candidate| candidate.id == pane)
                    })
                    .map_or(record.root.as_path(), pane_directory);
                // The listing names the branch until the server reads the Worktree's status.
                let repository = SidebarBadge::from_view(
                    &self.shared_repository_view(directory, cx),
                )
                .or_else(|| {
                    (!label.is_empty()).then(|| SidebarBadge::Branch {
                        glyph: HeadGlyph::from_detached(detached),
                        text: label.clone(),
                    })
                });
                let main = state
                    .registry
                    .key(id)
                    .is_some_and(|key| key.root() == key.repository().main_root());
                let removal = if !listed {
                    WorktreeRemoval::Unlisted
                } else if main {
                    WorktreeRemoval::Main
                } else {
                    WorktreeRemoval::Server
                };
                WorktreeRowViewModel {
                    worktree_id: id,
                    name: sanitize_for_display(&directory_name(&record.root), MAXIMUM_NAME_CHARS)
                        .into(),
                    label: label.into(),
                    detached,
                    path: record.root.display().to_string().into(),
                    directory: super::super::compact_home_path(
                        directory,
                        &self.local_home_directory_path,
                    )
                    .into(),
                    directory_tooltip: directory.display().to_string().into(),
                    repository,
                    main,
                    has_tabs: counts.contains_key(&id),
                    active: active == Some(id),
                    locked: record.locked,
                    missing: missing || record.missing,
                    removal,
                    shared: true,
                }
            };
            let mut groups = Vec::new();
            if let (Some(inventory), Some(current)) = (inventory, current.as_ref()) {
                let mut rows: Vec<WorktreeRowViewModel> = inventory
                    .worktrees
                    .iter()
                    .filter(|record| record.head != WorktreeHead::Bare)
                    .filter_map(|record| {
                        let key = WorktreeKey::new(current.clone(), record.root.clone());
                        Some(row(state.registry.id(&key)?, record, false, true))
                    })
                    .collect();
                // A Worktree removed outside SpaceTerm keeps its row while the server keeps Tabs
                // in it. Nothing recreates its directory.
                let removed: Vec<WorktreeRowViewModel> = counts
                    .keys()
                    .filter_map(|&id| {
                        let key = state.registry.key(id)?;
                        let listed = rows.iter().any(|row| row.worktree_id == id);
                        if key.repository() != current || listed {
                            return None;
                        }
                        Some(row(id, state.records.get(&id)?, true, false))
                    })
                    .collect();
                rows.extend(removed);
                groups.push(WorktreeGroup {
                    former_repository: None,
                    rows,
                });
            }
            let mut former: BTreeMap<&RepositoryIdentity, Vec<WorktreeRowViewModel>> =
                BTreeMap::new();
            for &id in counts.keys() {
                let Some(key) = state.registry.key(id) else {
                    continue;
                };
                if Some(key.repository()) == current.as_ref() {
                    continue;
                }
                if let Some(record) = state.records.get(&id) {
                    former
                        .entry(key.repository())
                        .or_default()
                        .push(row(id, record, false, false));
                }
            }
            groups.extend(former.into_iter().map(|(repository, rows)| WorktreeGroup {
                former_repository: Some(directory_name(repository.main_root())),
                rows,
            }));
            // A repository's Main Worktree alone leaves the row as it is outside a repository.
            if groups.iter().map(|group| group.rows.len()).sum::<usize>() < 2
                && groups.iter().all(|group| group.former_repository.is_none())
            {
                return None;
            }
            Some(WorktreeSection {
                expanded: state.expanded.unwrap_or(true),
                repository: inventory.map(|inventory| {
                    super::super::compact_home_path(
                        &inventory.main_root,
                        &self.local_home_directory_path,
                    )
                    .into()
                }),
                groups,
            })
        }

        pub(in crate::ui::workspace_manager) fn set_shared_worktrees_expanded(
            &mut self,
            workspace_id: WorkspaceId,
            expanded: bool,
        ) {
            if let Some(worktrees) = self.shared_repositories.worktrees.get_mut(&workspace_id) {
                worktrees.expanded = Some(expanded);
            }
        }

        /// Shows a shared Worktree's Tabs. It reports `None` for a Workspace this window owns.
        ///
        /// A shared Worktree without Tabs opens nothing: a Tab in a chosen Worktree is a change
        /// the server does not offer yet, and a missing one has no directory to start in.
        pub(in crate::ui::workspace_manager) fn open_shared_worktree(
            &mut self,
            workspace_id: WorkspaceId,
            worktree_id: WorktreeId,
            focus: bool,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Option<bool> {
            if !self.is_shared_workspace(workspace_id) {
                return None;
            }
            let manager = self
                .workspaces
                .workspace(workspace_id)
                .map(|workspace| workspace.payload().clone())?;
            Some(manager.update(cx, |manager, cx| {
                manager.show_shared_worktree(worktree_id, focus, window, cx)
            }))
        }

        /// A shared Worktree's root, as the server reported it.
        pub(in crate::ui::workspace_manager) fn shared_worktree_root(
            &self,
            workspace_id: WorkspaceId,
            worktree_id: WorktreeId,
        ) -> Option<PathBuf> {
            self.shared_repositories
                .worktrees
                .get(&workspace_id)?
                .registry
                .key(worktree_id)
                .map(|key| key.root().to_path_buf())
        }
    }

    /// The Worktree a Tab's server membership names. A root the repository lists now belongs to
    /// it; one this window named earlier keeps its repository, so a Tab left in a removed
    /// Worktree stays with it; one another repository the server read lists joins that one.
    fn membership_key(
        root: &Path,
        inventory: Option<&WorktreeInventory>,
        state: &SharedWorktrees,
        repositories: &SharedRepositories,
    ) -> WorktreeKey {
        if let Some(inventory) = inventory
            .filter(|inventory| inventory.worktrees.iter().any(|record| record.root == root))
        {
            return WorktreeKey::new(
                RepositoryIdentity::new(inventory.main_root.clone()),
                root.to_path_buf(),
            );
        }
        if let Some(key) = state
            .records
            .keys()
            .filter_map(|&id| state.registry.key(id))
            .find(|key| key.root() == root)
        {
            return key.clone();
        }
        let main_root = repositories.worktrees(root).or(inventory).map_or_else(
            || root.to_path_buf(),
            |inventory| inventory.main_root.clone(),
        );
        WorktreeKey::new(RepositoryIdentity::new(main_root), root.to_path_buf())
    }

    /// The record a Worktree this window never saw listed presents: the server's listing of it
    /// when another repository lists it, else a row with no branch whose directory is missing.
    fn unlisted_record(root: &Path, repositories: &SharedRepositories) -> WorktreeRecord {
        repositories
            .worktrees(root)
            .and_then(|inventory| {
                inventory
                    .worktrees
                    .iter()
                    .find(|record| record.root == root)
            })
            .cloned()
            .unwrap_or_else(|| WorktreeRecord {
                root: root.to_path_buf(),
                head: WorktreeHead::Branch(String::new()),
                locked: false,
                missing: true,
            })
    }
}
