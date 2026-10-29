//! Work commands share one snapshot, organization policy and journal dispatch boundary.
pub mod close;
pub mod pane;
pub mod paths;
pub mod tab;
pub mod workspace;
pub mod worktree;

use crate::contract::{Code, Source};
use crate::scope::{Current, Resolved, Snapshot, TargetError};
use spaceterm_server::hierarchy::{CreateControl, Id, Mutation};

pub fn create_control(
    agent: bool,
    current: Option<&Current>,
    snapshot: &Snapshot,
) -> CreateControl {
    if agent {
        return CreateControl::Creator;
    }
    match current.filter(|c| {
        c.server == snapshot.server
            && snapshot
                .inventory
                .workspaces
                .iter()
                .flat_map(|w| &w.tabs)
                .flat_map(|t| &t.panes)
                .any(|p| p.id == c.pane)
    }) {
        Some(current) => CreateControl::OriginController { pane: current.pane },
        None => CreateControl::Unclaimed,
    }
}

pub(crate) fn hex(id: &Id) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug)]
pub enum WorkError {
    Target(TargetError),
    Code(Code),
}

impl From<TargetError> for WorkError {
    fn from(error: TargetError) -> Self {
        Self::Target(error)
    }
}

pub struct Planned {
    pub target: Resolved,
    pub mutation: Mutation,
}

pub(crate) fn organize_error(error: spaceterm_server::organize::OrganizeError) -> Code {
    match error {
        spaceterm_server::organize::OrganizeError::Missing => Code::NotFound,
        spaceterm_server::organize::OrganizeError::NoFallbackWorktree => Code::NoFallbackWorktree,
    }
}

pub(crate) fn root_source(snapshot: &Snapshot, target: &mut Resolved, pane: Id) {
    if target.pane != Some(pane) {
        target.pane = Some(pane);
        target.sources.pane = Some(Source::Root);
    }
    if let Some(tab) = snapshot
        .inventory
        .workspaces
        .iter()
        .flat_map(|w| &w.tabs)
        .find(|t| t.panes.iter().any(|p| p.id == pane))
    {
        if target.tab.is_none() {
            target.tab = Some(tab.id);
            target.sources.tab = Some(Source::Root);
        }
        if target.worktree.is_none() && tab.worktree.is_some() {
            target.worktree.clone_from(&tab.worktree);
            target.sources.worktree = Some(Source::Root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{
        ScopeArgs, Selector,
        fixtures::{fixture, id},
    };
    use spaceterm_server::{hierarchy::Axis, worktree::Operation};

    fn args(workspace: Option<&str>, worktree: Option<&str>, tab: Option<&str>) -> ScopeArgs {
        ScopeArgs {
            workspace: workspace.map(|s| Selector::parse(s).unwrap()),
            worktree: worktree.map(|s| Selector::parse(s).unwrap()),
            tab: tab.map(|s| Selector::parse(s).unwrap()),
            pane: None,
        }
    }

    #[test]
    fn tab_creation_in_the_current_workspace_keeps_its_worktree() {
        let snapshot = fixture();
        let current = Current {
            server: id(9),
            pane: id(22),
        };
        let plan = tab::create(
            &snapshot,
            &args(Some("api"), None, None),
            Some(&current),
            CreateControl::Creator,
        )
        .unwrap();
        assert!(
            matches!(plan.mutation, Mutation::Worktree(Operation::OpenTab { path, source, .. }) if path == std::path::Path::new("/trees/feature") && source == Some(id(22)))
        );
        let split = pane::split(
            &snapshot,
            &args(None, None, Some("id:0b0b0b")),
            None,
            Axis::Down,
            CreateControl::Unclaimed,
        )
        .unwrap();
        assert!(
            matches!(split.mutation, Mutation::SplitPane { source, axis: Axis::Down, .. } if source == id(21))
        );
        assert_eq!(split.target.sources.pane, Some(Source::Root));
    }

    #[test]
    fn all_closes_keep_an_existing_outside_tab_or_require_a_fallback() {
        let mut snapshot = fixture();
        let scope = args(Some("api"), Some("feature/x"), None);
        assert!(
            tab::close(&snapshot, &scope, None, true)
                .unwrap()
                .fallback
                .is_none()
        );
        snapshot.inventory.workspaces[0].tabs.remove(0);
        snapshot.inventory.workspaces[0].root_tab = id(12);
        assert!(
            matches!(tab::close(&snapshot, &scope, None, true).unwrap().fallback, Some(Mutation::Worktree(Operation::OpenTab { path, .. })) if path == std::path::Path::new("/repo"))
        );
        snapshot.repositories.as_mut().unwrap().directories[0]
            .worktrees
            .as_mut()
            .unwrap()
            .worktrees[0]
            .missing = true;
        snapshot.repositories.as_mut().unwrap().directories[0]
            .worktrees
            .as_mut()
            .unwrap()
            .worktrees[2]
            .missing = true;
        assert!(matches!(
            tab::close(&snapshot, &scope, None, true),
            Err(WorkError::Code(Code::NoFallbackWorktree))
        ));
    }

    #[test]
    fn versioned_renames_use_the_snapshot_name_version() {
        let snapshot = fixture();
        let plan = workspace::rename(
            &snapshot,
            &args(Some("api"), None, None),
            None,
            "api-v2".into(),
        )
        .unwrap();
        assert!(
            matches!(plan.mutation, Mutation::RenameWorkspace { workspace, version: 2, name } if workspace == id(1) && name == "api-v2")
        );
    }

    #[test]
    fn creation_control_uses_agents_then_valid_current_context_then_unclaimed() {
        let snapshot = fixture();
        let current = Current {
            server: id(9),
            pane: id(22),
        };
        assert_eq!(
            create_control(true, Some(&current), &snapshot),
            CreateControl::Creator
        );
        assert_eq!(
            create_control(false, Some(&current), &snapshot),
            CreateControl::OriginController { pane: id(22) }
        );
        assert_eq!(
            create_control(false, None, &snapshot),
            CreateControl::Unclaimed
        );
        assert_eq!(
            create_control(
                false,
                Some(&Current {
                    server: id(8),
                    pane: id(22)
                }),
                &snapshot
            ),
            CreateControl::Unclaimed
        );
    }
}
