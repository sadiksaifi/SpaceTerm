//! Shared organization plans, so the desktop and the CLI create and close work identically.
use crate::hierarchy::{Axis, CreateControl, Id, Inventory, Mutation};
use crate::{repository, worktree};
use std::path::Path;

/// The mutation that opens a new Tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewTab {
    Mutation(Mutation),
    Worktree(worktree::Operation),
}

/// How closing every Tab of a Worktree keeps its Workspace open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosePlan {
    /// Opens the fallback Tab first, when the Workspace would otherwise lose its last Tab.
    pub fallback: Option<worktree::Operation>,
    /// The Tabs to close.
    pub tabs: Vec<Id>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OrganizeError {
    #[error("The target is not in the inventory.")]
    Missing,
    #[error("No other Worktree can hold the fallback Tab.")]
    NoFallbackWorktree,
}

/// The Workspace's repository facts, including Worktrees that have no Tabs.
pub fn workspace_worktrees<'a>(
    inv: &Inventory,
    repos: &'a repository::Snapshot,
    workspace: Id,
) -> Option<&'a repository::WorktreeInventory> {
    let workspace = inv.workspaces.iter().find(|w| w.id == workspace)?;
    let root_tab = workspace.tabs.iter().find(|t| t.id == workspace.root_tab)?;
    let root_pane = root_tab.panes.iter().find(|p| p.id == root_tab.root_pane)?;
    let directory = workspace
        .pinned_directory
        .as_deref()
        .unwrap_or_else(|| root_pane.repository_directory());
    repos
        .directories
        .iter()
        .find(|d| d.directory == directory)
        .and_then(|d| d.worktrees.as_ref())
        .or_else(|| {
            let root = root_tab.worktree_repository.as_ref()?;
            repos
                .directories
                .iter()
                .filter_map(|d| d.worktrees.as_ref())
                .find(|w| &w.main_root == root)
        })
}

pub fn new_tab(
    inv: &Inventory,
    workspace: Id,
    source: Option<Id>,
    worktree: Option<&Path>,
    control: CreateControl,
) -> Result<NewTab, OrganizeError> {
    let w = inv
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or(OrganizeError::Missing)?;
    let source_tab = source.and_then(|source| {
        w.tabs
            .iter()
            .find(|t| t.panes.iter().any(|p| p.id == source))
    });
    let tab = source_tab
        .or_else(|| w.tabs.iter().find(|t| t.id == w.root_tab))
        .ok_or(OrganizeError::Missing)?;
    let source = source
        .filter(|_| source_tab.is_some())
        .unwrap_or(tab.root_pane);
    if !tab.panes.iter().any(|p| p.id == source) {
        return Err(OrganizeError::Missing);
    }
    let path = worktree.or(tab.worktree.as_deref());
    if let Some(path) = path {
        let root = if worktree.is_none() || tab.worktree.as_deref() == Some(path) {
            tab.worktree_repository.clone()
        } else {
            None
        }
        .or_else(|| {
            let repos = inv.repositories.as_ref()?;
            let trees = workspace_worktrees(inv, repos, workspace)?;
            trees
                .worktrees
                .iter()
                .any(|t| t.root == path)
                .then(|| trees.main_root.clone())
        })
        .ok_or(OrganizeError::Missing)?;
        return Ok(NewTab::Worktree(worktree::Operation::OpenTab {
            workspace,
            source: Some(source),
            root,
            path: path.to_path_buf(),
            control,
        }));
    }
    Ok(NewTab::Mutation(Mutation::CreateTab {
        workspace,
        source: Some(source),
        control,
    }))
}

pub fn split(
    inv: &Inventory,
    tab: Id,
    source: Option<Id>,
    axis: Axis,
    control: CreateControl,
) -> Result<Mutation, OrganizeError> {
    let (workspace, tab) = inv
        .workspaces
        .iter()
        .find_map(|w| w.tabs.iter().find(|t| t.id == tab).map(|t| (w.id, t)))
        .ok_or(OrganizeError::Missing)?;
    let source = source
        .filter(|id| tab.panes.iter().any(|p| p.id == *id))
        .unwrap_or(tab.root_pane);
    if !tab.panes.iter().any(|p| p.id == source) {
        return Err(OrganizeError::Missing);
    }
    Ok(Mutation::SplitPane {
        workspace,
        tab: tab.id,
        source,
        axis,
        control,
    })
}

/// Main Worktree first, then Git's order, excluding missing and bare checkouts.
pub fn fallback_destination<'a>(
    inventory: &'a repository::WorktreeInventory,
    target: &Path,
) -> Option<&'a Path> {
    let usable = |record: &&repository::WorktreeRecord| {
        record.root != target && !record.missing && record.head != repository::WorktreeHead::Bare
    };
    inventory
        .worktrees
        .iter()
        .filter(usable)
        .find(|r| r.root == inventory.main_root)
        .or_else(|| inventory.worktrees.iter().find(usable))
        .map(|r| r.root.as_path())
}

pub fn worktree_close_plan(
    inv: &Inventory,
    repos: &repository::Snapshot,
    workspace: Id,
    anchor_tab: Id,
) -> Result<ClosePlan, OrganizeError> {
    let w = inv
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or(OrganizeError::Missing)?;
    let anchor = w
        .tabs
        .iter()
        .find(|t| t.id == anchor_tab)
        .ok_or(OrganizeError::Missing)?;
    let path = anchor.worktree.as_ref().ok_or(OrganizeError::Missing)?;
    let tabs: Vec<_> = w
        .tabs
        .iter()
        .filter(|t| {
            t.worktree == anchor.worktree && t.worktree_repository == anchor.worktree_repository
        })
        .map(|t| t.id)
        .collect();
    let fallback = if tabs.len() == w.tabs.len() {
        let trees = repos
            .directories
            .iter()
            .filter_map(|d| d.worktrees.as_ref())
            .find(|trees| Some(&trees.main_root) == anchor.worktree_repository.as_ref())
            .ok_or(OrganizeError::NoFallbackWorktree)?;
        let outside = fallback_destination(trees, path).ok_or(OrganizeError::NoFallbackWorktree)?;
        Some(worktree::Operation::OpenTab {
            workspace,
            source: None,
            root: trees.main_root.clone(),
            path: outside.to_path_buf(),
            control: CreateControl::Unclaimed,
        })
    } else {
        None
    };
    Ok(ClosePlan { fallback, tabs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::{Outcome, State};
    use crate::repository::{
        DirectoryFacts, GitToolStatus, PinCheckState, WorktreeHead, WorktreeInventory,
        WorktreeRecord,
    };

    fn state(pin: Option<&str>) -> State {
        let mut state = State::empty();
        state
            .apply(
                [1; 32],
                1,
                Mutation::CreateWorkspace {
                    name: "api".into(),
                    directory: pin.map(Into::into),
                    control: CreateControl::Creator,
                },
                Path::new("/home/account"),
                |_| true,
            )
            .unwrap();
        let pane = state.inventory.workspaces[0].tabs[0].root_pane;
        state.confirm_directory(pane, "/repo/source".into());
        state
    }

    fn trees() -> repository::Snapshot {
        repository::Snapshot {
            boot: [9; 32],
            revision: 1,
            git: GitToolStatus::Unknown,
            pinned_worktrees: Vec::new(),
            pin_check: PinCheckState::Ready,
            directories: vec![DirectoryFacts {
                directory: "/repo/source".into(),
                repository: None,
                failure: None,
                worktrees: Some(WorktreeInventory {
                    main_root: "/repo".into(),
                    current: None,
                    worktrees: vec![
                        WorktreeRecord {
                            root: "/repo".into(),
                            head: WorktreeHead::Branch("main".into()),
                            locked: false,
                            missing: false,
                        },
                        WorktreeRecord {
                            root: "/feature".into(),
                            head: WorktreeHead::Branch("feature/x".into()),
                            locked: false,
                            missing: false,
                        },
                    ],
                }),
            }],
        }
    }

    #[test]
    fn tab_and_split_creation_preserve_server_directory_policy_for_pinned_and_unpinned_workspaces()
    {
        for (pin, expected) in [(None, "/repo/source"), (Some("/pinned"), "/pinned")] {
            let mut state = state(pin);
            let workspace = state.inventory.workspaces[0].id;
            let NewTab::Mutation(mutation) = new_tab(
                &state.inventory,
                workspace,
                None,
                None,
                CreateControl::Unclaimed,
            )
            .unwrap() else {
                panic!("expected ordinary Tab")
            };
            let Outcome::Accepted { target } = state
                .apply([1; 32], 2, mutation, Path::new("/home/account"), |_| true)
                .unwrap()
            else {
                panic!("expected acceptance")
            };
            let tab = state.inventory.workspaces[0]
                .tabs
                .iter()
                .find(|t| t.id == target)
                .unwrap();
            assert_eq!(tab.panes[0].starting_directory, Path::new(expected));
            let mutation = split(
                &state.inventory,
                target,
                None,
                Axis::Down,
                CreateControl::Unclaimed,
            )
            .unwrap();
            let Outcome::Accepted { target: pane } = state
                .apply([1; 32], 3, mutation, Path::new("/home/account"), |_| true)
                .unwrap()
            else {
                panic!("expected acceptance")
            };
            assert_eq!(
                state.pane(pane).unwrap().2.starting_directory,
                Path::new(expected)
            );
        }
    }

    #[test]
    fn new_tab_inherits_source_membership_and_explicit_worktree_wins() {
        let mut state = state(None);
        let workspace = &mut state.inventory.workspaces[0];
        let source = workspace.tabs[0].root_pane;
        workspace.tabs[0].worktree = Some("/feature".into());
        workspace.tabs[0].worktree_repository = Some("/repo".into());
        let workspace = workspace.id;
        state.inventory.repositories = Some(trees());
        for (explicit, expected) in [(None, "/feature"), (Some(Path::new("/repo")), "/repo")] {
            assert_eq!(
                new_tab(
                    &state.inventory,
                    workspace,
                    Some(source),
                    explicit,
                    CreateControl::Creator
                )
                .unwrap(),
                NewTab::Worktree(worktree::Operation::OpenTab {
                    workspace,
                    source: Some(source),
                    root: "/repo".into(),
                    path: expected.into(),
                    control: CreateControl::Creator,
                })
            );
        }
    }

    #[test]
    fn unrelated_current_panes_use_stable_roots() {
        let state = state(None);
        let workspace = &state.inventory.workspaces[0];
        assert_eq!(
            new_tab(
                &state.inventory,
                workspace.id,
                Some([99; 32]),
                None,
                CreateControl::Unclaimed
            )
            .unwrap(),
            NewTab::Mutation(Mutation::CreateTab {
                workspace: workspace.id,
                source: Some(workspace.tabs[0].root_pane),
                control: CreateControl::Unclaimed,
            })
        );
        assert_eq!(
            split(
                &state.inventory,
                workspace.root_tab,
                Some([99; 32]),
                Axis::Right,
                CreateControl::Unclaimed
            )
            .unwrap(),
            Mutation::SplitPane {
                workspace: workspace.id,
                tab: workspace.root_tab,
                source: workspace.tabs[0].root_pane,
                axis: Axis::Right,
                control: CreateControl::Unclaimed,
            }
        );
    }

    #[test]
    fn close_all_requires_a_usable_fallback_before_emptying_the_workspace() {
        let mut state = state(None);
        let workspace = &mut state.inventory.workspaces[0];
        workspace.tabs[0].worktree = Some("/feature".into());
        workspace.tabs[0].worktree_repository = Some("/repo".into());
        let (workspace, anchor) = (workspace.id, workspace.root_tab);
        let mut repos = trees();
        let plan = worktree_close_plan(&state.inventory, &repos, workspace, anchor).unwrap();
        assert_eq!(plan.tabs, vec![anchor]);
        assert!(
            matches!(plan.fallback, Some(worktree::Operation::OpenTab { path, .. }) if path == Path::new("/repo"))
        );
        repos.directories[0].worktrees.as_mut().unwrap().worktrees[0].missing = true;
        assert_eq!(
            worktree_close_plan(&state.inventory, &repos, workspace, anchor),
            Err(OrganizeError::NoFallbackWorktree)
        );
        repos.directories[0].worktrees.as_mut().unwrap().worktrees[0].head = WorktreeHead::Bare;
        assert_eq!(
            worktree_close_plan(&state.inventory, &repos, workspace, anchor),
            Err(OrganizeError::NoFallbackWorktree)
        );
    }

    #[test]
    fn close_all_preserves_existing_tabs_outside_the_target_worktree() {
        let mut state = state(None);
        let workspace = &mut state.inventory.workspaces[0];
        workspace.tabs[0].worktree = Some("/feature".into());
        workspace.tabs[0].worktree_repository = Some("/repo".into());
        let mut outside = workspace.tabs[0].clone();
        outside.id = [2; 32];
        outside.worktree = Some("/repo".into());
        workspace.tabs.push(outside);
        let (workspace, anchor) = (workspace.id, workspace.root_tab);
        assert_eq!(
            worktree_close_plan(&state.inventory, &trees(), workspace, anchor).unwrap(),
            ClosePlan {
                fallback: None,
                tabs: vec![anchor]
            }
        );
    }
}
