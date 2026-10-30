//! Establish server-owned membership before reviewing a close that would empty a Workspace.

use spaceterm_server::hierarchy::{CloseTarget as ServerCloseTarget, Mutation, Outcome};
use spaceterm_server::worktree::Operation;
use spaceterm_ui::{Alert, AlertIntent, ModalAction, ModalActionRole, ModalId};

use super::*;
use crate::close_confirmation::CloseTarget;

impl WorkspaceManager {
    /// Fallback creation must not use the ordinary accepted-creation activation path.
    pub(in crate::ui::workspace_manager) fn is_worktree_close_fallback(
        &self,
        mutation: &Mutation,
    ) -> bool {
        self.sidebar_worktrees
            .close_fallbacks
            .values()
            .any(|pending| pending == mutation)
    }

    /// Returns true only when the all-Tabs close is handled here, before any approval exists.
    pub(in crate::ui::workspace_manager) fn request_worktree_close_fallback(
        &mut self,
        target: CloseTarget,
        server_target: ServerCloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let CloseTarget::Worktree { workspace_id, .. } = target else {
            return false;
        };
        // Repeated requests share the one unresolved original for this Workspace.
        if self
            .sidebar_worktrees
            .close_fallbacks
            .contains_key(&workspace_id)
        {
            return true;
        }
        let ServerCloseTarget::Worktree { workspace, tab } = server_target else {
            return false;
        };
        let Some(shared) = self.shared.owning(workspace_id) else {
            return false;
        };
        let Some(authoritative) = shared.latest_inventory().and_then(|inventory| {
            inventory
                .workspaces
                .iter()
                .find(|entry| entry.id == workspace)
        }) else {
            return false;
        };
        let Some(anchor) = authoritative.tabs.iter().find(|entry| entry.id == tab) else {
            return false;
        };
        let Some(path) = anchor.worktree.clone() else {
            return false;
        };
        let repository = anchor.worktree_repository.clone();
        let candidate = shared.latest_inventory().and_then(|inventory| {
            let repositories = inventory.repositories.as_ref()?;
            spaceterm_server::organize::worktree_close_plan(inventory, repositories, workspace, tab)
                .ok()
        });
        if candidate
            .as_ref()
            .is_some_and(|plan| plan.fallback.is_none())
        {
            return false;
        }
        if !shared.editability().available() {
            Self::show_shared_refusal(
                "establish a Tab outside the Worktree",
                shared.editability().into(),
                window,
                cx,
            );
            return true;
        }
        let Some(Operation::OpenTab {
            root,
            path: outside,
            ..
        }) = candidate
            .and_then(|plan| plan.fallback)
            .filter(|_| self.owner_writes_worktrees(workspace_id, cx))
        else {
            Self::worktree_fallback_failed(
                "Open a Tab in another available Worktree before closing these Tabs.",
                window,
                cx,
            );
            return true;
        };
        let Some(handle) = window.window_handle().downcast::<Self>() else {
            return true;
        };
        let mutation = Mutation::Worktree(Operation::OpenTab {
            workspace,
            source: None,
            root: root.clone(),
            path: outside.clone(),
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        });
        // The fallback presents its own failure, so the window presents it only if the person
        // never answers that alert.
        let answer = shared.edits().owner();
        let completion = match shared
            .edits()
            .submit_owned(mutation.clone(), answer.ticket())
        {
            Ok(completion) => completion,
            Err(reason) => {
                Self::show_shared_refusal(
                    "establish a Tab outside the Worktree",
                    reason,
                    window,
                    cx,
                );
                return true;
            }
        };
        self.sidebar_worktrees
            .close_fallbacks
            .insert(workspace_id, mutation);
        cx.spawn(async move |_, cx| {
            let result = completion.recv().await;
            let _ = handle.update(cx, |manager, window, cx| {
                // Reconcile first. Optimistic membership never satisfies this check.
                if let Some(owner) = manager.shared.owner(workspace_id) {
                    manager.sync_shared_workspaces(owner, window, cx);
                }
                manager
                    .sidebar_worktrees
                    .close_fallbacks
                    .remove(&workspace_id);
                let Ok(Ok(outcome)) = result else {
                    Self::worktree_fallback_failed(
                        "SpaceTerm couldn\u{2019}t open a Tab outside this Worktree.",
                        window,
                        cx,
                    );
                    return;
                };
                let Outcome::Accepted { target: accepted } = outcome.outcome else {
                    let lifecycle = answer.lifecycle();
                    Self::worktree_fallback_failed_with(
                        "SpaceTerm couldn\u{2019}t open a Tab outside this Worktree.",
                        lifecycle,
                        window,
                        cx,
                    );
                    return;
                };
                let current = manager
                    .shared
                    .owning(workspace_id)
                    .and_then(|shared| shared.latest_inventory())
                    .and_then(|inventory| {
                        inventory
                            .workspaces
                            .iter()
                            .find(|entry| entry.id == workspace)
                    });
                let usable = current.is_some_and(|current| {
                    current.tabs.iter().any(|entry| {
                        entry.id == accepted
                            && entry.worktree.as_ref() == Some(&outside)
                            && entry.worktree_repository.as_ref() == Some(&root)
                            && entry.panes.iter().any(|pane| pane.id == entry.root_pane)
                    })
                });
                if !usable {
                    Self::worktree_fallback_failed(
                        "Wait for a Tab outside this Worktree to open, then try again.",
                        window,
                        cx,
                    );
                    return;
                }
                // A removed or reassigned anchor cannot turn this old request into a new close.
                if current.is_some_and(|current| {
                    current.tabs.iter().any(|entry| {
                        entry.id == tab
                            && entry.worktree.as_ref() == Some(&path)
                            && entry.worktree_repository == repository
                    })
                }) {
                    // Capture current exact targets and fresh consent through the original path.
                    manager.request_shared_close(target, window, cx);
                }
            });
        })
        .detach();
        true
    }

    fn worktree_fallback_failed(
        message: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Self::worktree_fallback_failed_with(message, |_, _| {}, window, cx);
    }

    /// Presents the fallback's failure with `lifecycle` observing the alert.
    fn worktree_fallback_failed_with(
        message: &'static str,
        lifecycle: impl Fn(&spaceterm_ui::ModalLifecycleEvent, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = Alert::new(
            ModalId::new("worktree-tabs-not-closed"),
            "Tabs Not Closed",
            "Tabs Not Closed",
            message,
            vec![ModalAction::new(
                (),
                "OK",
                ModalActionRole::Cancel,
                "worktree-tabs-not-closed-ok",
            )],
        )
        .intent(AlertIntent::Warning)
        .present_with_lifecycle(window, cx, |_, _| {}, lifecycle);
    }
}
