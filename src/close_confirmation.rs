use crate::domain::{PaneId, TabId, WorkspaceId, WorktreeId};
use crate::terminal::PaneTerminalState;
use crate::terminal::metadata::TerminalMetadataSnapshot;

use crate::terminal::metadata::{CommandState, MetadataFreshness, PromptZone};

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
    Worktree {
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
    },
    KillWorkspace(WorkspaceId),
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
    /// Whether a program other than the shell owns the foreground of this exact Terminal
    /// Session, as SpaceTerm Core last read its Line Mode. `None` when no reading describes it.
    pub(crate) foreground_job: Option<bool>,
}

impl PaneCloseFacts<'_> {
    /// Whether current facts about this exact Terminal Session verify a running command: a
    /// program other than the shell owns its foreground, or live shell integration says a
    /// command's output is running. A Terminal Session alone, missing or stale prompt metadata,
    /// and a shell that has not drawn its first prompt verify nothing, so they close directly.
    pub(crate) fn running_command(&self) -> bool {
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
        if self.foreground_job == Some(true) {
            return true;
        }
        self.metadata.freshness == MetadataFreshness::Live
            && self.metadata.prompt_zone == PromptZone::CommandOutput
            && !matches!(
                self.metadata.command.as_ref().map(|command| &command.state),
                Some(CommandState::Finished { .. })
            )
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

    /// Ends the pending confirmation for `target`, returning whether a person approved it. A
    /// SpaceTerm Server owns a shared target, so the server rather than the local hierarchy
    /// decides whether what was approved still exists.
    pub(crate) fn settle_approval(
        &mut self,
        generation: u64,
        target: CloseTarget,
        confirmed: bool,
    ) -> bool {
        if self.pending != Some(PendingCloseConfirmation { generation, target }) {
            return false;
        }
        self.pending = None;
        confirmed
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
        closed_pane: T,
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

#[derive(Clone, Copy)]
pub(crate) enum HierarchyClose {
    Pane,
    Tab,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseContinuation {
    Remove,
    Parent,
}

impl HierarchyClose {
    /// Closing a final Pane or Tab closes its parent. A final Workspace leaves the empty view.
    pub(crate) const fn resolve(self, siblings: usize) -> CloseContinuation {
        if siblings > 1 {
            return CloseContinuation::Remove;
        }
        match self {
            Self::Pane | Self::Tab => CloseContinuation::Parent,
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
    fn only_current_facts_about_the_session_verify_a_running_command() {
        let screen =
            crate::terminal::ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix);
        let running = PaneTerminalState::Running;
        let exited = PaneTerminalState::exited(crate::terminal::TerminalSessionExit::Success);
        let failed = PaneTerminalState::failed(crate::terminal::TerminalFailure::pty("read"), None);
        use MetadataFreshness::{Live, Stale};
        use PromptZone::{CommandInput, CommandOutput, Prompt, Unknown};
        for (live, disconnected, state, freshness, zone, foreground_job, expected) in [
            // A fresh shell that has drawn no prompt yet runs nothing.
            (true, false, &running, Live, Unknown, None, false),
            (true, false, &running, Live, Unknown, Some(false), false),
            // A shell without integration still says when another program owns the terminal.
            (true, false, &running, Live, Unknown, Some(true), true),
            (true, false, &running, Live, CommandOutput, None, true),
            (true, false, &running, Live, CommandOutput, Some(true), true),
            (true, false, &running, Live, Prompt, None, false),
            (true, false, &running, Live, CommandInput, None, false),
            // Stale metadata verifies nothing; the shell's foreground still does.
            (true, false, &running, Stale, CommandOutput, None, false),
            (true, false, &running, Stale, Prompt, Some(true), true),
            (false, false, &running, Stale, Unknown, Some(true), false),
            (true, true, &running, Stale, Unknown, Some(true), false),
            (true, false, &exited, Live, CommandOutput, Some(true), false),
            (true, false, &failed, Live, CommandOutput, Some(true), false),
        ] {
            let mut metadata = (*screen.metadata).clone();
            metadata.freshness = freshness;
            metadata.prompt_zone = zone;
            let facts = PaneCloseFacts {
                live_session: live,
                state,
                disconnected,
                metadata: &metadata,
                foreground_job,
            };
            assert_eq!(
                facts.running_command(),
                expected,
                "{live} {disconnected} {freshness:?} {zone:?} {foreground_job:?}"
            );
        }
    }

    #[test]
    fn a_finished_command_runs_nothing() {
        let screen =
            crate::terminal::ScreenSnapshot::empty(crate::local_path::LocalPathSemantics::Posix);
        let mut metadata = (*screen.metadata).clone();
        metadata.prompt_zone = PromptZone::CommandOutput;
        metadata.command = Some(crate::terminal::metadata::CommandMetadata {
            line: "cargo test".into(),
            state: CommandState::Finished {
                exit_status: Some(0),
                duration: std::time::Duration::from_secs(1),
            },
        });
        for freshness in [MetadataFreshness::Live, MetadataFreshness::Stale] {
            metadata.freshness = freshness;
            let facts = PaneCloseFacts {
                live_session: true,
                state: &PaneTerminalState::Running,
                disconnected: false,
                metadata: &metadata,
                foreground_job: None,
            };
            assert!(!facts.running_command());
        }
    }

    #[test]
    fn close_confirmation_settles_only_its_own_pending_target_once() {
        let mut confirmation = CloseConfirmation::default();
        let first = confirmation.begin(target(1)).unwrap();
        assert!(confirmation.begin(target(2)).is_none());
        assert!(!confirmation.settle_approval(first, target(2), true));
        assert!(confirmation.pending().is_some());
        assert!(!confirmation.settle_approval(first, target(1), false));
        assert!(confirmation.pending().is_none());
        let second = confirmation.begin(target(1)).unwrap();
        confirmation.presentation_failed(first);
        assert!(confirmation.pending().is_some());
        assert!(confirmation.settle_approval(second, target(1), true));
        assert!(!confirmation.settle_approval(second, target(1), true));
    }

    #[test]
    fn a_final_pane_or_tab_escalates_to_its_parent() {
        for (scope, expected) in [
            (HierarchyClose::Pane, CloseContinuation::Parent),
            (HierarchyClose::Tab, CloseContinuation::Parent),
        ] {
            assert_eq!(scope.resolve(1), expected);
            assert_eq!(scope.resolve(2), CloseContinuation::Remove);
        }
    }
}
