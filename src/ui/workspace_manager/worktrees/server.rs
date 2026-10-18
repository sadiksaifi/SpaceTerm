//! Desktop dialogs submit Worktree operations to the same server edit journal as shared closes.
use super::*;
use crate::ui::server_worktrees::{Client, FormBackend, WriteError};
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
        let active_path = self
            .workspaces
            .workspace(workspace_id)
            .and_then(|w| w.payload().read(cx).active_worktree())
            .and_then(|id| {
                self.sidebar_worktrees
                    .workspaces
                    .get(&workspace_id)?
                    .registry
                    .key(id)
            })
            .map(|key| key.root().to_owned());
        let context = client.read(Read::Context { root: root.clone() }, cx);
        let handle = window.window_handle().downcast::<WorkspaceManager>();
        cx.spawn(async move |_, cx| {
            let result = context.await;
            let Ok(ReadReply::Context {
                settings,
                home,
                branches,
            }) = result
            else {
                if let Some(handle) = handle {
                    let _ = handle.update(cx, |_, window, cx| {
                        if !client.availability().0 {
                            Self::show_shared_refusal(
                                "create the Worktree",
                                crate::ui::shared_workspaces::SharedRefusalReason::Offline,
                                window,
                                cx,
                            );
                        } else {
                            let _ = Alert::new(
                                ModalId::new("server-worktree-context"),
                                "Couldn\u{2019}t Read Branches",
                                "Couldn\u{2019}t Read Branches",
                                crate::ui::worktree_form::BRANCHES_FAILED,
                                vec![ModalAction::new(
                                    false,
                                    "OK",
                                    ModalActionRole::Cancel,
                                    "server-worktree-context-ok",
                                )],
                            )
                            .intent(AlertIntent::Warning)
                            .present(window, cx, |_, _| {});
                        }
                    });
                }
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
                    default_base: inventory
                        .worktrees
                        .iter()
                        .find(|r| Some(&r.root) == active_path.as_ref())
                        .and_then(branch_of)
                        .or_else(|| inventory.worktrees.iter().find_map(branch_of)),
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
                        if let WorktreeFormEvent::Interrupted(path) = event {
                            let path = path.clone();
                            let handle = window.window_handle();
                            let owner = cx.weak_entity();
                            cx.defer(move |cx| {
                                let _ = handle.update(cx, |_, window, cx| {
                                    let _ = owner.update(cx, |m, cx| {
                                        m.server_worktree_interrupted(true, &path, window, cx)
                                    });
                                });
                            });
                        }
                        if let WorktreeFormEvent::Created(path) = event {
                            let revision = Client::get(cx)
                                .and_then(|client| client.source.snapshot().inventory)
                                .map_or(0, |i| i.revision);
                            manager.sidebar_worktrees.created_shared =
                                Some((workspace_id, path.clone(), revision));
                            manager.set_worktrees_expanded(workspace_id, true);
                            manager.open_created_server_worktree(window, cx);
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
        let Some((workspace, path, minimum_revision)) =
            self.sidebar_worktrees.created_shared.as_ref()
        else {
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
            if self
                .shared
                .as_ref()
                .and_then(|s| s.latest_inventory())
                .is_some_and(|i| i.revision >= *minimum_revision)
            {
                self.sidebar_worktrees.created_shared = None;
            }
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
        window: &mut Window,
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
            Self::show_shared_refusal("open the Worktree", shared.editability().into(), window, cx);
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
        let handle = window.window_handle();
        cx.spawn(async move |manager, cx| {
            let result = task.await.and_then(crate::ui::server_worktrees::result);
            let _ = handle.update(cx, |_, window, cx| {
                let _ = manager.update(cx, |_manager, cx| match result {
                    Err(WriteError::Refused(reason)) => {
                        Self::show_shared_refusal("open the Worktree", reason, window, cx)
                    }
                    Err(WriteError::Server(_)) => (), // The inventory poll presents the retained outcome once.
                    Ok(()) => (),
                });
            });
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
        let name = sanitize_for_display(&directory_name(&path), MAXIMUM_NAME_CHARS);
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
                        let error = result.err().unwrap_or(Error::Unavailable);
                        if !client.availability().0 {
                            Self::show_shared_refusal("remove the Worktree", crate::ui::shared_workspaces::SharedRefusalReason::Offline, window, cx);
                        } else if error == Error::Unavailable {
                            let title = format!("Couldn\u{2019}t Remove \u{201c}{name}\u{201d}");
                            let _ = Alert::new(ModalId::new("server-worktree-removal-check"),title.clone(),title,"SpaceTerm couldn\u{2019}t check it for changes, so nothing was removed.",vec![ModalAction::new(false,"OK",ModalActionRole::Cancel,"server-worktree-removal-check-ok")]).intent(AlertIntent::Warning).present(window,cx,|_,_|{});
                        } else {
                            manager.server_worktree_failure(&format!("Couldn\u{2019}t Remove \u{201c}{name}\u{201d}"),error,None,window,cx);
                        }
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
        let name = sanitize_for_display(&directory_name(&review.path), MAXIMUM_NAME_CHARS);
        let title = format!("Remove Worktree \u{201c}{name}\u{201d}?");
        let mut message = vec!["The local server deletes its directory with Git.".to_owned()];
        if review.changes {
            message.push("Its uncommitted changes and untracked files will be deleted.".into());
        }
        if review.submodules {
            message.push("Its submodules\u{2019} checkouts will be deleted.".into());
        }
        let other = review
            .tabs
            .iter()
            .filter(|t| self.shared.as_ref().and_then(|s| s.local(t.workspace)) != Some(workspace))
            .count();
        if !review.tabs.is_empty() {
            let elsewhere = match other {
                0 => String::new(),
                1 => ", including 1 in another Workspace".into(),
                n => format!(", including {n} in other Workspaces"),
            };
            message.push(format!(
                "Closes {} {} for every client{elsewhere}.",
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
        }
        let branch = self
            .shared_inventory(workspace)
            .and_then(|i| i.worktrees.iter().find(|r| r.root == review.path))
            .and_then(|r| match &r.head {
                WorktreeHead::Branch(branch) => {
                    Some(sanitize_for_display(branch, MAXIMUM_NAME_CHARS))
                }
                _ => None,
            });
        message.push(branch.map_or_else(
            || "Its commits stay in the repository.".into(),
            |branch| format!("The branch \u{201c}{branch}\u{201d} is kept."),
        ));
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
        .detail(super::super::compact_home_path(
            &review.path,
            &self.local_home_directory_path,
        ))
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
                            if let Err(WriteError::Refused(reason)) = result {
                                Self::show_shared_refusal(
                                    "remove the Worktree",
                                    reason,
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
        let missing = self
            .shared_inventory(review.workspace)
            .is_some_and(|i| !i.worktrees.iter().any(|r| r.root == review.review.path));
        if missing || !review.review.matches_inventory(inventory) {
            let Some(review) = self.sidebar_worktrees.server_review.take() else {
                return;
            };
            self.sidebar_worktrees.removal = None;
            let _ = review.presentation.dismiss(window, cx);
            self.server_worktree_failure(
                "Worktree Changed on Another Client",
                if missing {
                    Error::Missing
                } else {
                    Error::Changed
                },
                Some((review.workspace, review.worktree)),
                window,
                cx,
            );
        }
    }
    pub(in crate::ui::workspace_manager) fn server_worktree_interrupted(
        &mut self,
        creating: bool,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = sanitize_for_display(&directory_name(path), MAXIMUM_NAME_CHARS);
        let (title, verb, again) = if creating {
            (
                "Worktree Creation Interrupted",
                "created",
                "To create it, choose New Worktree again.",
            )
        } else {
            (
                "Worktree Removal Interrupted",
                "removed",
                "To remove it, choose Remove Worktree again.",
            )
        };
        let message = format!(
            "The server stopped before it confirmed whether Git {verb} \u{201c}{name}\u{201d}. SpaceTerm didn\u{2019}t repeat the request and refreshed the Worktrees from Git, so the sidebar shows what Git lists now. {again}"
        );
        let _ = Alert::new(
            ModalId::new("server-worktree-interrupted"),
            title,
            title,
            message,
            vec![ModalAction::new(
                false,
                "OK",
                ModalActionRole::Cancel,
                "server-worktree-interrupted-ok",
            )],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, |_, _| {});
    }
    pub(in crate::ui::workspace_manager) fn server_worktree_operation_failure(
        &mut self,
        operation: &Operation,
        error: Error,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (path, creating) = match operation {
            Operation::Create { path, .. } => (path, true),
            Operation::Remove { path, .. } | Operation::OpenTab { path, .. } => (path, false),
            Operation::SetLocation { .. } => return,
        };
        if error == Error::Interrupted && !matches!(operation, Operation::OpenTab { .. }) {
            self.server_worktree_interrupted(creating, path, window, cx);
            return;
        }
        let name = sanitize_for_display(&directory_name(path), MAXIMUM_NAME_CHARS);
        if matches!(operation, Operation::Remove { .. }) {
            let review = self.workspaces.iter().find_map(|workspace| {
                let id = workspace.id();
                self.shared_worktree_section(id, cx)?
                    .rows()
                    .find_map(|row| {
                        (self.shared_worktree_root(id, row.worktree_id).as_ref() == Some(path))
                            .then_some((id, row.worktree_id))
                    })
            });
            self.server_worktree_failure(
                &format!("Couldn\u{2019}t Remove \u{201c}{name}\u{201d}"),
                error,
                review,
                window,
                cx,
            );
            return;
        }
        let (title, message) = if creating {
            let reason = match error {
                Error::Location => {
                    "SpaceTerm couldn\u{2019}t use its location on the local server. Git didn\u{2019}t run, so nothing was created."
                }
                Error::Missing => {
                    "The repository is no longer at its location on the local server. Git didn\u{2019}t run, so nothing was created."
                }
                Error::Branch => {
                    "Git reported an error while creating it and may have left part of it on disk. SpaceTerm didn\u{2019}t repeat the request and refreshed the Worktrees from Git, so the sidebar shows what Git lists now. Review them before choosing New Worktree again."
                }
                _ => {
                    "The server couldn\u{2019}t confirm whether Git created it. SpaceTerm didn\u{2019}t repeat the request and refreshed the Worktrees from Git, so the sidebar shows what Git lists now. Review them before choosing New Worktree again."
                }
            };
            (
                format!("Couldn\u{2019}t Create \u{201c}{name}\u{201d}"),
                reason,
            )
        } else {
            let reason = match error {
                Error::Missing => "Git no longer lists it, so no Tab opened.",
                Error::Location => {
                    "Its directory isn\u{2019}t available on the server, so no Tab opened."
                }
                _ => "The server couldn\u{2019}t open a Tab in it.",
            };
            (
                format!("Couldn\u{2019}t Open \u{201c}{name}\u{201d}"),
                reason,
            )
        };
        let _ = Alert::new(
            ModalId::new("server-worktree-operation-failed"),
            title.clone(),
            title,
            message,
            vec![ModalAction::new(
                false,
                "OK",
                ModalActionRole::Cancel,
                "server-worktree-operation-failed-ok",
            )],
        )
        .intent(AlertIntent::Warning)
        .present(window, cx, |_, _| {});
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
                "A Tab was added or removed, a Terminal Session started or ended, or Git changed the target after the confirmation opened. Nothing was removed."
            }
            Error::Main => "The Main Worktree cannot be removed.",
            Error::Locked => "The Worktree is locked. Unlock it with Git before removing it.",
            Error::Pinned => "It holds a Pinned Directory. Unpin the directory before removing it.",
            Error::Missing => "Another client or Git removed it first. Nothing else changed.",
            Error::Interrupted => {
                "The server stopped before it confirmed the Git write. SpaceTerm didn\u{2019}t repeat the request and refreshed the Worktrees from Git, so the sidebar shows what Git lists now. Start a new operation only after reviewing it."
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
            if error == Error::Missing {
                "Worktree Already Removed".into()
            } else {
                title.to_owned()
            },
            if error == Error::Missing {
                "Worktree Already Removed".into()
            } else {
                title.to_owned()
            },
            message,
            actions,
        )
        .intent(if error == Error::Missing {
            AlertIntent::Informational
        } else {
            AlertIntent::Warning
        })
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
