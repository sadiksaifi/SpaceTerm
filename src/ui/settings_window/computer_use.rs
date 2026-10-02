//! The Privacy section's Screen Recording and Device Control rows: SpaceTerm's own grants for the
//! computer-use tools running in its Terminal Sessions, and how to set them up and recover them.
//!
//! A tool running in a Terminal Session takes screenshots and sends input through SpaceTerm's
//! grants, so these rows are where a person learns why such a tool reports missing access. The
//! Operating System reports only whether a grant is usable now, and a grant can stay switched on
//! after it stops working, so every readable state keeps a way to troubleshoot. Nothing here
//! captures the screen or sends input to test access, and a reset reaches one permission of the
//! running application only, after the person confirms it.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, SharedString, Task, Window, div};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use crate::platform::computer_use_access::{
    ComputerUseAccess, ComputerUseAccessError, ComputerUseAuthorization, ComputerUsePermission,
};
use crate::ui::appearance::ChromeAppearance;

use super::SettingsWindow;
use crate::ui::sidebar_window::form::{action_button, badge};

/// The fixed copy and selectors of one permission's row.
pub(super) struct PermissionText {
    /// The permission's name in running text.
    pub(super) name: &'static str,
    /// The System Settings list that holds the grant.
    pub(super) pane: &'static str,
    /// What a computer-use tool does with the grant.
    pub(super) purpose: &'static str,
    /// The system applies a changed grant only after the application reopens.
    pub(super) applies_after_reopen: bool,
    pub(super) control: &'static str,
    pub(super) request: &'static str,
    pub(super) open_settings: &'static str,
    pub(super) check_again: &'static str,
    pub(super) troubleshoot: &'static str,
    pub(super) state_allowed: &'static str,
    pub(super) state_not_allowed: &'static str,
    pub(super) state_unavailable: &'static str,
    pub(super) troubleshoot_modal: &'static str,
    pub(super) troubleshoot_open_settings: &'static str,
    pub(super) troubleshoot_reset: &'static str,
    pub(super) troubleshoot_done: &'static str,
    pub(super) reset_modal: &'static str,
    pub(super) reset_confirm: &'static str,
    pub(super) reset_cancel: &'static str,
}

pub(super) const SCREEN_RECORDING: PermissionText = PermissionText {
    name: "Screen Recording",
    pane: "Screen & System Audio Recording",
    purpose: "take screenshots",
    applies_after_reopen: true,
    control: "settings-screen-recording-access-control",
    request: "settings-screen-recording-access-request",
    open_settings: "settings-screen-recording-access-open-settings",
    check_again: "settings-screen-recording-access-check-again",
    troubleshoot: "settings-screen-recording-access-troubleshoot",
    state_allowed: "settings-screen-recording-access-state-allowed",
    state_not_allowed: "settings-screen-recording-access-state-not-allowed",
    state_unavailable: "settings-screen-recording-access-state-unavailable",
    troubleshoot_modal: "settings-screen-recording-troubleshoot",
    troubleshoot_open_settings: "settings-screen-recording-troubleshoot-open-settings",
    troubleshoot_reset: "settings-screen-recording-troubleshoot-reset",
    troubleshoot_done: "settings-screen-recording-troubleshoot-done",
    reset_modal: "settings-screen-recording-reset",
    reset_confirm: "settings-screen-recording-reset-confirm",
    reset_cancel: "settings-screen-recording-reset-cancel",
};

pub(super) const DEVICE_CONTROL: PermissionText = PermissionText {
    name: "Device Control",
    pane: "Device Control and Data Access",
    purpose: "click and type in other apps",
    applies_after_reopen: false,
    control: "settings-device-control-access-control",
    request: "settings-device-control-access-request",
    open_settings: "settings-device-control-access-open-settings",
    check_again: "settings-device-control-access-check-again",
    troubleshoot: "settings-device-control-access-troubleshoot",
    state_allowed: "settings-device-control-access-state-allowed",
    state_not_allowed: "settings-device-control-access-state-not-allowed",
    state_unavailable: "settings-device-control-access-state-unavailable",
    troubleshoot_modal: "settings-device-control-troubleshoot",
    troubleshoot_open_settings: "settings-device-control-troubleshoot-open-settings",
    troubleshoot_reset: "settings-device-control-troubleshoot-reset",
    troubleshoot_done: "settings-device-control-troubleshoot-done",
    reset_modal: "settings-device-control-reset",
    reset_confirm: "settings-device-control-reset-confirm",
    reset_cancel: "settings-device-control-reset-cancel",
};

/// The permission a Settings Row presents, when it presents one of these.
pub(super) const fn row_permission(row: super::SettingsRowId) -> Option<ComputerUsePermission> {
    match row {
        super::SettingsRowId::ScreenRecordingAccess => Some(ComputerUsePermission::ScreenRecording),
        super::SettingsRowId::DeviceControlAccess => Some(ComputerUsePermission::Accessibility),
        _ => None,
    }
}

pub(super) const fn permission_text(permission: ComputerUsePermission) -> &'static PermissionText {
    match permission {
        ComputerUsePermission::ScreenRecording => &SCREEN_RECORDING,
        ComputerUsePermission::Accessibility => &DEVICE_CONTROL,
    }
}

/// What SpaceTerm currently knows about one permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ComputerUseAccessStatus {
    /// No capability is composed, so access is neither readable nor changeable here.
    Unsupported,
    Authorization(ComputerUseAuthorization),
    /// Authorization could not be read or requested.
    Failed(ComputerUseAccessError),
}

/// The primary action the row offers for its status, when there is one worth offering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ComputerUseAccessAction {
    Request,
    OpenSettings,
    CheckAgain,
}

/// What the latest recovery step reported, kept beside the status it tried to change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecoveryNotice {
    OpenFailed,
    Resetting,
    ResetCompleted,
    ResetFailed,
}

/// The row's complete presentation, derived from its state so every state reads one way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ComputerUseAccessPresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: SharedString,
    pub(super) action: Option<ComputerUseAccessAction>,
    /// Whether Troubleshoot is offered, which it is whenever authorization is readable.
    pub(super) troubleshoot: Option<TroubleshootAvailability>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TroubleshootAvailability {
    Enabled,
    /// Withheld while a reset is running.
    Busy,
}

/// The troubleshooting guide's decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TroubleshootDecision {
    OpenSettings,
    Reset,
    Done,
}

/// Owns one permission's injected capability, last known status, and pending reset.
pub(super) struct ComputerUseAccessRow {
    permission: ComputerUsePermission,
    access: Option<Rc<dyn ComputerUseAccess>>,
    /// The running application, which is what a person turns on in System Settings.
    application_name: &'static str,
    status: ComputerUseAccessStatus,
    /// The system prompts at most once per application identity, so after one request the row
    /// sends the person to System Settings instead of offering a request that may do nothing.
    requested: bool,
    notice: Option<RecoveryNotice>,
    /// Returns the reset result to GPUI. Dropping the window drops the bridge.
    _reset: Option<Task<()>>,
}

impl ComputerUseAccessRow {
    fn new(
        permission: ComputerUsePermission,
        access: Option<Rc<dyn ComputerUseAccess>>,
        application_name: &'static str,
    ) -> Self {
        let mut row = Self {
            permission,
            access,
            application_name,
            status: ComputerUseAccessStatus::Unsupported,
            requested: false,
            notice: None,
            _reset: None,
        };
        row.refresh();
        row
    }

    #[cfg(test)]
    pub(super) fn status(&self) -> ComputerUseAccessStatus {
        self.status
    }

    #[cfg(test)]
    pub(super) fn notice(&self) -> Option<RecoveryNotice> {
        self.notice
    }

    /// Reads the current authorization, which can change in System Settings at any time.
    ///
    /// Opening System Settings never changes the status by itself; only a later read does.
    pub(super) fn refresh(&mut self) {
        self.apply(self.read());
    }

    fn read(&self) -> ComputerUseAccessStatus {
        match &self.access {
            None => ComputerUseAccessStatus::Unsupported,
            Some(access) => match access.authorization(self.permission) {
                Ok(authorization) => ComputerUseAccessStatus::Authorization(authorization),
                Err(error) => ComputerUseAccessStatus::Failed(error),
            },
        }
    }

    /// A notice describes the status it was reported against, so a changed status retires it.
    /// A running reset keeps its notice until the reset reports.
    fn apply(&mut self, status: ComputerUseAccessStatus) {
        if status != self.status && self.notice != Some(RecoveryNotice::Resetting) {
            self.notice = None;
        }
        self.status = status;
    }

    fn readable(&self) -> bool {
        matches!(self.status, ComputerUseAccessStatus::Authorization(_))
    }

    fn request(&mut self) {
        if self.status
            != ComputerUseAccessStatus::Authorization(ComputerUseAuthorization::NotGranted)
            || self.requested
        {
            return;
        }
        let Some(access) = self.access.clone() else {
            return;
        };
        match access.request_authorization(self.permission) {
            Ok(()) => {
                self.requested = true;
                self.notice = None;
                self.refresh();
            }
            Err(error) => self.apply(ComputerUseAccessStatus::Failed(error)),
        }
    }

    fn open_settings(&mut self) {
        if !self.readable() {
            return;
        }
        let Some(access) = &self.access else {
            return;
        };
        self.notice = access
            .open_settings(self.permission)
            .err()
            .map(|_| RecoveryNotice::OpenFailed);
    }

    fn can_reset(&self) -> bool {
        self.readable()
            && self.notice != Some(RecoveryNotice::Resetting)
            && self.access.as_ref().is_some_and(|access| access.can_reset())
    }

    fn finish_reset(&mut self, result: Option<Result<(), ComputerUseAccessError>>) {
        self.notice = None;
        self.requested = false;
        self.refresh();
        self.notice = Some(match result {
            Some(Ok(())) => RecoveryNotice::ResetCompleted,
            Some(Err(_)) | None => RecoveryNotice::ResetFailed,
        });
    }

    pub(super) fn presentation(&self) -> ComputerUseAccessPresentation {
        use ComputerUseAccessAction as Action;
        use ComputerUseAuthorization as Authorization;

        let text = permission_text(self.permission);
        let name = text.name;
        let application = self.application_name;
        let pane = text.pane;
        let reopen = if text.applies_after_reopen {
            format!(", then quit and reopen {application}")
        } else {
            String::new()
        };
        let notice = self.notice.map(|notice| match notice {
            RecoveryNotice::OpenFailed => format!(
                "System Settings could not be opened. Open it yourself and turn on \
                 {application} under Privacy & Security > {pane}{reopen}."
            ),
            RecoveryNotice::Resetting => {
                format!("Resetting {name} for {application}…")
            }
            RecoveryNotice::ResetCompleted => format!(
                "The system no longer has a {name} decision for {application}. Request access to add \
                 {application} again."
            ),
            RecoveryNotice::ResetFailed => format!(
                "The system could not reset {name} for {application}. Remove {application} under \
                 Privacy & Security > {pane}, then add it again."
            ),
        });
        let troubleshoot = self.readable().then_some(
            if self.notice == Some(RecoveryNotice::Resetting) {
                TroubleshootAvailability::Busy
            } else {
                TroubleshootAvailability::Enabled
            },
        );

        let (state, state_selector, explanation, action) = match self.status {
            ComputerUseAccessStatus::Unsupported => (
                "Unavailable",
                text.state_unavailable,
                format!("SpaceTerm does not manage {name} access on this platform."),
                None,
            ),
            ComputerUseAccessStatus::Authorization(Authorization::Granted) => (
                "Allowed",
                text.state_allowed,
                notice.unwrap_or_else(|| {
                    format!(
                        "Computer-use tools running in SpaceTerm can {}. If a tool still reports \
                         missing access, choose Troubleshoot.",
                        text.purpose
                    )
                }),
                None,
            ),
            ComputerUseAccessStatus::Authorization(Authorization::NotGranted) if !self.requested => (
                "Not Allowed",
                text.state_not_allowed,
                notice.unwrap_or_else(|| {
                    let steps = if text.applies_after_reopen {
                        format!(
                            "Request access, turn on {application} in System Settings, then quit \
                             and reopen {application}."
                        )
                    } else {
                        format!("Request access, then turn on {application} in System Settings.")
                    };
                    format!(
                        "Computer-use tools running in SpaceTerm need this to {}. {steps}",
                        text.purpose
                    )
                }),
                Some(Action::Request),
            ),
            ComputerUseAccessStatus::Authorization(Authorization::NotGranted) => (
                "Not Allowed",
                text.state_not_allowed,
                notice.unwrap_or_else(|| {
                    format!(
                        "Turn on {application} under Privacy & Security > {pane}{reopen}. If it is \
                         already on, choose Troubleshoot."
                    )
                }),
                Some(Action::OpenSettings),
            ),
            ComputerUseAccessStatus::Failed(error) => (
                "Unavailable",
                text.state_unavailable,
                match error {
                    ComputerUseAccessError::OffMainThread => {
                        format!("SpaceTerm could not check {name} access.")
                    }
                    ComputerUseAccessError::PlatformUnavailable => {
                        format!("The system did not report {name} access.")
                    }
                    ComputerUseAccessError::PlatformRejected => {
                        format!("The system rejected the {name} access request.")
                    }
                },
                Some(Action::CheckAgain),
            ),
        };
        ComputerUseAccessPresentation {
            state,
            state_selector,
            explanation: explanation.into(),
            action,
            troubleshoot,
        }
    }

    /// The step-by-step recovery a person follows when a tool reports missing access.
    fn troubleshooting(&self) -> (String, String, String) {
        let text = permission_text(self.permission);
        let name = text.name;
        let application = self.application_name;
        let pane = text.pane;
        let title = format!("Troubleshoot {name}");
        let message = format!(
            "The {name} grant can stop working after {application} is updated or signed again, \
             while its switch stays on. SpaceTerm cannot detect this. If a tool reports {name} \
             access missing, try these steps in order, even while access shows Allowed."
        );
        let reopen = if text.applies_after_reopen {
            format!(
                "3. Quit and reopen {application}. The system applies {name} changes only after the \
                 app reopens."
            )
        } else {
            format!("3. If the tool still reports missing access, quit and reopen {application}.")
        };
        let reset = if self.access.as_ref().is_some_and(|access| access.can_reset()) {
            " Alternatively, choose Reset Permission to clear the entry, then request access again."
        } else {
            ""
        };
        let detail = format!(
            "1. Quit the tool and start it again.\n\
             2. In System Settings, open Privacy & Security > {pane}. Remove {application} with the \
             minus button, add it again with the plus button, and turn it on.{reset}\n\
             {reopen} Quitting ends every running terminal session, so finish or save that work \
             first.\n\n\
             Some tools also run their own helper app that needs a separate grant. Follow the \
             tool's instructions for that app. A device management policy can also block the \
             grant, and SpaceTerm cannot change that policy."
        );
        (title, message, detail)
    }
}

/// Both computer-use permission rows.
pub(super) struct ComputerUseAccessRows {
    screen_recording: ComputerUseAccessRow,
    accessibility: ComputerUseAccessRow,
}

impl ComputerUseAccessRows {
    pub(super) fn new(
        access: Option<Rc<dyn ComputerUseAccess>>,
        application_name: &'static str,
    ) -> Self {
        Self {
            screen_recording: ComputerUseAccessRow::new(
                ComputerUsePermission::ScreenRecording,
                access.clone(),
                application_name,
            ),
            accessibility: ComputerUseAccessRow::new(
                ComputerUsePermission::Accessibility,
                access,
                application_name,
            ),
        }
    }

    pub(super) fn row(&self, permission: ComputerUsePermission) -> &ComputerUseAccessRow {
        match permission {
            ComputerUsePermission::ScreenRecording => &self.screen_recording,
            ComputerUsePermission::Accessibility => &self.accessibility,
        }
    }

    fn row_mut(&mut self, permission: ComputerUsePermission) -> &mut ComputerUseAccessRow {
        match permission {
            ComputerUsePermission::ScreenRecording => &mut self.screen_recording,
            ComputerUsePermission::Accessibility => &mut self.accessibility,
        }
    }

    pub(super) fn refresh(&mut self) {
        self.screen_recording.refresh();
        self.accessibility.refresh();
    }
}

impl SettingsWindow {
    /// Asks the system to prompt for one permission, then reads what it reports.
    pub(super) fn request_computer_use_access(
        &mut self,
        permission: ComputerUsePermission,
        cx: &mut Context<Self>,
    ) {
        self.computer_use_access.row_mut(permission).request();
        cx.notify();
    }

    pub(super) fn open_computer_use_settings(
        &mut self,
        permission: ComputerUsePermission,
        cx: &mut Context<Self>,
    ) {
        self.computer_use_access.row_mut(permission).open_settings();
        cx.notify();
    }

    pub(super) fn refresh_computer_use_access(&mut self, cx: &mut Context<Self>) {
        self.computer_use_access.refresh();
        cx.notify();
    }

    /// Presents the recovery guide, with System Settings and a confirmed reset one step away.
    pub(super) fn troubleshoot_computer_use_access(
        &mut self,
        permission: ComputerUsePermission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = self.computer_use_access.row(permission);
        if !row.readable() {
            return;
        }
        let text = permission_text(permission);
        let (title, message, detail) = row.troubleshooting();
        let mut actions = vec![
            ModalAction::new(
                TroubleshootDecision::OpenSettings,
                "Open System Settings",
                ModalActionRole::Affirmative,
                text.troubleshoot_open_settings,
            )
            .default_action(true),
        ];
        if row.can_reset() {
            actions.push(ModalAction::new(
                TroubleshootDecision::Reset,
                "Reset Permission…",
                ModalActionRole::Auxiliary,
                text.troubleshoot_reset,
            ));
        }
        actions.push(ModalAction::new(
            TroubleshootDecision::Done,
            "Done",
            ModalActionRole::Cancel,
            text.troubleshoot_done,
        ));
        let owner = cx.weak_entity();
        let handle = window.window_handle();
        let result = Alert::new(
            ModalId::new(text.troubleshoot_modal),
            title.clone(),
            title,
            message,
            actions,
        )
        .detail(detail)
        .present(window, cx, move |outcome, cx| {
            let AlertOutcome::Activated { action_id, .. } = outcome else {
                return;
            };
            match action_id {
                TroubleshootDecision::OpenSettings => {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.open_computer_use_settings(permission, cx);
                    });
                }
                TroubleshootDecision::Reset => {
                    let _ = handle.update(cx, |_, window, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.confirm_computer_use_reset(permission, window, cx);
                        });
                    });
                }
                TroubleshootDecision::Done => {}
            }
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm permission troubleshooting guide");
        }
    }

    /// Confirms a reset that clears one permission's decision for the running application only.
    fn confirm_computer_use_reset(
        &mut self,
        permission: ComputerUsePermission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = self.computer_use_access.row(permission);
        if !row.can_reset() {
            return;
        }
        let text = permission_text(permission);
        let name = text.name;
        let application = row.application_name;
        let title = format!("Reset {name} for {application}?");
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new(text.reset_modal),
            title.clone(),
            title,
            format!(
                "The system forgets {application}'s {name} decision. Other apps and permissions keep \
                 theirs."
            ),
            vec![
                ModalAction::new(
                    true,
                    "Reset",
                    ModalActionRole::Affirmative,
                    text.reset_confirm,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(false, "Cancel", ModalActionRole::Cancel, text.reset_cancel),
            ],
        )
        .intent(AlertIntent::Warning)
        .detail(format!(
            "Afterward, request access again and turn on {application} in System Settings. \
             Running terminal sessions keep running."
        ))
        .present(window, cx, move |outcome, cx| {
            if matches!(
                outcome,
                AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                let _ = owner.update(cx, |settings, cx| {
                    settings.reset_computer_use_access(permission, cx);
                });
            }
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm permission reset confirmation");
        }
    }

    /// Runs a confirmed reset. The native completion may run on any thread, so it only sends the
    /// closed result through a channel and the window applies it on the foreground executor.
    pub(super) fn reset_computer_use_access(
        &mut self,
        permission: ComputerUsePermission,
        cx: &mut Context<Self>,
    ) {
        let row = self.computer_use_access.row_mut(permission);
        if !row.can_reset() {
            return;
        }
        let Some(access) = row.access.clone() else {
            return;
        };
        let (sender, receiver) = async_channel::bounded(1);
        match access.reset(
            permission,
            Box::new(move |result| {
                let _ = sender.try_send(result);
            }),
        ) {
            Ok(()) => {
                row.notice = Some(RecoveryNotice::Resetting);
                row._reset = Some(cx.spawn(async move |settings, cx| {
                    let result = receiver.recv().await.ok();
                    let _ = settings.update(cx, |settings, cx| {
                        settings
                            .computer_use_access
                            .row_mut(permission)
                            .finish_reset(result);
                        cx.notify();
                    });
                }));
            }
            Err(_) => row.notice = Some(RecoveryNotice::ResetFailed),
        }
        cx.notify();
    }

    /// The state badge, the action for that state, and Troubleshoot whenever access is readable.
    pub(super) fn render_computer_use_access(
        &mut self,
        permission: ComputerUsePermission,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = permission_text(permission);
        let presentation = self.computer_use_access.row(permission).presentation();
        let state_selector = presentation.state_selector;
        let owner = cx.weak_entity();
        let action = presentation.action.map(|action| {
            let owner = owner.clone();
            match action {
                ComputerUseAccessAction::Request => {
                    action_button(text.request, "Request Access…", true, move |_, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.request_computer_use_access(permission, cx);
                        });
                    })
                }
                ComputerUseAccessAction::OpenSettings => action_button(
                    text.open_settings,
                    "Open System Settings",
                    true,
                    move |_, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.open_computer_use_settings(permission, cx);
                        });
                    },
                ),
                ComputerUseAccessAction::CheckAgain => {
                    action_button(text.check_again, "Check Again", true, move |_, cx| {
                        let _ = owner
                            .update(cx, |settings, cx| settings.refresh_computer_use_access(cx));
                    })
                }
            }
        });
        let troubleshoot = presentation.troubleshoot.map(|availability| {
            action_button(
                text.troubleshoot,
                "Troubleshoot…",
                availability == TroubleshootAvailability::Enabled,
                move |window, cx: &mut App| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.troubleshoot_computer_use_access(permission, window, cx);
                    });
                },
            )
        });
        div()
            .debug_selector(|| text.control.to_owned())
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .child(
                div()
                    .debug_selector(move || state_selector.to_owned())
                    .child(badge(presentation.state, appearance)),
            )
            .children(action)
            .children(troubleshoot)
            .into_any_element()
    }
}
