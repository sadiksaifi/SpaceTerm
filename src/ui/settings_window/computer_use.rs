//! The Privacy section's Screen Recording and Device Control rows: SpaceTerm's own grants for the
//! computer-use tools running in its Terminal Sessions, and how to set them up and recover them.
//!
//! A tool running in a Terminal Session takes screenshots and sends input through SpaceTerm's
//! grants, so these rows are where a person learns why such a tool reports missing access and
//! starts a Permission Setup. The Operating System reports only whether a grant is usable, and a
//! grant can stay switched on after it stops working, so every readable state keeps a way to
//! troubleshoot. Nothing here captures the screen or sends input to test access. A reset reaches
//! one permission of the running application only: Troubleshoot resets an entry after the person
//! confirms it, and Set Up clears an entry that does not grant the permission so the person can
//! add the running application again.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, SharedString, Task, Window, div};
use spaceterm_ui::{
    Alert, AlertIntent, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalId,
};

use crate::platform::computer_use_access::{
    AccessibilityNaming, ComputerUseAccess, ComputerUseAccessError, ComputerUseAccessSubscription,
    ComputerUseAuthorization, ComputerUsePermission,
};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::permission_setup::{
    PermissionCopy, PermissionSetup, PermissionSetupFailure, PermissionSetupStatus, permission_copy,
};

use super::SettingsWindow;
use crate::ui::sidebar_window::form::{action_button, badge};

/// The selectors of one permission's row.
pub(super) struct PermissionText {
    pub(super) control: &'static str,
    pub(super) set_up: &'static str,
    pub(super) cancel_setup: &'static str,
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
    control: "settings-screen-recording-access-control",
    set_up: "settings-screen-recording-access-set-up",
    cancel_setup: "settings-screen-recording-access-cancel-setup",
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

pub(super) const ACCESSIBILITY: PermissionText = PermissionText {
    control: "settings-accessibility-access-control",
    set_up: "settings-accessibility-access-set-up",
    cancel_setup: "settings-accessibility-access-cancel-setup",
    open_settings: "settings-accessibility-access-open-settings",
    check_again: "settings-accessibility-access-check-again",
    troubleshoot: "settings-accessibility-access-troubleshoot",
    state_allowed: "settings-accessibility-access-state-allowed",
    state_not_allowed: "settings-accessibility-access-state-not-allowed",
    state_unavailable: "settings-accessibility-access-state-unavailable",
    troubleshoot_modal: "settings-accessibility-troubleshoot",
    troubleshoot_open_settings: "settings-accessibility-troubleshoot-open-settings",
    troubleshoot_reset: "settings-accessibility-troubleshoot-reset",
    troubleshoot_done: "settings-accessibility-troubleshoot-done",
    reset_modal: "settings-accessibility-reset",
    reset_confirm: "settings-accessibility-reset-confirm",
    reset_cancel: "settings-accessibility-reset-cancel",
};

/// The permission a Settings Row presents, when it presents one of these.
pub(super) const fn row_permission(row: super::SettingsRowId) -> Option<ComputerUsePermission> {
    match row {
        super::SettingsRowId::ScreenRecordingAccess => Some(ComputerUsePermission::ScreenRecording),
        super::SettingsRowId::AccessibilityAccess => Some(ComputerUsePermission::Accessibility),
        _ => None,
    }
}

pub(super) const fn permission_text(permission: ComputerUsePermission) -> &'static PermissionText {
    match permission {
        ComputerUsePermission::ScreenRecording => &SCREEN_RECORDING,
        ComputerUsePermission::Accessibility => &ACCESSIBILITY,
    }
}

/// What SpaceTerm currently knows about one permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ComputerUseAccessStatus {
    /// No capability is composed, so access is neither readable nor changeable here.
    Unsupported,
    Authorization(ComputerUseAuthorization),
    /// Authorization could not be read.
    Failed(ComputerUseAccessError),
}

/// The primary action the row offers for its status, when there is one worth offering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ComputerUseAccessAction {
    /// Starts a Permission Setup.
    SetUp,
    /// Ends the running Permission Setup.
    CancelSetup,
    /// Opens the permission's list in System Settings, for a host that composes no Permission
    /// Setup.
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
    /// What System Settings calls the Accessibility permission, which holds for the process.
    naming: AccessibilityNaming,
    /// The running application, which is what a person turns on in System Settings.
    application_name: &'static str,
    status: ComputerUseAccessStatus,
    /// The permission's Permission Setup, or `None` when the host composes none.
    setup: Option<PermissionSetupStatus>,
    notice: Option<RecoveryNotice>,
    /// Returns the reset result to GPUI. Dropping the window drops the bridge.
    _reset: Option<Task<()>>,
}

impl ComputerUseAccessRow {
    fn new(
        permission: ComputerUsePermission,
        access: Option<Rc<dyn ComputerUseAccess>>,
        application_name: &'static str,
        setup: Option<PermissionSetupStatus>,
    ) -> Self {
        let mut row = Self {
            permission,
            naming: access
                .as_ref()
                .map_or_else(AccessibilityNaming::default, |access| {
                    access.accessibility_naming()
                }),
            access,
            application_name,
            status: ComputerUseAccessStatus::Unsupported,
            setup,
            notice: None,
            _reset: None,
        };
        row.refresh();
        row
    }

    /// Names the permission as System Settings does on the running system.
    pub(super) fn copy(&self) -> &'static PermissionCopy {
        permission_copy(self.permission, self.naming)
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
            && self
                .access
                .as_ref()
                .is_some_and(|access| access.can_reset())
    }

    fn finish_reset(&mut self, result: Option<Result<(), ComputerUseAccessError>>) {
        self.notice = None;
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
        let copy = self.copy();
        let name = copy.name;
        let application = self.application_name;
        let pane = copy.pane;
        let notice = self.notice.map(|notice| match notice {
            RecoveryNotice::OpenFailed => open_failed(application, pane),
            RecoveryNotice::Resetting => {
                format!("Resetting {name} for {application}…")
            }
            RecoveryNotice::ResetCompleted => format!(
                "The system no longer has a {name} decision for {application}. Choose Set Up to add \
                 {application} again."
            ),
            RecoveryNotice::ResetFailed => format!(
                "The system could not reset {name} for {application}. Remove {application} under \
                 Privacy & Security > {pane}, then add it again."
            ),
        });
        let troubleshoot =
            self.readable()
                .then_some(if self.notice == Some(RecoveryNotice::Resetting) {
                    TroubleshootAvailability::Busy
                } else {
                    TroubleshootAvailability::Enabled
                });

        let (state, state_selector, explanation, action) = match self.status {
            ComputerUseAccessStatus::Unsupported => (
                "Unavailable",
                text.state_unavailable,
                format!("{application} does not manage {name} access on this platform."),
                None,
            ),
            ComputerUseAccessStatus::Authorization(Authorization::Granted) => (
                "Allowed",
                text.state_allowed,
                notice.unwrap_or_else(|| {
                    format!(
                        "Computer-use tools running in {application} can {}. If a tool still \
                         reports \
                         missing access, choose Troubleshoot.",
                        copy.purpose
                    )
                }),
                None,
            ),
            ComputerUseAccessStatus::Authorization(Authorization::NotGranted) => {
                let (explanation, action) = match self.setup {
                    Some(PermissionSetupStatus::Running) => (
                        format!(
                            "Follow the guide in System Settings to add {application} to {pane}."
                        ),
                        Action::CancelSetup,
                    ),
                    Some(status) => {
                        let failure = match status {
                            PermissionSetupStatus::Failed(
                                PermissionSetupFailure::SettingsUnavailable,
                            ) => Some(open_failed(application, pane)),
                            PermissionSetupStatus::Failed(
                                PermissionSetupFailure::SettingsNotShown,
                            ) => Some(format!(
                                "System Settings did not come forward. Choose Set Up to try \
                                 again, or turn on {application} under Privacy & Security > \
                                 {pane}."
                            )),
                            PermissionSetupStatus::Idle | PermissionSetupStatus::Running => None,
                        };
                        (
                            notice.or(failure).unwrap_or_else(|| {
                                format!(
                                    "Computer-use tools running in {application} need this to \
                                     {}. \
                                     Choose Set Up to add {application} in System Settings.",
                                    copy.purpose
                                )
                            }),
                            Action::SetUp,
                        )
                    }
                    None => (
                        notice.unwrap_or_else(|| {
                            format!(
                                "Computer-use tools running in {application} need this to {}. \
                                 Turn on \
                                 {application} under Privacy & Security > {pane}.",
                                copy.purpose
                            )
                        }),
                        Action::OpenSettings,
                    ),
                };
                (
                    "Not Allowed",
                    text.state_not_allowed,
                    explanation,
                    Some(action),
                )
            }
            ComputerUseAccessStatus::Failed(error) => (
                "Unavailable",
                text.state_unavailable,
                match error {
                    ComputerUseAccessError::OffMainThread => {
                        format!("{application} could not check {name} access.")
                    }
                    ComputerUseAccessError::PlatformUnavailable => {
                        format!("The system did not report {name} access.")
                    }
                    ComputerUseAccessError::PlatformRejected => {
                        format!("The system rejected the {name} access check.")
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
        let copy = self.copy();
        let name = copy.name;
        let application = self.application_name;
        let pane = copy.pane;
        let title = format!("Troubleshoot {name}");
        let message = format!(
            "The {name} grant can stop working after {application} is updated or signed again, \
             while its switch stays on. {application} cannot detect this. If a tool reports {name} \
             access missing, try these steps in order, even while access shows Allowed."
        );
        let reopen =
            format!("3. If the tool still reports missing access, quit and reopen {application}.");
        let reset = if self
            .access
            .as_ref()
            .is_some_and(|access| access.can_reset())
        {
            " Alternatively, choose Reset Permission to clear the entry, then choose Set Up."
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
             grant, and {application} cannot change that policy."
        );
        (title, message, detail)
    }
}

fn open_failed(application: &str, pane: &str) -> String {
    format!(
        "System Settings could not be opened. Open it yourself and turn on {application} under \
         Privacy & Security > {pane}."
    )
}

/// Keeps the rows current with authorization changes the system reports while the window is open.
///
/// The system can answer a read with a value cached before a change until it reports that change,
/// which can arrive after the window became active again.
pub(super) struct ComputerUseAccessChanges {
    _subscription: Box<dyn ComputerUseAccessSubscription>,
    _refresh: Task<()>,
}

impl ComputerUseAccessChanges {
    pub(super) fn observe(
        access: Option<&Rc<dyn ComputerUseAccess>>,
        cx: &mut Context<SettingsWindow>,
    ) -> Option<Self> {
        let observation = access?.observe()?;
        let changed = observation.changed;
        let refresh = cx.spawn(async move |settings, cx| {
            while changed.recv().await.is_ok() {
                while changed.try_recv().is_ok() {}
                let refreshed = settings.update(cx, |settings, cx| {
                    settings.refresh_computer_use_access(cx);
                });
                if refreshed.is_err() {
                    break;
                }
            }
        });
        Some(Self {
            _subscription: observation.subscription,
            _refresh: refresh,
        })
    }
}

/// Both computer-use permission rows and the Permission Setup they start.
pub(super) struct ComputerUseAccessRows {
    screen_recording: ComputerUseAccessRow,
    accessibility: ComputerUseAccessRow,
    setup: Option<Entity<PermissionSetup>>,
}

impl ComputerUseAccessRows {
    pub(super) fn new(
        access: Option<Rc<dyn ComputerUseAccess>>,
        setup: Option<Entity<PermissionSetup>>,
        application_name: &'static str,
        cx: &App,
    ) -> Self {
        // A Permission Setup acts through the access capability, so it applies only beside one.
        let setup = setup.filter(|_| access.is_some());
        let status = |permission| {
            setup
                .as_ref()
                .map(|setup| setup.read(cx).status(permission))
        };
        Self {
            screen_recording: ComputerUseAccessRow::new(
                ComputerUsePermission::ScreenRecording,
                access.clone(),
                application_name,
                status(ComputerUsePermission::ScreenRecording),
            ),
            accessibility: ComputerUseAccessRow::new(
                ComputerUsePermission::Accessibility,
                access,
                application_name,
                status(ComputerUsePermission::Accessibility),
            ),
            setup,
        }
    }

    /// What System Settings calls the Accessibility permission on the running system.
    pub(super) fn naming(&self) -> AccessibilityNaming {
        self.accessibility.naming
    }

    pub(super) fn setup(&self) -> Option<&Entity<PermissionSetup>> {
        self.setup.as_ref()
    }

    /// Reads each permission's setup and authorization after the Permission Setup changed.
    pub(super) fn synchronize_setup(&mut self, cx: &App) {
        let Some(setup) = &self.setup else {
            return;
        };
        let setup = setup.read(cx);
        for permission in [
            ComputerUsePermission::ScreenRecording,
            ComputerUsePermission::Accessibility,
        ] {
            let row = self.row_mut(permission);
            row.setup = Some(setup.status(permission));
            row.refresh();
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
    /// Starts a Permission Setup of one permission. The setup reports its progress back to the
    /// row through its own notifications.
    pub(super) fn set_up_computer_use_access(
        &mut self,
        permission: ComputerUsePermission,
        cx: &mut Context<Self>,
    ) {
        let row = self.computer_use_access.row_mut(permission);
        if row.status
            != ComputerUseAccessStatus::Authorization(ComputerUseAuthorization::NotGranted)
        {
            return;
        }
        row.notice = None;
        if let Some(setup) = self.computer_use_access.setup().cloned() {
            setup.update(cx, |setup, cx| setup.start(&[permission], cx));
        }
        cx.notify();
    }

    pub(super) fn cancel_computer_use_setup(&mut self, cx: &mut Context<Self>) {
        if let Some(setup) = self.computer_use_access.setup().cloned() {
            setup.update(cx, |setup, cx| setup.cancel(cx));
        }
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
        let name = row.copy().name;
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
            "Afterward, choose Set Up to add {application} in System Settings again. Running \
             terminal sessions keep running."
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
                ComputerUseAccessAction::SetUp => {
                    action_button(text.set_up, "Set Up…", true, move |_, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.set_up_computer_use_access(permission, cx);
                        });
                    })
                }
                ComputerUseAccessAction::CancelSetup => {
                    action_button(text.cancel_setup, "Cancel Setup", true, move |_, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.cancel_computer_use_setup(cx);
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
