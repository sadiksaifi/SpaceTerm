//! The New Worktree dialog: where a Worktree's branch comes from, the branch, and its directory.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Context, Entity, EventEmitter, Render, SharedString, Task, Window,
    accesskit, div, px,
};
use spaceterm_ui::{
    ComboBox, ComboBoxItem, Dialog, DialogCloseDecision, DialogInitialFocus, DialogOutcome,
    DialogPendingCompletion, DialogSize, ModalAction, ModalActionRole, ModalId, SegmentedControl,
    SegmentedOption, TextInput, TextInputEscapeBehavior, TextInputEvent, TextInputReturnBehavior,
    TextInputVariant,
};

use super::appearance::{ChromeAppearance, gpui_color};
use super::chrome_typography::{ChromeTextStyleExt, TextRole};
use crate::platform::local_filesystem::{LocalFilesystemError, NewDirectoryTarget};
use crate::repository_status::presentation::{MAXIMUM_NAME_CHARS, sanitize_for_display};
use crate::worktrees::git::{BranchList, BranchName, WorktreeBranch, WorktreeCreateError};
use crate::worktrees::path_template::expand;
use crate::worktrees::ref_format::{BranchNameError, validate_branch_name};

const FORM_MODAL_ID: &str = "new-worktree-form";
const CREATE_ACTION_SELECTOR: &str = "new-worktree-create";
const CANCEL_ACTION_SELECTOR: &str = "new-worktree-cancel";
const BRANCHES_FAILED: &str = "SpaceTerm couldn\u{2019}t read this repository\u{2019}s branches.";
const CREATE_FAILED: &str = "Git couldn\u{2019}t create the Worktree.";

/// Runs the dialog's git work for one repository.
pub(crate) trait WorktreeFormBackend {
    /// The repository's branches, or `None` when git cannot read them.
    fn branches(&self, cx: &mut App) -> Task<Option<BranchList>>;
    fn create(
        &self,
        path: PathBuf,
        branch: WorktreeBranch,
        cx: &mut App,
    ) -> Task<Result<PathBuf, WorktreeCreateError>>;
}

/// Checks whether a new directory may go at a path.
pub(crate) type LocationProbe =
    Rc<dyn Fn(&Path) -> Result<NewDirectoryTarget, LocalFilesystemError>>;

/// What the dialog proposes from: the repository, the person's template, and its Worktrees.
pub(crate) struct WorktreeFormContext {
    /// The Main Worktree's directory name, for the template's `{repository}`.
    pub(crate) repository_name: String,
    pub(crate) template: String,
    pub(crate) home: PathBuf,
    /// Each checked-out branch and the directory name of the Worktree that has it.
    pub(crate) checked_out: Vec<(String, SharedString)>,
    /// The local branch a new branch starts from unless the person picks another.
    pub(crate) default_base: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorktreeFormEvent {
    /// Git created the Worktree at this directory.
    Created(PathBuf),
    Cancelled,
}

/// Where the new Worktree's branch comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BranchSource {
    New,
    Existing,
    Remote,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FormAction {
    Create,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FormField {
    Name,
    Branch,
    Location,
}

#[derive(Clone, Default)]
struct FormErrors {
    name: Option<SharedString>,
    branch: Option<SharedString>,
    location: Option<SharedString>,
}

impl FormErrors {
    fn first(&self, source: BranchSource) -> Option<FormField> {
        // Fields in the order the dialog shows them.
        let order: &[FormField] = match source {
            BranchSource::New => &[FormField::Name, FormField::Branch, FormField::Location],
            BranchSource::Existing => &[FormField::Branch, FormField::Location],
            BranchSource::Remote => &[FormField::Branch, FormField::Name, FormField::Location],
        };
        order
            .iter()
            .copied()
            .find(|field| self.get(*field).is_some())
    }

    fn get(&self, field: FormField) -> Option<&SharedString> {
        match field {
            FormField::Name => self.name.as_ref(),
            FormField::Branch => self.branch.as_ref(),
            FormField::Location => self.location.as_ref(),
        }
    }
}

pub(crate) struct WorktreeForm {
    backend: Rc<dyn WorktreeFormBackend>,
    context: WorktreeFormContext,
    probe: LocationProbe,
    source: BranchSource,
    name: Entity<TextInput>,
    location: Entity<TextInput>,
    base: Option<BranchName>,
    existing: Option<String>,
    remote: Option<String>,
    branches: Option<BranchList>,
    branches_failed: bool,
    /// The location and local name the dialog last proposed. A value that differs was typed.
    proposed_location: String,
    proposed_name: String,
    errors: FormErrors,
    submit_attempted: bool,
    backend_error: Option<&'static str>,
    open: bool,
    pending: bool,
    generation: u64,
    pending_path: Option<PathBuf>,
    pending_cancel: Option<DialogPendingCompletion>,
    _branches: Task<()>,
}

impl EventEmitter<WorktreeFormEvent> for WorktreeForm {}

impl WorktreeForm {
    pub(crate) fn new(
        backend: Rc<dyn WorktreeFormBackend>,
        context: WorktreeFormContext,
        probe: LocationProbe,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = text_input("new-worktree-name", "Branch name", "Required", window, cx);
        let location = text_input(
            "new-worktree-location",
            "Location",
            "Named after the branch",
            window,
            cx,
        );
        for (field, input) in [(FormField::Name, &name), (FormField::Location, &location)] {
            cx.subscribe(input, move |form, _, event: &TextInputEvent, cx| {
                if let TextInputEvent::ValueChanged(_) = event {
                    form.input_changed(field, cx);
                }
            })
            .detach();
        }
        let base = context.default_base.clone().map(BranchName::Local);
        let mut form = Self {
            backend,
            context,
            probe,
            source: BranchSource::New,
            name,
            location,
            base,
            existing: None,
            remote: None,
            branches: None,
            branches_failed: false,
            proposed_location: String::new(),
            proposed_name: String::new(),
            errors: FormErrors::default(),
            submit_attempted: false,
            backend_error: None,
            open: false,
            pending: false,
            generation: 0,
            pending_path: None,
            pending_cancel: None,
            _branches: Task::ready(()),
        };
        form.propose(cx);
        form
    }

    pub(crate) fn present(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.open {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        let branches = self.backend.branches(cx);
        let generation = self.generation;
        self._branches = cx.spawn(async move |form, cx| {
            let branches = branches.await;
            let _ = form.update(cx, |form, cx| {
                if form.generation == generation {
                    form.branches_loaded(branches, cx);
                }
            });
        });
        let owner = cx.weak_entity();
        let result_owner = owner.clone();
        let window_handle = window.window_handle();
        let dialog = Dialog::new(
            ModalId::new(FORM_MODAL_ID),
            "New Worktree",
            "New Worktree",
            vec![
                ModalAction::new(
                    FormAction::Create,
                    "Create",
                    ModalActionRole::Affirmative,
                    CREATE_ACTION_SELECTOR,
                )
                .default_action(true),
                ModalAction::new(
                    FormAction::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    CANCEL_ACTION_SELECTOR,
                ),
            ],
            DialogInitialFocus::Body(self.name.read(cx).focus_handle()),
        )
        .description(format!(
            "Check out a branch of {} in its own directory.",
            self.context.repository_name
        ))
        .size(DialogSize::Wide)
        .body(cx.entity());
        let completion = dialog.present(
            window,
            cx,
            move |request, completion, cx| {
                owner
                    .update(cx, |form, cx| {
                        form.handle_action(*request.action_id(), completion, window_handle, cx)
                    })
                    .unwrap_or(DialogCloseDecision::Deny {
                        first_invalid: None,
                    })
            },
            move |outcome, cx| {
                let _ = result_owner.update(cx, |form, cx| form.finish_dialog(outcome, cx));
            },
        );
        let Ok(_) = completion else {
            return false;
        };
        self.open = true;
        cx.notify();
        true
    }

    pub(crate) const fn is_open(&self) -> bool {
        self.open
    }

    fn branches_loaded(&mut self, branches: Option<BranchList>, cx: &mut Context<Self>) {
        self.branches_failed = branches.is_none();
        let branches = branches.unwrap_or_default();
        // The base falls back to the newest local branch when the default is gone.
        if self
            .base
            .as_ref()
            .is_none_or(|base| !branch_listed(&branches, base))
        {
            self.base = branches.local.first().cloned().map(BranchName::Local);
        }
        self.branches = Some(branches);
        self.revalidate(cx);
    }

    fn set_source(&mut self, source: BranchSource, cx: &mut Context<Self>) {
        if self.pending || self.source == source {
            return;
        }
        self.source = source;
        // The name means the new branch in both sources that create one.
        if source == BranchSource::Remote {
            self.propose_name(cx);
        }
        self.propose(cx);
        self.revalidate(cx);
    }

    fn input_changed(&mut self, field: FormField, cx: &mut Context<Self>) {
        if field == FormField::Name {
            self.propose(cx);
        }
        self.revalidate(cx);
    }

    fn choose(&mut self, field: FormField, choice: Choice, cx: &mut Context<Self>) {
        match (field, choice) {
            (FormField::Branch, Choice::Base(base)) => self.base = Some(base),
            (FormField::Branch, Choice::Existing(name)) => self.existing = Some(name),
            (FormField::Branch, Choice::Remote(remote)) => {
                self.remote = Some(remote);
                self.propose_name(cx);
            }
            _ => return,
        }
        self.backend_error = None;
        self.propose(cx);
        self.revalidate(cx);
    }

    /// Names the local branch after the chosen remote branch until the person types a name.
    fn propose_name(&mut self, cx: &mut Context<Self>) {
        let typed = self.name.read(cx).value().to_owned();
        if typed != self.proposed_name && !typed.is_empty() {
            return;
        }
        let proposed = self
            .remote
            .as_deref()
            .and_then(|remote| remote.split_once('/'))
            .map_or_else(String::new, |(_, branch)| branch.to_owned());
        self.name.update(cx, |input, cx| {
            input.set_value(proposed.clone(), cx);
        });
        self.proposed_name = proposed;
    }

    /// Fills the location from the template until the person types one.
    fn propose(&mut self, cx: &mut Context<Self>) {
        let typed = self.location.read(cx).value().to_owned();
        if typed != self.proposed_location {
            return;
        }
        let branch = self.branch_name(cx);
        let proposed = if branch.is_empty() {
            String::new()
        } else {
            expand(
                &self.context.template,
                &self.context.home,
                &self.context.repository_name,
                &branch,
            )
            .map(|path| compact_home(&path, &self.context.home))
            .unwrap_or_default()
        };
        self.location.update(cx, |input, cx| {
            input.set_value(proposed.clone(), cx);
        });
        self.proposed_location = proposed;
    }

    /// The branch the Worktree checks out, as the template names its directory.
    fn branch_name(&self, cx: &App) -> String {
        match self.source {
            BranchSource::New | BranchSource::Remote => {
                self.name.read(cx).value().trim().to_owned()
            }
            BranchSource::Existing => self.existing.clone().unwrap_or_default(),
        }
    }

    fn revalidate(&mut self, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        self.errors = self.validate(cx).err().unwrap_or_default();
        cx.notify();
    }

    fn visible_error(&self, field: FormField, cx: &App) -> Option<SharedString> {
        let error = self.errors.get(field)?;
        // An invalid name shows while the person types it; a missing value only after Create.
        let typed = field == FormField::Name && !self.name.read(cx).value().is_empty();
        (self.submit_attempted || typed).then(|| error.clone())
    }

    /// The Worktree the fields describe, or what to fix.
    fn validate(&self, cx: &App) -> Result<(PathBuf, WorktreeBranch), FormErrors> {
        let mut errors = FormErrors::default();
        let name = self.name.read(cx).value().trim().to_owned();
        let local = self
            .branches
            .as_ref()
            .map_or(&[][..], |branches| &branches.local[..]);
        let name_error = |name: &str| match validate_branch_name(name) {
            Err(error) => Some(name_message(error)),
            Ok(()) if local.iter().any(|branch| branch == name) => {
                Some("A branch with this name already exists.".into())
            }
            Ok(()) => None,
        };
        let branch = match self.source {
            BranchSource::New => {
                errors.name = name_error(&name);
                if self.base.is_none() {
                    errors.branch = Some("Choose a branch to start from.".into());
                }
                self.base.clone().map(|base| WorktreeBranch::New {
                    name: name.clone(),
                    base,
                })
            }
            BranchSource::Existing => match &self.existing {
                None => {
                    errors.branch = Some("Choose a branch.".into());
                    None
                }
                Some(name) => Some(WorktreeBranch::Existing { name: name.clone() }),
            },
            BranchSource::Remote => {
                errors.name = name_error(&name);
                if self.remote.is_none() {
                    errors.branch = Some("Choose a remote branch.".into());
                }
                self.remote.clone().map(|remote| WorktreeBranch::Remote {
                    remote,
                    name: name.clone(),
                })
            }
        };
        let location = self.location.read(cx).value().trim().to_owned();
        let path = if location.is_empty() {
            errors.location = Some("Enter a location.".into());
            None
        } else {
            let path = expand_home(&location, &self.context.home);
            if path.is_absolute() {
                Some(path)
            } else {
                errors.location = Some("Enter a full path that starts with / or ~/.".into());
                None
            }
        };
        match (path, branch) {
            (Some(path), Some(branch)) if errors.first(self.source).is_none() => Ok((path, branch)),
            _ => Err(errors),
        }
    }

    fn handle_action(
        &mut self,
        action: FormAction,
        completion: DialogPendingCompletion,
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) -> DialogCloseDecision {
        match action {
            FormAction::Cancel if self.pending => {
                if self.pending_cancel.is_some() {
                    return DialogCloseDecision::Deny {
                        first_invalid: None,
                    };
                }
                // A started write runs to its end; Cancel waits for it.
                self.pending_cancel = Some(completion);
                DialogCloseDecision::Pending
            }
            FormAction::Cancel => DialogCloseDecision::Allow,
            FormAction::Create if self.pending => DialogCloseDecision::Deny {
                first_invalid: None,
            },
            FormAction::Create => {
                self.submit_attempted = true;
                self.backend_error = None;
                let checked =
                    self.validate(cx)
                        .and_then(|(path, branch)| match (self.probe)(&path) {
                            Ok(NewDirectoryTarget::Free) => Ok((path, branch)),
                            Ok(NewDirectoryTarget::Occupied) => Err(FormErrors {
                                location: Some(
                                    "This folder isn\u{2019}t empty. Choose a new or empty folder."
                                        .into(),
                                ),
                                ..FormErrors::default()
                            }),
                            Err(_) => Err(FormErrors {
                                location: Some("SpaceTerm can\u{2019}t use this location.".into()),
                                ..FormErrors::default()
                            }),
                        });
                match checked {
                    Ok((path, branch)) => {
                        self.start_create(path, branch, completion, window_handle, cx);
                        DialogCloseDecision::Pending
                    }
                    Err(errors) => {
                        self.errors = errors;
                        let first_invalid = self
                            .errors
                            .first(self.source)
                            .and_then(|field| self.focus_for_field(field, cx));
                        cx.notify();
                        DialogCloseDecision::Deny { first_invalid }
                    }
                }
            }
        }
    }

    fn start_create(
        &mut self,
        path: PathBuf,
        branch: WorktreeBranch,
        completion: DialogPendingCompletion,
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        self.pending = true;
        self.set_editable(false, cx);
        let generation = self.generation;
        let task = self.backend.create(path, branch, cx);
        cx.notify();
        cx.spawn(async move |form, cx| {
            let result = task.await;
            let Ok(Some(settlement)) = form.update(cx, |form, cx| {
                form.settle_create(generation, result, completion, cx)
            }) else {
                return;
            };
            let _ = window_handle.update(cx, |_, window, cx| settlement.apply(window, cx));
        })
        .detach();
    }

    fn settle_create(
        &mut self,
        generation: u64,
        result: Result<PathBuf, WorktreeCreateError>,
        primary: DialogPendingCompletion,
        cx: &mut Context<Self>,
    ) -> Option<Settlement> {
        if !self.open || !self.pending || self.generation != generation {
            return None;
        }
        if let Ok(path) = result {
            self.pending_path = Some(path);
            return Some(Settlement::Success { primary });
        }
        self.pending = false;
        self.pending_path = None;
        self.set_editable(true, cx);
        let (field, message): (Option<FormField>, SharedString) = match result {
            Err(WorktreeCreateError::InvalidName) => (
                Some(FormField::Name),
                "Git doesn\u{2019}t accept this name.".into(),
            ),
            Err(WorktreeCreateError::BranchExists) => (
                Some(FormField::Name),
                "A branch with this name already exists.".into(),
            ),
            Err(WorktreeCreateError::BranchCheckedOut) => (
                Some(FormField::Branch),
                "Another Worktree has this branch checked out.".into(),
            ),
            Err(WorktreeCreateError::BranchMissing) => (
                Some(FormField::Branch),
                "This branch no longer exists.".into(),
            ),
            Err(WorktreeCreateError::Failed) | Ok(_) => (None, CREATE_FAILED.into()),
        };
        self.errors = FormErrors::default();
        match field {
            Some(FormField::Name) => self.errors.name = Some(message),
            Some(FormField::Branch) => self.errors.branch = Some(message),
            Some(FormField::Location) => self.errors.location = Some(message),
            None => self.backend_error = Some(CREATE_FAILED),
        }
        cx.notify();
        Some(Settlement::Failure {
            primary,
            cancel: self.pending_cancel.take(),
            first_invalid: field.and_then(|field| self.focus_for_field(field, cx)),
        })
    }

    fn finish_dialog(&mut self, outcome: DialogOutcome<FormAction>, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.pending = false;
        self.pending_cancel = None;
        self.generation = self.generation.wrapping_add(1);
        self.set_editable(true, cx);
        match (outcome, self.pending_path.take()) {
            (
                DialogOutcome::Completed {
                    action_id: FormAction::Create,
                    ..
                },
                Some(path),
            ) => cx.emit(WorktreeFormEvent::Created(path)),
            _ => cx.emit(WorktreeFormEvent::Cancelled),
        }
        cx.notify();
    }

    fn focus_for_field(&self, field: FormField, cx: &App) -> Option<gpui::FocusHandle> {
        match field {
            FormField::Name => Some(self.name.read(cx).focus_handle()),
            FormField::Location => Some(self.location.read(cx).focus_handle()),
            FormField::Branch => None,
        }
    }

    fn set_editable(&self, editable: bool, cx: &mut Context<Self>) {
        for input in [&self.name, &self.location] {
            input.update(cx, |input, cx| input.set_editable(editable, cx));
        }
    }
}

/// A combo box choice, by the source it belongs to.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Choice {
    Base(BranchName),
    Existing(String),
    Remote(String),
}

enum Settlement {
    Success {
        primary: DialogPendingCompletion,
    },
    Failure {
        primary: DialogPendingCompletion,
        cancel: Option<DialogPendingCompletion>,
        first_invalid: Option<gpui::FocusHandle>,
    },
}

impl Settlement {
    fn apply(self, window: &Window, cx: &mut App) {
        match self {
            Self::Success { primary } => {
                let _ = primary.allow(window, None, cx);
            }
            Self::Failure {
                primary,
                cancel,
                first_invalid,
            } => {
                let _ = primary.deny(window, first_invalid.clone(), cx);
                if let Some(cancel) = cancel {
                    let _ = cancel.deny(window, first_invalid, cx);
                }
            }
        }
    }
}

impl Render for WorktreeForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = super::appearance::shared_chrome(cx);
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let owner = cx.weak_entity();
        let enabled = !self.pending;
        let source = SegmentedControl::new(
            "new-worktree-source",
            "Branch",
            &self.source,
            [
                (BranchSource::New, "New Branch", "new"),
                (BranchSource::Existing, "Existing Branch", "existing"),
                (BranchSource::Remote, "Remote Branch", "remote"),
            ]
            .into_iter()
            .map(|(source, label, slug)| {
                SegmentedOption::new(source, label)
                    .debug_selector(format!("new-worktree-source-{slug}"))
            })
            .collect(),
        )
        .expect("three branch sources are within the bounded option set")
        .full_width(true)
        .disabled(!enabled)
        .debug_selector("new-worktree-source")
        .on_change(move |change, _, cx| {
            let source = *change.requested();
            let _ = owner.update(cx, |form, cx| form.set_source(source, cx));
        });
        let branch_field = match self.source {
            BranchSource::New => self.branch_picker(
                "Start From",
                self.base.clone().map(Choice::Base),
                self.base_items(),
                &appearance,
                cx,
            ),
            BranchSource::Existing => self.branch_picker(
                "Branch",
                self.existing.clone().map(Choice::Existing),
                self.existing_items(),
                &appearance,
                cx,
            ),
            BranchSource::Remote => self.branch_picker(
                "Remote Branch",
                self.remote.clone().map(Choice::Remote),
                self.remote_items(),
                &appearance,
                cx,
            ),
        };
        let name_field = |label: &'static str, form: &Self| {
            input_field(
                &appearance,
                enabled,
                label,
                form.name.clone(),
                form.name.read(cx).focus_handle(),
                form.visible_error(FormField::Name, cx),
                "new-worktree-name-frame",
                cx,
            )
        };
        let mut notes = vec![
            "Git hooks don\u{2019}t run, so a post-checkout hook\u{2019}s setup won\u{2019}t happen.",
        ];
        if self.source == BranchSource::Remote {
            notes.insert(
                0,
                "Uses branches already fetched. SpaceTerm doesn\u{2019}t fetch.",
            );
        }
        div()
            .chrome_text(appearance.typography.style(TextRole::Body))
            .flex()
            .flex_col()
            .gap(appearance.spacing(16.0))
            .child(source)
            .map(|body| match self.source {
                BranchSource::New => body
                    .child(name_field("Branch Name", self))
                    .child(branch_field),
                BranchSource::Existing => body.child(branch_field),
                BranchSource::Remote => body
                    .child(branch_field)
                    .child(name_field("Local Branch Name", self)),
            })
            .child(input_field(
                &appearance,
                enabled,
                "Location",
                self.location.clone(),
                self.location.read(cx).focus_handle(),
                self.visible_error(FormField::Location, cx),
                "new-worktree-location-frame",
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(4.0))
                    .chrome_text(appearance.typography.style(TextRole::Caption))
                    .text_color(gpui_color(colors.text_muted))
                    .children(notes.into_iter().enumerate().map(|(index, note)| {
                        div()
                            .id(("new-worktree-note", index))
                            .role(accesskit::Role::Label)
                            .aria_value(note)
                            .child(note)
                    })),
            )
            .when_some(
                self.backend_error
                    .or(self.branches_failed.then_some(BRANCHES_FAILED)),
                |body, error| {
                    body.child(
                        div()
                            .id("new-worktree-error")
                            .role(accesskit::Role::Label)
                            .aria_value(error)
                            .debug_selector(|| "new-worktree-error".to_owned())
                            .text_color(gpui_color(colors.error))
                            .child(error),
                    )
                },
            )
    }
}

impl WorktreeForm {
    fn base_items(&self) -> Vec<ComboBoxItem<Choice>> {
        let Some(branches) = &self.branches else {
            return Vec::new();
        };
        let local = branches.local.iter().map(|name| {
            ComboBoxItem::new(
                Choice::Base(BranchName::Local(name.clone())),
                sanitize_for_display(name, MAXIMUM_NAME_CHARS),
            )
        });
        let remote = branches.remote.iter().map(|name| {
            ComboBoxItem::new(
                Choice::Base(BranchName::Remote(name.clone())),
                sanitize_for_display(name, MAXIMUM_NAME_CHARS),
            )
            .description("Remote branch")
        });
        local.chain(remote).collect()
    }

    fn existing_items(&self) -> Vec<ComboBoxItem<Choice>> {
        let Some(branches) = &self.branches else {
            return Vec::new();
        };
        let checked_out_in = |name: &String| {
            self.context
                .checked_out
                .iter()
                .find(|(branch, _)| branch == name)
                .map(|(_, directory)| directory)
        };
        // The branches a person can pick come first; each group keeps git's newest-first order.
        let (available, taken): (Vec<_>, Vec<_>) = branches
            .local
            .iter()
            .partition(|name| checked_out_in(name).is_none());
        available
            .into_iter()
            .chain(taken)
            .map(|name| {
                let item = ComboBoxItem::new(
                    Choice::Existing(name.clone()),
                    sanitize_for_display(name, MAXIMUM_NAME_CHARS),
                )
                .debug_selector(format!("new-worktree-branch-{name}"));
                match checked_out_in(name) {
                    Some(directory) => item
                        .description(format!(
                            "Checked out in {}",
                            sanitize_for_display(directory, MAXIMUM_NAME_CHARS)
                        ))
                        .disabled(true),
                    None => item,
                }
            })
            .collect()
    }

    fn remote_items(&self) -> Vec<ComboBoxItem<Choice>> {
        let Some(branches) = &self.branches else {
            return Vec::new();
        };
        branches
            .remote
            .iter()
            .map(|name| {
                ComboBoxItem::new(
                    Choice::Remote(name.clone()),
                    sanitize_for_display(name, MAXIMUM_NAME_CHARS),
                )
            })
            .collect()
    }

    fn branch_picker(
        &self,
        label: &'static str,
        selected: Option<Choice>,
        items: Vec<ComboBoxItem<Choice>>,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
        let owner = cx.weak_entity();
        let error = self.visible_error(FormField::Branch, cx);
        let prompt = if self.branches.is_none() {
            "Loading Branches\u{2026}"
        } else if items.is_empty() {
            "No Branches"
        } else {
            "Choose a Branch"
        };
        div()
            .flex()
            .flex_col()
            .gap(appearance.spacing(6.0))
            .child(div().text_color(gpui_color(colors.text_muted)).child(label))
            .child(
                ComboBox::new("new-worktree-branch", label, selected, prompt, items)
                    .full_width(true)
                    .busy(self.branches.is_none())
                    .disabled(self.pending)
                    .debug_selector("new-worktree-branch")
                    .on_accept(move |acceptance, _, cx| {
                        let choice = acceptance.item_id().clone();
                        let _ = owner.update(cx, |form, cx| {
                            form.choose(FormField::Branch, choice, cx);
                        });
                    }),
            )
            .when_some(error, |field, error| {
                field.child(
                    div()
                        .id("new-worktree-branch-error")
                        .role(accesskit::Role::Label)
                        .aria_value(error.clone())
                        .debug_selector(|| "new-worktree-branch-error".to_owned())
                        .text_color(gpui_color(colors.error))
                        .child(error),
                )
            })
            .into_any_element()
    }
}

fn branch_listed(branches: &BranchList, branch: &BranchName) -> bool {
    match branch {
        BranchName::Local(name) => branches.local.contains(name),
        BranchName::Remote(name) => branches.remote.contains(name),
    }
}

fn name_message(error: BranchNameError) -> SharedString {
    match error {
        BranchNameError::Empty => "Enter a branch name.",
        BranchNameError::LeadingDash => "Invalid name: it can\u{2019}t start with a dash.",
        BranchNameError::InvalidCharacter => {
            "Invalid name: spaces and ~ ^ : ? * [ \\ aren\u{2019}t allowed."
        }
        BranchNameError::InvalidSequence => {
            "Invalid name: \u{201c}..\u{201d}, \u{201c}//\u{201d}, and \u{201c}@{\u{201d} aren\u{2019}t allowed."
        }
        BranchNameError::InvalidComponent => {
            "Invalid name: a part can\u{2019}t start with a dot or end with \u{201c}.lock\u{201d}."
        }
        BranchNameError::InvalidEnd => {
            "Invalid name: it can\u{2019}t start or end with a slash or end with a dot."
        }
        BranchNameError::Reserved => "Invalid name: Git reserves this name.",
    }
    .into()
}

/// `~/…` as the home folder's path.
fn expand_home(location: &str, home: &Path) -> PathBuf {
    match location.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if location == "~" => home.to_path_buf(),
        None => PathBuf::from(location),
    }
}

/// A path under the home folder as `~/…`, the way people type it.
fn compact_home(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

fn text_input(
    id: &'static str,
    accessibility_name: &'static str,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<WorktreeForm>,
) -> Entity<TextInput> {
    cx.new(|cx| {
        TextInput::new(id, accessibility_name, String::new(), window, cx)
            .placeholder(placeholder)
            .variant(TextInputVariant::Bare)
            .return_behavior(TextInputReturnBehavior::Propagate)
            .escape_behavior(TextInputEscapeBehavior::Propagate)
            .debug_selector(id)
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "A field composes its prepared presentation, editor state, and validation semantics"
)]
fn input_field(
    appearance: &ChromeAppearance,
    enabled: bool,
    label: &'static str,
    input: Entity<TextInput>,
    input_focus: gpui::FocusHandle,
    error: Option<SharedString>,
    error_selector: &'static str,
    cx: &App,
) -> impl IntoElement {
    let colors = appearance.host_colors(spaceterm_ui::ControlHost::Floating);
    div()
        .flex()
        .flex_col()
        .gap(appearance.spacing(6.0))
        .child(div().text_color(gpui_color(colors.text_muted)).child(label))
        .child(
            spaceterm_ui::field_frame(
                error_selector,
                &input_focus,
                spaceterm_ui::FieldState::default()
                    .disabled(!enabled)
                    .invalid(error.is_some()),
                super::chrome_geometry::RadiusRole::Control.pixels(),
                cx,
            )
            .h(appearance.height(28.0, 13.0))
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .items_center()
            .px(appearance.spacing(8.0))
            .text_color(gpui_color(colors.text))
            .on_click(move |_, window, cx| {
                input_focus.focus(window, cx);
                cx.stop_propagation();
            })
            .child(input),
        )
        .when_some(error, |field, error| {
            field.child(
                div()
                    .id(format!("{error_selector}-message"))
                    .role(accesskit::Role::Label)
                    .aria_value(error.clone())
                    .debug_selector(move || format!("{error_selector}-message"))
                    .mt(px(0.0))
                    .text_color(gpui_color(colors.error))
                    .child(error),
            )
        })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use spaceterm_ui::ModalLayer;

    use super::*;

    const HOME: &str = "/Users/person";

    #[derive(Default)]
    struct ScriptedBackend {
        results: RefCell<Vec<Result<(), WorktreeCreateError>>>,
        created: RefCell<Vec<(PathBuf, WorktreeBranch)>>,
    }

    impl WorktreeFormBackend for ScriptedBackend {
        fn branches(&self, _: &mut App) -> Task<Option<BranchList>> {
            Task::ready(Some(BranchList {
                local: vec!["main".into(), "fix/typo".into(), "feature/login".into()],
                remote: vec!["origin/main".into(), "origin/release/v2".into()],
            }))
        }

        fn create(
            &self,
            path: PathBuf,
            branch: WorktreeBranch,
            _: &mut App,
        ) -> Task<Result<PathBuf, WorktreeCreateError>> {
            self.created.borrow_mut().push((path.clone(), branch));
            Task::ready(
                self.results
                    .borrow_mut()
                    .pop()
                    .unwrap_or(Ok(()))
                    .map(|()| path),
            )
        }
    }

    struct Harness {
        form: Entity<WorktreeForm>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    type Events = Rc<RefCell<Vec<WorktreeFormEvent>>>;

    fn form_window(
        backend: Rc<ScriptedBackend>,
        free: bool,
        cx: &mut TestAppContext,
    ) -> (Entity<WorktreeForm>, Events, &mut VisualTestContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let events: Events = Rc::default();
        let captured = Rc::clone(&events);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let context = WorktreeFormContext {
                repository_name: "app".into(),
                template: crate::worktrees::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE.into(),
                home: HOME.into(),
                checked_out: vec![
                    ("main".into(), "app".into()),
                    ("feature/login".into(), "feature-login".into()),
                ],
                default_base: Some("main".into()),
            };
            let form = cx.new(|cx| {
                let probe: LocationProbe = Rc::new(move |_| {
                    Ok(if free {
                        NewDirectoryTarget::Free
                    } else {
                        NewDirectoryTarget::Occupied
                    })
                });
                WorktreeForm::new(backend, context, probe, window, cx)
            });
            cx.subscribe(&form, move |_, _, event, _| {
                captured.borrow_mut().push(event.clone());
            })
            .detach();
            Harness { form }
        });
        let form = harness.read_with(cx, |harness, _| harness.form.clone());
        cx.update(|window, cx| {
            window.activate_window();
            form.update(cx, |form, cx| assert!(form.present(window, cx)));
        });
        cx.run_until_parked();
        (form, events, cx)
    }

    /// Types into a field as the person would, which marks the value as theirs.
    fn type_into(
        form: &Entity<WorktreeForm>,
        field: FormField,
        value: &str,
        cx: &mut VisualTestContext,
    ) {
        form.update(cx, |form, cx| {
            let input = match field {
                FormField::Name => form.name.clone(),
                FormField::Location | FormField::Branch => form.location.clone(),
            };
            input.update(cx, |input, cx| {
                input.set_value(value, cx);
            });
            form.input_changed(field, cx);
        });
        cx.run_until_parked();
    }

    fn location(form: &Entity<WorktreeForm>, cx: &mut VisualTestContext) -> String {
        form.read_with(cx, |form, cx| form.location.read(cx).value().to_owned())
    }

    fn visible_errors(
        form: &Entity<WorktreeForm>,
        cx: &mut VisualTestContext,
    ) -> [Option<SharedString>; 3] {
        form.read_with(cx, |form, cx| {
            [FormField::Name, FormField::Branch, FormField::Location]
                .map(|field| form.visible_error(field, cx))
        })
    }

    fn create(cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds("modal-action-new-worktree-create")
            .expect("the Create action");
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn branch_picker_should_sanitize_labels_and_keep_raw_branch_choices(cx: &mut TestAppContext) {
        let (form, _, cx) = form_window(Rc::new(ScriptedBackend::default()), true, cx);
        let local = "feature/\u{202e}search".to_owned();
        let remote = format!("origin/{local}");
        form.update(cx, |form, cx| {
            form.branches_loaded(
                Some(BranchList {
                    local: vec![local.clone()],
                    remote: vec![remote.clone()],
                }),
                cx,
            );
        });

        let choices = form.read_with(cx, |form, _| {
            form.base_items()
                .into_iter()
                .chain(form.existing_items())
                .chain(form.remote_items())
                .map(|item| (item.id().clone(), item.label().to_owned()))
                .collect::<Vec<_>>()
        });

        assert_eq!(
            choices,
            [
                (
                    Choice::Base(BranchName::Local(local.clone())),
                    "feature/\u{fffd}search".into()
                ),
                (
                    Choice::Base(BranchName::Remote(remote.clone())),
                    "origin/feature/\u{fffd}search".into()
                ),
                (Choice::Existing(local), "feature/\u{fffd}search".into()),
                (
                    Choice::Remote(remote),
                    "origin/feature/\u{fffd}search".into()
                ),
            ]
        );
    }

    #[gpui::test]
    fn a_new_branch_should_propose_its_location_and_create_from_the_default_base(
        cx: &mut TestAppContext,
    ) {
        let backend = Rc::new(ScriptedBackend::default());
        let (form, events, cx) = form_window(Rc::clone(&backend), true, cx);

        type_into(&form, FormField::Name, "feature/search", cx);
        let proposed = location(&form, cx);
        create(cx);

        assert_eq!(proposed, "~/.worktrees/app/feature-search");
        assert_eq!(
            *backend.created.borrow(),
            [(
                PathBuf::from("/Users/person/.worktrees/app/feature-search"),
                WorktreeBranch::New {
                    name: "feature/search".into(),
                    base: BranchName::Local("main".into()),
                },
            )]
        );
        assert_eq!(
            *events.borrow(),
            [WorktreeFormEvent::Created(PathBuf::from(
                "/Users/person/.worktrees/app/feature-search"
            ))]
        );
    }

    #[gpui::test]
    fn the_branch_source_should_be_chosen_from_the_keyboard(cx: &mut TestAppContext) {
        let (form, _, cx) = form_window(Rc::default(), true, cx);

        cx.simulate_keystrokes("shift-tab right");
        cx.run_until_parked();

        assert_eq!(
            form.read_with(cx, |form, _| form.source),
            BranchSource::Existing
        );
    }

    #[gpui::test]
    fn names_git_refuses_or_already_has_should_show_while_typing(cx: &mut TestAppContext) {
        let (form, _, cx) = form_window(Rc::default(), true, cx);

        type_into(&form, FormField::Name, "my branch", cx);
        let invalid = visible_errors(&form, cx);
        type_into(&form, FormField::Name, "fix/typo", cx);
        let taken = visible_errors(&form, cx);
        type_into(&form, FormField::Name, "fix/other", cx);
        let valid = visible_errors(&form, cx);

        assert_eq!(
            (invalid[0].as_deref(), taken[0].as_deref(), valid),
            (
                Some("Invalid name: spaces and ~ ^ : ? * [ \\ aren\u{2019}t allowed."),
                Some("A branch with this name already exists."),
                [None, None, None],
            )
        );
    }

    #[gpui::test]
    fn a_typed_location_should_stay_when_the_branch_name_changes(cx: &mut TestAppContext) {
        let (form, _, cx) = form_window(Rc::default(), true, cx);

        type_into(&form, FormField::Name, "one", cx);
        type_into(&form, FormField::Location, "/srv/work/one", cx);
        type_into(&form, FormField::Name, "two", cx);

        assert_eq!(location(&form, cx), "/srv/work/one");
    }

    #[gpui::test]
    fn existing_branches_should_offer_only_those_no_worktree_has_checked_out(
        cx: &mut TestAppContext,
    ) {
        let backend = Rc::new(ScriptedBackend::default());
        let (form, _, cx) = form_window(Rc::clone(&backend), true, cx);
        form.update(cx, |form, cx| form.set_source(BranchSource::Existing, cx));

        let offered = form.read_with(cx, |form, _| {
            form.existing_items()
                .iter()
                .map(|item| {
                    (
                        item.label().to_owned(),
                        item.description_text().map(str::to_owned),
                    )
                })
                .collect::<Vec<_>>()
        });
        form.update(cx, |form, cx| {
            form.choose(FormField::Branch, Choice::Existing("fix/typo".into()), cx);
        });
        let proposed = location(&form, cx);
        create(cx);

        assert_eq!(
            offered,
            [
                ("fix/typo".to_owned(), None),
                ("main".to_owned(), Some("Checked out in app".to_owned())),
                (
                    "feature/login".to_owned(),
                    Some("Checked out in feature-login".to_owned())
                ),
            ]
        );
        assert_eq!(proposed, "~/.worktrees/app/fix-typo");
        assert_eq!(
            backend.created.borrow()[0].1,
            WorktreeBranch::Existing {
                name: "fix/typo".into()
            }
        );
    }

    #[gpui::test]
    fn a_remote_branch_should_name_its_local_branch_until_the_person_types_one(
        cx: &mut TestAppContext,
    ) {
        let backend = Rc::new(ScriptedBackend::default());
        let (form, _, cx) = form_window(Rc::clone(&backend), true, cx);
        form.update(cx, |form, cx| {
            form.set_source(BranchSource::Remote, cx);
            form.choose(
                FormField::Branch,
                Choice::Remote("origin/release/v2".into()),
                cx,
            );
        });
        let proposed = form.read_with(cx, |form, cx| form.name.read(cx).value().to_owned());
        let proposed_location = location(&form, cx);
        type_into(&form, FormField::Name, "v2", cx);
        form.update(cx, |form, cx| {
            form.choose(FormField::Branch, Choice::Remote("origin/main".into()), cx);
        });
        let kept = form.read_with(cx, |form, cx| form.name.read(cx).value().to_owned());
        create(cx);

        assert_eq!(
            (proposed.as_str(), proposed_location.as_str(), kept.as_str()),
            ("release/v2", "~/.worktrees/app/release-v2", "v2")
        );
        assert_eq!(
            backend.created.borrow()[0].1,
            WorktreeBranch::Remote {
                remote: "origin/main".into(),
                name: "v2".into(),
            }
        );
    }

    #[gpui::test]
    fn an_occupied_location_should_keep_the_dialog_open_without_running_git(
        cx: &mut TestAppContext,
    ) {
        let backend = Rc::new(ScriptedBackend::default());
        let (form, events, cx) = form_window(Rc::clone(&backend), false, cx);

        type_into(&form, FormField::Name, "fresh", cx);
        create(cx);

        assert_eq!(
            visible_errors(&form, cx)[2].as_deref(),
            Some("This folder isn\u{2019}t empty. Choose a new or empty folder.")
        );
        assert!(backend.created.borrow().is_empty());
        assert!(events.borrow().is_empty());
        assert!(form.read_with(cx, |form, _| form.is_open()));
    }

    #[gpui::test]
    fn a_refused_create_should_show_its_reason_on_the_field_and_stay_open(cx: &mut TestAppContext) {
        let backend = Rc::new(ScriptedBackend::default());
        backend
            .results
            .borrow_mut()
            .push(Err(WorktreeCreateError::BranchExists));
        let (form, events, cx) = form_window(Rc::clone(&backend), true, cx);

        type_into(&form, FormField::Name, "fresh", cx);
        create(cx);

        assert_eq!(
            visible_errors(&form, cx)[0].as_deref(),
            Some("A branch with this name already exists.")
        );
        assert!(events.borrow().is_empty());
        assert!(form.read_with(cx, |form, _| form.is_open() && !form.pending));
        assert!(cx.update(|window, cx| {
            form.read(cx)
                .name
                .read(cx)
                .focus_handle()
                .is_focused(window)
        }));
    }
}
