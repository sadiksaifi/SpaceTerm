use crate::domain::{PaneId, TabId, WorkspaceId};
use crate::terminal::PaneTerminalState;
use crate::terminal::metadata::TerminalMetadataSnapshot;

use crate::terminal::metadata::{CommandState, MetadataFreshness, PromptZone};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseScope {
    Pane,
    Tab,
    Workspace,
    Window,
    Application,
}

impl CloseScope {
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Pane => "Close Pane?",
            Self::Tab => "Close Tab?",
            Self::Workspace => "Close Workspace?",
            Self::Window => "Close Window?",
            Self::Application => "Quit SpaceTerm?",
        }
    }

    pub(crate) const fn destructive_label(self) -> &'static str {
        match self {
            Self::Pane => "Close Pane",
            Self::Tab => "Close Tab",
            Self::Workspace => "Close Workspace",
            Self::Window => "Close Window",
            Self::Application => "Quit SpaceTerm",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseTarget {
    Pane {
        workspace_id: WorkspaceId,
        tab_id: TabId,
        pane_id: PaneId,
    },
    Tab {
        workspace_id: WorkspaceId,
        tab_id: TabId,
    },
    Workspace(WorkspaceId),
    Window,
    Application,
}

impl CloseTarget {
    pub(crate) const fn scope(self) -> CloseScope {
        match self {
            Self::Pane { .. } => CloseScope::Pane,
            Self::Tab { .. } => CloseScope::Tab,
            Self::Workspace(_) => CloseScope::Workspace,
            Self::Window => CloseScope::Window,
            Self::Application => CloseScope::Application,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PendingCloseConfirmation {
    pub(crate) generation: u64,
    pub(crate) target: CloseTarget,
}

/// Content-free input facts captured without lifecycle interpretation by the Pane.
pub(crate) struct PaneCloseFacts<'a> {
    pub(crate) live_session: bool,
    pub(crate) state: &'a PaneTerminalState,
    pub(crate) disconnected: bool,
    pub(crate) metadata: &'a TerminalMetadataSnapshot,
}

impl PaneCloseFacts<'_> {
    fn requires_confirmation(&self) -> bool {
        if !self.live_session
            || matches!(self.state, PaneTerminalState::Exited(_))
            || self
                .state
                .failure()
                .is_some_and(|failure| failure.is_fatal())
            || self.disconnected
        {
            return false;
        }
        if self.metadata.freshness == MetadataFreshness::Stale {
            return true;
        }
        if matches!(
            self.metadata.command.as_ref().map(|command| &command.state),
            Some(CommandState::Finished { .. })
        ) {
            return false;
        }
        matches!(
            self.metadata.prompt_zone,
            PromptZone::Unknown | PromptZone::CommandOutput
        )
    }
}

/// An immutable inventory is the single scope-resolution surface for every close request.
#[derive(Default)]
pub(crate) struct CloseHierarchy {
    panes: Vec<(WorkspaceId, TabId, PaneId, bool)>,
}

impl CloseHierarchy {
    pub(crate) fn insert(
        &mut self,
        workspace: WorkspaceId,
        tab: TabId,
        pane: PaneId,
        facts: PaneCloseFacts<'_>,
    ) {
        self.panes
            .push((workspace, tab, pane, facts.requires_confirmation()));
    }

    pub(crate) fn requires_confirmation(&self, target: CloseTarget) -> Option<bool> {
        let mut found = matches!(target, CloseTarget::Window | CloseTarget::Application);
        let mut requires = false;
        for &(workspace, tab, pane, running) in &self.panes {
            let matches = match target {
                CloseTarget::Pane {
                    workspace_id,
                    tab_id,
                    pane_id,
                } => (workspace, tab, pane) == (workspace_id, tab_id, pane_id),
                CloseTarget::Tab {
                    workspace_id,
                    tab_id,
                } => (workspace, tab) == (workspace_id, tab_id),
                CloseTarget::Workspace(id) => workspace == id,
                CloseTarget::Window | CloseTarget::Application => true,
            };
            if matches {
                found = true;
                requires |= running;
            }
        }
        found.then_some(requires)
    }
}

#[derive(Default)]
pub(crate) struct CloseConfirmation {
    generation: u64,
    pending: Option<PendingCloseConfirmation>,
}

impl CloseConfirmation {
    pub(crate) const fn pending(&self) -> Option<PendingCloseConfirmation> {
        self.pending
    }

    pub(crate) fn begin(&mut self, target: CloseTarget) -> Option<u64> {
        if self.pending.is_some() {
            return None;
        }
        let generation = self.generation.checked_add(1)?;
        self.generation = generation;
        self.pending = Some(PendingCloseConfirmation { generation, target });
        Some(generation)
    }

    pub(crate) fn settle(
        &mut self,
        generation: u64,
        target: CloseTarget,
        confirmed: bool,
        hierarchy: &CloseHierarchy,
    ) -> Option<CloseTarget> {
        if self.pending != Some(PendingCloseConfirmation { generation, target }) {
            return None;
        }
        self.pending = None;
        (confirmed && hierarchy.requires_confirmation(target).is_some()).then_some(target)
    }

    pub(crate) fn presentation_failed(&mut self, generation: u64) {
        if self
            .pending
            .is_some_and(|pending| pending.generation == generation)
        {
            self.pending = None;
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ClosePaneOutcome<T> {
    PaneClosed {
        closed_pane_id: PaneId,
        focused_pane_id: PaneId,
        closed_terminal: T,
    },
    CloseTab {
        tab_id: TabId,
    },
}

pub(crate) enum CloseTabOutcome<T> {
    TabClosed {
        closed_tab_id: TabId,
        active_tab_id: TabId,
        payload: T,
    },
    CloseWorkspace {
        final_tab_id: TabId,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CloseWorkspaceOutcome<T> {
    WorkspaceClosed {
        closed_workspace_id: WorkspaceId,
        active_workspace_id: WorkspaceId,
        payload: T,
    },
    FinalWorkspaceReplaced {
        closed_workspace_id: WorkspaceId,
        replacement_workspace_id: WorkspaceId,
        payload: T,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum FinalTabCloseOutcome<T> {
    WorkspaceClosed {
        closed_workspace_id: WorkspaceId,
        active_workspace_id: WorkspaceId,
        payload: T,
    },
    CloseOperatingSystemWindow {
        workspace_id: WorkspaceId,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum HierarchyClose {
    Pane,
    Tab,
    Workspace,
    FinalTab,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseContinuation {
    Remove,
    Parent,
    Replace,
    Window,
}

impl HierarchyClose {
    pub(crate) const fn resolve(self, siblings: usize) -> CloseContinuation {
        if siblings > 1 {
            return CloseContinuation::Remove;
        }
        match self {
            Self::Pane | Self::Tab => CloseContinuation::Parent,
            Self::Workspace => CloseContinuation::Replace,
            Self::FinalTab => CloseContinuation::Window,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(pane: u64) -> CloseTarget {
        CloseTarget::Pane {
            workspace_id: WorkspaceId::new(1),
            tab_id: TabId::new(1),
            pane_id: PaneId::new(pane),
        }
    }

    #[test]
    fn close_confirmation_classifies_lifecycle_and_metadata_at_the_hierarchy_interface() {
        let screen =
            crate::terminal::ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix);
        let running = PaneTerminalState::Running;
        let exited = PaneTerminalState::exited(crate::terminal::SessionExit::Success);
        let failed = PaneTerminalState::failed(crate::terminal::TerminalFailure::pty("read"), None);
        for (live, disconnected, state, freshness, zone, expected) in [
            (
                true,
                false,
                &running,
                MetadataFreshness::Live,
                PromptZone::Unknown,
                true,
            ),
            (
                true,
                false,
                &running,
                MetadataFreshness::Live,
                PromptZone::CommandOutput,
                true,
            ),
            (
                true,
                false,
                &running,
                MetadataFreshness::Live,
                PromptZone::Prompt,
                false,
            ),
            (
                true,
                false,
                &running,
                MetadataFreshness::Live,
                PromptZone::CommandInput,
                false,
            ),
            (
                true,
                false,
                &running,
                MetadataFreshness::Stale,
                PromptZone::Prompt,
                true,
            ),
            (
                false,
                false,
                &running,
                MetadataFreshness::Stale,
                PromptZone::Unknown,
                false,
            ),
            (
                true,
                true,
                &running,
                MetadataFreshness::Stale,
                PromptZone::Unknown,
                false,
            ),
            (
                true,
                false,
                &exited,
                MetadataFreshness::Stale,
                PromptZone::Unknown,
                false,
            ),
            (
                true,
                false,
                &failed,
                MetadataFreshness::Stale,
                PromptZone::Unknown,
                false,
            ),
        ] {
            let mut metadata = (*screen.metadata).clone();
            metadata.freshness = freshness;
            metadata.prompt_zone = zone;
            let mut hierarchy = CloseHierarchy::default();
            hierarchy.insert(
                WorkspaceId::new(1),
                TabId::new(1),
                PaneId::new(1),
                PaneCloseFacts {
                    live_session: live,
                    state,
                    disconnected,
                    metadata: &metadata,
                },
            );
            assert_eq!(hierarchy.requires_confirmation(target(1)), Some(expected));
        }
    }

    #[test]
    fn close_confirmation_aggregates_exact_scopes_and_rejects_stale_settlement() {
        let mut hierarchy = CloseHierarchy {
            panes: vec![
                (WorkspaceId::new(1), TabId::new(1), PaneId::new(1), true),
                (WorkspaceId::new(1), TabId::new(1), PaneId::new(2), false),
            ],
        };
        for scope in [
            target(1),
            CloseTarget::Tab {
                workspace_id: WorkspaceId::new(1),
                tab_id: TabId::new(1),
            },
            CloseTarget::Workspace(WorkspaceId::new(1)),
            CloseTarget::Window,
            CloseTarget::Application,
        ] {
            assert_eq!(hierarchy.requires_confirmation(scope), Some(true));
        }
        assert_eq!(hierarchy.requires_confirmation(target(2)), Some(false));
        assert_eq!(hierarchy.requires_confirmation(target(3)), None);
        let mut confirmation = CloseConfirmation::default();
        let first = confirmation.begin(target(1)).unwrap();
        assert!(confirmation.begin(target(2)).is_none());
        assert!(
            confirmation
                .settle(first, target(2), true, &hierarchy)
                .is_none()
        );
        assert!(confirmation.pending().is_some());
        assert!(
            confirmation
                .settle(first, target(1), false, &hierarchy)
                .is_none()
        );
        let second = confirmation.begin(target(1)).unwrap();
        confirmation.presentation_failed(first);
        assert!(confirmation.pending().is_some());
        hierarchy.panes.remove(0);
        assert!(
            confirmation
                .settle(second, target(1), true, &hierarchy)
                .is_none()
        );
        assert!(confirmation.pending().is_none());
        let third = confirmation.begin(target(2)).unwrap();
        assert_eq!(
            confirmation.settle(third, target(2), true, &hierarchy),
            Some(target(2))
        );
        assert!(
            confirmation
                .settle(third, target(2), true, &hierarchy)
                .is_none()
        );
    }

    #[test]
    fn final_child_escalation_and_explicit_workspace_replacement_are_distinct() {
        for (scope, expected) in [
            (HierarchyClose::Pane, CloseContinuation::Parent),
            (HierarchyClose::Tab, CloseContinuation::Parent),
            (HierarchyClose::Workspace, CloseContinuation::Replace),
            (HierarchyClose::FinalTab, CloseContinuation::Window),
        ] {
            assert_eq!(scope.resolve(1), expected);
            assert_eq!(scope.resolve(2), CloseContinuation::Remove);
        }
    }
}
