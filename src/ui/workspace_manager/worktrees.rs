//! Each local git Workspace's Worktrees in the sidebar. A Workspace lists the Worktrees of the
//! repository its sidebar row reads, by the same directory rule as its branch badge, so an
//! unpinned Workspace follows its Root Pane.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, Context, Entity, SharedString, Subscription, Window};

use super::WorkspaceManager;
use super::repository::{RowFacts, RowKey};
use crate::domain::{
    CurrentDirectory, RepositoryIdentity, WorkspaceEntry, WorkspaceId, WorktreeId, WorktreeKey,
    WorktreeRegistry,
};
use crate::repository_status::RepositoryMachine;
use crate::repository_status::presentation::{HeadGlyph, SidebarBadge};
use crate::repository_status::scheduler::SourceDirectory;
use crate::ui::TabManager;
use crate::ui::workspace_sidebar::{WorktreeGroup, WorktreeRowViewModel, WorktreeSection};
use crate::ui::worktree_store::{InstalledWorktrees, WorktreeStore, WorktreesChanged};
use crate::worktrees::WorktreeSnapshot;
use crate::worktrees::catalog::{WorktreeInterestId, WorktreeListing};
use crate::worktrees::listing::{WorktreeHead, WorktreeRecord};

#[derive(Default)]
pub(super) struct SidebarWorktrees {
    workspaces: BTreeMap<WorkspaceId, WorkspaceWorktrees>,
}

struct WorkspaceWorktrees {
    store: Entity<WorktreeStore>,
    interest: WorktreeInterestId,
    directory: PathBuf,
    registry: WorktreeRegistry,
    /// The last listed record of each Worktree with a handle. A Worktree git no longer lists
    /// keeps its label while its Tabs stay open.
    records: BTreeMap<WorktreeId, WorktreeRecord>,
    /// The person's choice, which replaces the default for the rest of the session.
    expanded: Option<bool>,
    _changes: Subscription,
}

impl WorkspaceWorktrees {
    fn listed(&self, cx: &App) -> Option<Arc<WorktreeSnapshot>> {
        match self.store.read(cx).listing(self.interest) {
            WorktreeListing::Listed(snapshot) => Some(snapshot),
            WorktreeListing::Pending | WorktreeListing::Outside => None,
        }
    }
}

impl WorkspaceManager {
    /// Follows each local Workspace's repository and keeps its Tabs in its Worktrees.
    pub(super) fn sync_sidebar_worktrees(&mut self, cx: &mut Context<Self>) {
        let Some(store) = InstalledWorktrees::store(cx) else {
            return;
        };
        let wanted: BTreeMap<WorkspaceId, PathBuf> = self
            .workspaces
            .iter()
            .filter_map(|workspace| Some((workspace.id(), self.worktree_directory(workspace, cx)?)))
            .collect();
        let gone: Vec<WorkspaceId> = self
            .sidebar_worktrees
            .workspaces
            .keys()
            .filter(|workspace_id| !wanted.contains_key(workspace_id))
            .copied()
            .collect();
        for workspace_id in gone {
            if let Some(worktrees) = self.sidebar_worktrees.workspaces.remove(&workspace_id) {
                worktrees
                    .store
                    .update(cx, |store, cx| store.unregister(worktrees.interest, cx));
                cx.notify();
            }
        }
        for (workspace_id, directory) in wanted {
            match self.sidebar_worktrees.workspaces.get_mut(&workspace_id) {
                Some(worktrees) if worktrees.directory != directory => {
                    worktrees.directory = directory.clone();
                    let interest = worktrees.interest;
                    worktrees.store.update(cx, |store, cx| {
                        store.set_directory(interest, directory, cx);
                    });
                }
                Some(_) => {}
                None => self.register_worktrees(workspace_id, directory, &store, cx),
            }
            self.reconcile_worktree_tabs(workspace_id, cx);
        }
    }

    pub(super) fn release_sidebar_worktrees(&mut self, cx: &mut App) {
        for (_, worktrees) in std::mem::take(&mut self.sidebar_worktrees.workspaces) {
            worktrees
                .store
                .update(cx, |store, cx| store.unregister(worktrees.interest, cx));
        }
    }

    /// Shows a Worktree's Tabs, opening its first Tab when it has none. A Missing Worktree has no
    /// directory to open.
    pub(super) fn open_worktree(
        &mut self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(worktrees) = self.sidebar_worktrees.workspaces.get(&workspace_id) else {
            return false;
        };
        let (Some(key), Some(record)) = (
            worktrees.registry.key(worktree_id),
            worktrees.records.get(&worktree_id),
        ) else {
            return false;
        };
        let listed = worktrees.listed(cx).is_some_and(|snapshot| {
            snapshot.repository == *key.repository()
                && snapshot
                    .worktrees
                    .iter()
                    .any(|listed| listed.root == key.root())
        });
        // A Worktree without a directory to start in shows only the Tabs it still has.
        if (record.missing || !listed) && !self.worktree_has_tabs(workspace_id, worktree_id, cx) {
            return false;
        }
        let root = key.root().to_path_buf();
        let Some(manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            return false;
        };
        manager.update(cx, |manager, cx| {
            manager.open_worktree(worktree_id, root, focus, window, cx);
        });
        true
    }

    /// Opens another Tab in a Worktree, or its first Tab when it has none.
    pub(super) fn new_worktree_tab(
        &mut self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let had_tabs = self.worktree_has_tabs(workspace_id, worktree_id, cx);
        if !self.open_worktree(workspace_id, worktree_id, true, window, cx) {
            return false;
        }
        if had_tabs && let Some(workspace) = self.workspaces.workspace(workspace_id) {
            workspace
                .payload()
                .update(cx, |manager, cx| manager.create_tab(window, cx));
        }
        true
    }

    pub(super) fn set_worktrees_expanded(&mut self, workspace_id: WorkspaceId, expanded: bool) {
        if let Some(worktrees) = self.sidebar_worktrees.workspaces.get_mut(&workspace_id) {
            worktrees.expanded = Some(expanded);
        }
    }

    pub(super) fn worktrees_window_activated(cx: &mut App) {
        if let Some(store) = InstalledWorktrees::store(cx) {
            store.update(cx, |store, cx| store.window_activated(cx));
        }
    }

    /// The Worktrees a Workspace row discloses, or `None` for a Workspace outside a repository.
    pub(super) fn worktree_section(
        &self,
        workspace_id: WorkspaceId,
        cx: &App,
    ) -> Option<WorktreeSection> {
        let worktrees = self.sidebar_worktrees.workspaces.get(&workspace_id)?;
        let manager = self.workspaces.workspace(workspace_id)?.payload().read(cx);
        let counts = manager.worktree_tab_counts();
        let active = manager.active_worktree();
        let snapshot = worktrees.listed(cx);
        let row = |id: WorktreeId, record: &WorktreeRecord, missing: bool| {
            let (label, detached) = match &record.head {
                WorktreeHead::Branch(branch) => (branch.clone(), false),
                WorktreeHead::Detached(commit) => (commit.clone(), true),
                WorktreeHead::Bare => (String::new(), false),
            };
            let directory = self.worktree_row_directory(workspace_id, id, &record.root, cx);
            // The listing names the branch until the Worktree's status is read.
            let repository = self.worktree_badge(workspace_id, id, cx).or_else(|| {
                (!label.is_empty()).then(|| SidebarBadge::Branch {
                    glyph: HeadGlyph::from_detached(detached),
                    text: label.clone(),
                })
            });
            WorktreeRowViewModel {
                worktree_id: id,
                name: directory_name(&record.root),
                label: label.into(),
                detached,
                path: record.root.display().to_string().into(),
                directory: super::compact_home_path(&directory, &self.local_home_directory_path)
                    .into(),
                directory_tooltip: directory.display().to_string().into(),
                repository,
                main: worktrees
                    .registry
                    .key(id)
                    .is_some_and(|key| key.root() == key.repository().main_root()),
                has_tabs: counts.contains_key(&id),
                active: active == Some(id),
                locked: record.locked,
                missing: missing || record.missing,
            }
        };
        let mut groups = Vec::new();
        let current = snapshot.as_ref().map(|snapshot| &snapshot.repository);
        if let Some(snapshot) = &snapshot {
            let mut rows: Vec<WorktreeRowViewModel> = snapshot
                .worktrees
                .iter()
                .filter(|record| record.head != WorktreeHead::Bare)
                .filter_map(|record| {
                    let key = WorktreeKey::new(snapshot.repository.clone(), record.root.clone());
                    Some(row(worktrees.registry.id(&key)?, record, false))
                })
                .collect();
            // A Worktree removed outside SpaceTerm keeps its row while its Tabs stay open.
            let removed: Vec<WorktreeRowViewModel> = counts
                .keys()
                .filter_map(|&id| {
                    let key = worktrees.registry.key(id)?;
                    let listed = rows.iter().any(|row| row.worktree_id == id);
                    if key.repository() != &snapshot.repository || listed {
                        return None;
                    }
                    Some(row(id, worktrees.records.get(&id)?, true))
                })
                .collect();
            rows.extend(removed);
            groups.push(WorktreeGroup {
                former_repository: None,
                rows,
            });
        }
        let mut former: BTreeMap<&RepositoryIdentity, Vec<WorktreeRowViewModel>> = BTreeMap::new();
        for &id in counts.keys() {
            let Some(key) = worktrees.registry.key(id) else {
                continue;
            };
            if Some(key.repository()) == current {
                continue;
            }
            if let Some(record) = worktrees.records.get(&id) {
                former
                    .entry(key.repository())
                    .or_default()
                    .push(row(id, record, false));
            }
        }
        groups.extend(former.into_iter().map(|(repository, rows)| WorktreeGroup {
            former_repository: Some(repository_name(repository)),
            rows,
        }));
        // A repository's Main Worktree alone leaves the row as it is outside a git repository.
        if groups.iter().map(|group| group.rows.len()).sum::<usize>() < 2
            && groups.iter().all(|group| group.former_repository.is_none())
        {
            return None;
        }
        Some(WorktreeSection {
            expanded: worktrees.expanded.unwrap_or(true),
            repository: snapshot.as_ref().map(|snapshot| {
                super::compact_home_path(
                    snapshot.repository.main_root(),
                    &self.local_home_directory_path,
                )
                .into()
            }),
            groups,
        })
    }

    /// The Repository Status each disclosed Worktree row reads: its last-used directory, while the
    /// row is disclosed or is the Active Worktree its collapsed Workspace row shows.
    pub(super) fn worktree_row_facts(&self, cx: &App) -> Vec<(RowKey, RowFacts)> {
        let mut facts = Vec::new();
        for (&workspace_id, worktrees) in &self.sidebar_worktrees.workspaces {
            let Some(section) = self.worktree_section(workspace_id, cx) else {
                continue;
            };
            for row in section.rows().filter(|row| !row.missing) {
                let Some(key) = worktrees.registry.key(row.worktree_id) else {
                    continue;
                };
                let directory =
                    self.worktree_row_directory(workspace_id, row.worktree_id, key.root(), cx);
                facts.push((
                    RowKey::Worktree(workspace_id, row.worktree_id),
                    RowFacts {
                        machine: RepositoryMachine::Local,
                        directory: SourceDirectory::Local(directory),
                        available: true,
                        visible: section.expanded || row.active,
                    },
                ));
            }
        }
        facts
    }

    /// The directory of the Pane last used in a Worktree, else the Worktree's root.
    fn worktree_row_directory(
        &self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        root: &std::path::Path,
        cx: &App,
    ) -> PathBuf {
        self.workspaces
            .workspace(workspace_id)
            .and_then(|workspace| {
                match workspace
                    .payload()
                    .read(cx)
                    .worktree_directory(worktree_id, cx)?
                {
                    CurrentDirectory::Local(directory) => Some(directory),
                    CurrentDirectory::Remote(_) => None,
                }
            })
            .unwrap_or_else(|| root.to_path_buf())
    }

    fn worktree_has_tabs(&self, workspace_id: WorkspaceId, id: WorktreeId, cx: &App) -> bool {
        self.workspaces
            .workspace(workspace_id)
            .is_some_and(|workspace| {
                workspace
                    .payload()
                    .read(cx)
                    .worktree_tab_counts()
                    .contains_key(&id)
            })
    }

    fn register_worktrees(
        &mut self,
        workspace_id: WorkspaceId,
        directory: PathBuf,
        store: &Entity<WorktreeStore>,
        cx: &mut Context<Self>,
    ) {
        let interest = store.update(cx, |store, cx| store.register(directory.clone(), cx));
        let changes = cx.subscribe(store, |_, _, _: &WorktreesChanged, cx| cx.notify());
        self.sidebar_worktrees.workspaces.insert(
            workspace_id,
            WorkspaceWorktrees {
                store: store.clone(),
                interest,
                directory,
                registry: WorktreeRegistry::default(),
                records: BTreeMap::new(),
                expanded: None,
                _changes: changes,
            },
        );
    }

    /// Names every listed Worktree, moves Tabs outside any Worktree into the one containing the
    /// Workspace's directory, and lets an unpinned Workspace's Root Tab follow its Root Pane.
    fn reconcile_worktree_tabs(&mut self, workspace_id: WorkspaceId, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspaces.workspace(workspace_id) else {
            return;
        };
        let pinned = workspace.pinned_directory().is_some();
        let manager = workspace.payload().clone();
        let Some(worktrees) = self.sidebar_worktrees.workspaces.get_mut(&workspace_id) else {
            return;
        };
        let listing = worktrees.store.read(cx).listing(worktrees.interest);
        let current = match &listing {
            WorktreeListing::Pending => return,
            WorktreeListing::Outside => None,
            WorktreeListing::Listed(snapshot) => {
                let mut ids = Vec::new();
                for record in &snapshot.worktrees {
                    let key = WorktreeKey::new(snapshot.repository.clone(), record.root.clone());
                    let Ok(id) = worktrees.registry.id_for(&key) else {
                        return;
                    };
                    worktrees.records.insert(id, record.clone());
                    ids.push(id);
                }
                snapshot.current.and_then(|index| {
                    Some((ids[index], snapshot.worktrees.get(index)?.root.clone()))
                })
            }
        };
        let counts = manager.read(cx).worktree_tab_counts();
        let listed_roots: Vec<&std::path::Path> = match &listing {
            WorktreeListing::Listed(snapshot) => snapshot
                .worktrees
                .iter()
                .map(|record| record.root.as_path())
                .collect(),
            _ => Vec::new(),
        };
        let unused: Vec<WorktreeId> = worktrees
            .records
            .keys()
            .copied()
            .filter(|id| {
                !counts.contains_key(id)
                    && current.as_ref().is_none_or(|(current, _)| current != id)
                    && worktrees
                        .registry
                        .key(*id)
                        .is_none_or(|key| !listed_roots.contains(&key.root()))
            })
            .collect();
        for id in unused {
            worktrees.registry.forget(id);
            worktrees.records.remove(&id);
        }
        manager.update(cx, |manager, cx| {
            if let Some(current) = &current {
                manager.move_worktree_tabs(None, Some(current.clone()), cx);
            }
            if !pinned {
                manager.move_root_tab(current, cx);
            }
        });
    }

    /// The local directory whose repository a Workspace row lists, by the branch badge's rule.
    fn worktree_directory(
        &self,
        workspace: &WorkspaceEntry<Entity<TabManager>>,
        cx: &App,
    ) -> Option<PathBuf> {
        let (machine, directory) = self.row_source(workspace, cx)?;
        match (machine, directory) {
            (RepositoryMachine::Local, SourceDirectory::Local(directory)) => Some(directory),
            _ => None,
        }
    }
}

/// The repository's name as a divider shows it: its Main Worktree's last component.
fn repository_name(repository: &RepositoryIdentity) -> SharedString {
    directory_name(repository.main_root())
}

/// A directory's last component, or the whole path for a root.
fn directory_name(directory: &std::path::Path) -> SharedString {
    directory
        .file_name()
        .map_or_else(
            || directory.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
        .into()
}
