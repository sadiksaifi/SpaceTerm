//! Next Worktree and Previous Worktree: stepping through the Active Workspace's Worktrees that
//! have Tabs, in sidebar order. Tabs outside every Worktree come first, as the Workspace row that
//! stands for them precedes its Worktree rows. A Worktree without Tabs is skipped, so stepping
//! never starts a Terminal Session.

use gpui::{App, Context, Window};

use super::super::WorkspaceManager;
use crate::domain::WorktreeId;

impl WorkspaceManager {
    /// Whether the Active Workspace has more than one Worktree with Tabs, or Tabs outside every
    /// Worktree and a Worktree with Tabs, to step between.
    pub(in crate::ui::workspace_manager) fn steps_worktrees(&self, cx: &App) -> bool {
        let manager = self.workspaces.active_workspace().payload().read(cx);
        manager.worktree_tab_counts().len() + usize::from(manager.has_unscoped_tabs()) > 1
    }

    /// Shows the next or previous Worktree with Tabs, wrapping at either end, and focuses its
    /// most recent Tab.
    pub(in crate::ui::workspace_manager) fn step_worktree(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace_id = self.workspaces.active_workspace_id();
        let Some(section) = self.worktree_section(workspace_id, cx) else {
            return;
        };
        let manager = self.workspaces.active_workspace().payload().read(cx);
        let unscoped = manager.has_unscoped_tabs();
        let active = manager.active_worktree();
        // `None` stands for the Tabs outside every Worktree.
        let open: Vec<Option<WorktreeId>> = unscoped
            .then_some(None)
            .into_iter()
            .chain(
                section
                    .rows()
                    .filter(|row| row.has_tabs)
                    .map(|row| Some(row.worktree_id)),
            )
            .collect();
        let position = open.iter().position(|id| *id == active);
        let Some(target) = step(open.len(), position, forward).map(|index| open[index]) else {
            return;
        };
        let shown = match target {
            Some(worktree_id) => self.open_worktree(workspace_id, worktree_id, true, window, cx),
            None => self.show_unscoped_tabs(workspace_id, true, window, cx),
        };
        if shown {
            self.focus(window, cx);
        }
        cx.notify();
    }
}

/// The index `forward` or backward from `active` among `count` entries, wrapping at either end.
/// Without an active entry, forward starts at the first and backward at the last.
fn step(count: usize, active: Option<usize>, forward: bool) -> Option<usize> {
    if count < 2 {
        return None;
    }
    Some(match (active, forward) {
        (Some(index), true) => (index + 1) % count,
        (Some(index), false) => (index + count - 1) % count,
        (None, true) => 0,
        (None, false) => count - 1,
    })
}

#[cfg(test)]
mod tests {
    use super::step;

    #[test]
    fn stepping_should_wrap_at_either_end_and_need_two_entries() {
        assert_eq!(step(3, Some(0), true), Some(1));
        assert_eq!(step(3, Some(2), true), Some(0));
        assert_eq!(step(3, Some(0), false), Some(2));
        assert_eq!(step(3, None, true), Some(0));
        assert_eq!(step(3, None, false), Some(2));
        assert_eq!(step(1, Some(0), true), None);
        assert_eq!(step(0, None, false), None);
    }
}
