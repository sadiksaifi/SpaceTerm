//! Each Workspace's Worktrees in the sidebar. The owning server lists a Workspace's Worktrees.

use std::path::PathBuf;

use gpui::{App, Context, Entity, SharedString, Window};

use super::WorkspaceManager;
use crate::domain::{WorkspaceId, WorktreeId};
use crate::ui::workspace_sidebar::WorktreeSection;
use crate::ui::worktree_form::WorktreeForm;

mod cycle;
mod removal;
mod server;

#[derive(Default)]
pub(super) struct SidebarWorktrees {
    form: Option<Entity<WorktreeForm>>,
    created_shared: Option<(WorkspaceId, PathBuf, u64)>,
    /// The removal being checked, confirmed, or run. One runs at a time.
    removal: Option<u64>,
    removal_generation: u64,
    server_review: Option<server::RemovalReview>,
}

impl WorkspaceManager {
    pub(super) fn worktree_creation_failure_presented(
        &self,
        path: &std::path::Path,
        error: spaceterm_server::worktree::Error,
        cx: &App,
    ) -> bool {
        self.sidebar_worktrees.form.as_ref().is_some_and(|form| {
            let form = form.read(cx);
            form.is_open()
                || (error == spaceterm_server::worktree::Error::Interrupted
                    && form.interrupted_path() == Some(path))
        })
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
        self.open_shared_worktree(workspace_id, worktree_id, focus, window, cx)
            .unwrap_or(false)
    }

    /// Opens another Tab in a Worktree, or its first Tab when it has none.
    pub(super) fn new_worktree_tab(
        &mut self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // The owning server opens the first Tab.
        self.is_shared_workspace(workspace_id)
            && self.open_server_worktree_tab(workspace_id, worktree_id, window, cx)
    }

    /// Whether a Workspace's owning server can create Worktrees in it.
    pub(super) fn creates_worktrees(&self, workspace_id: WorkspaceId, cx: &App) -> bool {
        self.is_shared_workspace(workspace_id)
            && crate::ui::server_worktrees::available(cx)
            && self.shared_inventory(workspace_id).is_some()
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
        self.is_shared_workspace(workspace_id) && self.new_shared_worktree(workspace_id, window, cx)
    }

    pub(super) fn set_worktrees_expanded(&mut self, workspace_id: WorkspaceId, expanded: bool) {
        self.set_shared_worktrees_expanded(workspace_id, expanded);
    }

    /// The Worktrees a Workspace row discloses, or `None` for a Workspace outside a repository.
    pub(super) fn worktree_section(
        &self,
        workspace_id: WorkspaceId,
        cx: &App,
    ) -> Option<WorktreeSection> {
        self.shared_worktree_section(workspace_id, cx)
    }
}

/// A directory's last component, or the whole path for a root.
pub(super) fn directory_name(directory: &std::path::Path) -> SharedString {
    directory
        .file_name()
        .map_or_else(
            || directory.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
        .into()
}
