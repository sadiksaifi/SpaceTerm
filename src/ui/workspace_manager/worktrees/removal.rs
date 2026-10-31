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
            .worktree_root(workspace_id, worktree_id)
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

    /// Asks the owning server to remove a Worktree, after it checks for changes removing it would
    /// discard.
    pub(in crate::ui::workspace_manager) fn request_worktree_removal(
        &mut self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_workspace(workspace_id) {
            self.request_server_worktree_removal(workspace_id, worktree_id, window, cx);
        }
    }
}
