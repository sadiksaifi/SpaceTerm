//! The Privacy section's Screen Recording and Device Control rows: SpaceTerm's own grants, which
//! programs running in its Terminal Sessions inherit, and how to set them up and recover them.
//!
//! Each row offers one action for its state. Set Up starts a Permission Setup, which also clears an
//! entry that grants nothing, so a missing grant needs no other recovery. An allowed grant offers
//! Troubleshoot, because a program can still report missing access. Its alert resets one
//! permission of the running application only after the person chooses Reset, then starts a
//! Permission Setup again. Nothing here captures the screen or sends input to test access.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, SharedString, Task, Window, div};
use spaceterm_ui::{
    Alert, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole,
    ModalId,
};

use crate::platform::permission_access::{
    AccessibilityNaming, PermissionAccess, PermissionAccessError, PermissionAccessSubscription,
    PermissionAuthorization, SystemPermission,
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
    pub(super) troubleshoot_cancel: &'static str,
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
    troubleshoot_cancel: "settings-screen-recording-troubleshoot-cancel",
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
    troubleshoot_cancel: "settings-accessibility-troubleshoot-cancel",
};

/// The permission a Settings Row presents, when it presents one of these.
pub(super) const fn row_permission(row: super::SettingsRowId) -> Option<SystemPermission> {
    match row {
        super::SettingsRowId::ScreenRecordingAccess => Some(SystemPermission::ScreenRecording),
        super::SettingsRowId::AccessibilityAccess => Some(SystemPermission::Accessibility),
        _ => None,
    }
}

pub(super) const fn permission_text(permission: SystemPermission) -> &'static PermissionText {
    match permission {
        SystemPermission::ScreenRecording => &SCREEN_RECORDING,
        SystemPermission::Accessibility => &ACCESSIBILITY,
    }
}

/// What SpaceTerm currently knows about one permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PermissionAccessStatus {
    /// No capability is composed, so access is neither readable nor changeable here.
    Unsupported,
    Authorization(PermissionAuthorization),
    /// Authorization could not be read.
    Failed(PermissionAccessError),
}

/// The one action the row offers for its status, when there is one worth offering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PermissionAccessAction {
    /// Starts a Permission Setup.
    SetUp,
    /// Ends the running Permission Setup.
    CancelSetup,
    /// Opens the permission's list in System Settings, for a host that composes no Permission
    /// Setup.
    OpenSettings,
    CheckAgain,
    /// Presents the recovery for a grant that a program still reports missing.
    Troubleshoot,
}

/// What the latest recovery step reported, kept beside the status it tried to change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecoveryNotice {
    OpenFailed,
    Resetting,
    ResetFailed,
}

/// The row's complete presentation, derived from its state so every state reads one way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PermissionAccessPresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: SharedString,
    pub(super) action: Option<PermissionAccessAction>,
    /// Whether the action accepts activation, which it does except while a reset runs.
    pub(super) action_enabled: bool,
}

/// The troubleshooting alert's decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TroubleshootDecision {
    Reset,
    OpenSettings,
    Cancel,
}

/// Owns one permission's injected capability, last known status, and pending reset.
pub(super) struct PermissionAccessRow {
    permission: SystemPermission,
    access: Option<Rc<dyn PermissionAccess>>,
    /// What System Settings calls the Accessibility permission, which holds for the process.
    naming: AccessibilityNaming,
    /// The running application, which is what a person turns on in System Settings.
    application_name: &'static str,
    status: PermissionAccessStatus,
    /// The permission's Permission Setup, or `None` when the host composes none.
    setup: Option<PermissionSetupStatus>,
    notice: Option<RecoveryNotice>,
    /// Returns the reset result to GPUI. Dropping the window drops the bridge.
    _reset: Option<Task<()>>,
}

impl PermissionAccessRow {
    fn new(
        permission: SystemPermission,
        access: Option<Rc<dyn PermissionAccess>>,
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
            status: PermissionAccessStatus::Unsupported,
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

    /// Reads the current authorization, which can change in System Settings at any time.
    ///
    /// Opening System Settings never changes the status by itself; only a later read does.
    pub(super) fn refresh(&mut self) {
        self.apply(self.read());
    }

    fn read(&self) -> PermissionAccessStatus {
        match &self.access {
            None => PermissionAccessStatus::Unsupported,
            Some(access) => match access.authorization(self.permission) {
                Ok(authorization) => PermissionAccessStatus::Authorization(authorization),
                Err(error) => PermissionAccessStatus::Failed(error),
            },
        }
    }

    /// A notice describes the status it was reported against, so a changed status retires it.
    /// A running reset keeps its notice until the reset reports.
    fn apply(&mut self, status: PermissionAccessStatus) {
        if status != self.status && self.notice != Some(RecoveryNotice::Resetting) {
            self.notice = None;
        }
        self.status = status;
    }

    fn readable(&self) -> bool {
        matches!(self.status, PermissionAccessStatus::Authorization(_))
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

    fn resetting(&self) -> bool {
        self.notice == Some(RecoveryNotice::Resetting)
    }

    fn can_reset(&self) -> bool {
        self.readable()
            && !self.resetting()
            && self
                .access
                .as_ref()
                .is_some_and(|access| access.can_reset())
    }

    /// Applies a reset's result and reports whether the reset succeeded. The capability verifies
    /// authorization before it reports, so the read here already reflects the reset.
    fn finish_reset(&mut self, result: Option<Result<(), PermissionAccessError>>) -> bool {
        self.notice = None;
        self.refresh();
        let succeeded = matches!(result, Some(Ok(())));
        if !succeeded {
            self.notice = Some(RecoveryNotice::ResetFailed);
        }
        succeeded
    }

    pub(super) fn presentation(&self) -> PermissionAccessPresentation {
        use PermissionAccessAction as Action;
        use PermissionAuthorization as Authorization;

        let text = permission_text(self.permission);
        let copy = self.copy();
        let name = copy.name;
        let application = self.application_name;
        let pane = copy.pane;
        let purpose = format!("Lets terminal programs {}.", copy.purpose);
        let notice = self.notice.map(|notice| match notice {
            RecoveryNotice::OpenFailed => open_failed(application, pane),
            RecoveryNotice::Resetting => format!("Resetting {name}…"),
            RecoveryNotice::ResetFailed => format!(
                "{name} could not be reset. Remove {application} from Privacy & Security > {pane}, \
                 then add it again."
            ),
        });

        let (state, state_selector, explanation, action) = match self.status {
            PermissionAccessStatus::Unsupported => (
                "Unavailable",
                text.state_unavailable,
                "Not available on this platform.".to_owned(),
                None,
            ),
            PermissionAccessStatus::Authorization(Authorization::Granted) => (
                "Allowed",
                text.state_allowed,
                notice.unwrap_or(purpose),
                Some(Action::Troubleshoot),
            ),
            PermissionAccessStatus::Authorization(Authorization::NotGranted) => {
                let (explanation, action) = match self.setup {
                    Some(PermissionSetupStatus::Running) => (
                        "Continue in System Settings.".to_owned(),
                        Action::CancelSetup,
                    ),
                    Some(status) => {
                        let failure = match status {
                            PermissionSetupStatus::Failed(
                                PermissionSetupFailure::SettingsUnavailable,
                            ) => Some(open_failed(application, pane)),
                            PermissionSetupStatus::Failed(
                                PermissionSetupFailure::SettingsNotShown,
                            ) => Some(
                                "System Settings did not open. Choose Set Up to try again."
                                    .to_owned(),
                            ),
                            PermissionSetupStatus::Idle | PermissionSetupStatus::Running => None,
                        };
                        (notice.or(failure).unwrap_or(purpose), Action::SetUp)
                    }
                    None => (notice.unwrap_or(purpose), Action::OpenSettings),
                };
                (
                    "Not Allowed",
                    text.state_not_allowed,
                    explanation,
                    Some(action),
                )
            }
            PermissionAccessStatus::Failed(error) => (
                "Unavailable",
                text.state_unavailable,
                match error {
                    PermissionAccessError::OffMainThread => {
                        format!("{application} could not check {name} access.")
                    }
                    PermissionAccessError::PlatformUnavailable => {
                        format!("The system did not report {name} access.")
                    }
                    PermissionAccessError::PlatformRejected => {
                        format!("The system rejected the {name} access check.")
                    }
                },
                Some(Action::CheckAgain),
            ),
        };
        PermissionAccessPresentation {
            state,
            state_selector,
            explanation: explanation.into(),
            action,
            action_enabled: !self.resetting(),
        }
    }
}

fn open_failed(application: &str, pane: &str) -> String {
    format!(
        "System Settings could not be opened. Turn on {application} in Privacy & Security > \
         {pane}."
    )
}

/// Keeps the rows current with authorization changes the system reports while the window is open.
///
/// The system can answer a read with a value cached before a change until it reports that change,
/// which can arrive after the window became active again.
pub(super) struct PermissionAccessChanges {
    _subscription: Box<dyn PermissionAccessSubscription>,
    _refresh: Task<()>,
}

impl PermissionAccessChanges {
    pub(super) fn observe(
        access: Option<&Rc<dyn PermissionAccess>>,
        cx: &mut Context<SettingsWindow>,
    ) -> Option<Self> {
        let observation = access?.observe()?;
        let changed = observation.changed;
        let refresh = cx.spawn(async move |settings, cx| {
            while changed.recv().await.is_ok() {
                while changed.try_recv().is_ok() {}
                let refreshed = settings.update(cx, |settings, cx| {
                    settings.refresh_permission_access(cx);
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

/// Both System Permission rows and the Permission Setup they start.
pub(super) struct PermissionAccessRows {
    screen_recording: PermissionAccessRow,
    accessibility: PermissionAccessRow,
    setup: Option<Entity<PermissionSetup>>,
}

impl PermissionAccessRows {
    pub(super) fn new(
        access: Option<Rc<dyn PermissionAccess>>,
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
            screen_recording: PermissionAccessRow::new(
                SystemPermission::ScreenRecording,
                access.clone(),
                application_name,
                status(SystemPermission::ScreenRecording),
            ),
            accessibility: PermissionAccessRow::new(
                SystemPermission::Accessibility,
                access,
                application_name,
                status(SystemPermission::Accessibility),
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
            SystemPermission::ScreenRecording,
            SystemPermission::Accessibility,
        ] {
            let row = self.row_mut(permission);
            row.setup = Some(setup.status(permission));
            row.refresh();
        }
    }

    pub(super) fn row(&self, permission: SystemPermission) -> &PermissionAccessRow {
        match permission {
            SystemPermission::ScreenRecording => &self.screen_recording,
            SystemPermission::Accessibility => &self.accessibility,
        }
    }

    fn row_mut(&mut self, permission: SystemPermission) -> &mut PermissionAccessRow {
        match permission {
            SystemPermission::ScreenRecording => &mut self.screen_recording,
            SystemPermission::Accessibility => &mut self.accessibility,
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
    pub(super) fn set_up_permission(
        &mut self,
        permission: SystemPermission,
        cx: &mut Context<Self>,
    ) {
        let row = self.permission_access.row_mut(permission);
        if row.status != PermissionAccessStatus::Authorization(PermissionAuthorization::NotGranted)
            || row.resetting()
        {
            return;
        }
        row.notice = None;
        if let Some(setup) = self.permission_access.setup().cloned() {
            setup.update(cx, |setup, cx| setup.start(&[permission], cx));
        }
        cx.notify();
    }

    pub(super) fn cancel_permission_setup(&mut self, cx: &mut Context<Self>) {
        if let Some(setup) = self.permission_access.setup().cloned() {
            setup.update(cx, |setup, cx| setup.cancel(cx));
        }
        cx.notify();
    }

    pub(super) fn open_permission_settings(
        &mut self,
        permission: SystemPermission,
        cx: &mut Context<Self>,
    ) {
        self.permission_access.row_mut(permission).open_settings();
        cx.notify();
    }

    pub(super) fn refresh_permission_access(&mut self, cx: &mut Context<Self>) {
        self.permission_access.refresh();
        cx.notify();
    }

    /// Presents the recovery for an allowed grant that a program still reports missing: reopen
    /// the program, then reset the permission and allow it again.
    pub(super) fn troubleshoot_permission(
        &mut self,
        permission: SystemPermission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = self.permission_access.row(permission);
        if row.status != PermissionAccessStatus::Authorization(PermissionAuthorization::Granted)
            || row.resetting()
        {
            return;
        }
        let text = permission_text(permission);
        let copy = row.copy();
        let name = copy.name;
        let pane = copy.pane;
        let application = row.application_name;
        let title = format!("{name} Not Working?");
        let reopen = "Quit and reopen the program that reports missing access.";
        let (message, primary) = if row.can_reset() {
            (
                format!(
                    "{reopen} If that does not help, reset {name} and allow {application} again."
                ),
                ModalAction::new(
                    TroubleshootDecision::Reset,
                    "Reset",
                    ModalActionRole::Affirmative,
                    text.troubleshoot_reset,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
            )
        } else {
            (
                format!(
                    "{reopen} If that does not help, remove {application} from Privacy & Security \
                     > {pane} and add it again."
                ),
                ModalAction::new(
                    TroubleshootDecision::OpenSettings,
                    "Open System Settings",
                    ModalActionRole::Affirmative,
                    text.troubleshoot_open_settings,
                )
                .default_action(true),
            )
        };
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new(text.troubleshoot_modal),
            title.clone(),
            title,
            message,
            vec![
                primary,
                ModalAction::new(
                    TroubleshootDecision::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    text.troubleshoot_cancel,
                ),
            ],
        )
        .detail("Some programs use a helper app that needs its own permission.")
        .present(window, cx, move |outcome, cx| {
            let AlertOutcome::Activated { action_id, .. } = outcome else {
                return;
            };
            let _ = owner.update(cx, |settings, cx| match action_id {
                TroubleshootDecision::Reset => settings.reset_permission(permission, cx),
                TroubleshootDecision::OpenSettings => {
                    settings.open_permission_settings(permission, cx);
                }
                TroubleshootDecision::Cancel => {}
            });
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm permission troubleshooting alert");
        }
    }

    /// Runs a chosen reset, then allows the running application again. The native completion may
    /// run on any thread, so it only sends the closed result through a channel and the window
    /// applies it on the foreground executor.
    pub(super) fn reset_permission(
        &mut self,
        permission: SystemPermission,
        cx: &mut Context<Self>,
    ) {
        let row = self.permission_access.row_mut(permission);
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
                        let reset = settings
                            .permission_access
                            .row_mut(permission)
                            .finish_reset(result);
                        if reset {
                            settings.allow_permission_again(permission, cx);
                        }
                        cx.notify();
                    });
                }));
            }
            Err(_) => row.notice = Some(RecoveryNotice::ResetFailed),
        }
        cx.notify();
    }

    /// Leads to the permission's list after a reset: through a Permission Setup when the host
    /// composes one, or straight to System Settings otherwise.
    fn allow_permission_again(&mut self, permission: SystemPermission, cx: &mut Context<Self>) {
        if self.permission_access.setup().is_some() {
            self.set_up_permission(permission, cx);
        } else {
            self.open_permission_settings(permission, cx);
        }
    }

    /// The state badge and, when one applies, the action beside it.
    pub(super) fn render_permission_access(
        &mut self,
        permission: SystemPermission,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = permission_text(permission);
        let presentation = self.permission_access.row(permission).presentation();
        let state_selector = presentation.state_selector;
        let enabled = presentation.action_enabled;
        let owner = cx.weak_entity();
        let action = presentation.action.map(|action| match action {
            PermissionAccessAction::SetUp => {
                action_button(text.set_up, "Set Up…", enabled, move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.set_up_permission(permission, cx);
                    });
                })
            }
            PermissionAccessAction::CancelSetup => {
                action_button(text.cancel_setup, "Cancel Setup", enabled, move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.cancel_permission_setup(cx);
                    });
                })
            }
            PermissionAccessAction::OpenSettings => action_button(
                text.open_settings,
                "Open System Settings",
                enabled,
                move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.open_permission_settings(permission, cx);
                    });
                },
            ),
            PermissionAccessAction::CheckAgain => {
                action_button(text.check_again, "Check Again", enabled, move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| settings.refresh_permission_access(cx));
                })
            }
            PermissionAccessAction::Troubleshoot => action_button(
                text.troubleshoot,
                "Troubleshoot…",
                enabled,
                move |window, cx: &mut App| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.troubleshoot_permission(permission, window, cx);
                    });
                },
            ),
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
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "permission_access_tests.rs"]
mod tests;
