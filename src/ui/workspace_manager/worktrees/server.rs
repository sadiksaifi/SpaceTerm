//! Desktop dialogs submit Worktree operations to the same server edit journal as shared closes.
use super::*;
use crate::ui::server_worktrees::{Client, FormBackend};
use spaceterm_server::worktree::{Operation, Read, ReadReply};
impl WorkspaceManager {
    pub(in crate::ui::workspace_manager) fn new_shared_worktree(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(shared) = self.shared.as_ref() else {
            return false;
        };
        if !shared.editability().available() {
            Self::show_shared_refusal(
                "create the Worktree",
                shared.editability().into(),
                window,
                cx,
            );
            return false;
        }
        let Some(workspace) = shared.server_workspace(workspace_id).map(|w| w.id) else {
            return false;
        };
        let Some(inventory) = self.shared_inventory(workspace_id).cloned() else {
            return false;
        };
        let Some(client) = Client::get(cx) else {
            return false;
        };
        let root = inventory.main_root.clone();
        let context = client.read(Read::Context { root: root.clone() }, cx);
        let handle = window.window_handle().downcast::<WorkspaceManager>();
        cx.spawn(async move |_, cx| {
            let Ok(ReadReply::Context {
                settings,
                home,
                branches,
            }) = context.await
            else {
                return;
            };
            let Some(handle) = handle else {
                return;
            };
            let _ = handle.update(cx, move |manager, window, cx| {
                if manager
                    .sidebar_worktrees
                    .form
                    .as_ref()
                    .is_some_and(|f| f.read(cx).is_open())
                {
                    return;
                }
                let branch_of = |record: &WorktreeRecord| match &record.head {
                    WorktreeHead::Branch(branch) => Some(branch.clone()),
                    _ => None,
                };
                let context = WorktreeFormContext {
                    repository_name: directory_name(&root).to_string(),
                    template: settings.template,
                    home,
                    default_base: inventory.worktrees.iter().find_map(branch_of),
                    checked_out: inventory
                        .worktrees
                        .iter()
                        .filter_map(|r| Some((branch_of(r)?, directory_name(&r.root))))
                        .collect(),
                };
                let backend = Rc::new(FormBackend {
                    client,
                    root,
                    workspace,
                    branches,
                });
                let probe: LocationProbe =
                    Rc::new(|_| Ok(crate::platform::local_filesystem::NewDirectoryTarget::Free));
                let form = cx.new(|cx| WorktreeForm::new(backend, context, probe, window, cx));
                cx.subscribe_in(
                    &form,
                    window,
                    move |manager, _, event: &WorktreeFormEvent, window, cx| {
                        if matches!(event, WorktreeFormEvent::Interrupted) {
                            let handle = window.window_handle();
                            let owner = cx.weak_entity();
                            cx.defer(move |cx| {
                                let _ = handle.update(cx, |_, window, cx| {
                                    let _ = owner.update(cx, |m, cx| {
                                        m.server_worktree_failure(
                                            "Worktree Creation Interrupted",
                                            Error::Interrupted,
                                            None,
                                            window,
                                            cx,
                                        )
                                    });
                                });
                            });
                        }
                        if matches!(event, WorktreeFormEvent::Created(_)) {
                            manager.set_worktrees_expanded(workspace_id, true);
                            cx.notify();
                        }
                    },
                )
                .detach();
                form.update(cx, |f, cx| f.present(window, cx));
                manager.sidebar_worktrees.form = Some(form);
            });
        })
        .detach();
        true
    }
    pub(in crate::ui::workspace_manager) fn open_created_server_worktree(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((workspace, path)) = self.sidebar_worktrees.created_shared.as_ref() else {
            return;
        };
        let Some(server) = self
            .shared
            .as_ref()
            .and_then(|s| s.server_workspace(*workspace))
        else {
            self.sidebar_worktrees.created_shared = None;
            return;
        };
        let Some(tab) = server
            .tabs
            .iter()
            .find(|t| t.worktree.as_ref() == Some(path))
            .map(|t| t.id)
        else {
            return;
        };
        let workspace = *workspace;
        self.sidebar_worktrees.created_shared = None;
        if self.activate_workspace(workspace, window, cx)
            && let Some(manager) = self
                .workspaces
                .workspace(workspace)
                .map(|w| w.payload().clone())
        {
            manager.update(cx, |m, cx| m.open_accepted_shared(tab, window, cx));
        }
    }
    pub(in crate::ui::workspace_manager) fn open_server_worktree_tab(
        &mut self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(shared) = self.shared.as_ref() else {
            return false;
        };
        let Some(workspace) = shared.server_workspace(workspace_id).map(|w| w.id) else {
            return false;
        };
        let Some(root) = self
            .shared_inventory(workspace_id)
            .map(|i| i.main_root.clone())
        else {
            return false;
        };
        let Some(path) = self.shared_worktree_root(workspace_id, worktree_id) else {
            return false;
        };
        if !shared.editability().available() {
            return false;
        }
        let Some(client) = Client::get(cx) else {
            return false;
        };
        let task = client.write(
            Operation::OpenTab {
                workspace,
                root,
                path,
            },
            cx,
        );
        cx.spawn(async move |_, _| {
            let _ = task.await;
        })
        .detach();
        true
    }
}

use spaceterm_server::worktree::{Error, Removal};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId, ModalPresentationHandle,
};
pub(super) struct RemovalReview {
    pub workspace: WorkspaceId,
    pub worktree: WorktreeId,
    pub review: Removal,
    pub presentation: ModalPresentationHandle,
}
impl WorkspaceManager {
    pub(in crate::ui::workspace_manager) fn request_server_worktree_removal(
        &mut self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_worktrees.removal.is_some() {
            return;
        }
        let Some(shared) = self.shared.as_ref() else {
            return;
        };
        if !shared.editability().available() {
            Self::show_shared_refusal(
                "remove the Worktree",
                shared.editability().into(),
                window,
                cx,
            );
            return;
        }
        let Some(root) = self
            .shared_inventory(workspace_id)
            .map(|i| i.main_root.clone())
        else {
            return;
        };
        let Some(path) = self.shared_worktree_root(workspace_id, worktree_id) else {
            return;
        };
        let Some(client) = Client::get(cx) else {
            return;
        };
        self.sidebar_worktrees.removal_generation =
            self.sidebar_worktrees.removal_generation.wrapping_add(1);
        let generation = self.sidebar_worktrees.removal_generation;
        self.sidebar_worktrees.removal = Some(generation);
        let read = client.read(Read::CheckRemoval { root, path }, cx);
        let handle = window.window_handle().downcast::<WorkspaceManager>();
        cx.spawn(async move |_, cx| {
            let result = read.await;
            let Some(handle) = handle else {
                return;
            };
            let _ = handle.update(cx, |manager, window, cx| {
                if manager.sidebar_worktrees.removal != Some(generation) {
                    return;
                }
                match result {
                    Ok(ReadReply::Removal(review)) => manager.present_server_removal(
                        workspace_id,
                        worktree_id,
                        review,
                        client,
                        window,
                        cx,
                    ),
                    _ => {
                        manager.sidebar_worktrees.removal = None;
                        manager.server_worktree_failure(
                            "Couldn't Check Worktree",
                            result.err().unwrap_or(Error::Unavailable),
                            None,
                            window,
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }
    fn present_server_removal(
        &mut self,
        workspace: WorkspaceId,
        worktree: WorktreeId,
        review: Removal,
        client: Client,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(generation) = self.sidebar_worktrees.removal else {
            return;
        };
        let name = directory_name(&review.path);
        let title = format!("Remove Worktree \"{name}\"?");
        let mut message = vec!["The local server deletes its directory with Git.".to_owned()];
        if review.changes {
            message.push("Its uncommitted changes and untracked files will be deleted.".into());
        }
        if review.submodules {
            message.push("Its submodules' checkouts will be deleted.".into());
        }
        let other = review
            .tabs
            .iter()
            .filter(|t| self.shared.as_ref().and_then(|s| s.local(t.workspace)) != Some(workspace))
            .count();
        message.push(format!(
            "Closes {} {} for every client, including {other} in other Workspaces.",
            review.tabs.len(),
            if review.tabs.len() == 1 {
                "Tab"
            } else {
                "Tabs"
            }
        ));
        let sessions = review
            .tabs
            .iter()
            .flat_map(|t| &t.panes)
            .filter(|p| p.session.is_some())
            .count();
        message.push(match sessions {0=>"No shell is running in them, so no Terminal Session ends.".into(),1=>"This ends 1 Terminal Session, even if another client controls its Pane, and stops its running commands.".into(),n=>format!("This ends {n} Terminal Sessions, including in Panes another client controls, and stops their running commands.")});
        message.push("Its branch and commits stay in the repository.".into());
        let operation = Operation::Remove {
            root: review.root.clone(),
            path: review.path.clone(),
            boot: review.boot,
            confirmation: review.confirmation,
        };
        let owner = cx.weak_entity();
        let handle = window.window_handle();
        let result = Alert::new(
            ModalId::new("worktree-removal"),
            title.clone(),
            title,
            message.join(" "),
            vec![
                ModalAction::new(
                    true,
                    if review.changes {
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
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "worktree-removal-cancel",
                ),
            ],
        )
        .detail(review.path.display().to_string())
        .intent(AlertIntent::Critical)
        .present(window, cx, move |outcome, cx| {
            let confirmed = matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            );
            let _ = owner.update(cx, |manager, cx| {
                if manager.sidebar_worktrees.removal != Some(generation) {
                    return;
                }
                manager.sidebar_worktrees.server_review = None;
                if !confirmed {
                    manager.sidebar_worktrees.removal = None;
                    return;
                }
                let write = client.write(operation.clone(), cx);
                cx.spawn(async move |manager, cx| {
                    let result = write.await.and_then(crate::ui::server_worktrees::result);
                    let _ = handle.update(cx, |_, window, cx| {
                        let _ = manager.update(cx, |manager, cx| {
                            if manager.sidebar_worktrees.removal == Some(generation) {
                                manager.sidebar_worktrees.removal = None;
                            }
                            if let Err(error) = result {
                                manager.server_worktree_failure(
                                    if error == Error::Interrupted {
                                        "Worktree Removal Interrupted"
                                    } else {
                                        "Couldn't Remove Worktree"
                                    },
                                    error,
                                    Some((workspace, worktree)),
                                    window,
                                    cx,
                                );
                            }
                            cx.notify();
                        });
                    });
                })
                .detach();
            });
        });
        match result {
            Ok(presentation) => {
                self.sidebar_worktrees.server_review = Some(RemovalReview {
                    workspace,
                    worktree,
                    review,
                    presentation,
                })
            }
            Err(_) => self.sidebar_worktrees.removal = None,
        }
    }
    pub(in crate::ui::workspace_manager) fn revalidate_server_removal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(review) = self.sidebar_worktrees.server_review.as_ref() else {
            return;
        };
        let Some(inventory) = self.shared.as_ref().and_then(|s| s.latest_inventory()) else {
            return;
        };
        if inventory
            .close
            .as_ref()
            .is_none_or(|c| c.boot != review.review.boot)
            || inventory.affected_worktree_tabs(&review.review.root, &review.review.path)
                != review.review.tabs
        {
            let Some(review) = self.sidebar_worktrees.server_review.take() else {
                return;
            };
            self.sidebar_worktrees.removal = None;
            let _ = review.presentation.dismiss(window, cx);
            self.server_worktree_failure(
                "Worktree Changed on Another Client",
                Error::Changed,
                Some((review.workspace, review.worktree)),
                window,
                cx,
            );
        }
    }
    pub(in crate::ui::workspace_manager) fn server_worktree_failure(
        &mut self,
        title: &str,
        error: Error,
        review: Option<(WorkspaceId, WorktreeId)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = match error {
            Error::Changed => {
                "A Tab was added or removed, a Terminal Session started or ended, or Git changed the target after the confirmation opened. The confirmed removal did not run."
            }
            Error::Main => "The Main Worktree cannot be removed.",
            Error::Locked => "The Worktree is locked. Unlock it with Git before removing it.",
            Error::Pinned => "It holds a Pinned Directory. Unpin the directory before removing it.",
            Error::Missing => "Git no longer lists that Worktree. Nothing else was removed.",
            Error::Interrupted => {
                "The server stopped before it confirmed the Git write. SpaceTerm didn't repeat the request and refreshed the Worktrees from Git, so the sidebar shows what Git lists now. Start a new operation only after reviewing it."
            }
            Error::Location => {
                "The server could not use this location. Choose a new or empty folder."
            }
            Error::Branch => {
                "Git could not create the Worktree with this branch. Review the branch and the Worktrees from Git."
            }
            Error::Unavailable => {
                "The server could not complete the Worktree operation. Review the Worktrees from Git before trying again."
            }
        };
        let mut actions = Vec::new();
        if error == Error::Changed && review.is_some() {
            actions.push(
                ModalAction::new(
                    true,
                    "Review",
                    ModalActionRole::Affirmative,
                    "worktree-removal-review",
                )
                .with_emphasis(ModalActionEmphasis::Prominent),
            );
        }
        actions.push(ModalAction::new(
            false,
            if actions.is_empty() { "OK" } else { "Cancel" },
            ModalActionRole::Cancel,
            "worktree-removal-failed-ok",
        ));
        let owner = cx.weak_entity();
        let handle = window.window_handle();
        let _ = Alert::new(
            ModalId::new("worktree-removal-changed"),
            title.to_owned(),
            title.to_owned(),
            message,
            actions,
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, move |outcome, cx| {
            if matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) && let Some((workspace, worktree)) = review
            {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = owner.update(cx, |m, cx| {
                        m.request_server_worktree_removal(workspace, worktree, window, cx)
                    });
                });
            }
        });
    }
}
