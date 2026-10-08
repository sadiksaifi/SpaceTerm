//! Copying, closing, and removing one Worktree from its sidebar row. Removing closes the
//! Worktree's Tabs, then deletes its directory with `git worktree remove`. The branch stays.

use std::path::PathBuf;

use gpui::{App, Context, SharedString, Window};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use super::super::WorkspaceManager;
use crate::close_confirmation::CloseTarget;
use crate::domain::{WorkspaceId, WorktreeId, WorktreeKey};
use crate::terminal::native_services::clipboard::TextClipboardTarget;
use crate::ui::workspace_sidebar::WorktreeRemoval;
use crate::worktrees::git::{RemovalCheck, RemovalExpectation, WorktreeRemoveError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemovalAction {
    Remove,
    Cancel,
}

/// What removing a Worktree acts on. It is read when the person asks and again when they
/// confirm, because the listing and the Tabs can change in between.
struct RemovalTarget {
    name: SharedString,
    branch: Option<SharedString>,
    root: PathBuf,
    main_root: PathBuf,
    common: PathBuf,
    missing: bool,
    tabs: usize,
    running: bool,
}

/// A removal the person confirmed. `force` is set only when they chose to discard changes, and
/// `expected` is what they saw at the Worktree's location.
struct ConfirmedRemoval {
    generation: u64,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    force: bool,
    expected: RemovalExpectation,
}

impl WorkspaceManager {
    /// Copies a Worktree's root as text.
    pub(in crate::ui::workspace_manager) fn copy_worktree_path(
        &self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        cx: &mut App,
    ) {
        let Some(root) = self
            .sidebar_worktrees
            .workspaces
            .get(&workspace_id)
            .and_then(|worktrees| worktrees.registry.key(worktree_id))
            .map(|key| key.root().display().to_string())
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

    /// Closes every Tab of a Worktree and keeps its row. A Workspace always keeps a Tab, so when
    /// the Worktree holds them all, a Tab opens in the Main Worktree first.
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
            (
                manager.worktree_tab_ids(worktree_id),
                manager.has_tabs_outside(worktree_id),
            )
        };
        if tab_ids.is_empty() {
            return true;
        }
        if !outside {
            let Some(main) = self.main_worktree_id(workspace_id, worktree_id) else {
                return false;
            };
            // A Tab that fails to start reports itself; keep every Tab of the Worktree then.
            if !self.new_worktree_tab(workspace_id, main, window, cx)
                || !manager.read(cx).has_tabs_outside(worktree_id)
            {
                return false;
            }
        }
        manager.update(cx, |manager, cx| {
            for tab_id in tab_ids {
                manager.close_tab_authorized(tab_id, window, cx);
            }
        });
        if self.workspaces.active_workspace_id() == workspace_id {
            self.focus(window, cx);
        }
        cx.notify();
        true
    }

    /// Asks to remove a Worktree, after checking it for changes removing it would discard.
    pub(in crate::ui::workspace_manager) fn request_worktree_removal(
        &mut self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_worktrees.removal.is_some() {
            return;
        }
        let Some(target) = self.removal_target(workspace_id, worktree_id, cx) else {
            return;
        };
        let Some(window_handle) = window.window_handle().downcast::<WorkspaceManager>() else {
            return;
        };
        let Some(store) = self
            .sidebar_worktrees
            .workspaces
            .get(&workspace_id)
            .map(|worktrees| worktrees.store.clone())
        else {
            return;
        };
        self.sidebar_worktrees.removal_generation =
            self.sidebar_worktrees.removal_generation.wrapping_add(1);
        let generation = self.sidebar_worktrees.removal_generation;
        self.sidebar_worktrees.removal = Some(generation);
        // A Missing Worktree has no files to check; git removes it without force while its
        // location stays empty.
        if target.missing {
            let check = Ok((
                RemovalExpectation::Absent(self.local_filesystem.clone()),
                false,
            ));
            self.present_worktree_removal(generation, workspace_id, worktree_id, check, window, cx);
            return;
        }
        let filesystem = self.local_filesystem.clone();
        let check = store.update(cx, |store, cx| {
            store.check_removal(target.root, filesystem, cx)
        });
        cx.spawn(async move |_, cx| {
            let check = check
                .await
                .map(|RemovalCheck { identity, changes }| {
                    (RemovalExpectation::Worktree(identity), changes)
                })
                .ok_or(WorktreeRemoveError::Unchecked);
            let _ = window_handle.update(cx, |manager, window, cx| {
                manager.present_worktree_removal(
                    generation,
                    workspace_id,
                    worktree_id,
                    check,
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    #[cfg(test)]
    pub(in crate::ui::workspace_manager) const fn worktree_removal_pending(&self) -> bool {
        self.sidebar_worktrees.removal.is_some()
    }

    fn present_worktree_removal(
        &mut self,
        generation: u64,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        check: Result<(RemovalExpectation, bool), WorktreeRemoveError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_worktrees.removal != Some(generation) {
            return;
        }
        let (Some(target), Some(window_handle)) = (
            self.removal_target(workspace_id, worktree_id, cx),
            window.window_handle().downcast::<WorkspaceManager>(),
        ) else {
            self.sidebar_worktrees.removal = None;
            return;
        };
        // Without a check, a confirmation could not tell the Worktree from a replacement.
        let (expected, discard) = match check {
            Ok(check) => check,
            Err(error) => {
                self.sidebar_worktrees.removal = None;
                present_removal_failure(&target.name, false, error, window, cx);
                return;
            }
        };
        let title = format!("Remove Worktree \u{201c}{}\u{201d}?", target.name);
        let mut message = Vec::new();
        if discard {
            message.push("Its uncommitted changes and untracked files will be deleted.".to_owned());
        }
        if target.tabs > 0 {
            if target.tabs == 1 {
                message.push("Closes 1 Tab.".to_owned());
            } else {
                message.push(format!("Closes {} Tabs.", target.tabs));
            }
            if target.running {
                message.push(if target.tabs == 1 {
                    "The command running in it will stop.".to_owned()
                } else {
                    "Commands running in them will stop.".to_owned()
                });
            }
        }
        match &target.branch {
            Some(branch) => message.push(format!("The branch \u{201c}{branch}\u{201d} is kept.")),
            None => message.push("Its commits stay in the repository.".to_owned()),
        }
        let result = Alert::new(
            ModalId::new("worktree-removal"),
            title.clone(),
            title,
            message.join(" "),
            vec![
                ModalAction::new(
                    RemovalAction::Remove,
                    if discard {
                        "Remove and Discard Changes"
                    } else {
                        "Remove"
                    },
                    ModalActionRole::Affirmative,
                    "worktree-removal-confirm",
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    RemovalAction::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "worktree-removal-cancel",
                ),
            ],
        )
        .detail(super::super::compact_home_path(
            &target.root,
            &self.local_home_directory_path,
        ))
        .intent(AlertIntent::Critical)
        .present(window, cx, move |outcome, cx| {
            let confirmed = matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: RemovalAction::Remove,
                    ..
                }
            );
            let _ = window_handle.update(cx, |manager, window, cx| {
                if confirmed {
                    manager.commit_worktree_removal(
                        ConfirmedRemoval {
                            generation,
                            workspace_id,
                            worktree_id,
                            force: discard,
                            expected: expected.clone(),
                        },
                        window,
                        cx,
                    );
                } else if manager.sidebar_worktrees.removal == Some(generation) {
                    manager.sidebar_worktrees.removal = None;
                }
            });
        });
        if let Err(error) = result {
            self.sidebar_worktrees.removal = None;
            eprintln!("failed to present the Worktree removal confirmation: {error}");
        }
    }

    /// Checks that the confirmed Worktree is still at its location, then closes its Tabs and has
    /// git delete its directory. Only a confirmed discard passes `--force`, and git checks the
    /// Worktree again right before deleting it.
    fn commit_worktree_removal(
        &mut self,
        confirmed: ConfirmedRemoval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_worktrees.removal != Some(confirmed.generation) {
            return;
        }
        let (Some(target), Some(store), Some(window_handle)) = (
            self.removal_target(confirmed.workspace_id, confirmed.worktree_id, cx),
            self.sidebar_worktrees
                .workspaces
                .get(&confirmed.workspace_id)
                .map(|worktrees| worktrees.store.clone()),
            window.window_handle().downcast::<WorkspaceManager>(),
        ) else {
            self.sidebar_worktrees.removal = None;
            return;
        };
        let name = target.name.clone();
        let expected = confirmed.expected.clone();
        let check = store.update(cx, |store, cx| {
            store.confirm_location(target.root, expected, cx)
        });
        cx.spawn(async move |_, cx| {
            let result = check.await;
            let _ = window_handle.update(cx, |manager, window, cx| match result {
                Ok(()) => manager.remove_confirmed_worktree(confirmed, window, cx),
                Err(error) => {
                    if manager.sidebar_worktrees.removal == Some(confirmed.generation) {
                        manager.sidebar_worktrees.removal = None;
                        present_removal_failure(&name, confirmed.force, error, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    fn remove_confirmed_worktree(
        &mut self,
        confirmed: ConfirmedRemoval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ConfirmedRemoval {
            generation,
            workspace_id,
            worktree_id,
            force,
            expected,
        } = confirmed;
        if self.sidebar_worktrees.removal != Some(generation) {
            return;
        }
        let (Some(target), Some(store), Some(window_handle)) = (
            self.removal_target(workspace_id, worktree_id, cx),
            self.sidebar_worktrees
                .workspaces
                .get(&workspace_id)
                .map(|worktrees| worktrees.store.clone()),
            window.window_handle().downcast::<WorkspaceManager>(),
        ) else {
            self.sidebar_worktrees.removal = None;
            return;
        };
        if !self.close_worktree_tabs(workspace_id, worktree_id, window, cx) {
            self.sidebar_worktrees.removal = None;
            return;
        }
        let name = target.name.clone();
        let removal = store.update(cx, |store, cx| {
            store.remove(
                target.main_root,
                target.common,
                target.root,
                force,
                expected,
                cx,
            )
        });
        cx.spawn(async move |_, cx| {
            let result = removal.await;
            let _ = window_handle.update(cx, |manager, window, cx| {
                if manager.sidebar_worktrees.removal == Some(generation) {
                    manager.sidebar_worktrees.removal = None;
                }
                if let Err(error) = result {
                    present_removal_failure(&name, force, error, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The Worktree a removal acts on, or `None` once its row no longer offers Remove.
    fn removal_target(
        &self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        cx: &App,
    ) -> Option<RemovalTarget> {
        let section = self.worktree_section(workspace_id, cx)?;
        let row = section
            .rows()
            .find(|row| row.worktree_id == worktree_id)
            .filter(|row| row.removal == WorktreeRemoval::Allowed)?;
        let worktrees = self.sidebar_worktrees.workspaces.get(&workspace_id)?;
        let snapshot = worktrees.listed(cx)?;
        let key = worktrees.registry.key(worktree_id)?;
        let hierarchy = self.close_hierarchy(cx);
        let close = CloseTarget::Worktree {
            workspace_id,
            worktree_id,
        };
        Some(RemovalTarget {
            name: row.name.clone(),
            branch: (!row.detached && !row.label.is_empty()).then(|| row.label.clone()),
            root: key.root().to_path_buf(),
            main_root: snapshot.repository.main_root().to_path_buf(),
            common: snapshot.common_directory.clone(),
            missing: row.missing,
            tabs: hierarchy.affected_tab_count(close),
            running: hierarchy.requires_confirmation(close) == Some(true),
        })
    }

    /// The Main Worktree of the repository a Worktree belongs to.
    fn main_worktree_id(
        &self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
    ) -> Option<WorktreeId> {
        let registry = &self
            .sidebar_worktrees
            .workspaces
            .get(&workspace_id)?
            .registry;
        let repository = registry.key(worktree_id)?.repository().clone();
        let main_root = repository.main_root().to_path_buf();
        registry.id(&WorktreeKey::new(repository, main_root))
    }
}

fn present_removal_failure(
    name: &SharedString,
    forced: bool,
    error: WorktreeRemoveError,
    window: &mut Window,
    cx: &mut Context<WorkspaceManager>,
) {
    let message = match (error, forced) {
        (WorktreeRemoveError::Replaced, _) => {
            "A different Worktree is now at its location, so nothing was removed."
        }
        (WorktreeRemoveError::Unchecked, _) => {
            "SpaceTerm couldn\u{2019}t check it for changes, so nothing was removed."
        }
        (WorktreeRemoveError::Failed, true) => {
            "Git couldn\u{2019}t delete its directory. A file in it may be in use."
        }
        (WorktreeRemoveError::Failed, false) => {
            "Git couldn\u{2019}t delete its directory. If it changed after you confirmed, remove \
             it again to discard the changes."
        }
    };
    let title = format!("Couldn\u{2019}t Remove \u{201c}{name}\u{201d}");
    if let Err(error) = Alert::new(
        ModalId::new("worktree-removal-failed"),
        title.clone(),
        title,
        message,
        vec![ModalAction::new(
            (),
            "OK",
            ModalActionRole::Cancel,
            "worktree-removal-failed-ok",
        )],
    )
    .intent(AlertIntent::Warning)
    .present(window, cx, |_, _| {})
    {
        eprintln!("failed to present the Worktree removal failure: {error}");
    }
}
