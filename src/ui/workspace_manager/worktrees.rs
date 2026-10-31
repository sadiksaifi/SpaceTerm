//! Each Workspace's Worktrees in the sidebar. The owning server lists a Workspace's Worktrees.

use std::path::PathBuf;

use gpui::{App, Context, Entity, SharedString, Window};

use super::WorkspaceManager;
use crate::domain::{WorkspaceId, WorktreeId};
use crate::ui::workspace_sidebar::WorktreeSection;
use crate::ui::worktree_form::WorktreeForm;

mod cycle;
pub(super) mod fallback;
mod removal;
mod server;

#[derive(Default)]
pub(super) struct SidebarWorktrees {
    form: Option<Entity<WorktreeForm>>,
    close_fallbacks: std::collections::HashMap<WorkspaceId, spaceterm_server::hierarchy::Mutation>,
    created_shared: Option<(WorkspaceId, PathBuf, u64)>,
    /// The removal being checked, confirmed, or run. One runs at a time.
    removal: Option<u64>,
    removal_generation: u64,
    server_review: Option<server::RemovalReview>,
}

impl WorkspaceManager {
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

    /// Whether a Workspace's owning server can create Worktrees in it. That server runs Git on its
    /// own computer, whichever route reaches it.
    pub(super) fn creates_worktrees(&self, workspace_id: WorkspaceId, cx: &App) -> bool {
        self.owner_writes_worktrees(workspace_id, cx)
            && self.shared_inventory(workspace_id).is_some()
    }

    /// Whether this client has a writer for the server that owns a Workspace, which takes its
    /// Worktree writes. Reading it reaches no computer.
    pub(super) fn owner_writes_worktrees(&self, workspace_id: WorkspaceId, cx: &App) -> bool {
        use crate::ui::server_worktrees::Client;

        let Some(origin) = self
            .shared
            .owner(workspace_id)
            .and_then(|key| self.shared.origin(key))
        else {
            return false;
        };
        match origin {
            super::ServerOrigin::Local => cx.has_global::<Client>(),
            super::ServerOrigin::Direct(direct) => Client::direct(&direct.route, cx).is_some(),
        }
    }

    /// The edit writer of the server that owns a Workspace. Every Worktree read and write for
    /// that Workspace goes through it, so Git runs on the owning server's computer and never on
    /// this one's unless that server is this computer's.
    pub(super) fn owning_worktree_client(
        &self,
        workspace_id: WorkspaceId,
        cx: &mut App,
    ) -> Option<crate::ui::server_worktrees::Client> {
        self.owning_client(workspace_id, cx)
    }

    /// The name of the other computer whose server owns a Workspace, or `None` for this
    /// computer's.
    pub(super) fn owning_machine(&self, workspace_id: WorkspaceId) -> Option<String> {
        self.shared
            .owner(workspace_id)
            .and_then(|key| self.shared.origin(key))
            .and_then(super::ServerOrigin::remote_name)
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
