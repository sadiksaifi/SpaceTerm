#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the managed SSH host form lands before its Workspace Manager integration"
    )
)]

use std::num::NonZeroU16;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyWindowHandle, App, Context, Entity, EventEmitter, Render, Task, Window, div, px};
use spaceterm_ui::{
    Dialog, DialogCloseDecision, DialogCompletion, DialogInitialFocus, DialogOutcome,
    DialogPendingCompletion, DialogSize, ModalAction, ModalActionRole, ModalId, TextInput,
    TextInputEscapeBehavior, TextInputEvent, TextInputReturnBehavior, TextInputVariant,
};

use crate::ssh::destination::SshHostAlias;
use crate::ssh::managed_hosts::{
    ManagedSshHost, ManagedSshHostField, ManagedSshHostValidationError, ManagedSshHostValueError,
};
use crate::theme::{ACTIVE_THEME, Color};

const FORM_MODAL_ID: &str = "managed-ssh-host-form";
const SAVE_ACTION_SELECTOR: &str = "managed-ssh-host-save";
const CANCEL_ACTION_SELECTOR: &str = "managed-ssh-host-cancel";
const SAVE_FAILURE_MESSAGE: &str =
    "SpaceTerm couldn\u{2019}t save this SSH host. Check permissions and try again.";
const COLLISION_MESSAGE: &str = "That SSH host alias is already configured.";
const HOST_IN_USE_MESSAGE: &str = "This SSH host is in use by a Remote Project Workspace. Close that Workspace before editing it.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ManagedHostFormBackendError {
    AliasCollision,
    HostInUse,
    SaveFailed,
}

pub(super) trait ManagedHostFormBackend: Send + Sync {
    fn save(
        &self,
        host: ManagedSshHost,
        editing_alias: Option<SshHostAlias>,
    ) -> Task<Result<(), ManagedHostFormBackendError>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SshHostFormMode {
    Add,
    Edit(ManagedSshHost),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SshHostFormEvent {
    StateChanged,
    SavedAndConnect(ManagedSshHost),
    Saved(ManagedSshHost),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SshHostFormAction {
    Save,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SshHostFormField {
    Alias,
    HostName,
    User,
    Port,
    IdentityFile,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SshHostFormValues {
    alias: String,
    host_name: String,
    user: String,
    port: String,
    identity_file: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SshHostFormErrors {
    alias: Option<&'static str>,
    host_name: Option<&'static str>,
    user: Option<&'static str>,
    port: Option<&'static str>,
    identity_file: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SshHostFormTouched {
    alias: bool,
    host_name: bool,
    user: bool,
    port: bool,
    identity_file: bool,
}

impl SshHostFormTouched {
    fn mark(&mut self, field: SshHostFormField) {
        match field {
            SshHostFormField::Alias => self.alias = true,
            SshHostFormField::HostName => self.host_name = true,
            SshHostFormField::User => self.user = true,
            SshHostFormField::Port => self.port = true,
            SshHostFormField::IdentityFile => self.identity_file = true,
        }
    }

    fn contains(self, field: SshHostFormField) -> bool {
        match field {
            SshHostFormField::Alias => self.alias,
            SshHostFormField::HostName => self.host_name,
            SshHostFormField::User => self.user,
            SshHostFormField::Port => self.port,
            SshHostFormField::IdentityFile => self.identity_file,
        }
    }
}

impl SshHostFormErrors {
    fn first_invalid(&self) -> Option<SshHostFormField> {
        [
            (SshHostFormField::Alias, self.alias),
            (SshHostFormField::HostName, self.host_name),
            (SshHostFormField::User, self.user),
            (SshHostFormField::Port, self.port),
            (SshHostFormField::IdentityFile, self.identity_file),
        ]
        .into_iter()
        .find_map(|(field, error)| error.map(|_| field))
    }

    fn for_field(&self, field: SshHostFormField) -> Option<&'static str> {
        match field {
            SshHostFormField::Alias => self.alias,
            SshHostFormField::HostName => self.host_name,
            SshHostFormField::User => self.user,
            SshHostFormField::Port => self.port,
            SshHostFormField::IdentityFile => self.identity_file,
        }
    }
}

struct SshHostFormValidation {
    host: Option<ManagedSshHost>,
    errors: SshHostFormErrors,
}

pub(super) struct SshHostForm {
    backend: Arc<dyn ManagedHostFormBackend>,
    mode: SshHostFormMode,
    alias: Entity<TextInput>,
    host_name: Entity<TextInput>,
    user: Entity<TextInput>,
    port: Entity<TextInput>,
    identity_file: Entity<TextInput>,
    errors: SshHostFormErrors,
    touched: SshHostFormTouched,
    submit_attempted: bool,
    backend_error: Option<&'static str>,
    open: bool,
    pending: bool,
    lifecycle_generation: u64,
    operation_generation: u64,
    pending_host: Option<ManagedSshHost>,
    pending_cancel: Option<DialogPendingCompletion>,
    presentation: Option<DialogCompletion>,
}

impl EventEmitter<SshHostFormEvent> for SshHostForm {}

impl SshHostForm {
    pub(super) fn new(
        mode: SshHostFormMode,
        backend: Arc<dyn ManagedHostFormBackend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let values = initial_values(&mode);
        let alias = text_input(
            "managed-ssh-host-alias",
            "Alias",
            values.alias,
            "work",
            window,
            cx,
        );
        let host_name = text_input(
            "managed-ssh-host-name",
            "Host name",
            values.host_name,
            "server.example.com",
            window,
            cx,
        );
        let user = text_input(
            "managed-ssh-host-user",
            "User",
            values.user,
            "deploy",
            window,
            cx,
        );
        let port = text_input(
            "managed-ssh-host-port",
            "Port",
            values.port,
            "22",
            window,
            cx,
        );
        let identity_file = text_input(
            "managed-ssh-host-identity-file",
            "Identity file",
            values.identity_file,
            "~/.ssh/id_ed25519",
            window,
            cx,
        );
        for (field, input) in [
            (SshHostFormField::Alias, &alias),
            (SshHostFormField::HostName, &host_name),
            (SshHostFormField::User, &user),
            (SshHostFormField::Port, &port),
            (SshHostFormField::IdentityFile, &identity_file),
        ] {
            cx.subscribe(
                input,
                move |form, _, event: &TextInputEvent, cx| match event {
                    TextInputEvent::ValueChanged(_) => form.revalidate(cx),
                    TextInputEvent::FocusLost => {
                        form.touched.mark(field);
                        form.revalidate(cx);
                    }
                    _ => {}
                },
            )
            .detach();
        }
        let mut form = Self {
            backend,
            mode,
            alias,
            host_name,
            user,
            port,
            identity_file,
            errors: SshHostFormErrors::default(),
            touched: SshHostFormTouched::default(),
            submit_attempted: false,
            backend_error: None,
            open: false,
            pending: false,
            lifecycle_generation: 0,
            operation_generation: 0,
            pending_host: None,
            pending_cancel: None,
            presentation: None,
        };
        form.errors = validate_form_values(&form.values(cx)).errors;
        form
    }

    pub(super) fn present(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.open {
            return false;
        }
        self.lifecycle_generation = self.lifecycle_generation.wrapping_add(1);
        self.operation_generation = self.operation_generation.wrapping_add(1);
        self.pending = false;
        self.pending_host = None;
        self.pending_cancel = None;
        self.backend_error = None;
        self.touched = SshHostFormTouched::default();
        self.submit_attempted = false;
        self.revalidate(cx);
        self.set_editable(true, cx);
        let owner = cx.weak_entity();
        let result_owner = owner.clone();
        let window_handle = window.window_handle();
        let initial_focus = self.alias.read(cx).focus_handle();
        let dialog = Dialog::new(
            ModalId::new(FORM_MODAL_ID),
            self.accessibility_title(),
            self.visible_title(),
            vec![
                ModalAction::new(
                    SshHostFormAction::Save,
                    "Save",
                    ModalActionRole::Affirmative,
                    SAVE_ACTION_SELECTOR,
                )
                .default_action(true),
                ModalAction::new(
                    SshHostFormAction::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    CANCEL_ACTION_SELECTOR,
                ),
            ],
            DialogInitialFocus::Body(initial_focus),
        )
        .description("Save an SSH destination for Remote Projects.")
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
        let Ok(completion) = completion else {
            return false;
        };
        self.presentation = Some(completion);
        self.open = true;
        cx.emit(SshHostFormEvent::StateChanged);
        cx.notify();
        true
    }

    pub(super) const fn is_open(&self) -> bool {
        self.open
    }

    pub(super) const fn is_pending(&self) -> bool {
        self.pending
    }

    fn accessibility_title(&self) -> &'static str {
        match self.mode {
            SshHostFormMode::Add => "Add SSH host",
            SshHostFormMode::Edit(_) => "Edit SSH host",
        }
    }

    fn visible_title(&self) -> &'static str {
        match self.mode {
            SshHostFormMode::Add => "Add SSH Host",
            SshHostFormMode::Edit(_) => "Edit SSH Host",
        }
    }

    fn editing_alias(&self) -> Option<SshHostAlias> {
        match &self.mode {
            SshHostFormMode::Add => None,
            SshHostFormMode::Edit(host) => Some(host.alias().clone()),
        }
    }

    fn values(&self, cx: &App) -> SshHostFormValues {
        SshHostFormValues {
            alias: self.alias.read(cx).value().to_owned(),
            host_name: self.host_name.read(cx).value().to_owned(),
            user: self.user.read(cx).value().to_owned(),
            port: self.port.read(cx).value().to_owned(),
            identity_file: self.identity_file.read(cx).value().to_owned(),
        }
    }

    fn revalidate(&mut self, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        self.errors = validate_form_values(&self.values(cx)).errors;
        self.backend_error = None;
        cx.emit(SshHostFormEvent::StateChanged);
        cx.notify();
    }

    fn visible_error(&self, field: SshHostFormField) -> Option<&'static str> {
        (self.submit_attempted || self.touched.contains(field))
            .then(|| self.errors.for_field(field))
            .flatten()
    }

    fn handle_action(
        &mut self,
        action: SshHostFormAction,
        completion: DialogPendingCompletion,
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) -> DialogCloseDecision {
        match action {
            SshHostFormAction::Cancel if self.pending => {
                if self.pending_cancel.is_some() {
                    return DialogCloseDecision::Deny {
                        first_invalid: None,
                    };
                }
                self.pending_cancel = Some(completion);
                DialogCloseDecision::Pending
            }
            SshHostFormAction::Cancel => DialogCloseDecision::Allow,
            SshHostFormAction::Save if self.pending => DialogCloseDecision::Deny {
                first_invalid: None,
            },
            SshHostFormAction::Save => {
                let validation = validate_form_values(&self.values(cx));
                self.errors = validation.errors;
                let Some(host) = validation.host else {
                    self.submit_attempted = true;
                    self.backend_error = None;
                    let first_invalid = self
                        .errors
                        .first_invalid()
                        .map(|field| self.focus_for_field(field, cx));
                    cx.emit(SshHostFormEvent::StateChanged);
                    cx.notify();
                    return DialogCloseDecision::Deny { first_invalid };
                };
                self.start_save(host, completion, window_handle, cx);
                DialogCloseDecision::Pending
            }
        }
    }

    fn start_save(
        &mut self,
        host: ManagedSshHost,
        completion: DialogPendingCompletion,
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        self.pending = true;
        self.backend_error = None;
        self.pending_host = Some(host.clone());
        self.operation_generation = self.operation_generation.wrapping_add(1);
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        let task = self.backend.save(host, self.editing_alias());
        self.set_editable(false, cx);
        cx.emit(SshHostFormEvent::StateChanged);
        cx.notify();
        cx.spawn(async move |form, cx| {
            let result = task.await;
            let Ok(settlement) = form.update(cx, |form, cx| {
                form.prepare_save_settlement(
                    lifecycle_generation,
                    operation_generation,
                    result,
                    completion,
                    cx,
                )
            }) else {
                return;
            };
            let Some(settlement) = settlement else {
                return;
            };
            let _ = window_handle.update(cx, |_, window, cx| settlement.apply(window, cx));
        })
        .detach();
    }

    fn prepare_save_settlement(
        &mut self,
        lifecycle_generation: u64,
        operation_generation: u64,
        result: Result<(), ManagedHostFormBackendError>,
        primary: DialogPendingCompletion,
        cx: &mut Context<Self>,
    ) -> Option<SaveSettlement> {
        if !self.open
            || !self.pending
            || self.lifecycle_generation != lifecycle_generation
            || self.operation_generation != operation_generation
        {
            return None;
        }
        match result {
            Ok(()) => Some(SaveSettlement::Success { primary }),
            Err(error) => {
                self.pending = false;
                self.pending_host = None;
                self.submit_attempted = true;
                self.errors = validate_form_values(&self.values(cx)).errors;
                let first_invalid = match error {
                    ManagedHostFormBackendError::AliasCollision => {
                        self.errors.alias = Some(COLLISION_MESSAGE);
                        Some(self.alias.read(cx).focus_handle())
                    }
                    ManagedHostFormBackendError::HostInUse => {
                        self.backend_error = Some(HOST_IN_USE_MESSAGE);
                        None
                    }
                    ManagedHostFormBackendError::SaveFailed => {
                        self.backend_error = Some(SAVE_FAILURE_MESSAGE);
                        None
                    }
                };
                self.set_editable(true, cx);
                cx.emit(SshHostFormEvent::StateChanged);
                cx.notify();
                Some(SaveSettlement::Failure {
                    primary,
                    cancel: self.pending_cancel.take(),
                    first_invalid,
                })
            }
        }
    }

    fn finish_dialog(&mut self, outcome: DialogOutcome<SshHostFormAction>, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.pending = false;
        self.presentation = None;
        self.pending_cancel = None;
        self.lifecycle_generation = self.lifecycle_generation.wrapping_add(1);
        self.operation_generation = self.operation_generation.wrapping_add(1);
        self.set_editable(true, cx);
        match outcome {
            DialogOutcome::Completed {
                action_id: SshHostFormAction::Save,
                ..
            } => {
                if let Some(host) = self.pending_host.take() {
                    match self.mode {
                        SshHostFormMode::Add => cx.emit(SshHostFormEvent::SavedAndConnect(host)),
                        SshHostFormMode::Edit(_) => cx.emit(SshHostFormEvent::Saved(host)),
                    }
                }
            }
            DialogOutcome::Completed {
                action_id: SshHostFormAction::Cancel,
                ..
            }
            | DialogOutcome::ProgrammaticallyCompleted
            | DialogOutcome::Dismissed(_) => {
                self.pending_host = None;
                cx.emit(SshHostFormEvent::Cancelled);
            }
        }
        cx.emit(SshHostFormEvent::StateChanged);
        cx.notify();
    }

    fn focus_for_field(&self, field: SshHostFormField, cx: &App) -> gpui::FocusHandle {
        match field {
            SshHostFormField::Alias => self.alias.read(cx).focus_handle(),
            SshHostFormField::HostName => self.host_name.read(cx).focus_handle(),
            SshHostFormField::User => self.user.read(cx).focus_handle(),
            SshHostFormField::Port => self.port.read(cx).focus_handle(),
            SshHostFormField::IdentityFile => self.identity_file.read(cx).focus_handle(),
        }
    }

    fn set_editable(&self, editable: bool, cx: &mut Context<Self>) {
        for input in [
            &self.alias,
            &self.host_name,
            &self.user,
            &self.port,
            &self.identity_file,
        ] {
            input.update(cx, |input, cx| input.set_editable(editable, cx));
        }
    }
}

enum SaveSettlement {
    Success {
        primary: DialogPendingCompletion,
    },
    Failure {
        primary: DialogPendingCompletion,
        cancel: Option<DialogPendingCompletion>,
        first_invalid: Option<gpui::FocusHandle>,
    },
}

impl SaveSettlement {
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

impl Render for SshHostForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(form_field(
                "Alias",
                true,
                self.alias.clone(),
                self.alias.read(cx).focus_handle(),
                self.alias.read(cx).is_focused(),
                self.visible_error(SshHostFormField::Alias),
                "managed-ssh-host-alias-error",
            ))
            .child(form_field(
                "Host name",
                true,
                self.host_name.clone(),
                self.host_name.read(cx).focus_handle(),
                self.host_name.read(cx).is_focused(),
                self.visible_error(SshHostFormField::HostName),
                "managed-ssh-host-name-error",
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(12.0))
                    .child(div().min_w_0().flex_1().child(form_field(
                        "User",
                        false,
                        self.user.clone(),
                        self.user.read(cx).focus_handle(),
                        self.user.read(cx).is_focused(),
                        self.visible_error(SshHostFormField::User),
                        "managed-ssh-host-user-error",
                    )))
                    .child(div().w(px(144.0)).flex_shrink_0().child(form_field(
                        "Port",
                        false,
                        self.port.clone(),
                        self.port.read(cx).focus_handle(),
                        self.port.read(cx).is_focused(),
                        self.visible_error(SshHostFormField::Port),
                        "managed-ssh-host-port-error",
                    ))),
            )
            .child(form_field(
                "Identity file",
                false,
                self.identity_file.clone(),
                self.identity_file.read(cx).focus_handle(),
                self.identity_file.read(cx).is_focused(),
                self.visible_error(SshHostFormField::IdentityFile),
                "managed-ssh-host-identity-file-error",
            ))
            .when_some(self.backend_error, |form, error| {
                form.child(
                    div()
                        .debug_selector(|| "managed-ssh-host-backend-error".to_owned())
                        .text_size(px(12.0))
                        .text_color(gpui_color(ACTIVE_THEME.error))
                        .child(error),
                )
            })
    }
}

fn text_input(
    id: &'static str,
    accessibility_name: &'static str,
    value: String,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<SshHostForm>,
) -> Entity<TextInput> {
    cx.new(|cx| {
        TextInput::new(id, accessibility_name, value, window, cx)
            .placeholder(placeholder)
            .variant(TextInputVariant::Bare)
            .return_behavior(TextInputReturnBehavior::Propagate)
            .escape_behavior(TextInputEscapeBehavior::Propagate)
            .debug_selector(id)
    })
}

fn form_field(
    label: &'static str,
    required: bool,
    input: Entity<TextInput>,
    input_focus: gpui::FocusHandle,
    focused: bool,
    error: Option<&'static str>,
    error_selector: &'static str,
) -> impl IntoElement {
    let border_color = if error.is_some() {
        ACTIVE_THEME.error_border
    } else if focused {
        ACTIVE_THEME.border_selected
    } else {
        ACTIVE_THEME.border
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .text_size(px(12.0))
                .text_color(gpui_color(ACTIVE_THEME.text_muted))
                .child(label)
                .when(required, |label| {
                    label.child(
                        div()
                            .ml(px(3.0))
                            .text_color(gpui_color(ACTIVE_THEME.error))
                            .child("*"),
                    )
                }),
        )
        .child(
            div()
                .id(error_selector)
                .h(px(34.0))
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .flex()
                .items_center()
                .overflow_hidden()
                .px(px(10.0))
                .rounded(px(5.0))
                .border(px(1.0))
                .border_color(gpui_color(border_color))
                .bg(gpui_color(ACTIVE_THEME.element_background))
                .text_size(px(13.0))
                .text_color(gpui_color(ACTIVE_THEME.text))
                .on_click(move |_, window, cx| {
                    input_focus.focus(window);
                    cx.stop_propagation();
                })
                .child(input),
        )
        .when_some(error, |field, error| {
            field.child(
                div()
                    .debug_selector(move || error_selector.to_owned())
                    .text_size(px(11.0))
                    .text_color(gpui_color(ACTIVE_THEME.error))
                    .child(error),
            )
        })
}

fn initial_values(mode: &SshHostFormMode) -> SshHostFormValues {
    match mode {
        SshHostFormMode::Add => SshHostFormValues::default(),
        SshHostFormMode::Edit(host) => SshHostFormValues {
            alias: host.alias().as_str().to_owned(),
            host_name: host.host_name().to_owned(),
            user: host.user().unwrap_or_default().to_owned(),
            port: host
                .port()
                .map(|port| port.get().to_string())
                .unwrap_or_default(),
            identity_file: host.identity_file().unwrap_or_default().to_owned(),
        },
    }
}

fn validate_form_values(values: &SshHostFormValues) -> SshHostFormValidation {
    let port = parse_port(&values.port);
    let errors = SshHostFormErrors {
        alias: validation_error(
            ManagedSshHost::new(
                values.alias.clone(),
                "valid.example".to_owned(),
                None,
                None,
                None,
            ),
            SshHostFormField::Alias,
        ),
        host_name: validation_error(
            ManagedSshHost::new(
                "valid-alias".to_owned(),
                values.host_name.clone(),
                None,
                None,
                None,
            ),
            SshHostFormField::HostName,
        ),
        user: validation_error(
            ManagedSshHost::new(
                "valid-alias".to_owned(),
                "valid.example".to_owned(),
                optional_value(&values.user),
                None,
                None,
            ),
            SshHostFormField::User,
        ),
        port: port
            .as_ref()
            .err()
            .map(|_| "Enter a decimal port from 1 to 65535."),
        identity_file: validation_error(
            ManagedSshHost::new(
                "valid-alias".to_owned(),
                "valid.example".to_owned(),
                None,
                None,
                optional_value(&values.identity_file),
            ),
            SshHostFormField::IdentityFile,
        ),
    };
    let host = match (errors.first_invalid(), port) {
        (None, Ok(port)) => ManagedSshHost::new(
            values.alias.clone(),
            values.host_name.clone(),
            optional_value(&values.user),
            port,
            optional_value(&values.identity_file),
        )
        .ok(),
        _ => None,
    };
    SshHostFormValidation { host, errors }
}

fn parse_port(value: &str) -> Result<Option<NonZeroU16>, ()> {
    if value.is_empty() {
        return Ok(None);
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    value
        .parse::<u16>()
        .ok()
        .and_then(NonZeroU16::new)
        .map(Some)
        .ok_or(())
}

fn optional_value(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn validation_error(
    result: Result<ManagedSshHost, ManagedSshHostValidationError>,
    field: SshHostFormField,
) -> Option<&'static str> {
    let error = result.err()?;
    let expected = match field {
        SshHostFormField::Alias => ManagedSshHostField::Alias,
        SshHostFormField::HostName => ManagedSshHostField::HostName,
        SshHostFormField::User => ManagedSshHostField::User,
        SshHostFormField::IdentityFile => ManagedSshHostField::IdentityFile,
        SshHostFormField::Port => return None,
    };
    (error.field == expected).then(|| managed_validation_message(field, error.kind))
}

fn managed_validation_message(
    field: SshHostFormField,
    kind: ManagedSshHostValueError,
) -> &'static str {
    match (field, kind) {
        (SshHostFormField::Alias, ManagedSshHostValueError::Required) => "Alias is required.",
        (SshHostFormField::HostName, ManagedSshHostValueError::Required) => {
            "Host name is required."
        }
        (_, ManagedSshHostValueError::TooLong { .. }) => "This value is too long.",
        (SshHostFormField::IdentityFile, _) => {
            "Enter an absolute path or a path beginning with ~/ without wildcards."
        }
        (SshHostFormField::Alias, _) => {
            "Use one literal SSH alias without spaces, wildcards, or options."
        }
        (SshHostFormField::HostName, _) => {
            "Enter one host name or address without spaces or options."
        }
        (SshHostFormField::User, _) => "Enter one SSH user without spaces or options.",
        (SshHostFormField::Port, _) => "Enter a decimal port from 1 to 65535.",
    }
}

fn gpui_color(color: Color) -> gpui::Rgba {
    gpui::rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::Mutex;

    use gpui::{Keystroke, Modifiers, TestAppContext, VisualTestContext};
    use spaceterm_ui::ModalLayer;

    use super::*;

    type RecordedFormEvents = Rc<RefCell<Vec<SshHostFormEvent>>>;
    type FormWindow<'a> = (
        Entity<FormHarness>,
        Entity<SshHostForm>,
        RecordedFormEvents,
        &'a mut VisualTestContext,
    );

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct SaveRecord {
        host: ManagedSshHost,
        editing_alias: Option<SshHostAlias>,
    }

    struct ScriptedBackend {
        tasks: Mutex<VecDeque<Task<Result<(), ManagedHostFormBackendError>>>>,
        records: Mutex<Vec<SaveRecord>>,
    }

    impl ScriptedBackend {
        fn new(
            tasks: impl IntoIterator<Item = Task<Result<(), ManagedHostFormBackendError>>>,
        ) -> Self {
            Self {
                tasks: Mutex::new(tasks.into_iter().collect()),
                records: Mutex::new(Vec::new()),
            }
        }

        fn ready(result: Result<(), ManagedHostFormBackendError>) -> Arc<Self> {
            Arc::new(Self::new([Task::ready(result)]))
        }

        fn records(&self) -> Vec<SaveRecord> {
            self.records.lock().unwrap().clone()
        }
    }

    impl ManagedHostFormBackend for ScriptedBackend {
        fn save(
            &self,
            host: ManagedSshHost,
            editing_alias: Option<SshHostAlias>,
        ) -> Task<Result<(), ManagedHostFormBackendError>> {
            self.records.lock().unwrap().push(SaveRecord {
                host,
                editing_alias,
            });
            self.tasks
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(ManagedHostFormBackendError::SaveFailed)))
        }
    }

    struct FormHarness {
        form: Entity<SshHostForm>,
        events: Rc<RefCell<Vec<SshHostFormEvent>>>,
    }

    impl Render for FormHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    fn form_window<'a>(
        mode: SshHostFormMode,
        backend: Arc<ScriptedBackend>,
        cx: &'a mut TestAppContext,
    ) -> FormWindow<'a> {
        cx.update(crate::ui::init);
        let injected: Arc<dyn ManagedHostFormBackend> = backend;
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let form = cx.new(|cx| SshHostForm::new(mode, injected, window, cx));
            let events = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&events);
            cx.subscribe(&form, move |_, _, event, _| {
                captured.borrow_mut().push(event.clone());
            })
            .detach();
            FormHarness { form, events }
        });
        let (form, events) = harness.read_with(cx, |harness, _| {
            (harness.form.clone(), Rc::clone(&harness.events))
        });
        cx.update(|window, cx| {
            window.activate_window();
            form.update(cx, |form, cx| assert!(form.present(window, cx)));
        });
        cx.run_until_parked();
        (harness, form, events, cx)
    }

    fn set_input(
        form: &Entity<SshHostForm>,
        input: &Entity<TextInput>,
        value: &str,
        cx: &mut VisualTestContext,
    ) {
        input.update(cx, |input, cx| {
            input.set_value(value, cx);
        });
        form.update(cx, |form, cx| form.revalidate(cx));
        cx.run_until_parked();
    }

    fn set_valid_add_values(form: &Entity<SshHostForm>, cx: &mut VisualTestContext) {
        let inputs = form.read_with(cx, |form, _| {
            (
                form.alias.clone(),
                form.host_name.clone(),
                form.user.clone(),
                form.port.clone(),
                form.identity_file.clone(),
            )
        });
        for (input, value) in [
            (&inputs.0, "work"),
            (&inputs.1, "server.example"),
            (&inputs.2, "deploy"),
            (&inputs.3, "2222"),
            (&inputs.4, "~/.ssh/id_ed25519"),
        ] {
            set_input(form, input, value, cx);
        }
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let selector = match selector {
            SAVE_ACTION_SELECTOR => "modal-action-managed-ssh-host-save",
            CANCEL_ACTION_SELECTOR => "modal-action-managed-ssh-host-cancel",
            _ => selector,
        };
        let bounds = cx.debug_bounds(selector).unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn existing_host() -> ManagedSshHost {
        ManagedSshHost::new(
            "work".to_owned(),
            "old.example".to_owned(),
            Some("deploy".to_owned()),
            NonZeroU16::new(2222),
            Some("~/.ssh/id_ed25519".to_owned()),
        )
        .unwrap()
    }

    #[test]
    fn validation_should_map_every_managed_field_and_port_inline() {
        let cases = [
            (
                SshHostFormValues {
                    alias: "bad alias".to_owned(),
                    host_name: "host.example".to_owned(),
                    ..SshHostFormValues::default()
                },
                SshHostFormField::Alias,
            ),
            (
                SshHostFormValues {
                    alias: "work".to_owned(),
                    host_name: "bad host".to_owned(),
                    ..SshHostFormValues::default()
                },
                SshHostFormField::HostName,
            ),
            (
                SshHostFormValues {
                    alias: "work".to_owned(),
                    host_name: "host.example".to_owned(),
                    user: "bad user".to_owned(),
                    ..SshHostFormValues::default()
                },
                SshHostFormField::User,
            ),
            (
                SshHostFormValues {
                    alias: "work".to_owned(),
                    host_name: "host.example".to_owned(),
                    port: "65536".to_owned(),
                    ..SshHostFormValues::default()
                },
                SshHostFormField::Port,
            ),
            (
                SshHostFormValues {
                    alias: "work".to_owned(),
                    host_name: "host.example".to_owned(),
                    identity_file: "relative/key".to_owned(),
                    ..SshHostFormValues::default()
                },
                SshHostFormField::IdentityFile,
            ),
        ];

        for (values, expected) in cases {
            let validation = validate_form_values(&values);
            assert_eq!(validation.errors.first_invalid(), Some(expected));
            assert!(validation.errors.for_field(expected).is_some());
            assert!(validation.host.is_none());
        }
    }

    #[test]
    fn port_should_accept_only_optional_decimal_one_through_65535() {
        assert_eq!(parse_port(""), Ok(None));
        assert_eq!(parse_port("1"), Ok(NonZeroU16::new(1)));
        assert_eq!(parse_port("65535"), Ok(NonZeroU16::new(65535)));
        for invalid in ["0", "65536", "+22", " 22", "22 ", "2.2", "ssh"] {
            assert_eq!(parse_port(invalid), Err(()), "accepted {invalid:?}");
        }
    }

    #[gpui::test]
    fn invalid_default_save_should_deny_without_writing_and_focus_first_field(
        cx: &mut TestAppContext,
    ) {
        let backend = ScriptedBackend::ready(Ok(()));
        let (_, form, _, cx) = form_window(SshHostFormMode::Add, Arc::clone(&backend), cx);

        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("enter").unwrap(), cx);
        });
        cx.run_until_parked();

        assert!(backend.records().is_empty());
        assert!(form.read_with(cx, |form, _| form.is_open()));
        assert!(cx.debug_bounds("managed-ssh-host-alias-error").is_some());
        assert!(cx.update(|window, cx| {
            form.read(cx)
                .alias
                .read(cx)
                .focus_handle()
                .is_focused(window)
        }));
    }

    #[gpui::test]
    fn typing_and_tab_navigation_should_not_write(cx: &mut TestAppContext) {
        let backend = ScriptedBackend::ready(Ok(()));
        let (_, form, _, cx) = form_window(SshHostFormMode::Add, Arc::clone(&backend), cx);
        let (alias, host_name) =
            form.read_with(cx, |form, _| (form.alias.clone(), form.host_name.clone()));
        set_input(&form, &alias, "work", cx);
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();

        assert!(backend.records().is_empty());
        assert!(cx.update(|window, cx| host_name.read(cx).focus_handle().is_focused(window)));
    }

    #[gpui::test]
    fn cancel_should_close_without_writing(cx: &mut TestAppContext) {
        let backend = ScriptedBackend::ready(Ok(()));
        let (_, form, events, cx) = form_window(SshHostFormMode::Add, Arc::clone(&backend), cx);

        click(CANCEL_ACTION_SELECTOR, cx);

        assert!(backend.records().is_empty());
        assert!(!form.read_with(cx, |form, _| form.is_open()));
        assert!(events.borrow().contains(&SshHostFormEvent::Cancelled));
    }

    #[gpui::test]
    fn add_default_save_should_emit_saved_and_connect(cx: &mut TestAppContext) {
        let backend = ScriptedBackend::ready(Ok(()));
        let (_, form, events, cx) = form_window(SshHostFormMode::Add, Arc::clone(&backend), cx);
        set_valid_add_values(&form, cx);

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let records = backend.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].editing_alias, None);
        assert!(events.borrow().iter().any(|event| matches!(
            event,
            SshHostFormEvent::SavedAndConnect(host) if host.alias().as_str() == "work"
        )));
    }

    #[gpui::test]
    fn edit_should_preserve_values_pass_original_alias_and_emit_saved(cx: &mut TestAppContext) {
        let backend = ScriptedBackend::ready(Ok(()));
        let (_, form, events, cx) = form_window(
            SshHostFormMode::Edit(existing_host()),
            Arc::clone(&backend),
            cx,
        );
        let host_name = form.read_with(cx, |form, _| form.host_name.clone());
        set_input(&form, &host_name, "new.example", cx);

        click(SAVE_ACTION_SELECTOR, cx);

        let records = backend.records();
        assert_eq!(records[0].editing_alias.as_ref().unwrap().as_str(), "work");
        assert_eq!(records[0].host.host_name(), "new.example");
        assert!(events.borrow().iter().any(|event| matches!(
            event,
            SshHostFormEvent::Saved(host) if host.host_name() == "new.example"
        )));
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, SshHostFormEvent::SavedAndConnect(_)))
        );
    }

    #[gpui::test]
    fn alias_collision_should_restore_exact_values_inline_and_refocus_alias(
        cx: &mut TestAppContext,
    ) {
        let backend = ScriptedBackend::ready(Err(ManagedHostFormBackendError::AliasCollision));
        let (_, form, _, cx) = form_window(SshHostFormMode::Add, backend, cx);
        set_valid_add_values(&form, cx);
        let before = form.read_with(cx, |form, cx| form.values(cx));

        click(SAVE_ACTION_SELECTOR, cx);

        assert_eq!(form.read_with(cx, |form, cx| form.values(cx)), before);
        assert_eq!(
            form.read_with(cx, |form, _| form.errors.alias),
            Some(COLLISION_MESSAGE)
        );
        assert!(cx.update(|window, cx| {
            form.read(cx)
                .alias
                .read(cx)
                .focus_handle()
                .is_focused(window)
        }));
    }

    #[gpui::test]
    fn recoverable_failure_should_restore_editing_and_content_free_actionable_error(
        cx: &mut TestAppContext,
    ) {
        let backend = ScriptedBackend::ready(Err(ManagedHostFormBackendError::SaveFailed));
        let (_, form, _, cx) = form_window(SshHostFormMode::Add, backend, cx);
        set_valid_add_values(&form, cx);
        let before = form.read_with(cx, |form, cx| form.values(cx));

        click(SAVE_ACTION_SELECTOR, cx);

        assert_eq!(form.read_with(cx, |form, cx| form.values(cx)), before);
        assert_eq!(
            form.read_with(cx, |form, _| (form.pending, form.backend_error)),
            (false, Some(SAVE_FAILURE_MESSAGE))
        );
        assert!(cx.debug_bounds("managed-ssh-host-backend-error").is_some());
    }

    #[gpui::test]
    fn pending_save_should_be_read_only_and_duplicate_safe(cx: &mut TestAppContext) {
        let (sender, receiver) = async_channel::bounded(1);
        let task = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let backend = Arc::new(ScriptedBackend::new([task]));
        let (_, form, events, cx) = form_window(SshHostFormMode::Add, Arc::clone(&backend), cx);
        set_valid_add_values(&form, cx);
        let before = form.read_with(cx, |form, cx| form.values(cx));

        click(SAVE_ACTION_SELECTOR, cx);
        assert!(form.read_with(cx, |form, _| form.is_pending()));
        cx.simulate_keystrokes("x");
        click(SAVE_ACTION_SELECTOR, cx);
        click(CANCEL_ACTION_SELECTOR, cx);
        click(CANCEL_ACTION_SELECTOR, cx);

        assert_eq!(backend.records().len(), 1);
        assert_eq!(form.read_with(cx, |form, cx| form.values(cx)), before);
        sender.try_send(Ok(())).unwrap();
        cx.run_until_parked();
        assert!(
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, SshHostFormEvent::SavedAndConnect(_)))
        );
    }

    #[gpui::test]
    fn stale_backend_completion_should_not_affect_a_reopened_form(cx: &mut TestAppContext) {
        let (sender, receiver) = async_channel::bounded(1);
        let task = cx.update(|cx| {
            cx.background_executor()
                .spawn(async move { receiver.recv().await.unwrap() })
        });
        let backend = Arc::new(ScriptedBackend::new([task]));
        let (_, form, events, cx) = form_window(SshHostFormMode::Add, backend, cx);
        set_valid_add_values(&form, cx);
        click(SAVE_ACTION_SELECTOR, cx);
        let presentation = form.read_with(cx, |form, _| form.presentation.clone().unwrap());
        cx.update(|window, cx| presentation.dismiss(window, cx).unwrap());
        cx.run_until_parked();
        events.borrow_mut().clear();
        cx.update(|window, cx| {
            form.update(cx, |form, cx| assert!(form.present(window, cx)));
        });
        cx.run_until_parked();

        sender.try_send(Ok(())).unwrap();
        cx.run_until_parked();

        assert!(form.read_with(cx, |form, _| form.is_open()));
        assert!(!events.borrow().iter().any(|event| matches!(
            event,
            SshHostFormEvent::Saved(_) | SshHostFormEvent::SavedAndConnect(_)
        )));
    }
}
