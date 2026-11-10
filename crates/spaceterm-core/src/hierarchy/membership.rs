//! Which repository each Workspace is in and which Worktree each of its Tabs belongs to, under
//! [ADR 0020](../../../../docs/adr/0020-tag-tabs-by-worktree.md).
//!
//! A Workspace is in at most one repository, and every Tab that has a Worktree has one of that
//! repository's. A change of repository therefore carries every Tab in the same step.

use std::path::{Path, PathBuf};

use super::{Id, Pane, State, Tab, Workspace, WorktreeMembership};
use crate::repository::{Snapshot, WorktreeHead};

/// What the latest read says about a directory.
enum Place<'a> {
    /// A successful read placed it in this Worktree of the repository with this Main Worktree.
    Worktree {
        root: &'a Path,
        repository: &'a Path,
    },
    /// A successful read placed it outside any repository.
    Outside,
    /// The read failed, or placed it where no Tab can belong: a Git directory, or a Missing or
    /// bare Worktree.
    Unreadable,
    /// No read has listed its Worktrees yet.
    Unread,
}

fn place<'a>(snapshot: &'a Snapshot, directory: &Path) -> Place<'a> {
    let Some(facts) = snapshot.facts(directory) else {
        return Place::Unread;
    };
    if facts.outside_repository() {
        return Place::Outside;
    }
    if facts.failure.is_some() {
        return Place::Unreadable;
    }
    let Some(list) = &facts.worktrees else {
        return if facts.inside_git_directory {
            Place::Unreadable
        } else {
            Place::Unread
        };
    };
    match list.current.and_then(|current| list.worktrees.get(current)) {
        Some(record) if !record.missing && record.head != WorktreeHead::Bare => Place::Worktree {
            root: &record.root,
            repository: &list.main_root,
        },
        _ => Place::Unreadable,
    }
}

fn root_directory(tab: &Tab) -> Option<&Path> {
    tab.panes
        .iter()
        .find(|pane| pane.id == tab.root_pane)
        .map(Pane::repository_directory)
}

/// The repository a Workspace is in and the Worktree a Tab without one joins: the Root Tab's
/// when it has one, else the first Tab's that does.
fn home(workspace: &Workspace) -> Option<(PathBuf, PathBuf)> {
    let placed = |tab: &Tab| Some((tab.worktree_repository.clone()?, tab.worktree.clone()?));
    workspace
        .tabs
        .iter()
        .find(|tab| tab.id == workspace.root_tab)
        .and_then(placed)
        .or_else(|| workspace.tabs.iter().find_map(placed))
}

fn leave(tab: &mut Tab) -> bool {
    let left = tab.worktree.take().is_some();
    let forgot = tab.worktree_repository.take().is_some();
    let follows = std::mem::take(&mut tab.worktree_membership) != WorktreeMembership::Following;
    left || forgot || follows
}

fn join(tab: &mut Tab, root: &Path, repository: &Path) -> bool {
    if tab.worktree.as_deref() == Some(root)
        && tab.worktree_repository.as_deref() == Some(repository)
    {
        return false;
    }
    tab.worktree = Some(root.to_owned());
    tab.worktree_repository = Some(repository.to_owned());
    true
}

/// Gives a saved membership the Main Worktree that locates its repository, once a read lists it.
fn locate(tab: &mut Tab, snapshot: &Snapshot) -> bool {
    if tab.worktree_repository.is_some() {
        return false;
    }
    let Some(worktree) = tab.worktree.as_deref() else {
        return false;
    };
    let main = snapshot.directories.iter().find_map(|facts| {
        let list = facts
            .worktrees
            .as_ref()
            .filter(|_| facts.failure.is_none())?;
        list.worktrees
            .iter()
            .any(|record| record.root == worktree)
            .then_some(&list.main_root)
    });
    match main {
        Some(main) => {
            tab.worktree_repository = Some(main.clone());
            true
        }
        None => false,
    }
}

/// Moves the Workspace's leader, then brings every other Tab into the Workspace's repository.
///
/// The leader is the Pinned Directory, else the only Tab, else the Root Tab of a Workspace in no
/// repository. A Workspace with several Tabs in a repository has none, so no `cd` moves it.
fn reconcile(workspace: &mut Workspace, snapshot: &Snapshot) -> bool {
    let mut changed = false;
    for tab in &mut workspace.tabs {
        changed |= locate(tab, snapshot);
    }
    let pinned = match workspace.pinned_directory.as_deref() {
        Some(pin) => match place(snapshot, pin) {
            Place::Worktree { root, repository } => {
                Some(Some((repository.to_owned(), root.to_owned())))
            }
            Place::Outside => {
                for tab in &mut workspace.tabs {
                    changed |= leave(tab);
                }
                Some(None)
            }
            // A failed or pending read never moves a Tab.
            Place::Unreadable | Place::Unread => Some(home(workspace)),
        },
        None => None,
    };
    let home = pinned.unwrap_or_else(|| {
        let in_repository = home(workspace).is_some();
        let root_tab = workspace.root_tab;
        let lone = workspace.tabs.len() == 1;
        if let Some(tab) = workspace
            .tabs
            .iter_mut()
            .find(|tab| tab.id == root_tab)
            .filter(|_| lone || !in_repository)
            && let Some(directory) = root_directory(tab).map(Path::to_owned)
        {
            match (place(snapshot, &directory), tab.worktree_membership) {
                (Place::Outside, WorktreeMembership::Following | WorktreeMembership::Repaired) => {
                    changed |= leave(tab)
                }
                (Place::Worktree { root, repository }, WorktreeMembership::Following) => {
                    changed |= join(tab, root, repository);
                }
                _ => {}
            }
        }
        home(workspace)
    });
    let Some((repository, fallback)) = home else {
        return changed;
    };
    let stays = workspace.pinned_directory.is_some() || workspace.tabs.len() > 1;
    for tab in &mut workspace.tabs {
        if tab.worktree.is_none() || tab.worktree_repository.as_deref() != Some(&repository) {
            let own = root_directory(tab).map(|directory| place(snapshot, directory));
            let root = match own {
                // The Tab joins once its own read says which Worktree its shell is in.
                Some(Place::Unread) => continue,
                Some(Place::Worktree {
                    root,
                    repository: own,
                }) if own == repository => root,
                _ => &fallback,
            };
            changed |= join(tab, root, &repository);
        }
        // Only a `cd` in an only Tab makes a Tab follow its shell again.
        if stays && tab.worktree_membership == WorktreeMembership::Following {
            tab.worktree_membership = WorktreeMembership::Opened;
            changed = true;
        }
    }
    changed
}

impl Workspace {
    /// The shell in `pane` changed directory. From here on an only Tab takes its unpinned
    /// Workspace along, whatever kept it in its Worktree before. A repaired Tab still joins no
    /// other Worktree under [ADR 0044](../../../../docs/adr/0044-restore-panes-in-their-last-confirmed-directory.md).
    pub(super) fn shell_moved(&mut self, pane: Id) {
        if self.pinned_directory.is_none()
            && let [tab] = self.tabs.as_mut_slice()
            && tab.root_pane == pane
            && tab.worktree_membership == WorktreeMembership::Opened
        {
            tab.worktree_membership = WorktreeMembership::Following;
        }
    }

    /// The Main Worktree of the repository this Workspace is in.
    pub fn repository(&self) -> Option<&Path> {
        fn placed(tab: &Tab) -> Option<&Path> {
            tab.worktree
                .as_ref()
                .and(tab.worktree_repository.as_deref())
        }
        self.tabs
            .iter()
            .find(|tab| tab.id == self.root_tab)
            .and_then(placed)
            .or_else(|| self.tabs.iter().find_map(placed))
    }
}

impl State {
    /// Applies what `snapshot` read to every Workspace's repository and its Tabs' Worktrees, and
    /// renames the Workspaces that entered or left a repository. Returns whether anything
    /// changed.
    pub fn reconcile_worktrees(&mut self, snapshot: &Snapshot) -> bool {
        let mut changed = false;
        for workspace in &mut self.inventory.workspaces {
            changed |= reconcile(workspace, snapshot);
        }
        if changed {
            self.recalculate_names();
            self.inventory.revision = self.inventory.revision.saturating_add(1);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::super::{CreateControl, Mutation, Outcome};
    use super::*;
    use crate::repository::{
        DirectoryFacts, Repository, RepositoryHead, RepositoryReadError, WorktreeInventory,
        WorktreeRecord,
    };

    const DEVICE: Id = [1; 32];

    /// One Workspace whose only Tab's shell is in `directory`.
    fn workspace_in(directory: &str) -> State {
        let mut state = State::empty();
        state
            .apply(
                DEVICE,
                1,
                Mutation::CreateWorkspace {
                    name: String::new(),
                    directory: None,
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        let pane = state.inventory.workspaces[0].tabs[0].root_pane;
        state.confirm_directory(pane, directory.into());
        state
    }

    /// A read of `directory` inside the Worktree `current` of the repository at `main`.
    fn inside(directory: &str, main: &str, current: &str, roots: &[&str]) -> DirectoryFacts {
        DirectoryFacts {
            inside_git_directory: false,
            directory: directory.into(),
            repository: Some(Repository {
                root: current.into(),
                head: RepositoryHead::Branch("main".into()),
                commit: None,
                upstream: None,
                operation: None,
                changes: Err(RepositoryReadError::Unavailable),
                counting: None,
                pull_request: None,
            }),
            failure: None,
            worktrees: Some(WorktreeInventory {
                main_root: main.into(),
                current: roots.iter().position(|root| *root == current),
                worktrees: roots
                    .iter()
                    .map(|root| WorktreeRecord {
                        root: root.into(),
                        head: WorktreeHead::Branch("main".into()),
                        locked: false,
                        missing: false,
                    })
                    .collect(),
            }),
        }
    }

    fn outside(directory: &str) -> DirectoryFacts {
        DirectoryFacts {
            inside_git_directory: false,
            directory: directory.into(),
            repository: None,
            failure: None,
            worktrees: None,
        }
    }

    /// The repository at `/repo` with the linked Worktree `/feature`, another repository at
    /// `/other`, and directories outside both.
    fn reads() -> Snapshot {
        const REPO: [&str; 2] = ["/repo", "/feature"];
        Snapshot {
            boot: [9; 32],
            revision: 1,
            git: Default::default(),
            directories: vec![
                inside("/repo", "/repo", "/repo", &REPO),
                inside("/repo/src", "/repo", "/repo", &REPO),
                inside("/feature", "/repo", "/feature", &REPO),
                inside("/other", "/other", "/other", &["/other"]),
                outside("/tmp"),
                outside("/var"),
                outside("/home/test"),
            ],
            pinned_worktrees: Vec::new(),
            pin_check: Default::default(),
        }
    }

    fn open_in_feature(state: &mut State) -> Id {
        let workspace = state.inventory.workspaces[0].id;
        state
            .add_worktree_tab(
                None,
                workspace,
                "/repo".into(),
                "/feature".into(),
                "/feature".into(),
            )
            .unwrap()
    }

    fn pin(state: &mut State, directory: Option<&str>) {
        let workspace = &state.inventory.workspaces[0];
        let (workspace, version) = (workspace.id, workspace.pin_version);
        let sequence = state.next_sequence(DEVICE);
        let outcome = state
            .apply(
                DEVICE,
                sequence,
                Mutation::PinWorkspace {
                    workspace,
                    version,
                    directory: directory.map(Into::into),
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        assert!(matches!(outcome, Outcome::Accepted { .. }));
    }

    /// Each Tab's Worktree, in Tab order.
    fn worktrees(state: &State) -> Vec<Option<&str>> {
        state.inventory.workspaces[0]
            .tabs
            .iter()
            .map(|tab| tab.worktree.as_deref().and_then(Path::to_str))
            .collect()
    }

    fn name(state: &State) -> &str {
        &state.inventory.workspaces[0].name
    }

    fn root_pane(state: &State, tab: usize) -> Id {
        state.inventory.workspaces[0].tabs[tab].root_pane
    }

    #[test]
    fn a_cd_out_of_the_repository_moves_nothing_while_another_tab_is_in_it() {
        let mut state = workspace_in("/repo");
        state.reconcile_worktrees(&reads());
        open_in_feature(&mut state);

        state.confirm_directory(root_pane(&state, 0), "/tmp".into());
        state.reconcile_worktrees(&reads());

        assert_eq!(worktrees(&state), [Some("/repo"), Some("/feature")]);
        assert_eq!(name(&state), "repo");
    }

    #[test]
    fn a_cd_into_another_repository_or_worktree_moves_nothing_while_another_tab_is_in_it() {
        let mut state = workspace_in("/repo");
        state.reconcile_worktrees(&reads());
        open_in_feature(&mut state);

        for directory in ["/other", "/feature"] {
            state.confirm_directory(root_pane(&state, 0), directory.into());
            state.reconcile_worktrees(&reads());

            assert_eq!(worktrees(&state), [Some("/repo"), Some("/feature")]);
            assert_eq!(name(&state), "repo");
        }
    }

    #[test]
    fn an_only_tab_takes_its_workspace_out_of_into_and_between_repositories() {
        let mut state = workspace_in("/repo");
        state.reconcile_worktrees(&reads());
        assert_eq!(
            (worktrees(&state), name(&state)),
            (vec![Some("/repo")], "repo")
        );

        for (directory, worktree, named) in [
            ("/tmp", None, "tmp"),
            ("/other", Some("/other"), "other"),
            ("/feature", Some("/feature"), "repo"),
        ] {
            state.confirm_directory(root_pane(&state, 0), directory.into());
            state.reconcile_worktrees(&reads());

            assert_eq!((worktrees(&state), name(&state)), (vec![worktree], named));
        }
    }

    #[test]
    fn a_tab_left_alone_outside_its_worktree_stays_until_its_next_cd() {
        let mut state = workspace_in("/repo");
        state.reconcile_worktrees(&reads());
        let feature = open_in_feature(&mut state);
        state.confirm_directory(root_pane(&state, 0), "/tmp".into());
        state.reconcile_worktrees(&reads());

        let closing = state.inventory.workspaces[0]
            .tabs
            .iter()
            .find(|tab| tab.id == feature)
            .unwrap()
            .root_pane;
        state.remove_pane(closing);
        state.reconcile_worktrees(&reads());
        assert_eq!(
            (worktrees(&state), name(&state)),
            (vec![Some("/repo")], "repo")
        );

        state.confirm_directory(root_pane(&state, 0), "/var".into());
        state.reconcile_worktrees(&reads());
        assert_eq!((worktrees(&state), name(&state)), (vec![None], "var"));
    }

    #[test]
    fn closing_the_root_tab_keeps_the_promoted_tab_in_its_worktree() {
        let mut state = workspace_in("/repo");
        state.reconcile_worktrees(&reads());
        open_in_feature(&mut state);
        state.confirm_directory(root_pane(&state, 1), "/tmp".into());
        state.reconcile_worktrees(&reads());

        state.remove_pane(root_pane(&state, 0));
        state.reconcile_worktrees(&reads());

        assert_eq!(
            (worktrees(&state), name(&state)),
            (vec![Some("/feature")], "repo")
        );
    }

    #[test]
    fn a_workspace_in_no_repository_enters_the_one_its_root_tab_enters_with_every_tab() {
        let mut state = workspace_in("/tmp");
        let workspace = state.inventory.workspaces[0].id;
        state
            .apply(
                DEVICE,
                2,
                Mutation::CreateTab {
                    workspace,
                    source: None,
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();
        state.confirm_directory(root_pane(&state, 1), "/feature".into());
        state.reconcile_worktrees(&reads());
        assert_eq!((worktrees(&state), name(&state)), (vec![None, None], "tmp"));

        state.confirm_directory(root_pane(&state, 0), "/repo".into());
        state.reconcile_worktrees(&reads());

        assert_eq!(worktrees(&state), [Some("/repo"), Some("/feature")]);
        assert_eq!(name(&state), "repo");
    }

    #[test]
    fn a_new_tab_opens_in_its_source_tabs_worktree() {
        let mut state = workspace_in("/repo");
        state.reconcile_worktrees(&reads());
        open_in_feature(&mut state);
        let workspace = state.inventory.workspaces[0].id;
        let source = root_pane(&state, 1);

        state
            .apply(
                DEVICE,
                2,
                Mutation::CreateTab {
                    workspace,
                    source: Some(source),
                    control: CreateControl::Creator,
                },
                "/home/test".as_ref(),
                |_| true,
            )
            .unwrap();

        assert_eq!(
            worktrees(&state),
            [Some("/repo"), Some("/feature"), Some("/feature")]
        );
    }

    #[test]
    fn changing_the_pin_moves_every_tab_into_the_pins_repository() {
        let mut state = workspace_in("/repo");
        pin(&mut state, Some("/repo"));
        state.reconcile_worktrees(&reads());
        open_in_feature(&mut state);
        state.confirm_directory(root_pane(&state, 0), "/tmp".into());
        state.reconcile_worktrees(&reads());
        assert_eq!(worktrees(&state), [Some("/repo"), Some("/feature")]);

        pin(&mut state, Some("/other"));
        state.reconcile_worktrees(&reads());
        assert_eq!(worktrees(&state), [Some("/other"), Some("/other")]);
        assert_eq!(name(&state), "other");

        pin(&mut state, Some("/var"));
        state.reconcile_worktrees(&reads());
        assert_eq!((worktrees(&state), name(&state)), (vec![None, None], "var"));
    }

    #[test]
    fn unpinning_moves_nothing_until_the_only_tabs_next_cd() {
        let mut state = workspace_in("/repo");
        pin(&mut state, Some("/repo"));
        state.reconcile_worktrees(&reads());
        state.confirm_directory(root_pane(&state, 0), "/tmp".into());
        state.reconcile_worktrees(&reads());

        pin(&mut state, None);
        state.reconcile_worktrees(&reads());
        assert_eq!(
            (worktrees(&state), name(&state)),
            (vec![Some("/repo")], "repo")
        );

        state.confirm_directory(root_pane(&state, 0), "/var".into());
        state.reconcile_worktrees(&reads());
        assert_eq!((worktrees(&state), name(&state)), (vec![None], "var"));
    }

    #[test]
    fn a_workspace_in_a_repository_is_named_for_its_main_worktree() {
        let mut state = workspace_in("/repo/src");
        state.reconcile_worktrees(&reads());
        assert_eq!(name(&state), "repo");

        let mut state = workspace_in("/feature");
        state.reconcile_worktrees(&reads());
        assert_eq!(name(&state), "repo");
    }

    /// A Core before this rule let the Root Tab leave while other Tabs stayed, and let a Tab
    /// join a repository its Workspace was not in.
    #[test]
    fn tabs_saved_outside_their_workspaces_repository_join_it() {
        let mut state = workspace_in("/tmp");
        open_in_feature(&mut state);
        let workspace = state.inventory.workspaces[0].id;
        state
            .add_worktree_tab(
                None,
                workspace,
                "/other".into(),
                "/other".into(),
                "/other".into(),
            )
            .unwrap();

        state.reconcile_worktrees(&reads());

        assert_eq!(
            worktrees(&state),
            [Some("/feature"), Some("/feature"), Some("/feature")]
        );
        assert_eq!(name(&state), "repo");
    }

    #[test]
    fn a_failed_or_pending_read_moves_no_tab() {
        let mut state = workspace_in("/repo");
        state.reconcile_worktrees(&reads());
        let mut failed = reads();
        failed.directories.push(DirectoryFacts {
            failure: Some(RepositoryReadError::Unavailable),
            ..outside("/slow")
        });

        for directory in ["/slow", "/never-read"] {
            state.confirm_directory(root_pane(&state, 0), directory.into());
            state.reconcile_worktrees(&failed);

            assert_eq!(worktrees(&state), [Some("/repo")]);
        }
    }
}
