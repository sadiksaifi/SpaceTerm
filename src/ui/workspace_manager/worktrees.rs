//! Each local git Workspace's Worktrees in the sidebar. A Workspace lists the Worktrees of the
//! repository its sidebar row reads, by the same directory rule as its branch badge, so an
//! unpinned Workspace follows its Root Pane.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, AppContext as _, Context, Entity, SharedString, Subscription, Task, Window};

use super::WorkspaceManager;
use super::repository::{RowFacts, RowKey};
use crate::domain::{
    CurrentDirectory, PinnedDirectory, RepositoryIdentity, WorkspaceEntry, WorkspaceId, WorktreeId,
    WorktreeKey, WorktreeRegistry,
};
use crate::repository_status::RepositoryMachine;
use crate::repository_status::presentation::{HeadGlyph, SidebarBadge};
use crate::repository_status::scheduler::SourceDirectory;
use crate::ui::TabManager;
use crate::ui::workspace_sidebar::{
    WorktreeGroup, WorktreeRemoval, WorktreeRowViewModel, WorktreeSection,
};
use crate::ui::worktree_form::{
    LocationProbe, WorktreeForm, WorktreeFormBackend, WorktreeFormContext, WorktreeFormEvent,
};
use crate::ui::worktree_store::{InstalledWorktrees, WorktreeStore, WorktreesChanged};
use crate::worktrees::WorktreeSnapshot;
use crate::worktrees::catalog::{WorktreeInterestId, WorktreeListing};
use crate::worktrees::git::{BranchList, WorktreeBranch, WorktreeCreateError};
use crate::worktrees::listing::{WorktreeHead, WorktreeRecord};

mod cycle;
mod removal;

#[derive(Default)]
pub(super) struct SidebarWorktrees {
    workspaces: BTreeMap<WorkspaceId, WorkspaceWorktrees>,
    form: Option<Entity<WorktreeForm>>,
    created: Option<CreatedWorktree>,
    /// The removal being checked, confirmed, or run. One runs at a time.
    removal: Option<u64>,
    removal_generation: u64,
}

/// A Worktree git created whose first Tab opens once the listing shows it.
struct CreatedWorktree {
    workspace_id: WorkspaceId,
    path: PathBuf,
    /// The roots listed before the create. Git may spell the new root differently from the
    /// typed path, so the one new root also names it.
    known: Vec<PathBuf>,
    _changes: Subscription,
}

/// Runs the New Worktree dialog's git work through the shared store.
struct StoreFormBackend {
    store: Entity<WorktreeStore>,
    root: PathBuf,
    common: PathBuf,
}

impl WorktreeFormBackend for StoreFormBackend {
    fn branches(&self, cx: &mut App) -> Task<Option<BranchList>> {
        let root = self.root.clone();
        self.store.update(cx, |store, cx| store.branches(root, cx))
    }

    fn create(
        &self,
        path: PathBuf,
        branch: WorktreeBranch,
        cx: &mut App,
    ) -> Task<Result<(), WorktreeCreateError>> {
        let (root, common) = (self.root.clone(), self.common.clone());
        self.store
            .update(cx, |store, cx| store.create(root, common, path, branch, cx))
    }
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

    /// The snapshot the row shows. After the Workspace's directory changes, it stays the last
    /// listed one until git lists the new directory, so the row doesn't regroup in between.
    fn presented(&self, cx: &App) -> Option<Arc<WorktreeSnapshot>> {
        match self.store.read(cx).presented(self.interest) {
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

    /// Shows the Tabs outside every Worktree, which an expanded Workspace row stands for once its
    /// Workspace has left a repository. Returns whether the Active Tab changed.
    pub(super) fn show_unscoped_tabs(
        &mut self,
        workspace_id: WorkspaceId,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(manager) = self
            .workspaces
            .workspace(workspace_id)
            .map(|workspace| workspace.payload().clone())
        else {
            return false;
        };
        manager.update(cx, |manager, cx| {
            manager.show_unscoped_tabs(focus, window, cx)
        })
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

    /// Whether a Workspace reads a local repository, where it can create Worktrees.
    pub(super) fn creates_worktrees(&self, workspace_id: WorkspaceId, cx: &App) -> bool {
        self.sidebar_worktrees
            .workspaces
            .get(&workspace_id)
            .is_some_and(|worktrees| worktrees.presented(cx).is_some())
    }

    /// Presents the New Worktree dialog for a Workspace's repository.
    pub(super) fn new_worktree(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .sidebar_worktrees
            .form
            .as_ref()
            .is_some_and(|form| form.read(cx).is_open())
        {
            return false;
        }
        let Some(worktrees) = self.sidebar_worktrees.workspaces.get(&workspace_id) else {
            return false;
        };
        let Some(snapshot) = worktrees.listed(cx) else {
            return false;
        };
        let store = worktrees.store.clone();
        let branch_of = |record: &WorktreeRecord| match &record.head {
            WorktreeHead::Branch(branch) => Some(branch.clone()),
            WorktreeHead::Detached(_) | WorktreeHead::Bare => None,
        };
        // A new branch starts from the Active Worktree's branch, else the Main Worktree's.
        let active = self
            .workspaces
            .workspace(workspace_id)
            .and_then(|workspace| workspace.payload().read(cx).active_worktree())
            .and_then(|id| worktrees.registry.key(id))
            .and_then(|key| {
                snapshot
                    .worktrees
                    .iter()
                    .find(|record| record.root == key.root())
            })
            .and_then(branch_of);
        let default_base = active.or_else(|| snapshot.worktrees.first().and_then(branch_of));
        let checked_out = snapshot
            .worktrees
            .iter()
            .filter_map(|record| {
                Some((
                    branch_of(record)?,
                    directory_name(&record.root).to_string().into(),
                ))
            })
            .collect();
        let context = WorktreeFormContext {
            repository_name: directory_name(snapshot.repository.main_root()).to_string(),
            template: store.read(cx).path_template(),
            home: self.local_home_directory_path.clone(),
            checked_out,
            default_base,
        };
        let backend = Rc::new(StoreFormBackend {
            store: store.clone(),
            root: snapshot.repository.main_root().to_path_buf(),
            common: snapshot.common_directory.clone(),
        });
        let filesystem = self.local_filesystem.clone();
        let probe: LocationProbe =
            Rc::new(move |path: &std::path::Path| filesystem.probe_new_directory(path));
        let known: Vec<PathBuf> = snapshot
            .worktrees
            .iter()
            .map(|record| record.root.clone())
            .collect();
        let form = cx.new(|cx| WorktreeForm::new(backend, context, probe, window, cx));
        cx.subscribe_in(&form, window, move |manager, _, event, window, cx| {
            if let WorktreeFormEvent::Created(path) = event {
                let changes = cx.subscribe_in(
                    &store,
                    window,
                    |manager, _, _: &WorktreesChanged, window, cx| {
                        manager.open_created_worktree(window, cx);
                    },
                );
                manager.set_worktrees_expanded(workspace_id, true);
                manager.sidebar_worktrees.created = Some(CreatedWorktree {
                    workspace_id,
                    path: path.clone(),
                    known: known.clone(),
                    _changes: changes,
                });
                manager.open_created_worktree(window, cx);
            }
        })
        .detach();
        let presented = form.update(cx, |form, cx| form.present(window, cx));
        self.sidebar_worktrees.form = Some(form);
        presented
    }

    #[cfg(test)]
    pub(super) fn worktree_form(&self) -> Option<Entity<WorktreeForm>> {
        self.sidebar_worktrees.form.clone()
    }

    /// Opens the first Tab of the Worktree the dialog created once its listing shows it.
    fn open_created_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(created) = &self.sidebar_worktrees.created else {
            return;
        };
        let workspace_id = created.workspace_id;
        self.sync_sidebar_worktrees(cx);
        let Some(created) = &self.sidebar_worktrees.created else {
            return;
        };
        let Some(worktrees) = self.sidebar_worktrees.workspaces.get(&workspace_id) else {
            self.sidebar_worktrees.created = None;
            return;
        };
        let Some(snapshot) = worktrees.listed(cx) else {
            return;
        };
        let added: Vec<&WorktreeRecord> = snapshot
            .worktrees
            .iter()
            .filter(|record| !created.known.contains(&record.root))
            .collect();
        let record = snapshot
            .worktrees
            .iter()
            .find(|record| record.root == created.path)
            .or_else(|| match added[..] {
                [record] => Some(record),
                _ => None,
            });
        let Some(id) = record.and_then(|record| {
            worktrees.registry.id(&WorktreeKey::new(
                snapshot.repository.clone(),
                record.root.clone(),
            ))
        }) else {
            return;
        };
        self.sidebar_worktrees.created = None;
        if self.activate_workspace(workspace_id, window, cx)
            && self.open_worktree(workspace_id, id, true, window, cx)
        {
            self.focus(window, cx);
        }
        cx.notify();
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
        let snapshot = worktrees.presented(cx);
        let pinned = self.local_pinned_directory(workspace_id);
        // A pinned Workspace lists the repository of its Pinned Directory, so git's current
        // Worktree holds it, through any symbolic link the Pinned Directory was chosen by.
        let pinned_root = pinned
            .as_ref()
            .and(snapshot.as_ref())
            .and_then(|snapshot| snapshot.worktrees.get(snapshot.current?))
            .map(|record| record.root.clone());
        // `listed` says whether the repository the Workspace reads lists the Worktree now.
        let row = |id: WorktreeId, record: &WorktreeRecord, missing: bool, listed: bool| {
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
            let main = worktrees
                .registry
                .key(id)
                .is_some_and(|key| key.root() == key.repository().main_root());
            let removal = if !listed {
                WorktreeRemoval::Unlisted
            } else if main {
                WorktreeRemoval::Main
            } else if record.locked {
                WorktreeRemoval::Locked
            } else if pinned_root.as_ref() == Some(&record.root)
                || pinned
                    .as_ref()
                    .is_some_and(|pinned| pinned.starts_with(&record.root))
            {
                WorktreeRemoval::HoldsPinnedDirectory
            } else {
                WorktreeRemoval::Allowed
            };
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
                main,
                has_tabs: counts.contains_key(&id),
                active: active == Some(id),
                locked: record.locked,
                missing: missing || record.missing,
                removal,
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
                    Some(row(worktrees.registry.id(&key)?, record, false, true))
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
                    Some(row(id, worktrees.records.get(&id)?, true, false))
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
                    .push(row(id, record, false, false));
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

    /// A local Workspace's Pinned Directory.
    fn local_pinned_directory(&self, workspace_id: WorkspaceId) -> Option<PathBuf> {
        match self
            .workspaces
            .workspace(workspace_id)?
            .pinned_directory()?
        {
            PinnedDirectory::Local(directory) => Some(directory.path().to_path_buf()),
            PinnedDirectory::Remote { .. } => None,
        }
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
