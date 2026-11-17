//! The Privacy section's Screen Recording and Device Control rows.
//! Nothing here captures the screen or sends input to test access.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, Task, Window};
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
    PermissionCopy, PermissionSetup, PermissionSetupFailure, PermissionSetupStatus, holder_name,
    permission_copy,
};

use super::SettingsWindow;
use super::access_row::{
    self, AccessAction, AccessControl, AccessPresentation, AccessSelectors, CaptionKind,
};

/// The selectors of one permission's row and of its reset confirmation.
pub(super) struct PermissionText {
    pub(super) row: AccessSelectors,
    pub(super) reset_modal: &'static str,
    pub(super) reset_confirm: &'static str,
    pub(super) reset_cancel: &'static str,
}

pub(super) const SCREEN_RECORDING: PermissionText = PermissionText {
    row: AccessSelectors {
        control: "settings-screen-recording-access-control",
        status: "settings-screen-recording-access-status",
        allow: "settings-screen-recording-access-allow",
        open_settings: "settings-screen-recording-access-open-settings",
        cancel: "settings-screen-recording-access-cancel",
        check_again: "settings-screen-recording-access-check-again",
        progress: "settings-screen-recording-access-progress",
    },
    reset_modal: "settings-screen-recording-reset",
    reset_confirm: "settings-screen-recording-reset-confirm",
    reset_cancel: "settings-screen-recording-reset-cancel",
};

pub(super) const ACCESSIBILITY: PermissionText = PermissionText {
    row: AccessSelectors {
        control: "settings-accessibility-access-control",
        status: "settings-accessibility-access-status",
        allow: "settings-accessibility-access-allow",
        open_settings: "settings-accessibility-access-open-settings",
        cancel: "settings-accessibility-access-cancel",
        check_again: "settings-accessibility-access-check-again",
        progress: "settings-accessibility-access-progress",
    },
    reset_modal: "settings-accessibility-reset",
    reset_confirm: "settings-accessibility-reset-confirm",
    reset_cancel: "settings-accessibility-reset-cancel",
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

/// What the latest recovery step reported, kept beside the status it tried to change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecoveryNotice {
    OpenFailed,
    Resetting,
    ResetFailed,
}

/// The reset confirmation's decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResetDecision {
    Reset,
    Cancel,
}

/// Owns one permission's injected capability, last known status, and pending reset.
pub(super) struct PermissionAccessRow {
    permission: SystemPermission,
    access: Option<Rc<dyn PermissionAccess>>,
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
        setup: Option<PermissionSetupStatus>,
    ) -> Self {
        let mut row = Self {
            permission,
            access,
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
        permission_copy(self.permission, self.naming())
    }

    /// Reads the current authorization, which can change in System Settings at any time.
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

    /// What System Settings calls the Accessibility permission, as the server reports it.
    fn naming(&self) -> AccessibilityNaming {
        self.access
            .as_ref()
            .map_or_else(AccessibilityNaming::default, |access| {
                access.accessibility_naming()
            })
    }

    /// What System Settings lists the grant under, which is what a person turns on.
    fn holder_name(&self) -> String {
        holder_name(self.access.as_ref().map(|access| access.holder()))
    }

    pub(super) fn presentation(&self) -> AccessPresentation {
        use PermissionAuthorization as Authorization;

        let copy = self.copy();
        let name = copy.name;
        let application = self.holder_name();
        let application = application.as_str();
        let pane = copy.pane;
        let purpose = (
            CaptionKind::Guidance,
            format!("Lets terminal programs {}.", copy.purpose),
        );
        let allowed = AccessControl::Status {
            label: "Allowed",
            allowed: true,
        };
        let waiting = AccessControl::Status {
            label: "Waiting",
            allowed: false,
        };
        let notice = self.notice.map(|notice| match notice {
            RecoveryNotice::OpenFailed => (CaptionKind::Error, open_failed(application, pane)),
            RecoveryNotice::Resetting => {
                (CaptionKind::Waiting, format!("Resetting {name}\u{2026}"))
            }
            RecoveryNotice::ResetFailed => (
                CaptionKind::Error,
                format!("Couldn\u{2019}t reset {name}. Try again."),
            ),
        });

        let (control, (caption_kind, caption)) = match self.status {
            // A running reset owns the row until it reports, whatever the reads in between say,
            // failed reads included.
            _ if self.resetting() => (waiting, notice.unwrap_or(purpose)),
            PermissionAccessStatus::Unsupported => (
                AccessControl::Status {
                    label: "Unavailable",
                    allowed: false,
                },
                (
                    CaptionKind::Guidance,
                    "Unavailable on this platform.".to_owned(),
                ),
            ),
            PermissionAccessStatus::Authorization(Authorization::Granted) => {
                (allowed, notice.unwrap_or(purpose))
            }
            PermissionAccessStatus::Authorization(Authorization::NotGranted) => match self.setup {
                Some(PermissionSetupStatus::Running) => (
                    AccessControl::Action(AccessAction::Cancel),
                    (
                        CaptionKind::Waiting,
                        "Continue in System Settings.".to_owned(),
                    ),
                ),
                Some(status) => {
                    let failure = match status {
                        PermissionSetupStatus::Failed(
                            PermissionSetupFailure::SettingsUnavailable,
                        ) => Some((CaptionKind::Error, open_failed(application, pane))),
                        PermissionSetupStatus::Failed(PermissionSetupFailure::SettingsNotShown) => {
                            Some((
                                CaptionKind::Error,
                                "Couldn\u{2019}t open System Settings. Choose Allow\u{2026} to try again."
                                    .to_owned(),
                            ))
                        }
                        PermissionSetupStatus::Idle | PermissionSetupStatus::Running => None,
                    };
                    (
                        AccessControl::Action(AccessAction::Allow),
                        notice.or(failure).unwrap_or(purpose),
                    )
                }
                None => (
                    AccessControl::Action(AccessAction::OpenSettings),
                    notice.unwrap_or(purpose),
                ),
            },
            PermissionAccessStatus::Failed(error) => (
                AccessControl::Action(AccessAction::CheckAgain),
                (
                    CaptionKind::Error,
                    match error {
                        PermissionAccessError::PlatformUnavailable => {
                            format!("Couldn\u{2019}t check {name} access.")
                        }
                        PermissionAccessError::PlatformRejected => {
                            format!("{name} access check failed.")
                        }
                    },
                ),
            ),
        };
        AccessPresentation {
            control,
            caption: caption.into(),
            caption_kind,
            resettable: self.status
                == PermissionAccessStatus::Authorization(Authorization::Granted)
                && self.can_reset(),
        }
    }
}

fn open_failed(application: &str, pane: &str) -> String {
    format!("Open System Settings and allow {application} in Privacy & Security > {pane}.")
}

/// Keeps the rows current with authorization changes the system reports while the window is open. A
/// read can return a cached value until the system reports the change.
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
                // One server report holds every System Permission, so a change also refreshes the
                // microphone and camera rows.
                let refreshed = settings.update(cx, |settings, cx| {
                    settings.refresh_permission_access(cx);
                    settings.refresh_capture_access(cx);
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
                status(SystemPermission::ScreenRecording),
            ),
            accessibility: PermissionAccessRow::new(
                SystemPermission::Accessibility,
                access,
                status(SystemPermission::Accessibility),
            ),
            setup,
        }
    }

    /// What System Settings calls the Accessibility permission on the running system.
    pub(super) fn naming(&self) -> AccessibilityNaming {
        self.accessibility.naming()
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

    /// Asks before resetting an allowed grant, because the reset removes access until the person
    /// allows it again.
    pub(super) fn confirm_permission_reset(
        &mut self,
        permission: SystemPermission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = self.permission_access.row(permission);
        if !row.presentation().resettable {
            return;
        }
        let text = permission_text(permission);
        let name = row.copy().name;
        let application = row.holder_name();
        let title = format!("Reset {name}?");
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new(text.reset_modal),
            title.clone(),
            title,
            format!("{application} loses {name} access, then System Settings opens so you can allow it again."),
            vec![
                ModalAction::new(
                    ResetDecision::Reset,
                    "Reset",
                    ModalActionRole::Affirmative,
                    text.reset_confirm,
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    ResetDecision::Cancel,
                    "Cancel",
                    ModalActionRole::Cancel,
                    text.reset_cancel,
                ),
            ],
        )
        .detail(
            "If a program can\u{2019}t use it, quit and reopen the program first. Some programs use a helper app that needs its own permission.",
        )
        .present(window, cx, move |outcome, cx| {
            let AlertOutcome::Activated {
                action_id: ResetDecision::Reset,
                ..
            } = outcome
            else {
                return;
            };
            let _ = owner.update(cx, |settings, cx| settings.reset_permission(permission, cx));
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm permission reset alert");
        }
    }

    /// Runs a chosen reset, then allows the running application again. The native completion may
    /// run on any thread, so it sends the result through a channel to the foreground executor.
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

    /// The row's one trailing control: its next step, or its state.
    pub(super) fn render_permission_access(
        &mut self,
        permission: SystemPermission,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let control = self
            .permission_access
            .row(permission)
            .presentation()
            .control;
        let owner = cx.weak_entity();
        access_row::render_control(
            &permission_text(permission).row,
            control,
            appearance,
            move |action, _, cx| {
                let _ = owner.update(cx, |settings, cx| match action {
                    AccessAction::Allow => settings.set_up_permission(permission, cx),
                    AccessAction::Cancel => settings.cancel_permission_setup(cx),
                    AccessAction::OpenSettings => {
                        settings.open_permission_settings(permission, cx);
                    }
                    AccessAction::CheckAgain => settings.refresh_permission_access(cx),
                });
            },
        )
    }
}

#[cfg(test)]
#[path = "permission_access_tests.rs"]
mod tests;
