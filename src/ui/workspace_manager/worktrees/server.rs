//! Desktop dialogs submit Worktree operations to the same server edit journal as shared closes.
use std::rc::Rc;

use gpui::AppContext as _;

use super::*;
use crate::repository_status::presentation::{MAXIMUM_NAME_CHARS, sanitize_for_display};
use crate::ui::server_worktrees::{Client, FormBackend, WriteError};
use crate::ui::workspaces::{OutcomeNotice, WorkspaceSubmission};
use crate::ui::worktree_form::{WorktreeFormContext, WorktreeFormEvent};
use crate::worktrees::listing::{WorktreeHead, WorktreeRecord};
use spaceterm_server::hierarchy::Mutation;
use spaceterm_server::worktree::{Operation, Read, ReadReply};
impl WorkspaceManager {
    pub(in crate::ui::workspace_manager) fn new_server_worktree(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // The Workspace's own server runs Git, on its own computer.
        let Some(binding) = self.binding.owning(workspace_id) else {
            return false;
        };
        if !binding.editability().available() {
            Self::show_refusal(
                "create the Worktree",
                binding.editability().into(),
                window,
                cx,
            );
            return false;
        }
        let Some(workspace) = binding.server_workspace(workspace_id).map(|w| w.id) else {
            return false;
        };
        let Some(inventory) = self.workspace_inventory(workspace_id).cloned() else {
            return false;
        };
        let Some(client) = self.owning_worktree_client(workspace_id, cx) else {
            return false;
        };
        let machine = self
            .owning_machine(workspace_id)
            .map(|machine| sanitize_for_display(&machine, MAXIMUM_NAME_CHARS));
        let root = inventory.main_root.clone();
        let active_path = self
            .workspaces
            .workspace(workspace_id)
            .and_then(|w| w.payload().read(cx).active_worktree())
            .and_then(|id| self.worktree_root(workspace_id, id));
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
                            Self::show_refusal(
                                "create the Worktree",
                                crate::ui::workspaces::WorkspaceRefusalReason::Offline,
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
                    client: client.clone(),
                    root,
                    workspace,
                    branches,
                    machine,
                    owner: std::cell::RefCell::default(),
                });
                let form = cx.new(|cx| WorktreeForm::new(backend, context, window, cx));
                cx.subscribe_in(
                    &form,
                    window,
                    move |manager, _, event: &WorktreeFormEvent, window, cx| {
                        if let WorktreeFormEvent::Created(path) = event {
                            let revision =
                                client.source.snapshot().inventory.map_or(0, |i| i.revision);
                            manager.sidebar_worktrees.created =
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
        let Some((workspace, path, minimum_revision)) = self.sidebar_worktrees.created.as_ref()
        else {
            return;
        };
        let Some(server) = self
            .binding
            .owning(*workspace)
            .and_then(|s| s.server_workspace(*workspace))
        else {
            self.sidebar_worktrees.created = None;
            return;
        };
        let Some(tab) = server
            .tabs
            .iter()
            .find(|t| t.worktree.as_ref() == Some(path))
            .map(|t| t.id)
        else {
            if self
                .binding
                .owning(*workspace)
                .and_then(|s| s.latest_inventory())
                .is_some_and(|i| i.revision >= *minimum_revision)
            {
                self.sidebar_worktrees.created = None;
            }
            return;
        };
        let workspace = *workspace;
        self.sidebar_worktrees.created = None;
        if self.activate_workspace(workspace, window, cx)
            && let Some(manager) = self
                .workspaces
                .workspace(workspace)
                .map(|w| w.payload().clone())
        {
            manager.update(cx, |m, cx| m.open_accepted(tab, window, cx));
        }
    }
    pub(in crate::ui::workspace_manager) fn open_server_worktree_tab(
        &mut self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(binding) = self.binding.owning(workspace_id) else {
            return false;
        };
        let Some(workspace) = binding.server_workspace(workspace_id).map(|w| w.id) else {
            return false;
        };
        let Some(root) = self
            .workspace_inventory(workspace_id)
            .map(|i| i.main_root.clone())
        else {
            return false;
        };
        let Some(path) = self.worktree_root(workspace_id, worktree_id) else {
            return false;
        };
        if !binding.editability().available() {
            Self::show_refusal(
                "open the Worktree",
                binding.editability().into(),
                window,
                cx,
            );
            return false;
        }
        let source = self
            .workspaces
            .workspace(workspace_id)
            .and_then(|w| w.payload().read(cx).worktree_pane(worktree_id, cx));
        let submission = binding
            .edits()
            .submit(Mutation::Worktree(Operation::OpenTab {
                workspace,
                source,
                root,
                path,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            }));
        if let WorkspaceSubmission::Refused(reason) = submission {
            Self::show_refusal("open the Worktree", reason, window, cx);
            return false;
        }
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
        let Some(binding) = self.binding.owning(workspace_id) else {
            return;
        };
        if !binding.editability().available() {
            Self::show_refusal(
                "remove the Worktree",
                binding.editability().into(),
                window,
                cx,
            );
            return;
        }
        let Some(root) = self
            .workspace_inventory(workspace_id)
            .map(|i| i.main_root.clone())
        else {
            return;
        };
        let Some(path) = self.worktree_root(workspace_id, worktree_id) else {
            return;
        };
        let Some(client) = self.owning_worktree_client(workspace_id, cx) else {
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
                            Self::show_refusal(
                                "remove the Worktree",
                                crate::ui::workspaces::WorkspaceRefusalReason::Offline,
                                window,
                                cx,
                            );
                        } else if error == Error::Unavailable {
                            let title = "Couldn\u{2019}t Check Worktree Changes".to_owned();
                            let _ = Alert::new(
                                ModalId::new("server-worktree-removal-check"),
                                title.clone(),
                                title,
                                "Try again before removing the Worktree.",
                                vec![ModalAction::new(
                                    false,
                                    "OK",
                                    ModalActionRole::Cancel,
                                    "server-worktree-removal-check-ok",
                                )],
                            )
                            .intent(AlertIntent::Warning)
                            .present(window, cx, |_, _| {});
                        } else {
                            manager.server_worktree_failure(
                                &format!("Couldn\u{2019}t Remove \u{201c}{name}\u{201d}"),
                                error,
                                None,
                                None,
                                window,
                                cx,
                            );
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
        let title = format!("Remove Worktree \u{201C}{name}\u{201D}?");
        let machine = self.owning_machine(workspace);
        let branch = self
            .workspace_inventory(workspace)
            .and_then(|i| i.worktrees.iter().find(|r| r.root == review.path))
            .and_then(|r| match &r.head {
                WorktreeHead::Branch(branch) => {
                    Some(sanitize_for_display(branch, MAXIMUM_NAME_CHARS))
                }
                _ => None,
            });
        let tabs = review.tabs.len();
        let sessions = review
            .tabs
            .iter()
            .flat_map(|tab| &tab.panes)
            .filter(|pane| pane.session.is_some())
            .count();
        let other = review
            .tabs
            .iter()
            .filter(|tab| {
                self.binding
                    .owning(workspace)
                    .and_then(|server| server.local(tab.workspace))
                    != Some(workspace)
            })
            .count();
        let specific = review.changes && review.submodules && tabs > 1 && other > 1 && sessions > 1;
        let message = if let Some(branch) = branch.as_ref().filter(|_| specific) {
            match &machine {
                Some(machine) => format!(
                    "This deletes its uncommitted changes and untracked files on \u{201C}{}\u{201D}, closes {tabs} Tabs, and ends {sessions} Terminal Sessions for every client. The branch \u{201C}{branch}\u{201D} is kept.",
                    sanitize_for_display(machine, MAXIMUM_NAME_CHARS)
                ),
                None => format!(
                    "This deletes its uncommitted changes and untracked files, closes {tabs} Tabs, and ends {sessions} Terminal Sessions for every client. The branch \u{201C}{branch}\u{201D} is kept."
                ),
            }
        } else {
            let directory = match &machine {
                Some(machine) => format!(
                    "Deletes its directory on \u{201C}{}\u{201D}",
                    sanitize_for_display(machine, MAXIMUM_NAME_CHARS)
                ),
                None => "Deletes its directory with Git".to_owned(),
            };
            let deleted = match (review.changes, review.submodules) {
                (true, true) => {
                    ", including uncommitted changes, untracked files, and submodule directories."
                }
                (true, false) => ", including uncommitted changes and untracked files.",
                (false, true) => ", including submodule directories.",
                (false, false) => ".",
            };
            let mut message = format!("{directory}{deleted}");
            if tabs > 0 {
                let noun = if tabs == 1 { "Tab" } else { "Tabs" };
                message.push_str(&format!(" Closes {tabs} {noun} for every client."));
                match sessions {
                    0 => {}
                    1 => message.push_str(" Ends 1 Terminal Session."),
                    count => message.push_str(&format!(" Ends {count} Terminal Sessions.")),
                }
            }
            message
        };
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
            message,
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
        // Another computer's path is shown whole; this computer's home says nothing about it.
        .detail(match machine {
            Some(_) => review.path.display().to_string(),
            None => super::super::compact_home_path(&review.path, &self.local_home_directory_path),
        })
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
                                Self::show_refusal("remove the Worktree", reason, window, cx);
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
        let Some(inventory) = self
            .binding
            .owning(review.workspace)
            .and_then(|s| s.latest_inventory())
        else {
            return;
        };
        let missing = self
            .workspace_inventory(review.workspace)
            .is_some_and(|i| !i.worktrees.iter().any(|r| r.root == review.review.path));
        if missing || !review.review.matches_inventory(inventory) {
            let Some(review) = self.sidebar_worktrees.server_review.take() else {
                return;
            };
            self.sidebar_worktrees.removal = None;
            let _ = review.presentation.dismiss(window, cx);
            self.server_worktree_failure(
                "Worktree Changed",
                if missing {
                    Error::Missing
                } else {
                    Error::Changed
                },
                Some((review.workspace, review.worktree)),
                None,
                window,
                cx,
            );
        }
    }
    /// Returns whether the alert was admitted.
    pub(in crate::ui::workspace_manager) fn server_worktree_interrupted(
        &mut self,
        creating: bool,
        path: &std::path::Path,
        notice: &OutcomeNotice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let name = sanitize_for_display(&directory_name(path), MAXIMUM_NAME_CHARS);
        let (title, action) = if creating {
            ("Check Worktree Creation", "New Worktree")
        } else {
            ("Check Worktree Removal", "Remove Worktree")
        };
        let message = format!(
            "The server stopped before SpaceTerm could confirm the result. Check \u{201C}{name}\u{201D} in the sidebar before choosing {action} again."
        );
        Alert::new(
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
        .present_with_lifecycle(window, cx, |_, _| {}, notice.lifecycle())
        .is_ok()
    }
    /// Presents a failed Worktree operation no other surface presented. Returns whether it was
    /// presented or its alert admitted; `notice` records the presentation.
    pub(in crate::ui::workspace_manager) fn server_worktree_operation_failure(
        &mut self,
        operation: &Operation,
        error: Error,
        notice: &OutcomeNotice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (path, creating) = match operation {
            Operation::Create { path, .. } => (path, true),
            Operation::Remove { path, .. } | Operation::OpenTab { path, .. } => (path, false),
            // Settings presents its own change unless it closed or moved to another server.
            Operation::SetLocation { .. } => {
                return self.server_location_failure(error, notice, window, cx);
            }
        };
        if error == Error::Interrupted && !matches!(operation, Operation::OpenTab { .. }) {
            return self.server_worktree_interrupted(creating, path, notice, window, cx);
        }
        let name = sanitize_for_display(&directory_name(path), MAXIMUM_NAME_CHARS);
        if matches!(operation, Operation::Remove { .. }) {
            let review = self.workspaces.iter().find_map(|workspace| {
                let id = workspace.id();
                self.server_worktree_section(id, cx)?
                    .rows()
                    .find_map(|row| {
                        (self.worktree_root(id, row.worktree_id).as_ref() == Some(path))
                            .then_some((id, row.worktree_id))
                    })
            });
            return self.server_worktree_failure(
                &format!("Couldn\u{2019}t Remove \u{201c}{name}\u{201d}"),
                error,
                review,
                Some(notice),
                window,
                cx,
            );
        }
        let (title, message) = if creating {
            let reason = match error {
                Error::Location => "Choose a new or empty directory and try again.".to_owned(),
                Error::Missing => {
                    "The repository is no longer available. Reopen it before creating a Worktree."
                        .to_owned()
                }
                Error::Branch => format!(
                    "Git could not finish creating the Worktree. Check \u{201C}{name}\u{201D} for incomplete files and review the sidebar before trying again."
                ),
                Error::InvalidName => "Git doesn\u{2019}t accept this branch name.".to_owned(),
                Error::BranchExists => {
                    "A branch with this name already exists. Choose another name and try again."
                        .to_owned()
                }
                Error::BranchCheckedOut => {
                    "Another Worktree has this branch checked out.".to_owned()
                }
                Error::BranchMissing => "This branch no longer exists.".to_owned(),
                _ => format!(
                    "SpaceTerm could not confirm whether the Worktree was created. Check \u{201C}{name}\u{201D} and the sidebar before trying again."
                ),
            };
            let title = match error {
                Error::Location
                | Error::Missing
                | Error::Branch
                | Error::InvalidName
                | Error::BranchExists
                | Error::BranchCheckedOut
                | Error::BranchMissing => {
                    format!("Couldn\u{2019}t Create Worktree \u{201C}{name}\u{201D}")
                }
                _ => "Check Worktree Creation".to_owned(),
            };
            (title, reason)
        } else {
            let reason = match error {
                Error::Missing => "Git no longer lists it.",
                Error::Location => "Its directory isn\u{2019}t available.",
                _ => "Couldn\u{2019}t open a Tab in it.",
            };
            (
                format!("Couldn\u{2019}t Open \u{201c}{name}\u{201d}"),
                reason.to_owned(),
            )
        };
        Alert::new(
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
        .present_with_lifecycle(window, cx, |_, _| {}, notice.lifecycle())
        .is_ok()
    }
    /// A New Worktree Location change that Settings did not present. Returns whether the alert
    /// was admitted.
    fn server_location_failure(
        &mut self,
        error: Error,
        notice: &OutcomeNotice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let message = match error {
            Error::Template(
                spaceterm_server::worktree::path_template::WorktreePathTemplateError::NotWritable,
            ) => {
                "The New Worktree Location directory is not writable. Choose another directory in Settings > Git."
            }
            Error::Template(_) => "Correct New Worktree Location in Settings > Git.",
            Error::Changed => {
                "Another device changed New Worktree Location first. Review it in Settings > Git."
            }
            _ => "Review New Worktree Location in Settings > Git.",
        };
        let title = "Couldn\u{2019}t Change New Worktree Location";
        Alert::new(
            ModalId::new("server-worktree-location-failed"),
            title,
            title,
            message,
            vec![ModalAction::new(
                false,
                "OK",
                ModalActionRole::Cancel,
                "server-worktree-location-failed-ok",
            )],
        )
        .intent(AlertIntent::Warning)
        .present_with_lifecycle(window, cx, |_, _| {}, notice.lifecycle())
        .is_ok()
    }
    /// Returns whether the alert was admitted. `notice` records the presentation of an unseen
    /// outcome.
    pub(in crate::ui::workspace_manager) fn server_worktree_failure(
        &mut self,
        title: &str,
        error: Error,
        review: Option<(WorkspaceId, WorktreeId)>,
        notice: Option<&OutcomeNotice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = if error == Error::Interrupted {
            "Check the Worktree"
        } else {
            title
        };
        let message = match error {
            Error::Changed => {
                "Nothing was removed. Review the updated Worktree and Terminal Sessions before removing it."
            }
            Error::Main => "The Main Worktree cannot be removed.",
            Error::Locked => "Unlock the Worktree with Git before removing it.",
            Error::Pinned => "Unpin the directory before removing it.",
            Error::Missing => "Another client or Git removed it.",
            Error::Interrupted => {
                "The server stopped before SpaceTerm could confirm the result. Review the sidebar before trying again."
            }
            Error::Location => "Choose a new or empty directory.",
            Error::Template(
                spaceterm_server::worktree::path_template::WorktreePathTemplateError::NotWritable,
            ) => {
                "The New Worktree Location directory is not writable. Choose another directory in Settings > Git."
            }
            Error::Template(_) => "Correct New Worktree Location in Settings > Git.",
            Error::Branch
            | Error::InvalidName
            | Error::BranchExists
            | Error::BranchCheckedOut
            | Error::BranchMissing => {
                "Check the selected branch and existing Worktrees before trying again."
            }
            Error::Unavailable => "Review the Worktrees in the sidebar before trying again.",
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
        let lifecycle = notice.map(|notice| notice.lifecycle());
        Alert::new(
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
        .present_with_lifecycle(
            window,
            cx,
            move |outcome, cx| {
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
            },
            move |event, cx| {
                if let Some(lifecycle) = &lifecycle {
                    lifecycle(event, cx);
                }
            },
        )
        .is_ok()
    }
}
