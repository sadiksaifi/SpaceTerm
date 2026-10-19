//! Copying, closing, and removing one Worktree from its sidebar row. The owning server removes a
//! Worktree.

use gpui::{App, Context, Window};

use super::super::WorkspaceManager;
use crate::domain::{WorkspaceId, WorktreeId};
use crate::terminal::native_services::clipboard::TextClipboardTarget;

impl WorkspaceManager {
    /// Copies a Worktree's root as text.
    pub(in crate::ui::workspace_manager) fn copy_worktree_path(
        &self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        cx: &mut App,
    ) {
        // A Worktree's root is the server's path, copied as text and nothing more.
        let Some(root) = self
            .shared_worktree_root(workspace_id, worktree_id)
            .map(|root| root.display().to_string())
        else {
            return;
        };
        if let Err(error) =
            self.pane_construction
                .text_clipboard()
                .write(TextClipboardTarget::Clipboard, &root, cx)
        {
            eprintln!("failed to copy the Worktree path: {error:?}");
        }
    }

    /// Closes every Tab of a Worktree and keeps its row. A Workspace keeps a Tab outside the
    /// Worktree, so when the Worktree holds them all, no Tab closes.
    pub(in crate::ui::workspace_manager) fn close_worktree_tabs(
        &mut self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
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
        let (tab_ids, outside) = {
            let manager = manager.read(cx);
            let tab_ids = manager.worktree_tab_ids(worktree_id);
            let outside = manager.has_tabs_besides(&tab_ids);
            (tab_ids, outside)
        };
        if tab_ids.is_empty() {
            return true;
        }
        if !outside {
            return false;
        }
        manager.update(cx, |manager, cx| {
            for tab_id in tab_ids {
                manager.close_tab_authorized(tab_id, window, cx);
            }
        });
        if self.workspaces.active_workspace_id() == Some(workspace_id) {
            self.focus(window, cx);
        }
        cx.notify();
        true
    }

    /// Asks the owning server to remove a Worktree, after it checks for changes removing it would
    /// discard.
    pub(in crate::ui::workspace_manager) fn request_worktree_removal(
        &mut self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_shared_workspace(workspace_id) {
            self.request_server_worktree_removal(workspace_id, worktree_id, window, cx);
        }
    }
}
