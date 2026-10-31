//! Repository Status and Worktrees of the server Workspaces this window presents.
//!
//! The owning server reads git for every directory its Workspaces show and publishes the facts
//! with its live inventory, so a Workspace nobody looks at stays as current as the Active one. This
//! window presents those facts in the views a local Workspace already has: the sidebar row's
//! branch or Pull Request, the collapsed title-bar chip, each Pane's Repository Status, and the
//! Worktree rows, with each Tab in the Worktree the server says it belongs to.
//!
//! This window reads no git for a Workspace. A server path is displayed and copied as
//! text; it grants no local file action. Explicit Worktree writes use the owning server.

pub(super) use server::WorkspaceRepositories;

mod server {
    use std::collections::{BTreeMap, HashMap};
    use std::path::{Path, PathBuf};

    use gpui::{App, Context, Subscription, Window};
    use spaceterm_server::hierarchy::{Id, Workspace as ServerWorkspace};
    use spaceterm_server::repository::{PinCheckState, WorktreeInventory};

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
    use crate::ui::workspace_sidebar::{
        WorktreeGroup, WorktreeRemoval, WorktreeRowViewModel, WorktreeSection,
    };
    use crate::ui::workspaces::{
        LocalPresentation, ServerRepositories, pane_directory, workspace_directory,
    };
    use crate::worktrees::listing::{WorktreeHead, WorktreeRecord};

    /// What this window keeps beside the server's facts: a stable handle for each
    /// Worktree, and whether a person collapsed its Workspace row.
    #[derive(Default)]
    pub(in crate::ui::workspace_manager) struct WorkspaceRepositories {
        worktrees: BTreeMap<WorkspaceId, PresentedWorktrees>,
        _pull_requests: Option<Subscription>,
    }

    #[derive(Default)]
    struct PresentedWorktrees {
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
        pub(in crate::ui::workspace_manager) fn follow_pull_requests(
            &mut self,
            cx: &mut Context<Self>,
        ) {
            let Some(store) = InstalledRepositoryStatus::store(cx) else {
                return;
            };
            self.workspace_repositories._pull_requests = Some(cx.subscribe(
                &store,
                |manager, _, _: &ServerPullRequestsChanged, cx| {
                    manager.present_repositories(cx);
                },
            ));
        }

        /// Presents the server's latest Git facts in every Workspace, active or not: each
        /// Tab's Worktree, each Pane's Repository Status, and the sidebar.
        pub(in crate::ui::workspace_manager) fn present_repositories(
            &mut self,
            cx: &mut Context<Self>,
        ) {
            let keys = self
                .binding
                .iter()
                .flat_map(|(_, binding)| binding.repositories().pull_request_keys())
                .collect::<Vec<_>>();
            if let Some(store) = InstalledRepositoryStatus::store(cx) {
                store.update(cx, |store, cx| store.follow_server_pull_requests(keys, cx));
            }
            let workspace_ids: Vec<WorkspaceId> = self
                .workspaces
                .iter()
                .map(|workspace| workspace.id())
                .filter(|&workspace_id| self.has_workspace(workspace_id))
                .collect();
            self.workspace_repositories
                .worktrees
                .retain(|workspace_id, _| workspace_ids.contains(workspace_id));
            for &workspace_id in &workspace_ids {
                self.reconcile_worktrees(workspace_id, cx);
            }
            let views = self.pane_views(&workspace_ids, cx);
            for workspace_id in workspace_ids {
                let Some(manager) = self
                    .workspaces
                    .workspace(workspace_id)
                    .map(|workspace| workspace.payload().clone())
                else {
                    continue;
                };
                manager.update(cx, |manager, cx| {
                    manager.present_repositories(
                        &|pane| views.get(&pane).cloned().unwrap_or_default(),
                        cx,
                    );
                });
            }
            cx.notify();
        }

        /// What one server directory presents, with this client's Settings and Pull Requests.
        fn repository_view(
            &self,
            workspace_id: WorkspaceId,
            directory: &Path,
            cx: &App,
        ) -> RepositoryView {
            #[cfg(feature = "developer-tools")]
            if let Some(view) = crate::ui::developer_workbench::repository_view_fixture(cx) {
                return view;
            }
            let Some(binding) = self.binding.owning(workspace_id) else {
                return RepositoryView::Hidden;
            };
            let store = InstalledRepositoryStatus::store(cx);
            let store = store.as_ref().map(|store| store.read(cx));
            let lookup = |key: &_| store.and_then(|store| store.server_pull_request(key));
            binding.repositories().view(
                directory,
                &LocalPresentation {
                    preferences: store.map(|store| store.preferences()).unwrap_or_default(),
                    connected: binding.connected(),
                    pull_request: &lookup,
                },
            )
        }

        /// Each Pane's view, by its server identity.
        fn pane_views(
            &self,
            workspace_ids: &[WorkspaceId],
            cx: &App,
        ) -> HashMap<Id, RepositoryView> {
            workspace_ids
                .iter()
                .filter_map(|&workspace_id| {
                    let workspace = self
                        .binding
                        .owning(workspace_id)?
                        .server_workspace(workspace_id)?;
                    Some((workspace_id, workspace))
                })
                .flat_map(|(workspace_id, workspace)| {
                    workspace
                        .tabs
                        .iter()
                        .flat_map(|tab| &tab.panes)
                        .map(move |pane| (workspace_id, pane))
                })
                .map(|(workspace_id, pane)| {
                    (
                        pane.id,
                        self.repository_view(workspace_id, pane_directory(pane), cx),
                    )
                })
                .collect()
        }

        /// The server Workspace one local Workspace presents, and the directory its row reads.
        fn workspace_row_source(
            &self,
            workspace_id: WorkspaceId,
        ) -> Option<(&ServerWorkspace, &Path)> {
            let workspace = self
                .binding
                .owning(workspace_id)?
                .server_workspace(workspace_id)?;
            Some((workspace, workspace_directory(workspace)?))
        }

        /// What a Workspace row's second line shows after its directory.
        pub(in crate::ui::workspace_manager) fn sidebar_badge(
            &self,
            workspace_id: WorkspaceId,
            cx: &App,
        ) -> Option<SidebarBadge> {
            let (_, directory) = self.workspace_row_source(workspace_id)?;
            SidebarBadge::from_view(&self.repository_view(workspace_id, directory, cx))
        }

        /// The collapsed title-bar chip's tooltip line for a Workspace.
        pub(in crate::ui::workspace_manager) fn chip_line(
            &self,
            workspace_id: WorkspaceId,
            cx: &App,
        ) -> Option<String> {
            let (_, directory) = self.workspace_row_source(workspace_id)?;
            chip_tooltip_line(&self.repository_view(workspace_id, directory, cx))
        }

        /// The Worktrees of the repository a Workspace's row reads, as git on the server
        /// lists them.
        pub(in crate::ui::workspace_manager) fn workspace_inventory(
            &self,
            workspace_id: WorkspaceId,
        ) -> Option<&WorktreeInventory> {
            let binding = self.binding.owning(workspace_id)?;
            binding
                .repositories()
                .workspace_worktrees(binding.server_workspace(workspace_id)?)
        }

        /// Names every Worktree the server lists or keeps Tabs in, and tags each Tab with the
        /// Worktree the server says it belongs to. Nothing here moves a Tab on its own: the
        /// server reconciles membership, including a Root Tab that follows its Root Pane.
        fn reconcile_worktrees(&mut self, workspace_id: WorkspaceId, cx: &mut Context<Self>) {
            let Some(binding) = self.binding.owning(workspace_id) else {
                return;
            };
            let Some(server) = binding.server_workspace(workspace_id) else {
                return;
            };
            let Some(manager) = self
                .workspaces
                .workspace(workspace_id)
                .map(|workspace| workspace.payload().clone())
            else {
                return;
            };
            let repositories = binding.repositories();
            let inventory = repositories.workspace_worktrees(server);
            let state = self
                .workspace_repositories
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
                let saved = tab.worktree_repository.as_deref();
                let key = membership_key(root, saved, inventory, state, repositories);
                let Ok(id) = state.registry.id_for(&key) else {
                    continue;
                };
                state
                    .records
                    .entry(id)
                    .or_insert_with(|| unlisted_record(root, saved, repositories));
                scopes.insert(tab.id, (id, root.clone()));
            }
            manager.update(cx, |manager, cx| {
                manager.apply_worktrees(
                    &|tab| {
                        server
                            .tabs
                            .iter()
                            .any(|published| published.id == tab)
                            .then(|| scopes.get(&tab).map(|(id, _)| *id))
                    },
                    cx,
                );
            });
            let counts = manager.read(cx).worktree_tab_counts();
            let Some(state) = self.workspace_repositories.worktrees.get_mut(&workspace_id) else {
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

        /// The Worktrees a Workspace row discloses, or `None` outside a repository.
        pub(in crate::ui::workspace_manager) fn server_worktree_section(
            &self,
            workspace_id: WorkspaceId,
            cx: &App,
        ) -> Option<WorktreeSection> {
            let (server, _) = self.workspace_row_source(workspace_id)?;
            let state = self.workspace_repositories.worktrees.get(&workspace_id)?;
            let manager = self.workspaces.workspace(workspace_id)?.payload().read(cx);
            let counts = manager.worktree_tab_counts();
            let active = manager.active_worktree();
            let inventory = self.workspace_inventory(workspace_id);
            // The owning server runs Git for its Workspace, on its own computer.
            let worktree_writes = self.owner_writes_worktrees(workspace_id, cx);
            let repositories = self.binding.owning(workspace_id)?.repositories();
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
                // The row describes the Pane last used in the Worktree, else its root. A Pane
                // that left keeps its Tab here, but its directory and Repository Status are its
                // own, so the row describes the root and names the Pane's directory apart.
                let pane = manager
                    .worktree_pane(id, cx)
                    .and_then(|pane| {
                        server
                            .tabs
                            .iter()
                            .flat_map(|tab| &tab.panes)
                            .find(|candidate| candidate.id == pane)
                    })
                    .map(pane_directory);
                let outside_pane = pane
                    .filter(|directory| !within_worktree(directory, &record.root, repositories));
                let directory = pane
                    .filter(|_| outside_pane.is_none())
                    .unwrap_or(record.root.as_path());
                // The listing names the branch until the server reads the Worktree's status.
                let repository =
                    SidebarBadge::from_view(&self.repository_view(workspace_id, directory, cx))
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
                // Only the owning server resolves a pin's physical location. Its list of
                // pinned Worktrees confirms a pin once a check is Ready, while a pin lexically
                // inside the root is confirmed without one.
                let owning = self.binding.owning(workspace_id);
                let pin_check = owning.map(|binding| binding.repositories().pin_check_state());
                let removal = if !listed {
                    WorktreeRemoval::Unlisted
                } else if main {
                    WorktreeRemoval::Main
                } else if record.locked {
                    WorktreeRemoval::Locked
                } else if (matches!(pin_check, Some(PinCheckState::Ready))
                    && owning.is_some_and(|binding| {
                        binding.repositories().holds_pinned_directory(&record.root)
                    }))
                    || owning.and_then(|s| s.latest_inventory()).is_some_and(|i| {
                        i.workspaces
                            .iter()
                            .filter_map(|w| w.pinned_directory.as_deref())
                            .any(|pin| pin.starts_with(&record.root))
                    })
                {
                    WorktreeRemoval::HoldsPinnedDirectory
                } else if !worktree_writes || !owning.is_some_and(|s| s.editability().available()) {
                    WorktreeRemoval::NeedsServer
                } else if matches!(pin_check, Some(PinCheckState::Checking)) {
                    WorktreeRemoval::CheckingPinnedDirectories
                } else if matches!(pin_check, Some(PinCheckState::Unavailable)) {
                    WorktreeRemoval::PinnedDirectoriesUnavailable
                } else {
                    WorktreeRemoval::Allowed
                };
                WorktreeRowViewModel {
                    worktree_id: id,
                    name: sanitize_for_display(&directory_name(&record.root), MAXIMUM_NAME_CHARS)
                        .into(),
                    label: label.into(),
                    detached,
                    path: record.root.display().to_string().into(),
                    directory: if self
                        .binding
                        .owning(workspace_id)
                        .is_some_and(|s| s.removing_worktree(&record.root))
                    {
                        "Removing…".into()
                    } else {
                        super::super::compact_home_path(directory, &self.local_home_directory_path)
                            .into()
                    },
                    directory_tooltip: directory.display().to_string().into(),
                    outside_pane_directory: outside_pane
                        .map(|directory| directory.display().to_string().into()),
                    repository,
                    main,
                    has_tabs: counts.contains_key(&id),
                    opens_first_tab: worktree_writes
                        && self
                            .binding
                            .owning(workspace_id)
                            .is_some_and(|s| s.editability().available()),
                    active: active == Some(id),
                    locked: record.locked,
                    missing: missing || record.missing,
                    removal,
                    closes_tabs: self
                        .binding
                        .owning(workspace_id)
                        .is_some_and(|binding| binding.editability().available())
                        && counts.contains_key(&id)
                        && (manager.has_tabs_besides(&manager.worktree_tab_ids(id))
                            || (worktree_writes
                                && inventory.is_some_and(|inventory| {
                                    spaceterm_server::organize::fallback_destination(
                                        inventory,
                                        &record.root,
                                    )
                                    .is_some()
                                }))),
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

        /// The person's choice for a Workspace's Worktree group, or `None` for the default.
        pub(in crate::ui::workspace_manager) fn worktrees_expanded(
            &self,
            workspace_id: WorkspaceId,
        ) -> Option<bool> {
            self.workspace_repositories
                .worktrees
                .get(&workspace_id)
                .and_then(|worktrees| worktrees.expanded)
        }

        /// Restores a choice another window left, before the server's Git facts arrive.
        pub(in crate::ui::workspace_manager) fn restore_worktrees_expanded(
            &mut self,
            workspace_id: WorkspaceId,
            expanded: bool,
        ) {
            self.workspace_repositories
                .worktrees
                .entry(workspace_id)
                .or_default()
                .expanded = Some(expanded);
        }

        pub(in crate::ui::workspace_manager) fn set_server_worktrees_expanded(
            &mut self,
            workspace_id: WorkspaceId,
            expanded: bool,
        ) {
            if let Some(worktrees) = self.workspace_repositories.worktrees.get_mut(&workspace_id) {
                worktrees.expanded = Some(expanded);
            }
        }

        /// Shows a Worktree's Tabs. It reports `None` for a Workspace this window does not present.
        ///
        /// An existing Worktree without Tabs requests its first Tab from the owning server.
        pub(in crate::ui::workspace_manager) fn open_server_worktree(
            &mut self,
            workspace_id: WorkspaceId,
            worktree_id: WorktreeId,
            focus: bool,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Option<bool> {
            if !self.has_workspace(workspace_id) {
                return None;
            }
            let manager = self
                .workspaces
                .workspace(workspace_id)
                .map(|workspace| workspace.payload().clone())?;
            let shown = manager.update(cx, |manager, cx| {
                manager.show_worktree(worktree_id, focus, window, cx)
            });
            Some(shown || self.open_server_worktree_tab(workspace_id, worktree_id, window, cx))
        }

        /// A Worktree's root, as the server reported it.
        pub(in crate::ui::workspace_manager) fn worktree_root(
            &self,
            workspace_id: WorkspaceId,
            worktree_id: WorktreeId,
        ) -> Option<PathBuf> {
            self.workspace_repositories
                .worktrees
                .get(&workspace_id)?
                .registry
                .key(worktree_id)
                .map(|key| key.root().to_path_buf())
        }
    }

    /// The Worktree a Tab's server membership names. A root the repository lists now belongs to
    /// it; one whose own Worktree or saved Main Worktree the server reads joins that repository.
    /// Until the server reads either, the saved Main Worktree still names the repository, so a Tab
    /// in a Worktree git pruned before a restart joins its repository once the server reads it. A
    /// membership saved without a Main Worktree keeps the repository this window named earlier,
    /// so a Tab left in a removed Worktree stays with it.
    fn membership_key(
        root: &Path,
        saved: Option<&Path>,
        inventory: Option<&WorktreeInventory>,
        state: &PresentedWorktrees,
        repositories: &ServerRepositories,
    ) -> WorktreeKey {
        let key = |main_root: &Path| {
            WorktreeKey::new(
                RepositoryIdentity::new(main_root.to_path_buf()),
                root.to_path_buf(),
            )
        };
        if let Some(inventory) = inventory
            .filter(|inventory| inventory.worktrees.iter().any(|record| record.root == root))
        {
            return key(&inventory.main_root);
        }
        if let Some(listing) = membership_listing(root, saved, repositories) {
            return key(&listing.main_root);
        }
        if let Some(saved) = saved {
            return key(saved);
        }
        if let Some(named) = state
            .records
            .keys()
            .filter_map(|&id| state.registry.key(id))
            .find(|named| named.root() == root)
        {
            return named.clone();
        }
        key(inventory.map_or(root, |inventory| inventory.main_root.as_path()))
    }

    /// Whether a Pane's directory is in the Worktree at `root`. The Worktree the server last read
    /// the directory in decides, so a Worktree nested in another's directory is its own. A failed
    /// read keeps that Worktree, as it keeps the directory's last Repository Status. Only a
    /// directory the server never placed in a Worktree is compared by path, unless a successful
    /// read found it outside every repository.
    fn within_worktree(directory: &Path, root: &Path, repositories: &ServerRepositories) -> bool {
        let Some(facts) = repositories.facts(directory) else {
            return directory.starts_with(root);
        };
        let known = facts
            .worktrees
            .as_ref()
            .and_then(|list| list.worktrees.get(list.current?))
            .map(|record| record.root.as_path())
            .or_else(|| {
                facts
                    .repository
                    .as_ref()
                    .map(|repository| repository.root.as_path())
            });
        match known {
            Some(known) => known == root,
            None => facts.failure.is_some() && directory.starts_with(root),
        }
    }

    /// The listing of the repository a Tab's membership is in: the server's reading of its
    /// Worktree, else of the Main Worktree saved with the membership. A restarted server reads
    /// nothing in a removed Worktree, and git stops listing it once pruned, so only the saved
    /// Main Worktree still finds its repository.
    fn membership_listing<'a>(
        root: &Path,
        saved: Option<&Path>,
        repositories: &'a ServerRepositories,
    ) -> Option<&'a WorktreeInventory> {
        repositories
            .worktrees(root)
            .or_else(|| repositories.worktrees(saved?))
    }

    /// The record a Worktree this window never saw listed presents: the server's listing of it
    /// when another repository lists it, else a row with no branch whose directory is missing.
    fn unlisted_record(
        root: &Path,
        saved: Option<&Path>,
        repositories: &ServerRepositories,
    ) -> WorktreeRecord {
        membership_listing(root, saved, repositories)
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
